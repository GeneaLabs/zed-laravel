//! Salsa 0.25 incremental computation database for Laravel LSP
//!
//! This module provides proper incremental computation using the Salsa framework.
//! It replaces the custom "Salsa-inspired" implementation in salsa_db.rs.
//!
//! # Actor Pattern for Async Integration
//!
//! Since Salsa's `Storage` type is not `Send+Sync`, we use an actor pattern to
//! run Salsa operations on a dedicated thread. The `SalsaActor` owns the database
//! and processes requests via channels.
#![allow(dead_code)]

use lru::LruCache;
use salsa::Setter;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, RwLock};
use tower_lsp::lsp_types::Url;
use tracing::{debug, info};

use crate::config::kebab_to_pascal_case;
use crate::middleware_parser::middleware_base_alias;
use crate::parser::{language_php, parse_php};
use crate::queries::extract_all_php_patterns;
// Single source of truth for the strip-then-join rule applied to relative
// fragments captured from PHP source — a leading `/` would otherwise make
// `Path::join` discard the base directory (issues #285, #290).
use crate::path_join::join_relative;
// Single source of truth for lexical path normalization. The local copy this
// replaced popped unconditionally on `..`, so a `..` walking past root would
// pop the `RootDir` component and silently relativize an absolute path (#117).
// `route_discovery::normalize_path` pops only a preceding `Normal` segment and
// preserves root / leading `..`. Every other module already delegates here.
use crate::route_discovery::normalize_path;
// The lexical (non-fail-closed) entry point of the shared containment guard —
// admits speculative candidates that don't exist on disk yet (issue #156).
use crate::path_containment::path_within_root_lexical;
// The two guards `ensure_external_php_source_loaded` splits across its
// branches (issue #364): the emit-safe one for the client-owned path it
// returns without reading, the registration one for the path it reads.
use crate::path_containment::{canonical_within_root_registration, path_within_root_emit_safe};

// ============================================================================
// Database Definition
// ============================================================================

/// Body-execution counters for the memoized backing-class queries (#339,
/// item 7). Incremented inside a query's BODY, so the count answers the one
/// question a return value cannot: was this served from the memo, or recomputed?
///
/// Per-database rather than a process-wide static, so a test measuring one
/// database is not perturbed by whatever the Salsa actor thread is doing in
/// another test. Shared across clones of the database, which is what Salsa's
/// own snapshotting produces.
#[derive(Debug, Default)]
pub struct QueryRunCounts {
    /// Body runs of [`render_source_files`].
    pub render_source_files: AtomicUsize,
    /// Body runs of [`blade_backing_class_sources`].
    pub blade_backing_class_sources: AtomicUsize,
}

impl QueryRunCounts {
    /// A plain snapshot of the counters, for transfer across the actor's
    /// async boundary.
    pub fn snapshot(&self) -> QueryRunCountsData {
        QueryRunCountsData {
            render_source_files: self.render_source_files.load(Ordering::Relaxed),
            blade_backing_class_sources: self.blade_backing_class_sources.load(Ordering::Relaxed),
        }
    }
}

/// Data transfer type for [`QueryRunCounts`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryRunCountsData {
    pub render_source_files: usize,
    pub blade_backing_class_sources: usize,
}

/// The Salsa database trait for Laravel LSP
#[salsa::db]
pub trait Db: salsa::Database {
    /// This database's query body-execution counters. See [`QueryRunCounts`].
    fn query_run_counts(&self) -> &QueryRunCounts;
}

/// The concrete database implementation
#[salsa::db]
#[derive(Default, Clone)]
pub struct LaravelDatabase {
    storage: salsa::Storage<Self>,
    /// Shared with every clone of this database, so a snapshot's query runs
    /// are counted against the same totals.
    run_counts: Arc<QueryRunCounts>,
}

#[salsa::db]
impl salsa::Database for LaravelDatabase {}

#[salsa::db]
impl Db for LaravelDatabase {
    fn query_run_counts(&self) -> &QueryRunCounts {
        &self.run_counts
    }
}

// ============================================================================
// Input Types - Source data provided to the system
// ============================================================================

/// Represents a source file in the workspace
#[salsa::input]
pub struct SourceFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// The document version from LSP
    #[returns(copy)]
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,
}

/// Represents a configuration file (composer.json, config/*.php)
#[salsa::input]
pub struct ConfigFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    #[returns(copy)]
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,
}

/// Represents the project's registered files for reference finding
/// Files are grouped by their source directory type
#[salsa::input]
pub struct ProjectFiles {
    /// Version incremented when file list changes
    #[returns(copy)]
    pub version: i32,

    /// PHP files in app/Http/Controllers
    #[returns(ref)]
    pub controller_files: Vec<PathBuf>,

    /// Blade files in view paths
    #[returns(ref)]
    pub view_files: Vec<PathBuf>,

    /// PHP files in app/Livewire
    #[returns(ref)]
    pub livewire_files: Vec<PathBuf>,

    /// PHP files in routes/
    #[returns(ref)]
    pub route_files: Vec<PathBuf>,
}

/// The project-wide render index: every `(view name, rendering file)` pair
/// the controller / Livewire scan has observed.
///
/// The flattened reverse of `ViewVarIndex::by_file`. Holding it as a Salsa
/// input is what turns backing-class resolution from an O(entire index) linear
/// sweep *per keystroke* into a memoized lookup that only recomputes when the
/// index itself changes (issue #339, item 7).
#[salsa::input]
pub struct RenderIndex {
    /// Version incremented when the index contents change
    #[returns(copy)]
    pub version: i32,

    /// `(view name, file that renders it)`, one entry per render site
    #[returns(ref)]
    pub entries: Vec<(String, PathBuf)>,
}

/// Represents a service provider file with priority
#[salsa::input]
pub struct ServiceProviderFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    #[returns(copy)]
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,

    /// Priority: 0=framework, 1=package, 2=module, 3=app
    #[returns(copy)]
    pub priority: u8,
}

/// Represents an environment file (.env, .env.local, .env.example)
#[salsa::input]
pub struct EnvFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when file changes
    #[returns(copy)]
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,

    /// Priority: 0=.env.example, 1=.env.local, 2=.env (highest)
    #[returns(copy)]
    pub priority: u8,
}

/// One Laravel translation catalogue: a `{lang_root}/{locale}/{file}.php` array
/// file, a `{lang_root}/vendor/{namespace}/{locale}/{file}.php` published
/// override, or a `{lang_root}/{locale}.json` text catalogue.
///
/// Registered lazily, once per path, by [`SalsaActor::ensure_lang_file`] — a
/// path that is absent, unreadable, or refused by the containment guard is
/// registered with **empty text**, which resolves to no key just as an absent
/// file does. That negative entry is what stops a 25-locale lookup from
/// re-probing 24 missing files on every hover (issue #293).
#[salsa::input]
pub struct LangFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when the file changes
    #[returns(copy)]
    pub version: i32,

    /// The file content; empty for an absent or refused file
    #[returns(ref)]
    pub text: String,
}

/// One directory that may contain locales — a lang root, a published
/// `{lang_root}/vendor/{namespace}` override dir, or a package's own lang dir
/// registered via `loadTranslationsFrom`.
///
/// Holds the directory's direct children rather than re-running `read_dir` per
/// lookup. Like [`LangFile`], an absent or refused directory is registered with
/// an **empty** listing, which contributes no locales exactly as a failed
/// `read_dir` did (issue #293).
#[salsa::input]
pub struct LangDir {
    /// The directory path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when the directory listing changes
    #[returns(copy)]
    pub version: i32,

    /// `(file name, is_dir)` for every direct child
    #[returns(ref)]
    pub entries: Vec<(String, bool)>,
}

/// One service provider that may register translation namespaces — a
/// `*ServiceProvider*.php` under `vendor/`, or any `.php` under
/// `app/Providers/`.
///
/// Separate from [`LangFile`] despite the identical shape: a provider is not a
/// catalogue, and invalidating one has different consequences (it drops the
/// namespace map, not a directory listing).
#[salsa::input]
pub struct TranslationProviderFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// Version incremented when the file changes
    #[returns(copy)]
    pub version: i32,

    /// The file content; empty for an absent or unreadable provider
    #[returns(ref)]
    pub text: String,
}

/// The discovered provider files, split by scan so their precedence survives:
/// app providers override vendor ones on a namespace conflict, because the app
/// boots last.
///
/// An input rather than a plain field so the walk is a tracked dependency like
/// [`ProjectFiles`], and a provider create/delete invalidates what derives from
/// it (issue #293).
#[salsa::input]
pub struct TranslationProviderFiles {
    /// Version incremented when the discovered set changes
    #[returns(copy)]
    pub version: i32,

    /// `*ServiceProvider*.php` under `vendor/`
    #[returns(ref)]
    pub vendor: Vec<PathBuf>,

    /// `.php` under `app/Providers/`
    #[returns(ref)]
    pub app: Vec<PathBuf>,
}

// ============================================================================
// Interned Types - Deduplicated strings
// ============================================================================

/// Interned string for view names (e.g., "users.profile")
#[salsa::interned]
pub struct ViewName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for component names (e.g., "button")
#[salsa::interned]
pub struct ComponentName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for directive names (e.g., "extends")
#[salsa::interned]
pub struct DirectiveName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for env variable names (e.g., "APP_DEBUG")
#[salsa::interned]
pub struct EnvVarName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for config keys (e.g., "app.name")
#[salsa::interned]
pub struct ConfigKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for middleware names (e.g., "auth", "throttle:60,1")
#[salsa::interned]
pub struct MiddlewareName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for translation keys (e.g., "messages.welcome")
#[salsa::interned]
pub struct TranslationKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for asset paths (e.g., "css/app.css")
#[salsa::interned]
pub struct AssetPath<'db> {
    #[returns(ref)]
    pub path: String,
}

/// Interned string for binding names (e.g., "auth", "App\\Contracts\\PaymentGateway")
#[salsa::interned]
pub struct BindingName<'db> {
    #[returns(ref)]
    pub name: String,
}

#[salsa::interned]
pub struct RouteName<'db> {
    #[returns(ref)]
    pub name: String,
}

#[salsa::interned]
pub struct UrlPath<'db> {
    #[returns(ref)]
    pub path: String,
}

#[salsa::interned]
pub struct ActionName<'db> {
    #[returns(ref)]
    pub action: String,
}

/// Interned string for package view namespace (e.g., "courier", "mail")
#[salsa::interned]
pub struct PackageNamespace<'db> {
    #[returns(ref)]
    pub namespace: String,
}

// ============================================================================
// Tracked Types - Computed/derived values
// ============================================================================

/// A parsed view reference found in code
#[salsa::tracked]
pub struct ViewReference<'db> {
    pub name: ViewName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
    #[returns(copy)]
    pub is_route_view: bool,
    /// `$view = '…'` class-property site — goto/hover yes, missing-view
    /// diagnostic no (see `ViewMatch::is_property_site`).
    #[returns(copy)]
    pub is_property_site: bool,
}

/// A parsed component reference found in code
#[salsa::tracked]
pub struct ComponentReference<'db> {
    pub name: ComponentName<'db>,
    pub tag_name: ComponentName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed directive reference found in code
#[salsa::tracked]
pub struct DirectiveReference<'db> {
    pub name: DirectiveName<'db>,
    #[returns(ref)]
    pub arguments: Option<String>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
    /// Column of first character INSIDE the quoted string (after opening quote)
    #[returns(copy)]
    pub string_column: u32,
    /// Column one past the last character INSIDE the quoted string (before closing quote)
    #[returns(copy)]
    pub string_end_column: u32,
}

/// A parsed env reference found in code
#[salsa::tracked]
pub struct EnvReference<'db> {
    pub name: EnvVarName<'db>,
    #[returns(copy)]
    pub has_fallback: bool,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed config reference found in code
#[salsa::tracked]
pub struct ConfigReference<'db> {
    pub key: ConfigKey<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// Interned string for Livewire component names
#[salsa::interned]
pub struct LivewireName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// A parsed Livewire component reference found in code
#[salsa::tracked]
pub struct LivewireReference<'db> {
    pub name: LivewireName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed middleware reference found in code
#[salsa::tracked]
pub struct MiddlewareReference<'db> {
    pub name: MiddlewareName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed translation reference found in code
#[salsa::tracked]
pub struct TranslationReference<'db> {
    pub key: TranslationKey<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// Asset helper type - mirrors queries::AssetHelperType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AssetHelperType {
    Asset,
    PublicPath,
    BasePath,
    AppPath,
    StoragePath,
    DatabasePath,
    LangPath,
    ConfigPath,
    ResourcePath,
    Mix,
    ViteAsset,
}

/// A parsed asset reference found in code
#[salsa::tracked]
pub struct AssetReference<'db> {
    pub path: AssetPath<'db>,
    #[returns(copy)]
    pub helper_type: AssetHelperType,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed binding reference found in code
#[salsa::tracked]
pub struct BindingReference<'db> {
    pub name: BindingName<'db>,
    #[returns(copy)]
    pub is_class_reference: bool,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed route() call found in code
#[salsa::tracked]
pub struct RouteReference<'db> {
    pub name: RouteName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed url() call found in code
#[salsa::tracked]
pub struct UrlReference<'db> {
    pub path: UrlPath<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// A parsed action() call found in code
#[salsa::tracked]
pub struct ActionReference<'db> {
    pub action: ActionName<'db>,
    #[returns(copy)]
    pub line: u32,
    #[returns(copy)]
    pub column: u32,
    #[returns(copy)]
    pub end_column: u32,
}

/// All patterns found in a file
/// Note: route_refs, url_refs, action_refs are parsed separately to keep field count under 12
/// (Salsa's tuple-based Hash impl has a 12-element limit)
#[salsa::tracked]
pub struct ParsedPatterns<'db> {
    #[returns(copy)]
    pub file: SourceFile,
    #[returns(ref)]
    pub views: Vec<ViewReference<'db>>,
    #[returns(ref)]
    pub components: Vec<ComponentReference<'db>>,
    #[returns(ref)]
    pub directives: Vec<DirectiveReference<'db>>,
    #[returns(ref)]
    pub env_refs: Vec<EnvReference<'db>>,
    #[returns(ref)]
    pub config_refs: Vec<ConfigReference<'db>>,
    #[returns(ref)]
    pub livewire_refs: Vec<LivewireReference<'db>>,
    #[returns(ref)]
    pub middleware_refs: Vec<MiddlewareReference<'db>>,
    #[returns(ref)]
    pub translation_refs: Vec<TranslationReference<'db>>,
    #[returns(ref)]
    pub asset_refs: Vec<AssetReference<'db>>,
    #[returns(ref)]
    pub binding_refs: Vec<BindingReference<'db>>,
}

/// Parsed Laravel configuration (from composer.json, config/view.php, etc.)
#[salsa::tracked]
pub struct LaravelConfigRef<'db> {
    /// Project root path
    #[returns(ref)]
    pub root: PathBuf,

    /// View paths configured in config/view.php
    #[returns(ref)]
    pub view_paths: Vec<PathBuf>,

    /// Component paths with optional namespace prefix
    #[returns(ref)]
    pub component_paths: Vec<(String, PathBuf)>,

    /// Livewire component path (if Livewire is installed)
    #[returns(ref)]
    pub livewire_path: Option<PathBuf>,

    /// Whether Livewire is installed (detected from composer.json)
    #[returns(copy)]
    pub has_livewire: bool,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract a string value from directive arguments like ('welcome') or ("welcome")
///
/// Returns (string_value, start_offset, end_offset) if found
fn extract_string_from_args(args: &str) -> Option<(String, usize, usize)> {
    // Find the first quote character (single or double)
    let chars: Vec<char> = args.chars().collect();
    let mut i = 0;

    // Skip until we find a quote
    while i < chars.len() {
        if chars[i] == '\'' || chars[i] == '"' {
            break;
        }
        i += 1;
    }

    if i >= chars.len() {
        return None;
    }

    let quote_char = chars[i];
    let start_pos = i + 1; // Position after opening quote
    i += 1;

    // Find the closing quote
    let mut content = String::new();
    while i < chars.len() && chars[i] != quote_char {
        content.push(chars[i]);
        i += 1;
    }

    if content.is_empty() {
        return None;
    }

    let end_pos = i; // Position of closing quote

    Some((content, start_pos, end_pos))
}

/// Extract translation key from PHP content inside {{ ... }} Blade echo statements
///
/// Handles common translation functions:
/// - __("Welcome to our app")
/// - __('messages.welcome')
/// - trans("messages.welcome")
/// - trans_choice("messages.items", $count)
/// - @lang("messages.welcome")
///
/// Returns (translation_key, start_offset, end_offset) if found
pub fn extract_translation_from_echo(php_content: &str) -> Option<(String, usize, usize)> {
    use regex::Regex;

    // Match translation function calls: __(), trans(), trans_choice()
    // We need separate patterns for single and double quotes since regex crate doesn't support backreferences
    static TRANS_REGEX_SINGLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?:__|trans|trans_choice)\s*\(\s*'([^']+)'"#).unwrap()
    });
    static TRANS_REGEX_DOUBLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?:__|trans|trans_choice)\s*\(\s*"([^"]+)""#).unwrap()
    });

    // Try single quotes first
    if let Some(captures) = TRANS_REGEX_SINGLE.captures(php_content) {
        let key_match = captures.get(1)?;
        let trans_key = key_match.as_str().to_string();
        let start_offset = key_match.start();
        let end_offset = key_match.end();
        return Some((trans_key, start_offset, end_offset));
    }

    // Try double quotes
    if let Some(captures) = TRANS_REGEX_DOUBLE.captures(php_content) {
        let key_match = captures.get(1)?;
        let trans_key = key_match.as_str().to_string();
        let start_offset = key_match.start();
        let end_offset = key_match.end();
        return Some((trans_key, start_offset, end_offset));
    }

    None
}

/// Parse @vite directive arguments and extract individual file paths with their positions
///
/// Handles both formats:
/// - @vite('resources/css/app.css')
/// - @vite(['resources/css/app.css', 'resources/js/app.js'])
///
/// Returns Vec of (path, line, column, end_column) for each file path
pub fn parse_vite_directive_assets(
    args: &str,
    directive_row: usize,
    directive_col: usize,
    directive_len: usize,
) -> Vec<(String, u32, u32, u32)> {
    let mut results = Vec::new();

    // The args from tree-sitter typically include the parentheses content
    // e.g., "(['resources/css/app.css', 'resources/js/app.js'])"
    let args = args.trim();

    // Track position within the arguments string
    let mut pos = 0;
    let chars: Vec<char> = args.chars().collect();

    while pos < chars.len() {
        // Find the start of a quoted string
        let quote_char = match chars[pos] {
            '\'' | '"' => chars[pos],
            _ => {
                pos += 1;
                continue;
            }
        };

        let quote_start = pos;
        pos += 1; // Move past opening quote

        // Find the end of the quoted string
        let mut path_chars = Vec::new();
        while pos < chars.len() && chars[pos] != quote_char {
            path_chars.push(chars[pos]);
            pos += 1;
        }

        if pos < chars.len() {
            let path: String = path_chars.into_iter().collect();
            // Calculate column positions for the path content (excluding quotes)
            // directive_col is where @ starts
            // directive_len is length of @vite (5)
            // quote_start is position of opening quote within args (which includes the paren)
            // +1 to skip the opening quote itself and point to the path content
            // +1 more because LSP columns are 0-based but we need to account for the @ symbol position
            let col = (directive_col + directive_len + quote_start + 2) as u32;
            // Empty entries (`@vite('')`) are kept, not dropped: Laravel can't
            // resolve them and throws at build time, so they must be flagged. A
            // zero-width range wouldn't render a squiggle, so span the two quote
            // characters around the (absent) content instead.
            let (col, end_col) = if path.is_empty() {
                (col.saturating_sub(1), col + 1)
            } else {
                (col, col + path.len() as u32) // Just the path, no quotes
            };

            results.push((path, directive_row as u32, col, end_col));
            pos += 1; // Move past closing quote
        }
    }

    results
}

// ============================================================================
// Query Functions - The actual computation
// ============================================================================

/// Parse a source file and extract all Laravel patterns
/// This is automatically memoized by Salsa
///
/// Uses single-pass extraction for performance:
/// - One query compilation (cached globally)
/// - One tree traversal per language (PHP/Blade)
/// - All patterns extracted in O(n) instead of O(n×k)
#[salsa::tracked]
pub fn parse_file_patterns<'db>(db: &'db dyn Db, file: SourceFile) -> ParsedPatterns<'db> {
    use crate::parser::{language_blade, language_php, parse_blade, parse_php};
    use crate::queries::{
        extract_all_blade_patterns, extract_all_php_patterns,
        AssetHelperType as QueryAssetHelperType,
    };

    let text = file.text(db);
    let path = file.path(db);
    let is_blade = path.to_string_lossy().ends_with(".blade.php");

    let mut views = Vec::new();
    let mut components = Vec::new();
    let mut directives = Vec::new();
    let mut env_refs = Vec::new();
    let mut config_refs = Vec::new();
    let mut livewire_refs = Vec::new();
    let mut middleware_refs = Vec::new();
    let mut translation_refs = Vec::new();
    let mut asset_refs = Vec::new();
    let mut binding_refs = Vec::new();

    // Parse Blade files - single pass extraction
    if is_blade {
        if let Ok(tree) = parse_blade(text) {
            let lang = language_blade();

            if let Ok(blade_patterns) = extract_all_blade_patterns(&tree, text, &lang) {
                // Process components
                for comp in blade_patterns.components {
                    let name = ComponentName::new(db, comp.component_name.to_string());
                    let tag = ComponentName::new(db, comp.tag_name.to_string());
                    components.push(ComponentReference::new(
                        db,
                        name,
                        tag,
                        comp.row as u32,
                        comp.column as u32,
                        comp.end_column as u32,
                    ));
                }

                // Process Livewire components
                for lw in blade_patterns.livewire {
                    let name = LivewireName::new(db, lw.component_name.to_string());
                    livewire_refs.push(LivewireReference::new(
                        db,
                        name,
                        lw.row as u32,
                        lw.column as u32,
                        lw.end_column as u32,
                    ));
                }

                // Process directives
                for dir in blade_patterns.directives {
                    // Handle @vite specially - extract individual asset paths
                    if dir.directive_name == "vite" {
                        if let Some(args) = dir.arguments {
                            let vite_assets = parse_vite_directive_assets(
                                args,
                                dir.row,
                                dir.column,
                                dir.directive_name.len() + 1,
                            );
                            for (path, line, col, end_col) in vite_assets {
                                let asset_path = AssetPath::new(db, path);
                                asset_refs.push(AssetReference::new(
                                    db,
                                    asset_path,
                                    AssetHelperType::ViteAsset,
                                    line,
                                    col,
                                    end_col,
                                ));
                            }
                        }
                        continue; // Don't add @vite as a directive
                    }

                    // Handle @lang specially - extract as translation reference
                    if dir.directive_name == "lang" {
                        if let Some(args) = dir.arguments {
                            // Extract the translation key from the arguments
                            // Args look like: ('welcome') or ("welcome")
                            if let Some((trans_key, start_offset, end_offset)) =
                                extract_string_from_args(args)
                            {
                                let key = TranslationKey::new(db, trans_key);
                                // Calculate column positions: directive_column + @lang + offset into args
                                // @lang is 5 chars, plus 1 for @
                                let base_col = dir.column + 6; // position after @lang
                                let col = base_col + start_offset;
                                let end_col = base_col + end_offset;
                                debug!(
                                    "📍 @lang translation: key='{}' row={} col={}-{} (args={:?})",
                                    key.key(db),
                                    dir.row,
                                    col,
                                    end_col,
                                    args
                                );
                                translation_refs.push(TranslationReference::new(
                                    db,
                                    key,
                                    dir.row as u32,
                                    col as u32,
                                    end_col as u32,
                                ));
                            }
                        }
                        continue; // Don't add @lang as a directive
                    }

                    let name = DirectiveName::new(db, dir.directive_name.to_string());
                    let args = dir.arguments.map(|s| s.to_string());
                    let full_end_column = dir.column + dir.full_text.len();
                    directives.push(DirectiveReference::new(
                        db,
                        name,
                        args,
                        dir.row as u32,
                        dir.column as u32,
                        full_end_column as u32,
                        dir.string_column as u32,
                        dir.string_end_column as u32,
                    ));
                }

                // Process PHP content inside {{ ... }} echo statements
                // Extract translation calls like __("Welcome"), trans("key"), etc.
                // (Per-echo logging demoted to debug — at scale, these were
                // tens of thousands of log lines that dominated warming cost.)
                debug!(
                    "🔍 Processing {} echo PHP snippets",
                    blade_patterns.echo_php.len()
                );
                for echo in blade_patterns.echo_php {
                    debug!(
                        "🔍 Echo PHP content: {:?} at row {} col {}",
                        echo.php_content, echo.row, echo.column
                    );
                    if let Some((trans_key, start_offset, end_offset)) =
                        extract_translation_from_echo(echo.php_content)
                    {
                        debug!(
                            "✅ Found translation '{}' at offsets {}-{}",
                            trans_key, start_offset, end_offset
                        );
                        let key = TranslationKey::new(db, trans_key.clone());
                        // Calculate column positions relative to the echo statement
                        let col = echo.column + start_offset;
                        let end_col = echo.column + end_offset;
                        debug!(
                            "📍 Translation ref: row={} col={}-{}",
                            echo.row, col, end_col
                        );
                        translation_refs.push(TranslationReference::new(
                            db,
                            key,
                            echo.row as u32,
                            col as u32,
                            end_col as u32,
                        ));
                    } else {
                        debug!("❌ No translation found in echo content");
                    }
                }
            }
        }
    }

    // For Blade files: every `{{ }}` / `{!! !!}` / `@php` region carries
    // PHP that tree-sitter-php can't recover when given the surrounding
    // Blade syntax. Extract each region individually, re-parse as PHP, and
    // accumulate its patterns into the same Salsa-tracked vectors that the
    // PHP path below populates. Without this, route/view/config/env/...
    // calls inside Blade `{{ }}` are invisible to find-references, hover,
    // and goto-definition.
    if is_blade {
        use crate::blade_embedded_php::{adjust_inner_position, extract_php_regions};
        let regions = extract_php_regions(text);
        let lang_php = language_php();
        for region in regions {
            let wrapped = format!("<?php {}", region.content);
            let Ok(snippet_tree) = parse_php(&wrapped) else {
                continue;
            };
            let Ok(snippet_patterns) = extract_all_php_patterns(&snippet_tree, &wrapped, &lang_php)
            else {
                continue;
            };
            for view in snippet_patterns.views {
                let (line, col) = adjust_inner_position(
                    view.row as u32,
                    view.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    view.row as u32,
                    view.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = ViewName::new(db, view.view_name.to_string());
                views.push(ViewReference::new(
                    db,
                    name,
                    line,
                    col,
                    end_col,
                    view.is_route_view,
                    view.is_property_site,
                ));
            }
            for env in snippet_patterns.env_calls {
                let (line, col) = adjust_inner_position(
                    env.row as u32,
                    env.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    env.row as u32,
                    env.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = EnvVarName::new(db, env.var_name.to_string());
                env_refs.push(EnvReference::new(
                    db,
                    name,
                    env.has_fallback,
                    line,
                    col,
                    end_col,
                ));
            }
            for config in snippet_patterns.config_calls {
                let (line, col) = adjust_inner_position(
                    config.row as u32,
                    config.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    config.row as u32,
                    config.end_column as u32,
                    region.row,
                    region.column,
                );
                let key = ConfigKey::new(db, config.config_key.to_string());
                config_refs.push(ConfigReference::new(db, key, line, col, end_col));
            }
            for mw in snippet_patterns.middleware_calls {
                let (line, col) = adjust_inner_position(
                    mw.row as u32,
                    mw.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    mw.row as u32,
                    mw.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = MiddlewareName::new(db, mw.middleware_name.to_string());
                middleware_refs.push(MiddlewareReference::new(db, name, line, col, end_col));
            }
            for trans in snippet_patterns.translation_calls {
                let (line, col) = adjust_inner_position(
                    trans.row as u32,
                    trans.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    trans.row as u32,
                    trans.end_column as u32,
                    region.row,
                    region.column,
                );
                let key = TranslationKey::new(db, trans.translation_key.to_string());
                translation_refs.push(TranslationReference::new(db, key, line, col, end_col));
            }
            for asset in snippet_patterns.asset_calls {
                let (line, col) = adjust_inner_position(
                    asset.row as u32,
                    asset.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    asset.row as u32,
                    asset.end_column as u32,
                    region.row,
                    region.column,
                );
                let path = AssetPath::new(db, asset.path.to_string());
                let helper_type = match asset.helper_type {
                    QueryAssetHelperType::Asset => AssetHelperType::Asset,
                    QueryAssetHelperType::PublicPath => AssetHelperType::PublicPath,
                    QueryAssetHelperType::BasePath => AssetHelperType::BasePath,
                    QueryAssetHelperType::AppPath => AssetHelperType::AppPath,
                    QueryAssetHelperType::StoragePath => AssetHelperType::StoragePath,
                    QueryAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
                    QueryAssetHelperType::LangPath => AssetHelperType::LangPath,
                    QueryAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
                    QueryAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
                    QueryAssetHelperType::Mix => AssetHelperType::Mix,
                    QueryAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
                };
                asset_refs.push(AssetReference::new(
                    db,
                    path,
                    helper_type,
                    line,
                    col,
                    end_col,
                ));
            }
            for binding in snippet_patterns.binding_calls {
                let (line, col) = adjust_inner_position(
                    binding.row as u32,
                    binding.column as u32,
                    region.row,
                    region.column,
                );
                let (_, end_col) = adjust_inner_position(
                    binding.row as u32,
                    binding.end_column as u32,
                    region.row,
                    region.column,
                );
                let name = BindingName::new(db, binding.binding_name.to_string());
                binding_refs.push(BindingReference::new(
                    db,
                    name,
                    binding.is_class_reference,
                    line,
                    col,
                    end_col,
                ));
            }
        }
    }

    // Full-file PHP parse — ONLY for .php files. See pattern_indexer.rs
    // for the rationale: tree-sitter-php on Blade content produces an
    // error tree that the PHP queries walk pathologically slowly on
    // certain real-world inputs (Flux icon SVG path data hit 394ms for
    // a 1.3KB file). All Blade-embedded PHP is extracted above via
    // extract_php_regions + per-region <?php-wrapped parsing.
    if !is_blade {
        if let Ok(tree) = parse_php(text) {
            let lang = language_php();

            if let Ok(php_patterns) = extract_all_php_patterns(&tree, text, &lang) {
                // Process views
                for view in php_patterns.views {
                    let name = ViewName::new(db, view.view_name.to_string());
                    views.push(ViewReference::new(
                        db,
                        name,
                        view.row as u32,
                        view.column as u32,
                        view.end_column as u32,
                        view.is_route_view,
                        view.is_property_site,
                    ));
                }

                // Process env calls
                for env in php_patterns.env_calls {
                    let name = EnvVarName::new(db, env.var_name.to_string());
                    env_refs.push(EnvReference::new(
                        db,
                        name,
                        env.has_fallback,
                        env.row as u32,
                        env.column as u32,
                        env.end_column as u32,
                    ));
                }

                // Process config calls
                for config in php_patterns.config_calls {
                    let key = ConfigKey::new(db, config.config_key.to_string());
                    config_refs.push(ConfigReference::new(
                        db,
                        key,
                        config.row as u32,
                        config.column as u32,
                        config.end_column as u32,
                    ));
                }

                // Process middleware calls
                for mw in php_patterns.middleware_calls {
                    let name = MiddlewareName::new(db, mw.middleware_name.to_string());
                    middleware_refs.push(MiddlewareReference::new(
                        db,
                        name,
                        mw.row as u32,
                        mw.column as u32,
                        mw.end_column as u32,
                    ));
                }

                // Process translation calls
                for trans in php_patterns.translation_calls {
                    let key = TranslationKey::new(db, trans.translation_key.to_string());
                    translation_refs.push(TranslationReference::new(
                        db,
                        key,
                        trans.row as u32,
                        trans.column as u32,
                        trans.end_column as u32,
                    ));
                }

                // Process asset calls
                for asset in php_patterns.asset_calls {
                    let path = AssetPath::new(db, asset.path.to_string());
                    let helper_type = match asset.helper_type {
                        QueryAssetHelperType::Asset => AssetHelperType::Asset,
                        QueryAssetHelperType::PublicPath => AssetHelperType::PublicPath,
                        QueryAssetHelperType::BasePath => AssetHelperType::BasePath,
                        QueryAssetHelperType::AppPath => AssetHelperType::AppPath,
                        QueryAssetHelperType::StoragePath => AssetHelperType::StoragePath,
                        QueryAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
                        QueryAssetHelperType::LangPath => AssetHelperType::LangPath,
                        QueryAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
                        QueryAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
                        QueryAssetHelperType::Mix => AssetHelperType::Mix,
                        QueryAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
                    };
                    asset_refs.push(AssetReference::new(
                        db,
                        path,
                        helper_type,
                        asset.row as u32,
                        asset.column as u32,
                        asset.end_column as u32,
                    ));
                }

                // Process binding calls
                for binding in php_patterns.binding_calls {
                    let name = BindingName::new(db, binding.binding_name.to_string());
                    binding_refs.push(BindingReference::new(
                        db,
                        name,
                        binding.is_class_reference,
                        binding.row as u32,
                        binding.column as u32,
                        binding.end_column as u32,
                    ));
                }

                // Note: route_refs, url_refs, action_refs are extracted in handle_get_patterns
                // to keep ParsedPatterns field count under Salsa's 12-element limit
            }

            // Component / Livewire tags built as PHP string literals — markup a
            // job or mailer assembles and renders later. Appended AFTER the
            // query-extracted patterns to match the order
            // `pattern_indexer::push_string_tags` produces, so a file's entries
            // never depend on which constructor built them.
            for tag in crate::php_string_components::scan_php_string_tags(&tree, text) {
                match tag.kind {
                    crate::php_string_components::StringTagKind::Component => {
                        let name = ComponentName::new(db, tag.name);
                        let full = ComponentName::new(db, tag.tag_name);
                        components.push(ComponentReference::new(
                            db,
                            name,
                            full,
                            tag.line,
                            tag.column,
                            tag.end_column,
                        ));
                    }
                    crate::php_string_components::StringTagKind::Livewire => {
                        let name = LivewireName::new(db, tag.name);
                        livewire_refs.push(LivewireReference::new(
                            db,
                            name,
                            tag.line,
                            tag.column,
                            tag.end_column,
                        ));
                    }
                }
            }
        }
    } // end if !is_blade

    ParsedPatterns::new(
        db,
        file,
        views,
        components,
        directives,
        env_refs,
        config_refs,
        livewire_refs,
        middleware_refs,
        translation_refs,
        asset_refs,
        binding_refs,
    )
}

/// Parse composer.json to detect installed packages
/// Returns (has_livewire, list of installed packages)
#[salsa::tracked]
pub fn parse_composer_json(db: &dyn Db, file: ConfigFile) -> (bool, Vec<String>) {
    let text = file.text(db);

    // Parse JSON to detect Livewire
    let has_livewire = text.contains("\"livewire/livewire\"");

    // Extract package names from require and require-dev
    let mut packages = Vec::new();

    // Simple extraction - look for package patterns in require sections
    // This is a simplified version; could use serde_json for full parsing
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') && trimmed.contains('/') && trimmed.contains(':') {
            // Extract package name from "vendor/package": "version"
            if let Some(end) = trimmed.find(':') {
                let name = trimmed[1..end - 1].to_string();
                if name.contains('/') {
                    packages.push(name);
                }
            }
        }
    }

    (has_livewire, packages)
}

/// Parse config/view.php to extract view paths
#[salsa::tracked(returns(clone))]
pub fn parse_view_config(db: &dyn Db, file: ConfigFile, root: PathBuf) -> Vec<PathBuf> {
    let text = file.text(db);
    let mut paths = Vec::new();

    // Look for resource_path('views') or base_path('some/path') patterns
    // This reuses logic from config.rs but in a Salsa-compatible way

    // Default: resources/views
    if text.contains("resource_path") && text.contains("views") {
        paths.push(root.join("resources/views"));
    }

    // Look for base_path calls
    for line in text.lines() {
        if line.contains("base_path") {
            // Extract path from base_path('path')
            if let Some(start) = line.find("base_path(") {
                let rest = &line[start + 10..];
                if let Some(quote_start) = rest.find(['\'', '"']) {
                    let quote_char = rest.chars().nth(quote_start).unwrap();
                    let path_start = quote_start + 1;
                    if let Some(quote_end) = rest[path_start..].find(quote_char) {
                        let path_str = &rest[path_start..path_start + quote_end];
                        paths.push(root.join(path_str));
                    }
                }
            }
        }
    }

    // If no paths found, use default
    if paths.is_empty() {
        paths.push(root.join("resources/views"));
    }

    paths
}

/// Parse a Blade file's loop-block structure (@foreach / @forelse / @for / @while).
/// Memoized: only re-runs when the file's text changes.
#[salsa::tracked(returns(clone))]
pub fn parse_blade_loop_blocks(
    db: &dyn Db,
    file: SourceFile,
) -> Vec<crate::blade_loops::BladeLoopBlock> {
    let text = file.text(db);
    crate::blade_loops::find_loop_blocks(text)
}

/// Parse simple `$name = ...;` assignments out of a Blade file's `@php ... @endphp` blocks.
/// Memoized: only re-runs when the file's text changes.
#[salsa::tracked(returns(clone))]
pub fn parse_blade_php_assignments(db: &dyn Db, file: SourceFile) -> Vec<(String, String)> {
    let text = file.text(db);
    crate::blade_php_block::extract_php_block_assignments(text)
}

// ============================================================================
// Blade backing-class resolution (issue #339, item 7)
// ============================================================================

/// Whether `path` is a plain `.php` file rather than a `.blade.php` template.
///
/// A Blade template has no standalone class for `component_member_locator` to
/// parse — its inline `new class extends Component` (or Volt front matter) is
/// handled separately, by [`blade_backing_class_sources`]'s `inline` arm.
pub fn is_plain_php_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("php")
        && !path.to_string_lossy().ends_with(".blade.php")
}

/// Every file that renders `view_name`, sorted lexicographically by path.
///
/// The sort is load-bearing, not cosmetic: [`blade_backing_class_files`]'s
/// consumers take the FIRST hit over this list, so an unsorted result would
/// flap between two contributing classes that both declare the same member.
///
/// Memoized: only re-runs when the [`RenderIndex`] input changes.
#[salsa::tracked(returns(clone))]
pub fn render_source_files(db: &dyn Db, index: RenderIndex, view_name: String) -> Vec<PathBuf> {
    db.query_run_counts()
        .render_source_files
        .fetch_add(1, Ordering::Relaxed);
    let mut files: Vec<PathBuf> = index
        .entries(db)
        .iter()
        .filter(|(view, _)| *view == view_name)
        .map(|(_, path)| path.clone())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// The plain-`.php` files backing a Blade template's rendered content — the
/// union of two independent sources, in precedence order:
///   1. the render index's contributors for `view_name` (a Filament-style
///      `$view`-property page, or any controller `view(...)` call site);
///   2. `livewire_paths`, the conventionally-resolved Livewire component class
///      for a Blade file that lives under Livewire's view path.
///
/// (1) comes first so a direct render site outranks the Livewire convention.
/// A partial that has NEITHER resolves through the component that rendered it,
/// one level up — but that walk lives in `Backend::blade_backing_class_
/// resolution` (#339, item 1), which calls this query per candidate rather than
/// climbing inside it, so a direct render site is always tried before any
/// ancestor.
///
/// Deduped, and restricted to plain `.php` paths. Existence is NOT checked
/// here: this query is pure, and the actor drops paths it cannot read when it
/// loads them as Salsa inputs.
///
/// Memoized: only re-runs when the [`RenderIndex`], the view name, or the
/// resolved Livewire paths change.
#[salsa::tracked(returns(clone))]
pub fn blade_backing_class_files(
    db: &dyn Db,
    index: RenderIndex,
    view_name: Option<String>,
    livewire_paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(view) = view_name {
        out.extend(render_source_files(db, index, view));
    }
    out.extend(livewire_paths);

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    out.retain(|p| is_plain_php_path(p) && seen.insert(p.clone()));
    out
}

/// Every `(path, source)` pair whose PHP class backs a Blade template: the
/// plain-`.php` backing files of [`blade_backing_class_files`], plus the Blade
/// file ITSELF (`inline`) when it declares an inline
/// `new class extends Component` (a Livewire v4 single-file component or a
/// class-based Volt component) or carries a functional Volt signature. Those
/// shapes have no standalone `.php` source — the members live in the
/// template's own front matter.
///
/// Memoized against each backing file's CONTENT, not merely its path: `files`
/// are [`SourceFile`] inputs, so editing a backing class invalidates this
/// query and the next call recomputes rather than serving a stale source.
#[salsa::tracked(returns(clone))]
pub fn blade_backing_class_sources(
    db: &dyn Db,
    files: Vec<SourceFile>,
    inline: Option<SourceFile>,
) -> Vec<(PathBuf, String)> {
    db.query_run_counts()
        .blade_backing_class_sources
        .fetch_add(1, Ordering::Relaxed);
    let mut out: Vec<(PathBuf, String)> = files
        .iter()
        .map(|file| (file.path(db).clone(), file.text(db).clone()))
        .collect();

    if let Some(blade) = inline {
        let text = blade.text(db);
        // A FUNCTIONAL Volt file declares no class at all, so
        // `detect_inline_livewire_class` is false for it — but its
        // `state([...])` keys and top-level closure assignments ARE the
        // component's members, and `component_member_locator` reads them.
        // Both inline shapes therefore hand the template itself to the
        // locator (#339, item 3).
        if crate::php_class::detect_inline_livewire_class(text)
            || crate::livewire_resolver::source_contains_volt_signature(text)
        {
            out.push((blade.path(db).clone(), text.clone()));
        }
    }
    out
}

/// Extract the document-symbol tree for a file (route file, Blade template,
/// Livewire component, or Eloquent model). Returns an empty vec for other file
/// kinds. Memoized: only re-runs when the file's text changes.
#[salsa::tracked(returns(clone))]
pub fn extract_document_symbols(
    db: &dyn Db,
    file: SourceFile,
) -> Vec<crate::document_symbols::SymbolEntry> {
    let path = file.path(db);
    let text = file.text(db);
    let kind = crate::document_symbols::classify_file(path);
    crate::document_symbols::extract_symbols(text, kind)
}

/// Resolve a `$this->X` member access against a Livewire component's PHP file.
/// Tries property type first, then method return type. Memoized per (file_version, member).
#[salsa::tracked(returns(clone))]
pub fn resolve_livewire_member_type(
    db: &dyn Db,
    file: SourceFile,
    member: String,
) -> Option<String> {
    let text = file.text(db);
    crate::php_class::resolve_member_type(text, &member)
}

/// Parse config/livewire.php to extract Livewire component path
#[salsa::tracked(returns(clone))]
pub fn parse_livewire_config(db: &dyn Db, file: ConfigFile, root: PathBuf) -> Option<PathBuf> {
    let text = file.text(db);

    // Look for class_namespace patterns
    if text.contains("App\\Livewire") || text.contains("App\\\\Livewire") {
        return Some(root.join("app").join("Livewire"));
    }

    if text.contains("App\\Http\\Livewire") || text.contains("App\\\\Http\\\\Livewire") {
        return Some(root.join("app/Http/Livewire"));
    }

    None
}

/// Build complete Laravel configuration from individual config files
#[salsa::tracked]
pub fn build_laravel_config<'db>(
    db: &'db dyn Db,
    root: PathBuf,
    composer: Option<ConfigFile>,
    view_config: Option<ConfigFile>,
    livewire_config: Option<ConfigFile>,
) -> LaravelConfigRef<'db> {
    // Parse composer.json for Livewire detection
    let has_livewire = composer
        .map(|f| parse_composer_json(db, f).0)
        .unwrap_or(false);

    // Parse view config for view paths
    let view_paths = view_config
        .map(|f| parse_view_config(db, f, root.clone()))
        .unwrap_or_else(|| vec![root.join("resources/views")]);

    // Build component paths from view paths
    let component_paths: Vec<(String, PathBuf)> = view_paths
        .iter()
        .map(|p| (String::new(), p.join("components")))
        .collect();

    // Parse livewire config for component path
    let livewire_path = if has_livewire {
        livewire_config
            .and_then(|f| parse_livewire_config(db, f, root.clone()))
            .or_else(|| {
                // Default Livewire paths
                let v3_path = root.join("app").join("Livewire");
                let v2_path = root.join("app/Http/Livewire");
                if v3_path.exists() {
                    Some(v3_path)
                } else if v2_path.exists() {
                    Some(v2_path)
                } else {
                    Some(v3_path) // Default to v3 path
                }
            })
    } else {
        None
    };

    LaravelConfigRef::new(
        db,
        root,
        view_paths,
        component_paths,
        livewire_path,
        has_livewire,
    )
}

// ============================================================================
// Environment Variable Parsing (Salsa-based)
// ============================================================================

/// A parsed environment variable (Salsa tracked)
#[salsa::tracked]
pub struct ParsedEnvVar<'db> {
    /// Variable name
    pub name: EnvVarName<'db>,
    /// Variable value
    #[returns(ref)]
    pub value: String,
    /// Line number in source file (0-indexed)
    #[returns(copy)]
    pub line: u32,
    /// Column of the variable name
    #[returns(copy)]
    pub column: u32,
    /// Column where value starts
    #[returns(copy)]
    pub value_column: u32,
    /// Whether this variable is commented out
    #[returns(copy)]
    pub is_commented: bool,
    /// Priority of the source file (higher wins)
    #[returns(copy)]
    pub priority: u8,
    /// Source file path
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// Parse an environment file and extract all variables
#[salsa::tracked]
pub fn parse_env_source<'db>(db: &'db dyn Db, file: EnvFile) -> Vec<ParsedEnvVar<'db>> {
    let text = file.text(db);
    let path = file.path(db);
    let priority = file.priority(db);
    let mut variables = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Check if line is commented — through the one rule every reader of
        // `.env` text classifies with, so Salsa's view of "commented" and the
        // buffer-local view the LSP handlers hit-test with cannot disagree.
        let (is_commented, working_line) =
            match crate::env_key_locator::commented_declaration_body(line) {
                Some(body) => (true, body),
                None => (false, line),
            };

        // Parse VAR=value format
        if let Some((name_part, value_part)) = working_line.split_once('=') {
            let name = name_part.trim();

            // Skip if not a valid variable name
            if name.is_empty() || name.contains(' ') {
                continue;
            }

            // Parse the value, handling quotes
            let value = parse_env_value_internal(value_part.trim());

            // Calculate column positions
            let name_column = line.find(name).unwrap_or(0) as u32;
            let value_column = line
                .find('=')
                .map(|pos| pos + 1)
                .unwrap_or(name_column as usize) as u32;

            let var_name = EnvVarName::new(db, name.to_string());
            variables.push(ParsedEnvVar::new(
                db,
                var_name,
                value,
                line_idx as u32,
                name_column,
                value_column,
                is_commented,
                priority,
                path.clone(),
            ));
        }
    }

    variables
}

/// Parse an environment variable value, handling quotes
fn parse_env_value_internal(value: &str) -> String {
    let value = value.trim();

    // Handle quoted values
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        // Remove quotes
        if value.len() >= 2 {
            return value[1..value.len() - 1].to_string();
        }
    }

    // Handle inline comments (# at end of line)
    if let Some(hash_pos) = value.find(" #") {
        return value[..hash_pos].trim().to_string();
    }

    value.to_string()
}

// ============================================================================
// Translation Resolution (Salsa-based) — issue #293
// ============================================================================

/// A resolved translation crossing the async boundary — the value as its raw
/// PHP/JSON literal, plus the catalogue it was read from. Hover renders the
/// source file as a link; go-to-definition navigates to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTranslationData {
    /// The translated value, as the raw PHP/JSON literal
    pub value: String,
    /// The catalogue the value came from
    pub source_file: PathBuf,
}

/// Walk a dotted key path inside one PHP array catalogue.
///
/// Memoized per `(file, key_path)`. An empty-text file — absent, unreadable or
/// containment-refused — yields `None`, exactly as the direct-`fs` resolver did
/// when the read failed.
#[salsa::tracked]
pub fn resolve_php_translation(
    db: &dyn Db,
    file: LangFile,
    key_path: Vec<String>,
) -> Option<String> {
    let text = file.text(db);
    if text.is_empty() {
        return None;
    }
    let refs: Vec<&str> = key_path.iter().map(String::as_str).collect();
    crate::config_lookup::resolve_in_source(text, &refs)
}

/// Look one text key up in a `{lang_root}/{locale}.json` catalogue.
///
/// Memoized per `(file, key)`. The value is returned single-quoted so it
/// matches the PHP-literal shape the rest of the pipeline unquotes.
#[salsa::tracked]
pub fn resolve_json_translation(db: &dyn Db, file: LangFile, key: String) -> Option<String> {
    let text = file.text(db);
    if text.is_empty() {
        return None;
    }
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text).ok()?;
    Some(format!("'{}'", map.get(&key)?.as_str()?))
}

/// The locale names one directory listing implies.
///
/// A subdirectory is a locale (`lang/de/`); a `.json` child is a locale
/// catalogue (`lang/de.json`). `vendor` is a namespace container, never a
/// locale. Memoized per directory listing, so a 25-locale project enumerates
/// once instead of once per hover, goto and diagnostic (issue #293).
#[salsa::tracked]
pub fn locales_in_dir(db: &dyn Db, dir: LangDir) -> Vec<String> {
    let mut locales = Vec::new();
    for (name, is_dir) in dir.entries(db) {
        let locale = if *is_dir {
            Some(name.clone())
        } else {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|e| e == "json")
                .then(|| {
                    std::path::Path::new(name)
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                })
                .flatten()
        };
        if let Some(locale) = locale {
            if locale != "vendor" && !locales.contains(&locale) {
                locales.push(locale);
            }
        }
    }
    locales
}

/// The string a PHP config value denotes, or `None` when it denotes none
/// statically.
///
/// Mirrors the two forms
/// [`crate::config::php_top_level_string_value`] accepts, because
/// [`crate::config_lookup::resolve_value`] hands back raw source text either
/// way: a quoted literal (`'en'` → `en`) and `env('NAME', 'default')`, whose
/// default argument is taken since a static reader cannot see the running
/// process's environment.
///
/// Everything else is unresolved — a constant, a concatenation, a function
/// call, or an `env('NAME')` with no default. Those return `None` rather than
/// the raw text, so a caller matching against real directory names can never
/// match on an expression.
fn config_string_literal(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(literal) = unquote_php_string(raw) {
        return Some(literal.to_string());
    }
    let args = raw.strip_prefix("env(")?.rsplit_once(')')?.0;
    unquote_php_string(args.split_once(',')?.1.trim()).map(str::to_string)
}

/// The body of a single- or double-quoted PHP string literal.
///
/// `None` when `raw` does not open and close with the same quote — an
/// unquoted constant, or a truncated `'en`.
fn unquote_php_string(raw: &str) -> Option<&str> {
    let quote = raw.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    raw.strip_prefix(quote)?.strip_suffix(quote)
}

/// Every `namespace -> lang dir` registration one provider declares.
///
/// Memoized per `(file, root)`. The vendor scan reads and substring-gates every
/// `*ServiceProvider*.php` in `vendor/` and AST-parses the survivors; before
/// this it ran as one uncached sweep whose result was then held for the entire
/// session with no way to refresh it (issue #293).
#[salsa::tracked]
pub fn translation_namespaces_in_provider(
    db: &dyn Db,
    file: TranslationProviderFile,
    root: PathBuf,
) -> Vec<(String, PathBuf)> {
    let text = file.text(db);
    if text.is_empty() {
        return Vec::new();
    }
    crate::vendor_translations::namespaces_in_source(text, file.path(db), &root)
}

/// Lang-file reads and locale enumeration, memoized through Salsa.
///
/// Owned by [`SalsaActor`] in production; constructible directly so tests can
/// exercise the real resolution path against a bare [`LaravelDatabase`] without
/// standing up the actor.
///
/// # Why this exists
///
/// Translation resolution used to read and parse lang files with
/// `std::fs::read_to_string` on **every** call, uncached and uninvalidated.
/// Since #288 resolved a key against every locale a project defines, one hover
/// on a 25-locale project meant a `read_dir` plus up to 25 file reads — repeated
/// for go-to-definition and again for diagnostics. Routing those reads through
/// Salsa inputs makes them incremental: read once, reuse until something
/// actually changes (issue #293).
///
/// # Containment
///
/// Five methods read from disk — each counted by [`Self::disk_reads`]. The
/// containment guard applies to exactly the two that build a path out of
/// untrusted text:
///
/// - [`Self::ensure_file`] and [`Self::ensure_dir`] are **guarded** and
///   fail-closed: a path that cannot be proven inside the project root is never
///   read (issue #248). Their candidate paths are built from `vendor::`
///   namespaces and `loadTranslationsFrom` arguments lifted verbatim out of
///   parsed source, so they are untrusted and may carry traversal or be
///   absolute. Keeping the guard here is what lets `translation_lookup` be pure
///   path arithmetic without weakening #248.
/// - [`Self::ensure_provider`], [`Self::ensure_provider_files`] and
///   [`Self::ensure_config`] are **unguarded**, and deliberately so: none of
///   them joins attacker-controlled text onto the root. The first two walk the
///   project's own `vendor/` and `app/Providers/` trees (see
///   `ensure_provider`'s own note); `ensure_config` names
///   `<root>/config/<group>.php` from a group segment that is hardcoded at its
///   only call site — `"app.locale"` and `"app.fallback_locale"` — never from
///   parsed source. There is nothing for #248 to fence.
///
/// A read is not the only way to touch disk: [`Self::completion_keys`] *stats*
/// the lang roots, and [`Self::ensure_config`] stats the config path through
/// `config_group_files`. Neither is counted, because neither opens a file —
/// the distinction is what lets a test attribute an exact number of reads to
/// one code path.
#[derive(Default)]
pub struct TranslationCache {
    /// Catalogues keyed by absolute path. An entry with **empty text** is a
    /// negative cache: the path is absent, unreadable, or containment-refused.
    /// Without it, a key defined only in `de` would re-probe the other 24
    /// locales' missing files on every single request.
    files: HashMap<PathBuf, LangFile>,
    /// Directory listings backing locale discovery, keyed by absolute path. An
    /// empty listing likewise caches "absent, or nothing in it".
    dirs: HashMap<PathBuf, LangDir>,
    /// Config-file texts backing completion's locale choice, keyed by the
    /// project config path the group resolves to (`<root>/config/<group>.php`).
    /// A `None` value is a negative cache: the group contributes no readable
    /// file, and the *absence* is what must not be re-probed — without it every
    /// completion request on a project with no `config/app.php` would re-stat
    /// it, which is the shape [`Self::ensure_file`] already fail-closes for.
    ///
    /// Not a Salsa input: nothing derives from this text through a tracked
    /// query, so there is no memoized result for a version bump to invalidate.
    /// A plain map keyed by path is the whole mechanism, and
    /// [`Self::invalidate_config`] is its only eviction.
    configs: HashMap<PathBuf, Option<String>>,
    /// Version counter shared by files and directories; bumped on every
    /// registration so Salsa sees a changed input.
    version: i32,
    /// Service providers registering translation namespaces, keyed by path.
    providers: HashMap<PathBuf, TranslationProviderFile>,
    /// The discovered provider set. `None` until the first scan, and reset by
    /// [`Self::invalidate_providers`] so a create or delete is picked up.
    provider_files: Option<TranslationProviderFiles>,
    /// Additional first-party provider files registered by the LSP host —
    /// module service providers discovered via the `modules.paths` setting,
    /// which live outside `app/Providers/` (e.g.
    /// `app/{Parent}/{Module}/Providers/`). Scanned like app providers, but
    /// ordered BEFORE them so a real `app/Providers/` registration still
    /// wins on a namespace conflict (the app boots last).
    extra_provider_files: Vec<PathBuf>,
    /// How many times this cache has touched disk — one per
    /// `fs::read_to_string` or `read_dir` that actually ran. Lets a test prove
    /// a second resolution is served from Salsa rather than re-read.
    ///
    /// Per-cache rather than a global counter: caches are per-actor, so every
    /// test owns its own and concurrent tests cannot perturb each other's
    /// counts. A process-wide counter made these assertions racy — any other
    /// test resolving a translation moved the number.
    disk_reads: usize,
}

impl TranslationCache {
    /// How many times this cache has touched disk. See [`Self::disk_reads`].
    pub fn disk_reads(&self) -> usize {
        self.disk_reads
    }

    /// Push a catalogue's authoritative text into Salsa — an editor buffer via
    /// `did_change`, or a fresh disk read. Creates the input on first sight and
    /// updates it in place afterwards, so dependent queries are invalidated
    /// rather than duplicated.
    pub fn register(&mut self, db: &mut LaravelDatabase, path: PathBuf, text: String) {
        self.version += 1;
        if let Some(file) = self.files.get(&path) {
            file.set_version(db).to(self.version);
            file.set_text(db).to(text);
        } else {
            let file = LangFile::new(&*db, path.clone(), self.version, text);
            self.files.insert(path, file);
        }
    }

    /// Drop a lang path's cached entry, and the directory listings that could
    /// have enumerated it, so the next lookup re-reads disk. Covers external
    /// create, change and delete alike — a create clears the negative entry, a
    /// delete clears the positive one.
    ///
    /// Two directory levels are cleared because both can be invalidated by one
    /// file event: for `lang/de/validation.php` the parent (`lang/de`) is the
    /// locale directory and the grandparent (`lang/`) is what enumerates `de`
    /// as a locale at all; for `lang/vendor/pkg/de/messages.php` the
    /// grandparent (`lang/vendor/pkg`) is the namespace directory locale
    /// discovery reads. A `lang/de.json` catalogue is covered by the first
    /// level alone.
    ///
    /// Eager invalidation, lazy re-read — the same trade `file_watcher` makes,
    /// so a `git checkout` touching many lang files does no I/O here and pays
    /// only for the catalogues a later request actually asks for.
    pub fn invalidate(&mut self, path: &Path) {
        self.files.remove(path);
        let mut dir = path.parent();
        for _ in 0..2 {
            let Some(d) = dir else { break };
            self.dirs.remove(d);
            dir = d.parent();
        }
    }

    /// The Salsa input for `path`, registering it on first touch.
    ///
    /// **Fail-closed** (issue #248): a path that cannot be proven inside `root`
    /// is registered with empty text rather than read, which resolves to no key
    /// exactly as a failed read did. `path_within_root` canonicalizes, so an
    /// absent path and an escaping symlink are both refused here — and both
    /// then cost zero further disk touches, because the refusal is cached.
    fn ensure_file(&mut self, db: &mut LaravelDatabase, path: &Path, root: &Path) -> LangFile {
        if let Some(file) = self.files.get(path) {
            return *file;
        }
        let text = if crate::path_containment::path_within_root(path, root) {
            self.disk_reads += 1;
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };
        self.register(db, path.to_path_buf(), text);
        self.files[path]
    }

    /// The Salsa input for `dir`'s listing, registering it on first touch.
    /// Fail-closed on the same terms as [`Self::ensure_file`]: a directory that
    /// cannot be proven inside `root` is registered with an empty listing
    /// rather than enumerated, so an escaped directory can never surface its
    /// subdirectories as this key's locales (issue #248).
    fn ensure_dir(&mut self, db: &mut LaravelDatabase, dir: &Path, root: &Path) -> LangDir {
        if let Some(handle) = self.dirs.get(dir) {
            return *handle;
        }
        let entries: Vec<(String, bool)> = if crate::path_containment::path_within_root(dir, root) {
            self.disk_reads += 1;
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| {
                            (
                                entry.file_name().to_string_lossy().into_owned(),
                                entry.path().is_dir(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.version += 1;
        let handle = LangDir::new(&*db, dir.to_path_buf(), self.version, entries);
        self.dirs.insert(dir.to_path_buf(), handle);
        handle
    }

    /// The text of the config file backing group `group` (`config/app.php` for
    /// `app`), read at most once per cache instance.
    ///
    /// `None` — cached as such — when no readable file contributes to the
    /// group. `config_group_files` owns which files those are and in what
    /// precedence; called with no module directories, as
    /// [`crate::config_lookup::resolve_value`] itself does, it yields the
    /// project file alone, so its first entry is the whole group. Completion
    /// deliberately keeps that no-module scope: widening which files decide the
    /// preview locale is a change of a different kind from caching the read.
    ///
    /// The `is_file()` inside `config_group_files` is why an absent file costs
    /// no counted read: the probe is a stat, and only a file that exists is
    /// opened. That is what lets a test attribute exactly one
    /// [`Self::disk_reads`] to config resolution by differencing two fixtures.
    fn ensure_config(&mut self, root: &Path, group: &str) -> Option<&str> {
        let key = root.join("config").join(format!("{group}.php"));
        if !self.configs.contains_key(&key) {
            let files = crate::config::config_group_files(root, &[], group);
            let mut text = None;
            if let Some(path) = files.first() {
                self.disk_reads += 1;
                text = std::fs::read_to_string(path).ok();
            }
            self.configs.insert(key.clone(), text);
        }
        self.configs[&key].as_deref()
    }

    /// The source text of the value at `dotted_key`, resolved against the
    /// cached config text rather than a fresh read.
    ///
    /// A wrapper around [`crate::config_lookup::resolve_in_source`], not a
    /// change to [`crate::config_lookup::resolve_value`]: that function still
    /// reads on every call, and `hover_for_config` still uses it, so a hover
    /// keeps reflecting an edit immediately. Only this completion path is
    /// cached, and only it needs the invalidation wiring that comes with a
    /// cache.
    fn config_value(&mut self, root: &Path, dotted_key: &str) -> Option<String> {
        let mut parts = dotted_key.split('.');
        let group = parts.next()?;
        let key_path: Vec<&str> = parts.collect();
        let text = self.ensure_config(root, group)?;
        crate::config_lookup::resolve_in_source(text, &key_path)
    }

    /// Drop a config file's cached text, so the next completion re-reads it.
    ///
    /// Clears a negative entry as well as a positive one: a `config/app.php`
    /// that did not exist when completion first asked is exactly the case a
    /// `CREATED` event reports, and leaving the cached absence in place would
    /// make the new file invisible for the rest of the session.
    pub fn invalidate_config(&mut self, path: &Path) {
        self.configs.remove(path);
    }

    /// The locale whose catalogues supply autocomplete's preview values.
    ///
    /// Completion offers keys from exactly one locale (see
    /// [`Self::completion_keys`]), so *which* one decides every value previewed
    /// next to a key. That used to be the alphabetically-first directory, which
    /// is deterministic but arbitrary: a project with `lang/de/`, `lang/en/` and
    /// `'locale' => 'en'` previewed German for every key (issue #340).
    ///
    /// The chain, first match wins:
    ///
    /// 1. `app.locale` from `config/app.php`, normalized by
    ///    [`config_string_literal`], when it names one of `candidates`;
    /// 2. `app.fallback_locale`, under the same normalization and the same
    ///    membership check — Laravel ships it `env()`-wrapped, so it needs the
    ///    normalization just as much as `app.locale` does;
    /// 3. the alphabetically-first candidate, preserving the pre-#340 behaviour
    ///    for any project whose config cannot be read statically.
    ///
    /// Both lookups go through [`Self::config_value`], so a project whose chain
    /// runs to step 2 still pays one read of `config/app.php`, not two — and a
    /// second completion request pays none (issue #349). Before that, this read
    /// bypassed [`Self::disk_reads`] entirely, so the cache-hit regression tests
    /// could not see it at all.
    ///
    /// `candidates` is read, never mutated: step 3 sees the whole list
    /// [`locales_in_dir`] returned, not what the earlier steps failed to match,
    /// so the fallback answers with the same locale it always did.
    ///
    /// `None` only when `candidates` is empty — a project with no locale
    /// directories has nothing to preview, whatever its config says.
    fn completion_locale(&mut self, root: &Path, candidates: &[String]) -> Option<String> {
        ["app.locale", "app.fallback_locale"]
            .into_iter()
            .find_map(|key| {
                let locale = config_string_literal(&self.config_value(root, key)?)?;
                candidates.contains(&locale).then_some(locale)
            })
            .or_else(|| candidates.iter().min().cloned())
    }

    /// Resolve `key` in `locale`, returning the value and the catalogue it came
    /// from. Candidates are tried in
    /// [`translation_candidates`](crate::translation_lookup::translation_candidates)
    /// order — first hit wins, so a published vendor override beats the
    /// package's own unpublished file.
    ///
    /// `vendor_map` is consulted only to *name* candidate paths; it never
    /// enters a Salsa query key. So it can neither defeat memoization of the
    /// common dotted path (which never looks at it) nor serve a stale
    /// namespaced resolution (candidates are recomputed from the live map on
    /// every call).
    pub fn resolve(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
        key: &str,
        locale: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
    ) -> Option<ResolvedTranslationData> {
        for candidate in
            crate::translation_lookup::translation_candidates(root, key, locale, vendor_map)
        {
            let file = self.ensure_file(db, candidate.path(), root);
            // Salsa hands back a borrow of the memoized value; clone it so the
            // database borrow ends before the next candidate needs `&mut db`.
            let value = match &candidate {
                crate::translation_lookup::TranslationCandidate::Php { key_path, .. } => {
                    resolve_php_translation(&*db, file, key_path.clone()).clone()
                }
                crate::translation_lookup::TranslationCandidate::Json { key, .. } => {
                    resolve_json_translation(&*db, file, key.clone()).clone()
                }
            };
            if let Some(value) = value {
                return Some(ResolvedTranslationData {
                    value,
                    source_file: candidate.path().to_path_buf(),
                });
            }
        }
        None
    }

    /// Every locale that could define `key`, `app_locale` first and the rest
    /// alphabetically.
    ///
    /// Never returns empty: a project with no discoverable locales — no lang
    /// directory at all, or one containing nothing — falls back to
    /// `["en"]`, so callers always have something to resolve against.
    ///
    /// An `app_locale` no directory defines simply doesn't appear, leaving the
    /// alphabetical order untouched.
    pub fn locales(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
        key: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
        app_locale: Option<&str>,
    ) -> Vec<String> {
        let mut locales: Vec<String> = Vec::new();
        for dir in crate::translation_lookup::locale_candidate_dirs(root, key, vendor_map) {
            let handle = self.ensure_dir(db, &dir, root);
            let found = locales_in_dir(&*db, handle).clone();
            // Dedupe across directories so a locale present in both the
            // published and the unpublished vendor directory is listed once.
            for locale in found {
                if !locales.contains(&locale) {
                    locales.push(locale);
                }
            }
        }

        if locales.is_empty() {
            return vec![crate::translation_lookup::DEFAULT_LOCALE.to_string()];
        }

        locales.sort();
        if let Some(app_locale) = app_locale {
            if let Some(idx) = locales.iter().position(|l| l == app_locale) {
                let leading = locales.remove(idx);
                locales.insert(0, leading);
            }
        }
        locales
    }

    /// Where `target` is declared inside the catalogue at `path`.
    ///
    /// Go-to-definition resolves a key to a file and then needs the key's own
    /// line within it. That second step used to re-read the file — and, for a
    /// `.php` catalogue, re-run a full tree-sitter parse — on every jump, even
    /// though the resolution immediately before it had just read the same file
    /// (issue #293).
    pub fn locate_key(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
        path: &Path,
        target: &TranslationKeyTarget,
    ) -> Option<KeyLocationData> {
        let file = self.ensure_file(db, path, root);
        let found = match target {
            TranslationKeyTarget::Php(key_path) => {
                locate_php_key_in_file(&*db, file, key_path.clone())
            }
            TranslationKeyTarget::Json(key) => locate_json_key_in_file(&*db, file, key.clone()),
        };
        found.map(
            |(line, start_column, end_column, _renameable)| KeyLocationData {
                line,
                start_column,
                end_column,
            },
        )
    }

    /// Every translation key autocomplete should offer, sorted and deduped.
    ///
    /// Semantics deliberately preserved from the direct-`fs` version: the
    /// **first** lang root that exists wins, and within it **one** locale
    /// answers for the whole project — a key present in one locale is offered
    /// regardless of which locale declares it, so enumerating the union would
    /// read every catalogue in the project to produce the same list. Only the
    /// reads changed: the directory listings and every catalogue's key
    /// extraction are now memoized, where before each completion request
    /// re-enumerated two directories and re-read *and re-parsed* every
    /// catalogue in the locale (issue #293).
    ///
    /// Which locale that is comes from [`completion_locale`]: the app's
    /// configured locale where `config/app.php` names one this project
    /// defines, else the alphabetically-first (issue #340).
    ///
    /// `exists()` on the lang roots is a stat, not a read, and picking the root
    /// this way keeps the pre-cache behaviour exactly: a first lang root that
    /// exists but holds no locale directory yields no completions rather than
    /// falling through to the second root.
    pub fn completion_keys(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
    ) -> Vec<TranslationKeyCompletionData> {
        // Root-catalogue scan. Its absence must not end the request early:
        // a project whose ONLY catalogues live under registered namespaces
        // (never published to a root `lang/`) still gets the namespaced
        // completions below.
        let mut completions = Vec::new();
        let lang_root = crate::translation_lookup::project_lang_roots(root)
            .into_iter()
            .find(|dir| dir.exists());
        let locale = lang_root.as_ref().and_then(|lang_root| {
            let listing = self.ensure_dir(db, lang_root, root);
            // `completion_locale` selects by NAME (configured locale first,
            // alphabetical minimum as the last resort) — never by position
            // in the filesystem-dependent listing order.
            let candidates = locales_in_dir(&*db, listing).clone();
            self.completion_locale(root, &candidates)
        });
        if let (Some(lang_root), Some(locale)) = (lang_root, locale) {
            let locale_dir = lang_root.join(&locale);
            let files = self.ensure_dir(db, &locale_dir, root).entries(&*db).clone();

            for (name, is_dir) in files {
                if is_dir {
                    continue;
                }
                let path = locale_dir.join(&name);
                if path.extension().is_none_or(|ext| ext != "php") {
                    continue;
                }
                let Some(base_key) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let source = format!("lang/{}/{}", locale, name);
                let file = self.ensure_file(db, &path, root);
                for (key, value) in
                    translation_keys_in_file(&*db, file, base_key.to_string()).clone()
                {
                    completions.push(TranslationKeyCompletionData {
                        key,
                        value,
                        source: source.clone(),
                    });
                }
            }
        }

        // Namespaced catalogues — every provider-registered `ns::` lang
        // directory (vendor packages, modules, app `loadTranslationsFrom`
        // calls). Without these, a project that keeps a namespace's
        // catalogues only under its registered directory (never published
        // to root `lang/vendor/…`) gets zero completions for
        // `ns::file.key`. Same locale-selection rule as the root scan.
        let namespaces = self.vendor_namespaces(db, root);
        let mut namespace_pairs: Vec<(String, PathBuf)> = namespaces.into_iter().collect();
        namespace_pairs.sort();
        for (namespace, ns_dir) in namespace_pairs {
            let listing = self.ensure_dir(db, &ns_dir, root);
            let ns_locales = locales_in_dir(&*db, listing).clone();
            let Some(ns_locale) = self.completion_locale(root, &ns_locales) else {
                continue;
            };
            let ns_locale_dir = ns_dir.join(&ns_locale);
            let files = self
                .ensure_dir(db, &ns_locale_dir, root)
                .entries(&*db)
                .clone();
            for (name, is_dir) in files {
                if is_dir {
                    continue;
                }
                let path = ns_locale_dir.join(&name);
                if path.extension().is_none_or(|ext| ext != "php") {
                    continue;
                }
                let Some(base_key) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let source = format!("{}::lang/{}/{}", namespace, ns_locale, name);
                let file = self.ensure_file(db, &path, root);
                for (key, value) in
                    translation_keys_in_file(&*db, file, base_key.to_string()).clone()
                {
                    completions.push(TranslationKeyCompletionData {
                        key: format!("{}::{}", namespace, key),
                        value,
                        source: source.clone(),
                    });
                }
            }
        }

        completions.sort_by(|a, b| a.key.cmp(&b.key));
        completions.dedup_by(|a, b| a.key == b.key);
        completions
    }

    /// Every locale file that declares `dotted_key`, with the position of the
    /// key's own characters — the declaration sites a rename must edit.
    ///
    /// Walks `lang/<locale>/<file>.php` for each locale directory under
    /// `<root>/lang`. Locales without the relevant file, or without the key in
    /// it, are simply skipped.
    ///
    /// Two things changed when this moved off direct `fs` (issue #293). It is
    /// now memoized, so renaming a key no longer re-reads and re-parses every
    /// locale's catalogue — the same files resolution had already loaded. And
    /// it is now **fenced**: the previous implementation joined a key-derived
    /// file segment onto a locale directory and read the result with no
    /// containment check at all, where routing through [`Self::ensure_file`]
    /// brings it under the same fail-closed guard as every other read
    /// (issue #248).
    ///
    /// That guard is defence in depth here rather than a closed hole: the key
    /// is split on `.` before the stem is used, so a `../` escape leaves the
    /// stem empty and never reaches a read. It still matters for a stem naming
    /// an absolute path, which `Path::join` would otherwise let collapse the
    /// base away.
    ///
    /// Deliberately still `lang/` only, not `resources/lang/` — that asymmetry
    /// is pre-existing rename behaviour, and widening which files a rename
    /// edits is a change of a different kind from caching one.
    pub fn locate_key_across_locales(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
        dotted_key: &str,
    ) -> Vec<TranslationKeyLocationData> {
        let Some((file_stem, key_path)) =
            crate::translation_key_locator::split_dotted_key(dotted_key)
        else {
            return Vec::new();
        };

        let lang_dir = root.join("lang");
        // The raw listing rather than `locales_in_dir`: this walk has always
        // considered exactly the subdirectories, so a `{locale}.json` stem is
        // not a locale here.
        let entries = self.ensure_dir(db, &lang_dir, root).entries(&*db).clone();

        let mut out = Vec::new();
        for (name, is_dir) in entries {
            if !is_dir {
                continue;
            }
            let locale_file = lang_dir.join(&name).join(format!("{file_stem}.php"));
            let file = self.ensure_file(db, &locale_file, root);
            // A `false` flag means the key has no quoted text to rewrite — a
            // list index (`page.items.0`) or a bare `404 =>`. Both resolve for
            // goto; neither is a rename target.
            if let Some(&(line, start_column, end_column, true)) =
                locate_php_key_in_file(&*db, file, key_path.clone()).as_ref()
            {
                out.push(TranslationKeyLocationData {
                    file_path: locale_file,
                    location: KeyLocationData {
                        line,
                        start_column,
                        end_column,
                    },
                });
            }
        }
        out
    }

    /// Drop everything derived from service providers, so the next namespace
    /// lookup rediscovers and re-reads them.
    ///
    /// Both halves are cleared: the discovered set (a provider may have been
    /// created or deleted) and every provider's cached text (one may have been
    /// edited). Lang catalogues are untouched — a provider edit changes *where*
    /// a namespaced key resolves, not what any catalogue contains.
    pub fn invalidate_providers(&mut self) {
        self.provider_files = None;
        self.providers.clear();
    }

    /// Replace the host-registered extra provider files (module providers
    /// from the `modules.paths` setting). A no-op when the set is unchanged;
    /// otherwise the discovered provider set is dropped so the next lookup
    /// folds the new files in.
    pub fn set_extra_provider_files(&mut self, mut files: Vec<PathBuf>) {
        files.sort();
        if files == self.extra_provider_files {
            return;
        }
        self.extra_provider_files = files;
        self.invalidate_providers();
    }

    /// The Salsa input for one provider's text, registering it on first touch.
    ///
    /// No containment guard here, unlike [`Self::ensure_file`], and the
    /// difference is deliberate: these paths come from walking the project's
    /// own `vendor/` and `app/Providers/` directories, not from joining an
    /// untrusted key segment onto a directory. There is nothing for #248 to
    /// fence — and canonicalizing every `*ServiceProvider*.php` in `vendor/`
    /// would add thousands of syscalls to buy nothing.
    fn ensure_provider(
        &mut self,
        db: &mut LaravelDatabase,
        path: &Path,
    ) -> TranslationProviderFile {
        if let Some(file) = self.providers.get(path) {
            return *file;
        }
        self.disk_reads += 1;
        let text = std::fs::read_to_string(path).unwrap_or_default();
        self.version += 1;
        let file = TranslationProviderFile::new(&*db, path.to_path_buf(), self.version, text);
        self.providers.insert(path.to_path_buf(), file);
        file
    }

    /// The discovered provider set, walking `vendor/` and `app/Providers/` on
    /// first touch.
    fn ensure_provider_files(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
    ) -> TranslationProviderFiles {
        if let Some(files) = self.provider_files {
            return files;
        }
        self.disk_reads += 1;
        let vendor = crate::vendor_translations::vendor_provider_candidates(root);
        let mut app = self.extra_provider_files.clone();
        app.extend(crate::vendor_translations::app_provider_candidates(root));
        self.version += 1;
        let files = TranslationProviderFiles::new(&*db, self.version, vendor, app);
        self.provider_files = Some(files);
        files
    }

    /// The project's `namespace -> lang directory` map, as registered by its
    /// service providers.
    ///
    /// Precedence is preserved from the direct-`fs` scans: **first-match-wins**
    /// within a scan (provider boot order is non-deterministic and we cannot
    /// rank packages without a full composer graph), and the **app scan
    /// overrides vendor** on conflict, because the app boots last.
    ///
    /// Every read behind this — the directory walk, each provider's text, the
    /// substring gate and the AST parse — is memoized. Previously the whole
    /// sweep ran once and its result was cached for the life of the session
    /// with no invalidation path at all, so a `composer update` or an edited
    /// `loadTranslationsFrom` was invisible until the LSP restarted. Since this
    /// map decides where a namespaced key resolves, that made a stale map a
    /// wrong answer rather than merely an old one (issue #293).
    pub fn vendor_namespaces(
        &mut self,
        db: &mut LaravelDatabase,
        root: &Path,
    ) -> HashMap<String, PathBuf> {
        let files = self.ensure_provider_files(db, root);
        let vendor = files.vendor(&*db).clone();
        let app = files.app(&*db).clone();

        let mut map: HashMap<String, PathBuf> = HashMap::new();
        for path in vendor {
            let file = self.ensure_provider(db, &path);
            for (namespace, dir) in
                translation_namespaces_in_provider(&*db, file, root.to_path_buf()).clone()
            {
                map.entry(namespace).or_insert(dir);
            }
        }
        for path in app {
            let file = self.ensure_provider(db, &path);
            for (namespace, dir) in
                translation_namespaces_in_provider(&*db, file, root.to_path_buf()).clone()
            {
                map.insert(namespace, dir);
            }
        }
        map
    }
}

/// One translation key offered by autocomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationKeyCompletionData {
    /// The full dot-notation key (e.g. `messages.welcome`)
    pub key: String,
    /// The translated value at **full length**, untruncated. Each render
    /// site clips it to its own budget via
    /// `completion_display::{COMPLETION_DETAIL_LIMIT, COMPLETION_DOC_LIMIT}`
    /// (issue #326).
    pub value: String,
    /// Display source (e.g. `lang/en/messages.php`)
    pub source: String,
}

/// One locale's declaration of a translation key, for building a rename edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationKeyLocationData {
    /// The catalogue declaring the key.
    pub file_path: PathBuf,
    /// Where in it the key's own characters sit.
    pub location: KeyLocationData,
}

/// Which key to locate inside a catalogue, and how to match it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationKeyTarget {
    /// Nested array path inside a `.php` catalogue.
    Php(Vec<String>),
    /// The source string itself, inside a `.json` catalogue.
    Json(String),
}

/// The line and column span of a key's declaration inside a catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLocationData {
    /// 0-based line
    pub line: u32,
    /// 0-based column of the key content (inside the quotes)
    pub start_column: u32,
    /// 0-based column one past the key content
    pub end_column: u32,
}

/// Every `key => value` pair one PHP catalogue declares, dot-prefixed with
/// `base_key`. Memoized per `(file, base_key)` — autocomplete used to re-read
/// and re-parse every catalogue in the locale on each request (issue #293).
///
/// Enumerated by the same tree-sitter walk that backs go-to-definition
/// (`config_key_locator`), not by a text scanner. Until #369 this counted `[`
/// and `]` per line, which mis-nested any catalogue holding a list entry, a
/// key split across lines, or a `]` inside a value — while goto, resolving the
/// identical file structurally, stayed correct.
#[salsa::tracked]
pub fn translation_keys_in_file(
    db: &dyn Db,
    file: LangFile,
    base_key: String,
) -> Vec<(String, String)> {
    let text = file.text(db);
    if text.is_empty() {
        return Vec::new();
    }
    crate::config_key_locator::enumerate_entries_in_source(text)
        .into_iter()
        .map(|(key, value, _position)| (format!("{base_key}.{key}"), value))
        .collect()
}

/// Where a nested key is declared inside a PHP catalogue, as
/// `(line, start_column, end_column)`.
///
/// A tuple rather than `KeyPosition` so the return type is trivially
/// `salsa::Update`; the handler widens it to [`KeyLocationData`].
///
/// Memoized per `(file, key_path)`: go-to-definition used to re-read the file
/// **and re-run a tree-sitter parse** on every jump (issue #293).
#[salsa::tracked]
pub fn locate_php_key_in_file(
    db: &dyn Db,
    file: LangFile,
    key_path: Vec<String>,
) -> Option<(u32, u32, u32, bool)> {
    let text = file.text(db);
    if text.is_empty() {
        return None;
    }
    let refs: Vec<&str> = key_path.iter().map(String::as_str).collect();
    let pos = crate::config_key_locator::locate_in_source(text, &refs)?;
    // The bool is "may a rename rewrite this", the only distinction this
    // boundary needs; `KeyKind` itself stays on the locator's side.
    Some((
        pos.line,
        pos.start_column,
        pos.end_column,
        pos.kind == crate::config_key_locator::KeyKind::Quoted,
    ))
}

/// Where a text key is declared inside a JSON catalogue, as
/// `(line, start_column, end_column)`.
///
/// The key is matched as a quoted literal and the span covers the key content
/// inside its quotes — the same shape the PHP locator returns, so
/// go-to-definition highlights identically across both catalogue formats.
#[salsa::tracked]
pub fn locate_json_key_in_file(
    db: &dyn Db,
    file: LangFile,
    key: String,
) -> Option<(u32, u32, u32, bool)> {
    let text = file.text(db);
    if text.is_empty() {
        return None;
    }
    let needle = format!("\"{key}\"");
    text.lines().enumerate().find_map(|(line_num, line)| {
        let col = line.find(&needle)?;
        // Skip the opening quote so the span covers the key itself.
        let start = (col + 1) as u32;
        // A JSON key is always a quoted literal, so always renameable — the
        // flag exists for PHP's list indices and bare integer keys.
        Some((line_num as u32, start, start + key.len() as u32, true))
    })
}

// ============================================================================
// Service Provider Parsing (Salsa-based)
// ============================================================================

/// Binding type for container bindings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum BindingTypeEnum {
    Singleton,
    Bind,
    Instance,
    Alias,
}

/// A parsed middleware registration (Salsa tracked)
#[salsa::tracked]
pub struct ParsedMiddlewareReg<'db> {
    /// Middleware alias (e.g., "auth", "throttle")
    pub alias: MiddlewareName<'db>,
    /// Full class name
    #[returns(ref)]
    pub class_name: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed container binding (Salsa tracked)
#[salsa::tracked]
pub struct ParsedBindingReg<'db> {
    /// Abstract name or interface
    pub abstract_name: BindingName<'db>,
    /// Concrete class name
    #[returns(ref)]
    pub concrete_class: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Type of binding
    #[returns(copy)]
    pub binding_type: BindingTypeEnum,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed macro/mixin registration (Salsa tracked).
///
/// Example: `Str::macro('uuid7', fn () => ...)` registers a `uuid7` macro on the
/// `Macroable` host `Illuminate\Support\Str`; `Str::mixin(new MyMixin)` registers
/// every public method of `MyMixin` as a macro on the same host. The
/// `receiver_fqcn` is the resolved host FQCN (token resolved through the file's
/// `use` imports + facade alias map, so it agrees with the call-site receiver
/// resolution), `macro_name` is the registered member name, and the source
/// file + line point at the **definition site** — the closure for a scalar macro,
/// the mixin method for a mixin-expanded one.
#[salsa::tracked]
pub struct ParsedMacroReg<'db> {
    /// Resolved Macroable host FQCN (e.g. "Illuminate\\Support\\Str").
    pub receiver_fqcn: BindingName<'db>,
    /// The registered macro/method name (e.g. "uuid7").
    pub macro_name: BindingName<'db>,
    /// Definition site file — the provider for a scalar macro (closure), or the
    /// mixin class file for a mixin-expanded method.
    #[returns(ref)]
    pub decl_file: PathBuf,
    /// 0-based definition line — the closure's line, or the mixin method's line.
    #[returns(copy)]
    pub decl_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
}

/// A parsed view namespace registration from loadViewsFrom() (Salsa tracked)
/// Example: $this->loadViewsFrom(__DIR__.'/../resources/views', 'courier')
#[salsa::tracked]
pub struct ParsedViewNamespaceReg<'db> {
    /// Package namespace (e.g., "courier")
    pub namespace: PackageNamespace<'db>,
    /// Resolved view path (if found)
    #[returns(ref)]
    pub view_path: Option<PathBuf>,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed Blade component registration from Blade::component() (Salsa tracked)
/// Example: Blade::component('package-alert', AlertComponent::class)
#[salsa::tracked]
pub struct ParsedBladeComponentReg<'db> {
    /// Component tag name (e.g., "package-alert")
    pub tag_name: ComponentName<'db>,
    /// Full class name
    #[returns(ref)]
    pub class_name: String,
    /// Resolved file path (if found)
    #[returns(ref)]
    pub file_path: Option<PathBuf>,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed component namespace registration from Blade::componentNamespace() (Salsa tracked)
/// Example: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
#[salsa::tracked]
pub struct ParsedComponentNamespaceReg<'db> {
    /// Component namespace prefix (e.g., "nightshade")
    pub prefix: PackageNamespace<'db>,
    /// PHP namespace (e.g., "Nightshade\\Views\\Components")
    #[returns(ref)]
    pub php_namespace: String,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed anonymous component path registration from
/// Blade::anonymousComponentPath() (Salsa tracked).
/// Example: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
#[salsa::tracked]
pub struct ParsedAnonymousComponentPathReg<'db> {
    /// Component prefix (e.g., "backstage")
    pub prefix: PackageNamespace<'db>,
    /// Resolved absolute directory holding the anonymous components
    #[returns(ref)]
    pub directory: PathBuf,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// A parsed anonymous component namespace registration from
/// Blade::anonymousComponentNamespace() (Salsa tracked).
/// Example: Blade::anonymousComponentNamespace('components.flux', 'flux')
#[salsa::tracked]
pub struct ParsedAnonymousComponentNamespaceReg<'db> {
    /// Component prefix (e.g., "flux")
    pub prefix: PackageNamespace<'db>,
    /// Directory relative to the view paths (dots normalized to slashes)
    #[returns(ref)]
    pub directory: String,
    /// Line in source file where registered
    #[returns(copy)]
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    #[returns(copy)]
    pub priority: u8,
    /// Source file where registered
    #[returns(ref)]
    pub source_file: PathBuf,
}

/// Parsed service provider content
#[salsa::tracked]
pub struct ParsedServiceProvider<'db> {
    /// Middleware registrations found in this provider
    #[returns(ref)]
    pub middleware: Vec<ParsedMiddlewareReg<'db>>,
    /// Container bindings found in this provider
    #[returns(ref)]
    pub bindings: Vec<ParsedBindingReg<'db>>,
    /// Macro/mixin registrations found in this provider (`Str::macro(...)`,
    /// `Str::mixin(...)`)
    #[returns(ref)]
    pub macros: Vec<ParsedMacroReg<'db>>,
    /// View namespace registrations from loadViewsFrom()
    #[returns(ref)]
    pub view_namespaces: Vec<ParsedViewNamespaceReg<'db>>,
    /// Manual Blade component registrations from Blade::component()
    #[returns(ref)]
    pub blade_components: Vec<ParsedBladeComponentReg<'db>>,
    /// Component namespace registrations from Blade::componentNamespace()
    #[returns(ref)]
    pub component_namespaces: Vec<ParsedComponentNamespaceReg<'db>>,
    /// Anonymous component path registrations from Blade::anonymousComponentPath()
    #[returns(ref)]
    pub anonymous_component_paths: Vec<ParsedAnonymousComponentPathReg<'db>>,
    /// Anonymous component namespace registrations from Blade::anonymousComponentNamespace()
    #[returns(ref)]
    pub anonymous_component_namespaces: Vec<ParsedAnonymousComponentNamespaceReg<'db>>,
}

/// Parse a service provider file and extract middleware, bindings, views, and components
#[salsa::tracked]
pub fn parse_service_provider_source<'db>(
    db: &'db dyn Db,
    file: ServiceProviderFile,
    root: PathBuf,
) -> ParsedServiceProvider<'db> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Matches $this->app->alias('concrete', 'alias')
        static ref ALIAS_RE: Regex = Regex::new(
            r#"\$this->app->alias\s*\(\s*\\?([A-Za-z0-9_\\]+)(?:::class)?\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches `$this->loadViewsFrom(<path expression>, 'namespace')` with
        /// arbitrary whitespace around the `->` (Pint puts a fluent `->` on its
        /// own line). The first argument is captured whole and handed to
        /// [`resolve_php_path_expr`], so `__DIR__ . '/rel'`,
        /// `realpath(__DIR__ . '/rel')` and `resource_path('views/vendor/ns')`
        /// all resolve through one path rather than a pattern each.
        ///
        /// Receivers other than `$this` (`static::`, `$this->app->`) are
        /// deliberately NOT matched: the framework resolves those against a
        /// different object, and guessing a directory for them would register
        /// a wrong path rather than none. Runtime-computed arguments
        /// (`loadViewsFrom($dir, $name)`) stay out of reach lexically — the
        /// builder-convention reconstruction further down covers the common
        /// package case.
        static ref LOAD_VIEWS_RE: Regex = Regex::new(
            r#"\$this\s*->\s*loadViewsFrom\s*\(\s*(.+?)\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a class-backed component registration with the tag first:
        /// `Blade::component('tag-name', Class::class)` — facade form — or
        /// `$blade->component('tag-name', Class::class)` — the instance form
        /// the framework itself uses (ViewServiceProvider registers
        /// `dynamic-component` on the compiler instance inside a tap() closure).
        static ref BLADE_COMPONENT_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class\s*\)"#
        ).unwrap();

        /// Same registration with the canonical argument order:
        /// `component(Class::class, 'tag-name')`. `BladeCompiler::component`
        /// accepts both orders and swaps internally; statically the `::class`
        /// suffix marks which argument is the class, so we match each order
        /// with its own pattern.
        static ref BLADE_COMPONENT_CLASS_FIRST_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*\\?([A-Za-z0-9_\\]+)::class\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a class-backed registration whose tag is a config-driven
        /// prefix concatenation: `Blade::component($prefix . 'card', Card::class)`.
        /// MaryUI registers its whole catalog this way, reading the prefix
        /// from `config('mary.prefix')` once at the top of the method.
        static ref BLADE_COMPONENT_PREFIXED_RE: Regex = Regex::new(
            r#"(?:Blade::|\$\w+->)component\s*\(\s*\$(\w+)\s*\.\s*['"]([^'"]+)['"]\s*,\s*\\?([A-Za-z0-9_\\]+)::class\s*\)"#
        ).unwrap();

        /// Matches the prefix-variable assignment feeding the form above:
        /// `$prefix = config('mary.prefix');` (with or without a default arg).
        static ref CONFIG_VAR_ASSIGN_RE: Regex = Regex::new(
            r#"\$(\w+)\s*=\s*config\(\s*['"]([\w.-]+)['"]\s*[,)]"#
        ).unwrap();

        /// Matches Blade::componentNamespace('Namespace\\Path', 'prefix')
        static ref COMPONENT_NAMESPACE_RE: Regex = Regex::new(
            r#"Blade::componentNamespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches a fluent package-builder name declaration: `->name('package')`.
        /// The literal `loadViewsFrom`/`loadTranslationsFrom` patterns above only
        /// see Laravel-native registration. Builder-convention providers (the
        /// dominant one being laravel-package-tools, but this is form-based, not
        /// vendor-tied) declare capabilities fluently — `->name('x')->hasViews()`
        /// — and the real `loadViewsFrom($computedDir, $name)` runs in a base
        /// class with runtime args the literal patterns can't see. This pair of
        /// patterns reconstructs that registration form.
        static ref BUILDER_NAME_RE: Regex = Regex::new(
            r#"->name\s*\(\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();

        /// Matches the builder view capability: `->hasViews()` or
        /// `->hasViews('explicit-namespace')`. The optional capture is the
        /// namespace override; absent, the namespace is the package short-name.
        static ref BUILDER_HAS_VIEWS_RE: Regex = Regex::new(
            r#"->hasViews\s*\(\s*(?:['"]([^'"]+)['"])?\s*\)"#
        ).unwrap();
    }

    let text = file.text(db);
    let path = file.path(db);
    let priority = file.priority(db);

    let mut middleware = Vec::new();
    let mut bindings = Vec::new();
    let mut view_namespaces = Vec::new();
    let mut blade_components = Vec::new();
    let mut component_namespaces = Vec::new();

    // Parse middleware using tree-sitter for accurate context-aware extraction
    if let Ok(tree) = parse_php(text) {
        let language = language_php();
        if let Ok(patterns) = extract_all_php_patterns(&tree, text, &language) {
            tracing::debug!(
                "📦 Parsing {:?}: {} alias defs, {} group defs",
                path,
                patterns.middleware_alias_defs.len(),
                patterns.middleware_group_defs.len()
            );
            // Process middleware alias definitions (from $middlewareAliases property)
            for alias_def in &patterns.middleware_alias_defs {
                let class_str = alias_def.class_name.trim_start_matches('\\');
                let file_path = resolve_class_to_file_internal(class_str, &root);

                let alias_name = MiddlewareName::new(db, alias_def.alias.to_string());
                middleware.push(ParsedMiddlewareReg::new(
                    db,
                    alias_name,
                    class_str.to_string(),
                    file_path,
                    // Tree-sitter's row is 0-based, but the
                    // `source_line` field is 1-based by convention
                    // (matches binding source_line and the goto-def
                    // consumer that subtracts 1). +1 to convert.
                    alias_def.row as u32 + 1,
                    priority,
                    path.clone(),
                ));
            }

            // Process middleware group definitions (from $middlewareGroups property)
            // Track existing aliases to avoid duplicates
            let existing_aliases: std::collections::HashSet<String> = middleware
                .iter()
                .map(|m| m.alias(db).name(db).to_string())
                .collect();

            for group_def in &patterns.middleware_group_defs {
                // Skip if already registered as an alias
                if existing_aliases.contains(group_def.group_name) {
                    continue;
                }

                tracing::debug!("   Found group: '{}'", group_def.group_name);
                let alias_name = MiddlewareName::new(db, group_def.group_name.to_string());
                middleware.push(ParsedMiddlewareReg::new(
                    db,
                    alias_name,
                    format!("MiddlewareGroup<{}>", group_def.group_name), // Placeholder to indicate it's a group
                    None, // Groups don't have a single file
                    // Same 0-based → 1-based correction as the alias
                    // branch above; goto-def + the rename locator both
                    // expect 1-based source_line.
                    group_def.row as u32 + 1,
                    priority,
                    path.clone(),
                ));
            }

            if !middleware.is_empty() {
                tracing::info!(
                    "🔐 Extracted {} middleware from {:?}: {:?}",
                    middleware.len(),
                    path,
                    middleware
                        .iter()
                        .map(|m| m.alias(db).name(db).to_string())
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    // Parse bind/singleton/scoped registrations via tree-sitter. Walking the
    // PHP AST (rather than the former BINDING_*_RE regexes) lets a closure's
    // return expression be resolved to its concrete model — a closure body's
    // nested braces, quotes, and multiple returns are beyond what a regex can
    // reliably parse. The argument node's kind classifies each concrete as a
    // `Class::class` const, a closure, or a bare key. Each abstract name is
    // registered once (first occurrence in source order wins).
    if let Ok(binding_tree) = parse_php(text) {
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&binding_tree, text);
        let mut bindings_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for pb in extract_provider_bindings(&binding_tree, text, &root, &aliases) {
            if !bindings_seen.insert(pb.abstract_name.clone()) {
                continue;
            }
            let file_path = if pb.resolve_file {
                resolve_class_to_file_internal(&pb.concrete_class, &root)
            } else {
                None
            };
            let binding_name = BindingName::new(db, pb.abstract_name);
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                pb.concrete_class,
                file_path,
                pb.binding_type,
                pb.source_line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse macro/mixin registrations (`Str::macro('foo', fn …)`,
    // `Str::mixin(new MyMixin)`) via tree-sitter. The receiver token is resolved
    // to its Macroable host FQCN the same way the call site resolves it (use
    // imports + facade alias map), so registry keys agree with lookup keys. The
    // first registration of a `(host, name)` pair in source order wins the dedup.
    let mut macros = Vec::new();
    if let Ok(macro_tree) = parse_php(text) {
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&macro_tree, text);
        let mut macros_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for pm in extract_provider_macros(&macro_tree, text, path, &root, &aliases) {
            if !macros_seen.insert((pm.receiver_fqcn.clone(), pm.macro_name.clone())) {
                continue;
            }
            let receiver = BindingName::new(db, pm.receiver_fqcn);
            let name = BindingName::new(db, pm.macro_name);
            macros.push(ParsedMacroReg::new(
                db,
                receiver,
                name,
                pm.decl_file,
                pm.decl_line,
                priority,
            ));
        }
    }

    // Parse alias registrations
    for cap in ALIAS_RE.captures_iter(text) {
        if let (Some(concrete), Some(alias)) = (cap.get(1), cap.get(2)) {
            let concrete_class = concrete.as_str().trim_start_matches('\\');
            let alias_name = alias.as_str();

            let line = text[..alias.start()].lines().count() as u32;
            let file_path = resolve_class_to_file_internal(concrete_class, &root);

            let binding_name = BindingName::new(db, alias_name.to_string());
            bindings.push(ParsedBindingReg::new(
                db,
                binding_name,
                concrete_class.to_string(),
                file_path,
                BindingTypeEnum::Alias,
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse loadViewsFrom() registrations. The first argument is captured as
    // a whole expression and resolved by `resolve_php_path_expr`, so every
    // supported shape goes through one path:
    //   $this->loadViewsFrom(__DIR__ . '/../resources/views', 'courier')
    //   $this->loadViewsFrom(realpath(__DIR__ . '/../views'), 'courier')
    //   $this->loadViewsFrom(resource_path('views/vendor/shop'), 'shop')
    // See the notes on LOAD_VIEWS_RE for the shapes deliberately left
    // unmatched, and why registering nothing beats registering a guess.
    let provider_dir = path.parent().unwrap_or(path.as_path());
    let registrations = LOAD_VIEWS_RE.captures_iter(text).filter_map(|cap| {
        let (expr, namespace) = (cap.get(1)?, cap.get(2)?);
        Some((
            resolve_php_path_expr(expr.as_str(), &root, provider_dir)?,
            namespace,
        ))
    });

    for (view_path, namespace) in registrations {
        let line = text[..namespace.start()].lines().count() as u32;
        let resolved_path = if view_path.exists() {
            Some(view_path.canonicalize().unwrap_or(view_path))
        } else {
            // For non-existent paths, store the normalized form so the
            // diagnostic can show the expected location even if it
            // doesn't resolve on disk yet.
            Some(normalize_path(&view_path))
        };

        let pkg_namespace = PackageNamespace::new(db, namespace.as_str().to_string());
        view_namespaces.push(ParsedViewNamespaceReg::new(
            db,
            pkg_namespace,
            resolved_path,
            line,
            priority,
            path.clone(),
        ));
    }

    // Parse imperative View-factory namespace registrations:
    //   View::addNamespace('ai-prompts', app_path('Ai/Prompts'))
    //   app('view')->prependNamespace('ns', resource_path('views/ns'))
    // Unlike loadViewsFrom() these take a Laravel path helper rather than a
    // __DIR__ concatenation, so the directory is resolved via the shared
    // path-expression resolver.
    {
        let provider_dir = path.parent().unwrap_or(path.as_path());
        for (namespace, directory, line) in
            extract_add_namespace_view_registrations(text, &root, provider_dir)
        {
            let pkg_namespace = PackageNamespace::new(db, namespace);
            view_namespaces.push(ParsedViewNamespaceReg::new(
                db,
                pkg_namespace,
                Some(directory),
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse the fluent package-builder view registration form:
    //   $package->name('filament')->hasViews();
    // Builder-convention providers register views from a base class
    // (`loadViewsFrom($computedDir, $name)`) with runtime-computed arguments, so
    // the literal LOAD_VIEWS_RE above never sees them. Reconstruct the
    // (namespace, directory) pair from the convention: the namespace is the
    // explicit `->hasViews('ns')` argument or the package short-name (a leading
    // `laravel-` stripped, matching the builder's own `shortName()` rule), and
    // the directory is the package's `resources/views` — one level up from the
    // provider's `src/` dir, which is where these builders resolve their base
    // path. The capability (`->hasViews(`) gates this: without it the provider
    // isn't registering views, so a stray `->name(` elsewhere can't misfire.
    if let Some(has_views) = BUILDER_HAS_VIEWS_RE.captures(text) {
        if let Some(name_cap) = BUILDER_NAME_RE.captures(text) {
            let package_name = name_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let namespace = has_views
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| builder_short_name(package_name));

            if !namespace.is_empty() {
                let line = text[..name_cap.get(0).map(|m| m.start()).unwrap_or(0)]
                    .lines()
                    .count() as u32;

                // Convention: provider in `<pkg>/src`, views at `<pkg>/resources/views`.
                let provider_dir = path.parent().unwrap_or(path.as_path());
                let view_path = provider_dir.join("../resources/views");
                let resolved_path = if view_path.exists() {
                    Some(view_path.canonicalize().unwrap_or(view_path))
                } else {
                    Some(normalize_path(&view_path))
                };

                let pkg_namespace = PackageNamespace::new(db, namespace);
                view_namespaces.push(ParsedViewNamespaceReg::new(
                    db,
                    pkg_namespace,
                    resolved_path,
                    line,
                    priority,
                    path.clone(),
                ));
            }
        }
    }

    // Parse class-backed component registrations, both argument orders and
    // both receivers (Blade:: facade and $instance->):
    //   Blade::component('package-alert', AlertComponent::class)
    //   $blade->component('dynamic-component', DynamicComponent::class)
    //   Blade::component(AlertComponent::class, 'alert')
    // A bare class name (`DynamicComponent::class`) is expanded to its FQN via
    // the file's `use` statements before file resolution, mirroring how PHP
    // itself resolves the reference.
    {
        let mut push_blade_component =
            |tag_name_str: &str, tag_offset: usize, class_match: regex::Match| {
                let class_str = expand_class_via_use_statements(
                    class_match.as_str().trim_start_matches('\\'),
                    text,
                );

                let line = text[..tag_offset].lines().count() as u32;
                let file_path = resolve_class_to_file_internal(&class_str, &root);

                let component_name = ComponentName::new(db, tag_name_str.to_string());
                blade_components.push(ParsedBladeComponentReg::new(
                    db,
                    component_name,
                    class_str,
                    file_path,
                    line,
                    priority,
                    path.clone(),
                ));
            };

        for cap in BLADE_COMPONENT_RE.captures_iter(text) {
            if let (Some(tag_name), Some(class)) = (cap.get(1), cap.get(2)) {
                push_blade_component(tag_name.as_str(), tag_name.start(), class);
            }
        }
        for cap in BLADE_COMPONENT_CLASS_FIRST_RE.captures_iter(text) {
            if let (Some(class), Some(tag_name)) = (cap.get(1), cap.get(2)) {
                push_blade_component(tag_name.as_str(), tag_name.start(), class);
            }
        }

        // Prefix-computed registrations (MaryUI's catalog form):
        //   $prefix = config('mary.prefix');
        //   Blade::component($prefix . 'card', Card::class);
        // The tag only exists after concatenating a config value, so resolve
        // the variable's config key from the same file and read the value the
        // way Laravel would at boot — the app's config override wins, else the
        // package's bundled config default. A key neither defines is PHP null,
        // which string-concatenates to '' (MaryUI's actual default).
        let config_vars: HashMap<&str, &str> = CONFIG_VAR_ASSIGN_RE
            .captures_iter(text)
            .filter_map(|cap| match (cap.get(1), cap.get(2)) {
                (Some(var), Some(key)) => Some((var.as_str(), key.as_str())),
                _ => None,
            })
            .collect();
        if !config_vars.is_empty() {
            for cap in BLADE_COMPONENT_PREFIXED_RE.captures_iter(text) {
                if let (Some(var), Some(suffix), Some(class)) = (cap.get(1), cap.get(2), cap.get(3))
                {
                    let Some(key) = config_vars.get(var.as_str()) else {
                        continue;
                    };
                    let prefix = crate::config::resolve_config_string_for_package(&root, key, path)
                        .unwrap_or_default();
                    let tag = format!("{prefix}{}", suffix.as_str());
                    push_blade_component(&tag, suffix.start(), class);
                }
            }
        }
    }

    // Parse Blade::componentNamespace() registrations
    // Example: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
    for cap in COMPONENT_NAMESPACE_RE.captures_iter(text) {
        if let (Some(php_ns), Some(prefix)) = (cap.get(1), cap.get(2)) {
            let php_namespace_str = php_ns.as_str();
            let prefix_str = prefix.as_str();

            let line = text[..prefix.start()].lines().count() as u32;

            let pkg_namespace = PackageNamespace::new(db, prefix_str.to_string());
            component_namespaces.push(ParsedComponentNamespaceReg::new(
                db,
                pkg_namespace,
                php_namespace_str.to_string(),
                line,
                priority,
                path.clone(),
            ));
        }
    }

    // Parse Blade::anonymousComponentPath() registrations.
    // Example: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
    let provider_dir = path.parent().unwrap_or(path.as_path());
    let mut anonymous_component_paths = Vec::new();
    for (prefix, directory, line) in extract_anonymous_component_paths(text, &root, provider_dir) {
        let pkg_namespace = PackageNamespace::new(db, prefix);
        anonymous_component_paths.push(ParsedAnonymousComponentPathReg::new(
            db,
            pkg_namespace,
            directory,
            line,
            priority,
            path.clone(),
        ));
    }

    // Parse Blade::anonymousComponentNamespace() registrations.
    // Example: Blade::anonymousComponentNamespace('components.flux', 'flux')
    let mut anonymous_component_namespaces = Vec::new();
    for (prefix, directory, line) in extract_anonymous_component_namespaces(text) {
        let pkg_namespace = PackageNamespace::new(db, prefix);
        anonymous_component_namespaces.push(ParsedAnonymousComponentNamespaceReg::new(
            db,
            pkg_namespace,
            directory,
            line,
            priority,
            path.clone(),
        ));
    }

    ParsedServiceProvider::new(
        db,
        middleware,
        bindings,
        macros,
        view_namespaces,
        blade_components,
        component_namespaces,
        anonymous_component_paths,
        anonymous_component_namespaces,
    )
}

/// A container binding extracted from a provider's AST, ready to become a
/// `ParsedBindingReg`. `concrete_class` is the resolved concrete FQCN, the
/// abstract name (bare form), or `"Closure"` for an unresolved closure;
/// `resolve_file` gates the PSR-4 file lookup (skipped for `"Closure"`).
struct ProviderBinding {
    abstract_name: String,
    concrete_class: String,
    binding_type: BindingTypeEnum,
    source_line: u32,
    resolve_file: bool,
}

/// A [`ClassFileResolver`](crate::member_resolver::ClassFileResolver) used while
/// resolving a closure's return expression during provider parsing: FQCN→file
/// via PSR-4, with no container-binding lookup (a closure that resolves another
/// bound key isn't modelled — it degrades to `"Closure"`).
struct ProviderBindingResolver<'a> {
    root: &'a Path,
}

impl crate::member_resolver::ClassFileResolver for ProviderBindingResolver<'_> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        resolve_class_to_file_internal(fqcn, self.root)
    }
}

/// Walk a provider's PHP AST for `$this->app->{bind,singleton,scoped}` container
/// registrations, classifying each by the kind of its concrete argument.
fn extract_provider_bindings(
    tree: &tree_sitter::Tree,
    text: &str,
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
) -> Vec<ProviderBinding> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    walk_provider_bindings(tree.root_node(), bytes, root, aliases, text, &mut out);
    out
}

/// Pre-order (document-order) descent collecting `ProviderBinding`s, so the
/// first registration of any key wins the dedup in the caller.
fn walk_provider_bindings(
    node: tree_sitter::Node,
    bytes: &[u8],
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
    text: &str,
    out: &mut Vec<ProviderBinding>,
) {
    if node.kind() == "member_call_expression" {
        if let Some(pb) = classify_binding_call(node, bytes, root, aliases, text) {
            out.push(pb);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_provider_bindings(child, bytes, root, aliases, text, out);
    }
}

/// Classify one `$this->app->{bind,singleton,scoped}('key', <concrete>)` call.
/// Returns `None` for any call that isn't such a registration, or whose
/// concrete argument can't be statically resolved to a class (e.g. a variable).
fn classify_binding_call(
    node: tree_sitter::Node,
    bytes: &[u8],
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
    text: &str,
) -> Option<ProviderBinding> {
    if !is_this_app_receiver(node.child_by_field_name("object")?, bytes) {
        return None;
    }
    let method = node.child_by_field_name("name")?.utf8_text(bytes).ok()?;
    let binding_type = match method {
        // `scoped` is a request-lifecycle singleton; for static resolution it
        // behaves identically to a singleton — a concrete bound to a key.
        "singleton" | "scoped" => BindingTypeEnum::Singleton,
        "bind" => BindingTypeEnum::Bind,
        _ => return None,
    };

    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut arg_exprs = args.named_children(&mut cursor).map(argument_value);

    let key_node = arg_exprs.next()??;
    let abstract_name = string_literal_text(key_node, bytes)?;
    let source_line = text[..key_node.start_byte()].lines().count() as u32;

    let (concrete_class, resolve_file) = match arg_exprs.next() {
        // Bare `$this->app->bind('name')`: concrete = abstract.
        None => (abstract_name.clone(), true),
        Some(Some(expr)) => match expr.kind() {
            "class_constant_access_expression" => (class_const_name(expr, bytes)?, true),
            "arrow_function" | "anonymous_function" => {
                match resolve_closure_concrete(expr, bytes, aliases, root) {
                    Some(fqcn) => (fqcn, true),
                    None => ("Closure".to_string(), false),
                }
            }
            // A variable, helper call, etc. — not statically a class. Skipped,
            // matching the former regexes, which only matched these three forms.
            _ => return None,
        },
        Some(None) => return None,
    };

    Some(ProviderBinding {
        abstract_name,
        concrete_class,
        binding_type,
        source_line,
        resolve_file,
    })
}

/// The value expression of a call argument. tree-sitter-php wraps each argument
/// in an `argument` node; for a named argument (`bind(abstract: 'k', concrete: …)`)
/// the parameter label is the `name` field, so the value is the other child —
/// `named_child(0)` alone would return the label and drop the binding.
fn argument_value(arg: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if arg.kind() != "argument" {
        return Some(arg);
    }
    let label = arg.child_by_field_name("name");
    (0..arg.named_child_count() as u32)
        .filter_map(|i| arg.named_child(i))
        .find(|&ch| Some(ch) != label)
}

/// Whether `object` is the `$this->app` receiver.
fn is_this_app_receiver(object: tree_sitter::Node, bytes: &[u8]) -> bool {
    object.kind() == "member_access_expression"
        && object
            .child_by_field_name("object")
            .and_then(|o| o.utf8_text(bytes).ok())
            == Some("$this")
        && object
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())
            == Some("app")
}

/// The content of a single/double-quoted string literal node, or `None`.
/// Descends to the `string_content` child, matching the rest of the LSP
/// (`route_chain::read_string_content`, `config_key_locator`); an empty literal
/// has no such child, so fall back to stripping a surrounding quote pair.
fn string_literal_text(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return Some(child.utf8_text(bytes).ok()?.to_string());
        }
    }
    Some(
        node.utf8_text(bytes)
            .ok()?
            .trim_start_matches(['\'', '"'])
            .trim_end_matches(['\'', '"'])
            .to_string(),
    )
}

/// The class named by a `Class::class` constant access, leading `\` trimmed.
/// tree-sitter-php parses both `Class::class` and `Class::SOME_CONST` as a
/// `class_constant_access_expression`, so require the constant child to be the
/// literal `class` — otherwise this is an ordinary constant, not a class
/// reference, and resolving its scope would point at the wrong target.
fn class_const_name(expr: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    if expr.named_child(1)?.utf8_text(bytes).ok()? != "class" {
        return None;
    }
    Some(
        expr.named_child(0)?
            .utf8_text(bytes)
            .ok()?
            .trim_start_matches('\\')
            .to_string(),
    )
}

/// A macro/mixin registration extracted from a provider's AST, ready to become
/// a `ParsedMacroReg`. `receiver_fqcn` is the resolved Macroable host FQCN,
/// `macro_name` the registered member, and `decl_file`/`decl_line` the
/// definition site (the closure for a scalar macro, the mixin method otherwise).
struct ProviderMacro {
    receiver_fqcn: String,
    macro_name: String,
    decl_file: PathBuf,
    decl_line: u32,
}

/// Walk a provider's PHP AST for `Receiver::macro('name', <closure>)` and
/// `Receiver::mixin(new Mixin)` static calls, resolving each to a definition
/// site. Mirrors [`extract_provider_bindings`]: a pre-order descent so the first
/// registration of a `(host, name)` pair in source order wins the caller's dedup.
///
/// ## Coverage boundaries (be honest about the caps)
///
/// - **Which files**: every file registered as a [`ServiceProviderFile`] Salsa
///   input — app providers (priority 3), module providers (2, from the
///   `modules.paths` globs), package providers (1), and framework providers
///   (0), the last two discovered by the vendor scan
///   (`rescan_vendor_providers`). Priority merging happens in
///   [`SalsaActor::build_macro_registry`], not here.
/// - **Which calls**: only a STATIC `Receiver::macro(...)` / `Receiver::mixin(...)`
///   (`scoped_call_expression`). The dominant registration site is a provider's
///   `boot()`, but the walk is whole-file, not `boot()`-scoped — so a macro
///   registered in any method of a registered provider is caught. An instance-form
///   `$compiler->macro(...)` is intentionally NOT matched (it's a
///   `member_call_expression`, and `macro` as an instance method is rarely a
///   Macroable registration on a resolvable host).
/// - **Known caps** (registrations we do NOT see): a macro registered OUTSIDE a
///   registered provider (e.g. directly in `routes/`, a test bootstrap, or a
///   package file that isn't a `*ServiceProvider.php`); a host token that doesn't
///   resolve to an FQCN statically; a `macro` name that isn't a plain string
///   literal (a variable / concatenation); and a `mixin` whose class can't be
///   resolved to an on-disk file. These degrade silently to "no macro" rather
///   than a wrong target.
fn extract_provider_macros(
    tree: &tree_sitter::Tree,
    text: &str,
    provider_path: &Path,
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
) -> Vec<ProviderMacro> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    walk_provider_macros(
        tree.root_node(),
        bytes,
        provider_path,
        root,
        aliases,
        &mut out,
    );
    out
}

fn walk_provider_macros(
    node: tree_sitter::Node,
    bytes: &[u8],
    provider_path: &Path,
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
    out: &mut Vec<ProviderMacro>,
) {
    if node.kind() == "scoped_call_expression" {
        classify_macro_call(node, bytes, provider_path, root, aliases, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_provider_macros(child, bytes, provider_path, root, aliases, out);
    }
}

/// Classify one `Receiver::macro('name', <closure>)` / `Receiver::mixin(<expr>)`
/// static call, pushing zero or more `ProviderMacro`s into `out`. A scalar macro
/// yields one entry (definition site = the closure); a mixin yields one entry per
/// public method of the resolved mixin class (definition site = each method).
/// Returns silently for any call that isn't such a registration or whose receiver
/// can't be resolved to a host FQCN.
fn classify_macro_call(
    node: tree_sitter::Node,
    bytes: &[u8],
    provider_path: &Path,
    root: &Path,
    aliases: &crate::query_chain::use_aliases::UseAliases,
    out: &mut Vec<ProviderMacro>,
) {
    let Some(method) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
    else {
        return;
    };
    if !matches!(method, "macro" | "mixin") {
        return;
    }
    // Resolve the receiver token to its Macroable host FQCN the same way the
    // call site does — through the file's `use` imports (and, for a facade
    // token, the alias map fed via the seed; user aliases ride at the snapshot
    // merge, not here). A token with a `\` separator is already qualified.
    let Some(scope) = node
        .child_by_field_name("scope")
        .and_then(|s| s.utf8_text(bytes).ok())
    else {
        return;
    };
    let receiver_fqcn = resolve_macro_host_fqcn(scope, aliases);

    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    let mut arg_exprs = args.named_children(&mut cursor).map(argument_value);

    if method == "macro" {
        // `macro('name', <closure>)` — the name is the first string argument and
        // the definition site is the closure (second argument).
        let Some(Some(name_node)) = arg_exprs.next() else {
            return;
        };
        let Some(macro_name) = string_literal_text(name_node, bytes) else {
            return;
        };
        let closure = arg_exprs.next().flatten();
        // The definition line: the closure if present (where the body lives),
        // otherwise the call itself. 0-based to match the rest of the stack. The
        // definition file for a scalar macro is the provider source itself.
        let decl_line = closure.unwrap_or(node).start_position().row as u32;
        out.push(ProviderMacro {
            receiver_fqcn,
            macro_name,
            decl_file: provider_path.to_path_buf(),
            decl_line,
        });
        return;
    }

    // `mixin(new MyMixin)` / `mixin(MyMixin::class)` — expand each public OR
    // protected method of the mixin class into a macro on the same host. The
    // mixin's methods ARE the registered members (Laravel reflects them onto the
    // host at runtime), so each one's definition site is its own declaration in
    // the mixin file.
    let Some(Some(arg)) = arg_exprs.next() else {
        return;
    };
    let Some(mixin_fqcn) = mixin_class_fqcn(arg, bytes, aliases) else {
        return;
    };
    let Some(mixin_file) = resolve_class_to_file_internal(&mixin_fqcn, root) else {
        return;
    };
    // Analyze the mixin to enumerate its methods with their declaration lines —
    // the same inheritance-resolved walk the model surfaces use (and the same
    // `ReflectionClass::getMethods` inheritance scope Laravel reflects over). A
    // mixin that can't be analyzed (unreadable, no class) yields nothing.
    let Some(view) = crate::laravel_introspector::chain::analyze(&mixin_file, root) else {
        return;
    };
    for m in &view.all_methods {
        let method = &m.value;
        // Laravel's `Macroable::mixin` reflects with
        // `getMethods(ReflectionMethod::IS_PUBLIC | ReflectionMethod::IS_PROTECTED)`
        // and `setAccessible(true)` on each before registering it — so PUBLIC and
        // PROTECTED methods alike become live macros; only PRIVATE methods are
        // excluded. Laravel does NOT filter by `__` name (a real mixin never
        // returns a closure from `__construct`/`__call`, so they don't surface as
        // callable macros in practice), so we mirror that and exclude purely by
        // visibility.
        if method.visibility == crate::laravel_introspector::walker::PhpVisibility::Private {
            continue;
        }
        // The method's own declaring file (a trait the mixin composes lives
        // elsewhere); fall back to the mixin file when the source class isn't
        // separately resolvable.
        let decl_file = resolve_class_to_file_internal(&m.source_class, root)
            .unwrap_or_else(|| mixin_file.clone());
        out.push(ProviderMacro {
            receiver_fqcn: receiver_fqcn.clone(),
            macro_name: method.name.clone(),
            decl_file,
            decl_line: method.start_line,
        });
    }
}

/// Resolve the mixin class FQCN from a `mixin(...)` argument — either
/// `new MyMixin` / `new MyMixin()` (an `object_creation_expression`) or
/// `MyMixin::class` (a `class_constant_access_expression`) — through the file's
/// `use` imports. Returns `None` for any other argument shape (a variable, a
/// computed expression — none of which name a mixin class statically).
fn mixin_class_fqcn(
    arg: tree_sitter::Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
) -> Option<String> {
    let class_ref = match arg.kind() {
        "class_constant_access_expression" => class_const_name(arg, bytes)?,
        "object_creation_expression" => {
            // The class name is the first named child that is a name / qualified
            // name / relative name (skipping the `new` keyword and any argument
            // list). `relative_name` covers `new namespace\MyMixin`; this matches
            // the sibling `new X` resolvers in `query_chain::extractor` and
            // `query_chain::flow`, which feed the same raw text through
            // `resolve_class_name`. Collect into a Vec so the walk cursor doesn't
            // outlive the borrow.
            let mut cursor = arg.walk();
            let children: Vec<_> = arg.named_children(&mut cursor).collect();
            let name_node = children
                .into_iter()
                .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))?;
            name_node.utf8_text(bytes).ok()?.to_string()
        }
        _ => return None,
    };
    Some(
        crate::query_chain::use_aliases::resolve_class_name(&class_ref, aliases)
            .trim_start_matches('\\')
            .to_string(),
    )
}

/// Resolve a `Receiver::macro(...)` scope token to its Macroable host FQCN.
/// Expands the file's `use` imports and strips a leading `\`; a bare token with
/// no import stays as written (the framework Macroables — `Str`, `Arr`,
/// `Request`, … — are referenced by their imported short name in practice, and
/// the call-site resolver qualifies the same way).
fn resolve_macro_host_fqcn(
    scope: &str,
    aliases: &crate::query_chain::use_aliases::UseAliases,
) -> String {
    crate::query_chain::use_aliases::resolve_class_name(scope, aliases)
        .trim_start_matches('\\')
        .to_string()
}

/// Extract `$app->withAliases([...])` facade-alias registrations from a
/// `bootstrap/app.php` source into a token → facade-FQCN map (`'Auth' =>
/// 'Illuminate\Support\Facades\Auth'`).
///
/// `withAliases` is the Laravel 11+ way to override `Facade::defaultAliases()`,
/// the modern counterpart to `config/app.php`'s `aliases` array (parsed by
/// [`crate::config::parse_facade_aliases`]). It is a `member_call_expression`
/// whose method is `withAliases` and whose single argument is an array literal
/// of `'Alias' => Class::class` pairs. We walk the AST (mirroring
/// [`extract_provider_bindings`]) so each `::class` value resolves through the
/// file's `use` imports — `withAliases([…Auth::class])` under
/// `use Illuminate\Support\Facades\Auth;` yields the full FQCN.
///
/// Only `Class::class` values are honored; a non-`::class` entry (a bare
/// string, a computed expression) is skipped — it can't name a facade class
/// statically.
pub fn extract_with_aliases(tree: &tree_sitter::Tree, text: &str) -> HashMap<String, String> {
    let bytes = text.as_bytes();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(tree, text);
    let mut out = HashMap::new();
    walk_with_aliases(tree.root_node(), bytes, &aliases, &mut out);
    out
}

/// Pre-order descent collecting `withAliases([...])` entries.
fn walk_with_aliases(
    node: tree_sitter::Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    out: &mut HashMap<String, String>,
) {
    if node.kind() == "member_call_expression"
        && node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(bytes).ok())
            == Some("withAliases")
    {
        collect_with_aliases_args(node, bytes, aliases, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_with_aliases(child, bytes, aliases, out);
    }
}

/// Pull `'Alias' => Class::class` pairs from the array-literal argument of a
/// `withAliases(...)` call, resolving each `::class` value through `aliases`.
fn collect_with_aliases_args(
    call: tree_sitter::Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    out: &mut HashMap<String, String>,
) {
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        let Some(value) = argument_value(arg) else {
            continue;
        };
        if value.kind() != "array_creation_expression" {
            continue;
        }
        let mut el_cursor = value.walk();
        for element in value.named_children(&mut el_cursor) {
            if element.kind() != "array_element_initializer" {
                continue;
            }
            let Some(key_node) = element
                .child_by_field_name("key")
                .or_else(|| element.named_child(0))
            else {
                continue;
            };
            let Some(value_node) = element
                .child_by_field_name("value")
                .or_else(|| element.named_child(1))
            else {
                continue;
            };
            let Some(alias) = string_literal_text(key_node, bytes) else {
                continue;
            };
            // Only `Class::class` values name a facade class; resolve the bare
            // class name through the file's `use` imports to its full FQCN.
            if value_node.kind() != "class_constant_access_expression" {
                continue;
            }
            let Some(class_ref) = class_const_name(value_node, bytes) else {
                continue;
            };
            let fqcn = crate::query_chain::use_aliases::resolve_class_name(&class_ref, aliases);
            out.insert(alias, fqcn.trim_start_matches('\\').to_string());
        }
    }
}

/// Resolve the concrete model a binding closure returns, or `None` to fall back
/// to `"Closure"`. The contract is to degrade cleanly — only a single, concrete,
/// on-disk class is ever returned; the hard tiers (relationship hops, multiple
/// returns, union/nullable return types) yield `None` rather than a wrong guess.
fn resolve_closure_concrete(
    closure: tree_sitter::Node,
    bytes: &[u8],
    aliases: &crate::query_chain::use_aliases::UseAliases,
    root: &Path,
) -> Option<String> {
    // An explicit, single, named return type is the most authoritative signal.
    // optional_type (`?Tenant`), union_type (`A|B`), intersection_type, and
    // primitive_type are the ambiguous tiers — never resolved from the hint;
    // they fall through to the body expression below.
    if let Some(rt) = closure.child_by_field_name("return_type") {
        if rt.kind() == "named_type" {
            if let Ok(raw) = rt.utf8_text(bytes) {
                let fqcn = crate::query_chain::use_aliases::resolve_class_name(raw, aliases);
                if resolve_class_to_file_internal(&fqcn, root).is_some() {
                    return Some(fqcn);
                }
            }
        }
    }

    // Otherwise resolve the return expression. An arrow body is the expression
    // itself; a block body must have exactly one `return <expr>;` (multiple
    // returns are the conditional hard tier — give up rather than pick one).
    let body = closure.child_by_field_name("body")?;
    let expr = if body.kind() == "compound_statement" {
        single_return_expr(body)?
    } else {
        body
    };

    let resolver = ProviderBindingResolver { root };
    let classviews = crate::member_resolver::ClassViewCache::new();
    let (fqcn, confidence) = crate::member_resolver::resolve_expression_type(
        expr,
        bytes,
        aliases,
        &resolver,
        &classviews,
        root,
    )?;
    if !matches!(confidence, Confidence::High | Confidence::Medium) {
        return None;
    }
    // `resolve_expression_type` expands `use`-aliases and absolute names but
    // leaves a bare SAME-NAMESPACE `new X` unqualified — the real Laravel shape
    // `singleton('auth', fn ($app) => new AuthManager($app))` in a provider that
    // lives in `AuthManager`'s own namespace returns the bare `AuthManager`,
    // which then fails the on-disk gate below and degrades the binding to
    // "Closure". Qualify it against the closure's FILE namespace with the SAME
    // helper the static-receiver arm uses, so the bare name becomes
    // `Illuminate\Auth\AuthManager` before the gate. (A name `resolve_class_name`
    // already qualified — imported or absolute — is left untouched.)
    let fqcn = crate::member_resolver::qualify_fqcn(fqcn, expr, bytes);
    // Only store a concrete that names a real class file — an inferred FQCN that
    // doesn't resolve to disk would be a guess, and the contract is to degrade
    // to "Closure" rather than point at a wrong or absent target.
    resolve_class_to_file_internal(&fqcn, root)
        .is_some()
        .then_some(fqcn)
}

/// The expression of a block body's sole `return <expr>;`, or `None` when there
/// are zero or multiple returns (the conditional/multiple-return hard tier).
/// Nested closures' own returns are ignored.
fn single_return_expr(block: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut found: Option<tree_sitter::Node> = None;
    let mut stack = vec![block];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_statement" {
            if found.is_some() {
                return None;
            }
            found = n.named_child(0);
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            // A nested closure's returns belong to it, not this block.
            if matches!(child.kind(), "arrow_function" | "anonymous_function") {
                continue;
            }
            stack.push(child);
        }
    }
    found
}

/// Resolve a PHP path expression to an absolute filesystem path without
/// executing PHP. Handles the path forms that appear in real service-provider
/// `anonymousComponentPath()` calls:
///
/// - Laravel path helpers: `resource_path('x')`, `base_path('x')`, `app_path('x')`,
///   `storage_path('x')`, `public_path('x')`, `config_path('x')`,
///   `database_path('x')`, `lang_path('x')` — and their no-argument forms.
/// - `__DIR__ . '/relative'` — resolved against the provider file's directory.
/// - A plain string literal — absolute as-is, otherwise joined to the project root.
///
/// Returns `None` for expressions we can't statically resolve (e.g. a variable).
fn resolve_php_path_expr(expr: &str, root: &Path, provider_dir: &Path) -> Option<PathBuf> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// `helper('sub/dir')` or `helper()` for the Laravel path helpers.
        static ref HELPER_RE: Regex = Regex::new(
            r#"^(resource_path|base_path|app_path|storage_path|public_path|config_path|database_path|lang_path)\s*\(\s*(?:['"]([^'"]*)['"]\s*)?\)$"#
        ).unwrap();
        /// `__DIR__ . '/relative'`
        static ref DIR_CONST_RE: Regex = Regex::new(
            r#"^__DIR__\s*\.\s*['"]([^'"]+)['"]$"#
        ).unwrap();
        /// A bare string literal.
        static ref LITERAL_RE: Regex = Regex::new(r#"^['"]([^'"]+)['"]$"#).unwrap();
        /// `realpath(<expr>)` — unwrapped and resolved as its inner expression.
        static ref REALPATH_RE: Regex = Regex::new(r#"^realpath\s*\(\s*(.+?)\s*\)$"#).unwrap();
    }

    let expr = expr.trim();

    // `realpath(X)` is a pure syntactic pass-through here: it resolves exactly
    // as `X` does. PHP's runtime behaviour — following symlinks, returning
    // `false` for a missing path — is out of scope for a static pass, which
    // never touches the filesystem to decide what a registration names.
    if let Some(cap) = REALPATH_RE.captures(expr) {
        return resolve_php_path_expr(cap.get(1).unwrap().as_str(), root, provider_dir);
    }

    if let Some(cap) = HELPER_RE.captures(expr) {
        let helper = cap.get(1).unwrap().as_str();
        let sub = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let base = match helper {
            "base_path" => root.to_path_buf(),
            "resource_path" => root.join("resources"),
            "app_path" => root.join("app"),
            "storage_path" => root.join("storage"),
            "public_path" => root.join("public"),
            "config_path" => root.join("config"),
            "database_path" => root.join("database"),
            "lang_path" => root.join("lang"),
            _ => return None,
        };
        let joined = if sub.is_empty() {
            base
        } else {
            join_relative(&base, sub)
        };
        return Some(normalize_path(&joined));
    }

    if let Some(cap) = DIR_CONST_RE.captures(expr) {
        let sub = cap.get(1).unwrap().as_str();
        return Some(normalize_path(&join_relative(provider_dir, sub)));
    }

    if let Some(cap) = LITERAL_RE.captures(expr) {
        let lit = cap.get(1).unwrap().as_str();
        let p = Path::new(lit);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(lit)
        };
        return Some(normalize_path(&joined));
    }

    None
}

/// Extract `Blade::anonymousComponentPath(<path>, 'prefix')` registrations from
/// service-provider source. Pure (regex + path resolution) so it can be unit
/// tested without a Salsa database. Returns `(prefix, absolute_directory,
/// source_line)` tuples; registrations whose path argument can't be statically
/// resolved are skipped.
fn extract_anonymous_component_paths(
    text: &str,
    root: &Path,
    provider_dir: &Path,
) -> Vec<(String, PathBuf, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Group 1 is the path expression (non-greedy, single line); group 2 is
        /// the string prefix. The two-argument (prefixed) form is the only one
        /// that produces `<x-prefix::component>` namespaced usage.
        static ref ANON_PATH_RE: Regex = Regex::new(
            r#"Blade::anonymousComponentPath\s*\(\s*(.+?)\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ANON_PATH_RE.captures_iter(text) {
        if let (Some(path_expr), Some(prefix)) = (cap.get(1), cap.get(2)) {
            if let Some(directory) = resolve_php_path_expr(path_expr.as_str(), root, provider_dir) {
                let line = text[..prefix.start()].lines().count() as u32;
                out.push((prefix.as_str().to_string(), directory, line));
            }
        }
    }
    out
}

/// Extract runtime view-namespace registrations made through the `View` factory:
///   View::addNamespace('ns', app_path('Ai/Prompts'))
///   View::prependNamespace('ns', resource_path('views/ns'))
///   app('view')->addNamespace('ns', base_path('packages/ns/views'))
///   $factory->addNamespace('ns', __DIR__ . '/../views')
///
/// Laravel's literal `$this->loadViewsFrom(__DIR__.'…', 'ns')` is matched
/// elsewhere (`LOAD_VIEWS_RE`); this covers the imperative facade/factory form,
/// where the directory argument is commonly a Laravel path helper rather than a
/// `__DIR__` concatenation. The path expression is delegated to
/// `resolve_php_path_expr`, so `app_path()`, `base_path()`, `resource_path()`,
/// the other path helpers, `__DIR__ . '…'`, and bare string literals all
/// resolve. Registrations whose path argument can't be statically resolved
/// (e.g. a variable) are skipped. Returns `(namespace, absolute_directory,
/// source_line)` tuples.
fn extract_add_namespace_view_registrations(
    text: &str,
    root: &Path,
    provider_dir: &Path,
) -> Vec<(String, PathBuf, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        /// Receiver is the `View` facade, an `app('view')` resolve, or any
        /// `$factory->` instance; method is `addNamespace` or `prependNamespace`
        /// (both register a hint path — `prepend` only changes precedence).
        /// Group 1 is the namespace string; group 2 is the path expression,
        /// allowing one level of nested parentheses so helper calls like
        /// `app_path('Ai/Prompts')` are captured whole rather than truncated at
        /// the inner `)`.
        static ref ADD_NAMESPACE_RE: Regex = Regex::new(
            r#"(?:View::|app\(\s*['"]view['"]\s*\)->|\$\w+->)(?:add|prepend)Namespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*((?:[^()]|\([^()]*\))+?)\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ADD_NAMESPACE_RE.captures_iter(text) {
        if let (Some(namespace), Some(path_expr)) = (cap.get(1), cap.get(2)) {
            if let Some(directory) = resolve_php_path_expr(path_expr.as_str(), root, provider_dir) {
                let line = text[..namespace.start()].lines().count() as u32;
                out.push((namespace.as_str().to_string(), directory, line));
            }
        }
    }
    out
}

/// Extract `Blade::anonymousComponentNamespace('dir', 'prefix')` registrations.
/// The directory is relative to the registered view paths; dots are normalized
/// to slashes (Laravel resolves it through the dot-notation view finder).
/// Returns `(prefix, view_relative_directory, source_line)` tuples.
fn extract_anonymous_component_namespaces(text: &str) -> Vec<(String, String, u32)> {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        static ref ANON_NS_RE: Regex = Regex::new(
            r#"Blade::anonymousComponentNamespace\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#
        ).unwrap();
    }

    let mut out = Vec::new();
    for cap in ANON_NS_RE.captures_iter(text) {
        if let (Some(directory), Some(prefix)) = (cap.get(1), cap.get(2)) {
            let line = text[..prefix.start()].lines().count() as u32;
            let normalized = directory.as_str().replace('.', "/");
            out.push((prefix.as_str().to_string(), normalized, line));
        }
    }
    out
}

/// The namespace a fluent package-builder derives from a package name when
/// `->hasViews()` is called without an explicit argument: everything after a
/// leading `laravel-`. Mirrors the builder's own `shortName()`
/// (`Str::after($name, 'laravel-')`) so discovery matches runtime resolution.
///
/// `filament` → `filament`; `laravel-foo` → `foo`; `my-laravel-bar` → `bar`.
pub(crate) fn builder_short_name(package_name: &str) -> String {
    package_name
        .split_once("laravel-")
        .map(|(_, after)| after.to_string())
        .unwrap_or_else(|| package_name.to_string())
}

/// Expand a bare class name from a registration argument to its FQN using the
/// source file's `use` statements, the same way PHP resolves the reference.
/// `DynamicComponent` + `use Illuminate\View\DynamicComponent;` →
/// `Illuminate\View\DynamicComponent`. Aliased imports (`use Foo\Bar as Baz;`)
/// match on the alias. Names already carrying a `\` are returned unchanged;
/// so is a name with no matching import (group-use bodies are not expanded —
/// resolution then simply fails downstream, same as before).
fn expand_class_via_use_statements(class_name: &str, source: &str) -> String {
    if class_name.contains('\\') {
        return class_name.to_string();
    }

    for line in source.lines() {
        let trimmed = line.trim();
        let Some(import) = trimmed.strip_prefix("use ") else {
            continue;
        };
        // `use function`/`use const` imports and trait-`use` inside class
        // bodies (no namespace separator, e.g. `use HasFactory;`) are not
        // class imports we can expand from.
        if import.starts_with("function ") || import.starts_with("const ") {
            continue;
        }
        let Some(import) = import.strip_suffix(';') else {
            continue;
        };

        let (fqn, visible_name) = match import.split_once(" as ") {
            Some((fqn, alias)) => (fqn.trim(), alias.trim()),
            None => {
                let fqn = import.trim();
                let basename = fqn.rsplit('\\').next().unwrap_or(fqn);
                (fqn, basename)
            }
        };

        if visible_name == class_name && fqn.contains('\\') {
            return fqn.trim_start_matches('\\').to_string();
        }
    }

    class_name.to_string()
}

/// Resolve a class name to a file path using PSR-4 conventions
fn resolve_class_to_file_internal(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    // PSR-4 via the composer autoload map first — the authoritative answer
    // for any installed package (vendor or app). The legacy prefix mappings
    // below stay as fallbacks for projects without a readable autoload map.
    if let Some((namespace, class)) = class_name.rsplit_once('\\') {
        let autoload = crate::composer_autoload::ComposerAutoload::for_project(root_path);
        for dir in autoload.resolve_namespace_dirs(namespace) {
            let file = dir.join(class).with_extension("php");
            if file.exists() {
                return Some(file);
            }
        }
    }

    // Common namespace to directory mappings
    let mappings = [
        ("App\\", "app/"),
        ("Illuminate\\", "vendor/laravel/framework/src/Illuminate/"),
        ("Laravel\\", "vendor/laravel/"),
    ];

    for (namespace, dir) in &mappings {
        if class_name.starts_with(namespace) {
            let relative = class_name.strip_prefix(namespace)?;
            let file_path = root_path
                .join(dir)
                .join(relative.replace('\\', "/"))
                .with_extension("php");
            if file_path.exists() {
                return Some(file_path);
            }
        }
    }

    // Try direct class name as path
    let direct_path = root_path
        .join(class_name.replace('\\', "/"))
        .with_extension("php");
    if direct_path.exists() {
        return Some(direct_path);
    }

    None
}

// ============================================================================
// Helper Functions
// ============================================================================

impl LaravelDatabase {
    /// Create a new database instance
    pub fn new() -> Self {
        Self::default()
    }
}

// ============================================================================
// Data Transfer Types - Plain structs for sending data across threads
// ============================================================================

/// View reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViewReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
    /// `$view = '…'` class-property site — feeds goto/hover and the render
    /// index, skipped by the missing-view diagnostic (a string property
    /// named `view` on a non-Filament class is not a Blade reference).
    #[serde(default)]
    pub is_property_site: bool,
}

/// Inertia page reference data for transfer across async boundaries (issue
/// #10). `name` is the page name (`/`-nested, no extension); the span covers
/// the page-name string inside the quotes. Drives goto-definition, the
/// missing-page diagnostic, hover, and the create-page code action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InertiaReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Component reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComponentReferenceData {
    pub name: String,
    pub tag_name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Directive reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectiveReferenceData {
    pub name: String,
    pub arguments: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    /// Column of first character INSIDE the quoted string (after opening quote)
    pub string_column: u32,
    /// Column one past the last character INSIDE the quoted string (before closing quote)
    pub string_end_column: u32,
}

/// Env reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnvReferenceData {
    pub name: String,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Config reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A class FQCN referenced from a Blade `@use` directive. Positions span the
/// name's characters INSIDE the quotes, so navigation and highlight land on the
/// class rather than on the whole directive.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Livewire reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LivewireReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Middleware reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MiddlewareReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Translation reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranslationReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssetReferenceData {
    pub path: String,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Binding reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BindingReferenceData {
    pub name: String,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Route reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RouteReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// URL reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UrlReferenceData {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Curated-helper-identifier reference data for transfer across async
/// boundaries. `name` is one of the seven curated global helpers (`route`,
/// `view`, `config`, `auth`, `app`, `session`, `cache`); the position spans the
/// function-NAME token (`route` in `route('home')`), not its string argument.
/// Drives the Laravel-aware helper hover card (#58).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HelperReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Action reference data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionReferenceData {
    pub action: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Feature reference data for transfer across async boundaries (Laravel Pennant)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeatureReferenceData {
    /// The feature name (string key like 'new-api' or class name like 'NewApi')
    pub feature_name: String,
    /// The method being called (active, inactive, value, when, etc.)
    pub method_name: String,
    /// Whether this is a class-based feature (Feature::active(NewApi::class))
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Confidence that a captured member-access site's receiver was resolved to a
/// concrete declaring class.
///
/// Populated by M3's receiver resolution; at capture time (M2) every site is
/// [`Confidence::Unresolved`]. The tiers mirror the plan's resolution tiers:
/// HIGH (static call, `(new X)`, typed param, `@var`, simple local assignment),
/// MEDIUM (multi-hop reassignment / indirect flow), LOW (foreach iter var,
/// typed property, return chain — captured but not yet resolvable; widened in
/// later work, never guessed).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Confidence {
    High,
    Medium,
    Low,
    /// Not yet run through the resolver — the state at capture time (M2).
    #[default]
    Unresolved,
}

/// The kind of member a resolved access maps to.
///
/// `None` on the reference until M3 classifies the site against the
/// class-hierarchy index. The Eloquent-magic variants are what make
/// find-references / rename / hover magic-aware; `PlainMember` is a generic
/// (non-magic) property on a resolved class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MagicMemberKind {
    /// Eloquent local scope accessed via `__call` (`scopeActive` → `->active()`).
    Scope,
    /// Eloquent accessor / attribute (`getFullNameAttribute` / `Attribute`).
    Accessor,
    /// Eloquent relationship method accessed as a property (`$user->posts`).
    Relationship,
    /// Database column surfaced as a model attribute (`$user->email`).
    Column,
    /// Dynamic finder (`User::whereEmail(...)` → `where('email', ...)`).
    DynamicFinder,
    /// Runtime-registered macro / mixin method (`Str::macro('foo', fn …)`,
    /// `Str::mixin(new MyMixin)`). Resolved via the project-wide macro registry
    /// rather than the receiver class's own surfaces — its definition site is the
    /// registered closure (or the mixin method), carried in the registry entry.
    Macro,
    /// A method reached through a FACADE proxy (`Auth::check()`,
    /// `Cache::get()`). The facade's own class carries only `@method` docblocks,
    /// so resolution walked it to the bound concrete (facade FQCN → accessor →
    /// container binding); `declaring_fqcn` is that concrete. The goto/hover
    /// target is the member's declaration on the concrete when it declares one
    /// (`AuthManager::check`), DEGRADING to the concrete CLASS when the concrete
    /// only forwards the call via `__call`/a guard (`Auth::guard()` is
    /// `@method`-documented, not declared). Distinct from `PlainMember` —
    /// which the consumers drop as Intelephense's territory — because a facade
    /// call is precisely what Intelephense CAN'T see through, so we own it.
    FacadeMethod,
    /// A model's `factory()` call (`User::factory()`). The method itself is
    /// `HasFactory::factory()` — vendor trait magic no PHP LSP resolves to the
    /// project's factory class without ide-helper. `declaring_fqcn` is the
    /// resolved factory FQCN (`newFactory()` override or Laravel convention);
    /// the goto/hover target is that factory class's declaration line.
    Factory,
    /// A method called on a factory-rooted chain (`User::factory()->state(…)`,
    /// a custom state like `->suspended()`). `declaring_fqcn` is the class that
    /// actually declares the method (the project factory, or the vendor
    /// `Factories\Factory` base). Distinct from `PlainMember` — which consumers
    /// drop as Intelephense's — because the chain's factory subject is exactly
    /// what Intelephense can't type without ide-helper.
    FactoryMethod,
    /// A many-to-many `->pivot` attribute on a model that declares a custom
    /// pivot class (`protected $pivotClass = MembershipPivot::class;`).
    /// `declaring_fqcn` is that pivot FQCN; the target is its class line.
    Pivot,
    /// A query/Eloquent builder method reached via `__call`/`forwardCallTo`
    /// (`Model::orderByDesc(...)`, `->where(...)`) that isn't declared on the
    /// model or its class hierarchy at all. Unlike `PlainMember`, this is
    /// deliberately NOT dropped as Intelephense's territory: without a
    /// generated `_ide_helper.php`, Intelephense has no way to see the
    /// `Model`/`Eloquent\Builder` → `Query\Builder` forwarding either, so
    /// hovering these is a real gap this LSP can close on its own —
    /// `declaring_fqcn` names the REAL vendor class the method lives on
    /// (`Illuminate\Database\Query\Builder`, usually), sourced straight from
    /// vendor code via [`crate::laravel_introspector::BuilderMethodIndex`],
    /// no ide-helper stubs required.
    BuilderMethod,
    /// Generic (non-magic) property on a resolved class.
    PlainMember,
}

/// How a member was syntactically accessed. Drives which magic kinds are even
/// possible: a scope is only reachable via a call, an accessor only via a
/// property read. Lives here (not `member_resolver`, which re-exports it)
/// because it travels inside [`MemberAccessReferenceData`] through the
/// per-file pattern cache. NOTE: that cache is bincode (non-self-describing),
/// so `serde(default)` does NOT make old caches decodable — the
/// `pattern_disk_cache` SCHEMA_VERSION bump is what protects against stale
/// shapes. Any future field change here needs another bump.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum AccessForm {
    /// `$user->email` — property read (no call parens).
    #[default]
    Property,
    /// `User::active()` — static call (`::`).
    StaticCall,
    /// `$user->active()` / `$user->posts()` — instance method call (`->m()`).
    InstanceCall,
}

impl AccessForm {
    /// Call-form (`::m()` or `->m()`) vs property read.
    pub fn is_call(self) -> bool {
        matches!(self, AccessForm::StaticCall | AccessForm::InstanceCall)
    }
}

/// A member access (`$user->email`, `$user->active()`, `User::whereEmail()`)
/// captured for the magic-member semantic index.
///
/// **Capture-only at M2.** The `member`, `receiver`, byte ranges, nullsafe
/// flag, and position fields are populated now. The resolution fields
/// (`declaring_fqcn`, `kind`, `confidence`) are a reserved scaffold M3 fills
/// once receiver resolution + `ClassView` classification land — until then
/// `declaring_fqcn`/`kind` are `None` and `confidence` is
/// [`Confidence::Unresolved`]. Wiring the index here once keeps M3 a pure
/// "fill in resolution" diff with no structural churn.
/// A Blade `@foreach`/`@forelse` loop's item variable + iterable, captured for
/// magic-member loop-variable typing. `{{ $user->email }}` inside
/// `@foreach($users as $user)` types `$user` from `$users`' element type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BladeLoopVar {
    /// Loop value variable, without `$` (`user` from `… as $user`).
    pub item_var: String,
    /// Iterable expression, as written (`$users`, `$this->users`, `User::all()`).
    pub iterable: String,
    /// 0-based line of the `@foreach`/`@forelse` directive.
    pub start_line: u32,
    /// 0-based line of the matching `@endforeach`/`@endforelse`; `u32::MAX` if
    /// the loop is unclosed (treat as extending to end of file).
    pub end_line: u32,
}

/// Extract the `@foreach`/`@forelse` loops worth capturing for loop-variable
/// typing: those with an iterable and a value variable. The value variable is
/// the last of the parsed loop variables (`$key => $value` keeps `value`).
pub fn blade_loop_vars(content: &str) -> Vec<BladeLoopVar> {
    use crate::blade_loops::{find_loop_blocks, BladeLoopType};
    find_loop_blocks(content)
        .into_iter()
        .filter(|b| matches!(b.loop_type, BladeLoopType::Foreach | BladeLoopType::Forelse))
        .filter_map(|b| {
            let iterable = b.iterable?;
            let item_var = b.variables.last()?.0.clone();
            Some(BladeLoopVar {
                item_var,
                iterable,
                start_line: b.start_line as u32,
                end_line: b.end_line.map(|e| e as u32).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// Member accesses written inside `@foreach`/`@forelse` *iterable* expressions
/// (`@foreach($this->entities as $e)` → a read of `$this->entities`). These
/// live in directive arguments, not `{{ }}` echoes or PHP blocks, so the normal
/// capture misses them — yet they're real references a find-references should
/// surface. We synthesize a `MemberAccessReferenceData` for the last `->member`
/// of each member-access iterable, positioned at the member name in the
/// directive line.
pub fn blade_loop_iterable_accesses(content: &str) -> Vec<MemberAccessReferenceData> {
    let mut out = Vec::new();
    for loop_var in blade_loop_vars(content) {
        let iter = loop_var.iterable.trim();
        // Only member-access iterables (`$x->y`, `$this->y`); a bare `$users`
        // collection has no member to reference.
        let Some(arrow) = iter.rfind("->") else {
            continue;
        };
        let member = &iter[arrow + 2..];
        let receiver = &iter[..arrow];
        if member.is_empty() || !member.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        // Locate the iterable on its directive line to position the member.
        let Some(line_text) = content.lines().nth(loop_var.start_line as usize) else {
            continue;
        };
        let Some(iter_col) = line_text.find(iter) else {
            continue;
        };
        let member_col = (iter_col + arrow + 2) as u32;
        out.push(MemberAccessReferenceData {
            member: member.to_string(),
            receiver: receiver.to_string(),
            receiver_byte_start: 0,
            receiver_byte_end: 0,
            is_nullsafe: false,
            form: AccessForm::Property,
            line: loop_var.start_line,
            column: member_col,
            end_column: member_col + member.len() as u32,
            declaring_fqcn: None,
            kind: None,
            confidence: Confidence::Unresolved,
        });
    }
    out
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemberAccessReferenceData {
    /// The accessed member name (`email`, `posts`, `profile`).
    pub member: String,
    /// Raw source text of the receiver expression (`$user`, `$this`).
    pub receiver: String,
    /// Byte range of the receiver expression in the file — lets the M3
    /// resolver locate the receiver node in the live tree for
    /// `var_type::resolve`.
    pub receiver_byte_start: usize,
    pub receiver_byte_end: usize,
    /// Whether the access used the nullsafe operator (`?->`).
    pub is_nullsafe: bool,
    /// How the member was accessed. Call-form sites can only classify as
    /// scopes / dynamic finders / relationships; property-form as accessors /
    /// relationships / columns. (`serde(default)` helps only self-describing
    /// formats — the bincode pattern cache is guarded by its SCHEMA_VERSION
    /// bump, not by this default.)
    #[serde(default)]
    pub form: AccessForm,
    /// Position of the member name (0-based — repo convention).
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    // ─── Reserved resolution scaffold (filled by M3) ───
    /// Declaring class FQCN once the receiver resolves (inheritance/trait
    /// resolved). `None` until M3.
    #[serde(default)]
    pub declaring_fqcn: Option<String>,
    /// What kind of member this resolves to. `None` until M3 classifies.
    #[serde(default)]
    pub kind: Option<MagicMemberKind>,
    /// Resolution confidence. [`Confidence::Unresolved`] until M3.
    #[serde(default)]
    pub confidence: Confidence,
}

// ─── M1 single-parse capture: per-file resolution context ──────────────────
//
// Captured at PARSE time (tree in hand) so the whole-project magic-member
// resolve passes never re-read or re-parse a target file. Each site's `recipe`
// encodes the INTRA-file half of receiver resolution as a small owned value;
// the cross-file half (class→file lookup, `ClassView` analysis, facade
// accessor, container binding registry, auth model) is completed at resolve
// against the actor snapshots + memos. The engine that consumes this lives in
// `member_resolver` (PHP sites) and `view_var_index` (view renders + Volt); it
// mirrors the tree engine's control flow branch-for-branch so the resolved
// entries + deps stay byte-identical to the re-parse path.
//
// Grows `ParsedPatternsData`, so `pattern_disk_cache::SCHEMA_VERSION` is bumped
// (10 → 11): the bincode envelope is non-self-describing, so stale slim entries
// must re-parse rather than mis-decode.

/// A receiver expression's compiled resolution recipe — the intra-file half of
/// [`member_resolver`]'s `resolve_receiver`. Cross-file completion happens at
/// eval; each variant names exactly what stays deferred.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReceiverRecipeData {
    /// Fully resolved intra-file: flow-tracked variable, `$this`, typed
    /// `$this->prop`, `foreach` element, or `self`/`static`. The final answer —
    /// no cross-file work remains.
    Resolved {
        fqcn: String,
        confidence: Confidence,
    },
    /// `auth()->user()` / `Auth::user()` / `request()->user()`. Resolves to the
    /// configured auth model at eval; `fallback` is what the tree resolver would
    /// try next when no auth model is configured (auth is checked BEFORE the
    /// shape match, so its miss falls through to the rest of `resolve_receiver`).
    AuthUser { fallback: Box<ReceiverRecipeData> },
    /// A Gate-ability closure's first parameter — resolves to the auth model at
    /// eval, or `None` (terminal: the variable arm tries nothing else).
    GateClosureUser,
    /// `app('key')` / `resolve('key')` — the container binding key. Concrete
    /// looked up in the binding registry at eval.
    ContainerKey(String),
    /// A zero-arg Laravel helper (`view()`, `cache()`, …) — the helper name.
    /// Concrete looked up via the helper→binding map at eval.
    HelperBinding(String),
    /// A static class-name receiver (`User::…`, `Auth::…`). `qualified` is the
    /// namespace-resolved FQCN (computed at parse from the file aliases +
    /// namespace); at eval the facade interception runs first (needs the facade
    /// alias snapshot), then the class-index / macro-host gate.
    StaticName {
        raw: String,
        qualified: String,
        is_namespaced: bool,
    },
    /// `$obj->method()` — resolve `object`, then the method's declared return
    /// type (read from the DECLARING class's file at eval — legitimately
    /// cross-file).
    MethodReturn {
        object: Box<ReceiverRecipeData>,
        method: String,
    },
    /// Nothing resolvable intra-file (the tree resolver returned `None`).
    Unresolvable,
}

/// Per-site captured context, positionally parallel to
/// `ParsedPatternsData.member_access_refs`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteContextData {
    /// The receiver's compiled recipe.
    pub recipe: ReceiverRecipeData,
    /// The call-form builder-chain fallback recipe, present only when the direct
    /// recipe might fail and the receiver roots in a resolvable chain
    /// (`User::query()->active()`, `$user->posts()->active()`).
    pub chain: Option<ChainRecipeData>,
    /// The lexically enclosing class FQCN — per SITE (a file may hold several
    /// classes, or an anonymous Volt class). Feeds the builder→enclosing-model
    /// retry.
    pub enclosing_class_fqcn: Option<String>,
    /// Whether this receiver roots in the enclosing `scope*` method's parameter
    /// — the gate for the builder retry.
    pub is_scope_param_receiver: bool,
}

/// A call-form receiver's builder-chain fallback — the compiled form of
/// `resolve_call_chain_receiver`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChainRecipeData {
    pub root: ChainRootData,
    /// Member names walked from the receiver toward the root — checked against
    /// the resolved class's relationships at eval for the relation-hop bail.
    pub links: Vec<String>,
}

/// The root of a builder chain: an explicit static scope, or a nested receiver
/// recipe for a variable/other root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChainRootData {
    StaticScope {
        qualified: String,
        confidence: Confidence,
        first_method: String,
    },
    Var(Box<ReceiverRecipeData>),
}

/// A `view()`-data value expression's compiled typing — either resolved
/// intra-file (flow classifier hit) or a recipe to finish cross-file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ValueExprPlanData {
    /// The flow expression classifier resolved it (pure intra-file) — final.
    Resolved {
        fqcn: String,
        confidence: Confidence,
    },
    /// Flow missed; the receiver resolver finishes it at eval.
    Recipe(ReceiverRecipeData),
}

/// One `view('name', […])` render site's compiled plan (pass 1). `items` are in
/// traversal order so the resolve replay reproduces last-wins map semantics.
///
/// A Filament-style `protected string $view = '…';` class property is also a
/// render site (see `view_var_index::declared_view_literal`), but its vars come
/// from the class's declared surface (typed props / `#[Computed]` / `mount()`)
/// rather than a `view()` call's data argument — `surface` carries that plan
/// instead, with `items` left empty. `#[serde(default)]` so a pattern-cache
/// entry from before this field existed deserializes with `None` rather than
/// failing — harmless since the schema bump below already forces those entries
/// to re-parse regardless.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewRenderPlanData {
    pub view_name: String,
    pub items: Vec<(String, ValueExprPlanData)>,
    #[serde(default)]
    pub surface: Option<VoltSurfaceData>,
}

/// A Volt front-matter property plan, replayed in declaration order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VoltPropPlanData {
    /// Typed public property — authoritative, overwrites (`insert`).
    TypedProp { name: String, fqcn: String },
    /// A single direct `or_insert` value (a `mount()` `$this->prop = $param`
    /// assignment, or a `$x = computed(...)` binding). Writes straight into the
    /// surface with `or_insert` — first write per key wins, both within and
    /// across these items (the tree engine's `out.entry(k).or_insert(...)`).
    OrInsert {
        name: String,
        plan: ValueExprPlanData,
    },
    /// ONE `with()`/`state()`/`render()` HANDLER's ordered value items. The tree
    /// engine resolves each into a temp map with `insert` — so WITHIN the
    /// handler it is last-RESOLVING-wins (an unresolvable later value does NOT
    /// overwrite an earlier resolved one) — then folds the temp into the surface
    /// with `or_insert` (first-HANDLER-wins). Because resolvability is cross-file
    /// (eval-time only), the items are kept in ORDER here and gated at eval; they
    /// are NOT pre-deduped at capture.
    OrInsertGroup(Vec<(String, ValueExprPlanData)>),
    /// `#[Computed]` method — prefer the body-inferred type, else the declared
    /// return type; `or_insert`.
    Computed {
        name: String,
        body: Option<ValueExprPlanData>,
        declared: Option<String>,
    },
}

/// The Volt component's compiled front-matter surface (item 8).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VoltSurfaceData {
    pub items: Vec<VoltPropPlanData>,
    /// The front-matter block's own `use` aliases — the surface's value recipes
    /// resolve facades against THESE (not the file-level, empty-for-Blade map).
    pub aliases: HashMap<String, String>,
}

/// A Livewire/Volt component's identity + declared member names, when both are
/// capturable intra-file. `members` is `None` for a multi-file-component Blade
/// TEMPLATE, whose class lives in a sibling `.php` (read at eval — legitimately
/// cross-file).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComponentContextData {
    pub key: String,
    pub members: Option<Vec<String>>,
}

/// Everything the resolve passes need from a file's own source, captured once at
/// parse. `None` on `ParsedPatternsData` for files with nothing to resolve
/// (icon Blade templates, vendor files) — a single tag byte, honoring the
/// zero-added-cost budget for pattern-free files.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MemberContextData {
    /// File `use`-alias map (alias → FQCN), persisted once instead of the two
    /// per-file re-derivations the resolve + hierarchy passes did. Empty for
    /// Blade files (their chain receivers compile from `use`-less snippets).
    /// The only eval-time consumer is facade resolution on a `StaticName`
    /// recipe; namespace-qualification is baked into each recipe at compile (so
    /// the file's `namespace …;` needs no separate field — it survives as each
    /// `StaticName`'s `qualified` + `is_namespaced`).
    pub aliases: HashMap<String, String>,
    /// One entry per `member_access_refs` element, in the SAME order — the
    /// positionally-parallel invariant the resolve pass relies on.
    pub sites: Vec<SiteContextData>,
    /// `view('name', […])` render structure per controller (pass 1).
    pub view_renders: Vec<ViewRenderPlanData>,
    /// Volt front-matter surface (item 8).
    pub volt_surface: Option<VoltSurfaceData>,
    /// Livewire/Volt component identity + member names (item 8).
    pub component: Option<ComponentContextData>,
}

/// Hover payload for a resolved magic member (M6). Crosses the Salsa async
/// boundary, so it owns plain data (no lifetimes / borrows). `decl_file` /
/// `decl_line` locate the declaration for a source link — `None` when the
/// declaring class isn't in the hierarchy index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MagicMemberHoverData {
    pub declaring_fqcn: String,
    pub member: String,
    pub kind: MagicMemberKind,
    pub confidence: Confidence,
    pub decl_file: Option<PathBuf>,
    /// 0-based start line of the declaration (method or property), for the link.
    pub decl_line: Option<u32>,
    /// 0-based end line — present only for a *method* declaration, so the async
    /// hover builder knows to read the declaring file and extract a snippet.
    pub decl_end_line: Option<u32>,
    /// True when the resolver couldn't classify the member but the receiver
    /// resolved to a model — a likely *plain DB column* (not `$casts`-declared,
    /// so invisible to the source-only `ClassView`). The main side must confirm
    /// it against migrations/DB before rendering, and skip the card otherwise.
    pub tentative: bool,
    /// For `MagicMemberKind::BuilderMethod` only: the real vendor signature
    /// (e.g. `public function orderByDesc($column, $order = 'desc')`), pulled
    /// straight from `BuilderMethodIndex` — no `decl_file`/`decl_line` snippet
    /// extraction applies here, the text is already extracted. `None` for
    /// every other kind.
    pub builder_signature: Option<String>,
    /// For `MagicMemberKind::BuilderMethod` only: the method's PHPDoc summary
    /// line, if vendor source has one. `None` for every other kind.
    pub builder_summary: Option<String>,
}

/// Resolution result for renaming a magic member (M7). Crosses the async
/// boundary (owns plain data). Only method-backed kinds — relationship, scope,
/// accessor, dynamic finder — produce this; columns/plain members return `None`
/// (a DB column rename is a migration concern, out of scope).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MagicMemberRenameData {
    pub fqcn: String,
    /// Usage name (the find-references key + the call-site rewrite text).
    pub member: String,
    pub kind: MagicMemberKind,
    /// The actual declared method name (`scopeActive`, `getFullNameAttribute`,
    /// `posts`) — the decl site to rewrite, transformed by the caller.
    pub method_name: String,
    pub decl_file: PathBuf,
}

/// Laravel configuration data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaravelConfigData {
    pub root: PathBuf,
    pub view_paths: Vec<PathBuf>,
    pub component_paths: Vec<(String, PathBuf)>,
    pub livewire_path: Option<PathBuf>,
    pub has_livewire: bool,
    /// Package view namespaces from loadViewsFrom() calls
    /// Maps namespace (e.g., "courier") to view path
    pub view_namespaces: HashMap<String, PathBuf>,
    /// Package component namespaces from Blade::componentNamespace() calls
    /// Maps prefix (e.g., "nightshade") to PHP namespace
    pub component_namespaces: HashMap<String, String>,
    /// Anonymous component paths from Blade::anonymousComponentPath($path, 'prefix').
    /// Maps prefix (e.g., "backstage") to the **absolute** directory holding the
    /// anonymous components. Resolution is `{dir}/{component}.blade.php` — no
    /// `components/` segment is appended, because Laravel registers the directory
    /// itself (unlike the package-publish `resources/views/vendor/<ns>/` convention).
    pub anonymous_component_paths: HashMap<String, PathBuf>,
    /// Anonymous component namespaces from Blade::anonymousComponentNamespace($dir, 'prefix').
    /// Maps prefix (e.g., "flux") to a directory **relative to the view paths**
    /// (dots normalized to slashes). Resolution is
    /// `{view_path}/{dir}/{component}.blade.php`.
    pub anonymous_component_namespaces: HashMap<String, String>,
    /// Component aliases registered via Blade::component($view, $alias) or via
    /// config-based registration loops. Maps alias (e.g., "light-button") to
    /// the target view path in dot notation (e.g., "components.buttons.light-button").
    /// Consulted before falling back to the directory-convention lookup.
    pub component_aliases: HashMap<String, String>,
    /// Icon-set component aliases registered via blade-icons' Factory pattern.
    /// Maps the full tag name (e.g., "heroicon-o-clock") to the absolute SVG
    /// file path. Built by walking vendor packages with `resources/svg/` +
    /// `config/blade-*.php` shape and combining the prefix with each SVG file.
    pub icon_aliases: HashMap<String, String>,
    /// Class-backed component registrations from
    /// `Blade::component('tag', Class::class)` (facade or instance form,
    /// either argument order). Maps the `<x-{tag}>` tag to the registered
    /// class's resolved file. Laravel core registers `dynamic-component` →
    /// `Illuminate\View\DynamicComponent` this way. `serde(default)` keeps
    /// disk-cached configs written before this field deserializable.
    #[serde(default)]
    pub class_component_files: HashMap<String, PathBuf>,
}

impl LaravelConfigData {
    /// Resolve a view name to possible file paths
    ///
    /// Returns all possible paths where this view could exist,
    /// in order of priority.
    pub fn resolve_view_path(&self, view_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Handle package views (e.g., "package::view.name")
        let (namespace, actual_view) = if let Some(pos) = view_name.find("::") {
            let namespace = &view_name[..pos];
            let view = &view_name[pos + 2..];
            (Some(namespace), view)
        } else {
            (None, view_name)
        };

        // Convert dots to path separators
        let view_path = actual_view.replace('.', "/");

        // If there's a namespace, resolve using package view paths
        if let Some(ns) = namespace {
            if let Some(package_view_path) = self.view_namespaces.get(ns) {
                // Package views - use the registered path
                let mut full_path = package_view_path.join(&view_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }
            // Also check vendor published views: resources/views/vendor/{namespace}/
            let mut vendor_path = self
                .root
                .join("resources/views/vendor")
                .join(ns)
                .join(&view_path);
            vendor_path.set_extension("blade.php");
            paths.push(vendor_path);
        } else {
            // Regular views - check each configured view path
            for base_path in &self.view_paths {
                let mut full_path = self.root.join(base_path).join(&view_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }
        }

        paths
    }

    /// Resolve a component name to file path.
    ///
    /// Defense-in-depth (#109): the component name comes straight from a Blade
    /// tag, and the dot→slash substitution below turns a leading-dot, empty, or
    /// slash-bearing name into an absolute, empty, or `..`-traversing
    /// filesystem path — `PathBuf::join` discards the receiver when the
    /// argument is absolute, so `<flux:.etc.passwd>` would otherwise yield
    /// `/etc/passwd` and `<flux:foo/../../../etc/passwd>` would traverse out.
    /// We reject such names up front and, as a backstop, drop any built
    /// candidate that escapes `self.root` (after lexically resolving `..`)
    /// before returning.
    pub fn resolve_component_path(&self, component_name: &str) -> Vec<PathBuf> {
        // Flux's `<flux:button>` sugar maps to the `flux` anonymous-component
        // namespace; rewrite the single-colon prefix to the `::` form so the
        // namespace resolution treats it like `<x-flux::button>`.
        let flux_normalized = normalize_flux_tag_name(component_name);
        let component_name = flux_normalized.as_deref().unwrap_or(component_name);

        // Reject names that would escape the project root before building any
        // path. The "actual" component is the part after a `::` namespace
        // prefix; its dots become slashes downstream, so bail out with no
        // candidates when it is:
        //   - empty             → `<flux:>` / bare `flux::` nonsense
        //   - starts with a dot → leading-dot names and `../` traversals; also
        //     the `flux:.etc.passwd` → `/etc/passwd` absolute attack shape
        //   - contains a `/`    → a literal slash never appears in a real
        //     Blade/Flux name (nesting uses dots, namespaces use `::`). It is
        //     how an attacker smuggles a mid-path `..` traversal
        //     (`flux::foo/../../../etc/passwd`) or an absolute `/etc/passwd`
        //     past the dot→slash mapping, so rejecting the whole slash-bearing
        //     class closes it at the source (Holmes, PR #150 review). This
        //     subsumes the old `replace('.', "/").starts_with('/')` check.
        let actual_component = match component_name.find("::") {
            Some(pos) => &component_name[pos + 2..],
            None => component_name,
        };
        if actual_component.is_empty()
            || actual_component.starts_with('.')
            || actual_component.contains('/')
        {
            return Vec::new();
        }

        // Build the raw candidates, then enforce a project-root containment
        // backstop: any path that escapes `self.root` is dropped before return.
        // This uses the *lexical* entry point of the shared containment guard
        // (`path_containment::path_within_root_lexical`), which rejects an
        // out-of-root candidate lexically *without canonicalizing the candidate* —
        // so an out-of-root candidate is never `stat`-probed on disk here, the
        // existence oracle issue #145 closes. A lexically-in-root candidate is
        // then canonicalized to reject symlink escapes; a speculative candidate
        // that doesn't exist on disk yet can't be canonicalized, so it falls back
        // to the proven lexical result (collapsing any interior `..`/`.` before
        // the prefix test) rather than fail-closing — admitting the
        // not-yet-created candidate while still refusing a `root/sub/../../escape`.
        // The fail-closed `path_within_root` would wrongly drop every speculative
        // candidate here.
        let mut paths = self.component_path_candidates(component_name);
        paths.retain(|path| path_within_root_lexical(path, &self.root));
        paths
    }

    /// Build the raw component-file candidates for an already-Flux-normalized,
    /// already-validated component name. Only [`resolve_component_path`] should
    /// call this — it applies the name guard and root-containment filter that
    /// keep the candidates from escaping the project root.
    fn component_path_candidates(&self, component_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Icon-set check first: <x-heroicon-o-clock> and friends resolve to a
        // concrete SVG file path. The blade-icons Factory registers each icon
        // at runtime via a loop over filesystem manifests, so static AST analysis
        // can't extract the pairs — we precompute the map by walking the SVG
        // directories of any blade-icons-shaped vendor package.
        if !component_name.contains("::") {
            if let Some(svg_path) = self.icon_aliases.get(component_name) {
                paths.push(PathBuf::from(svg_path));
                return paths;
            }
        }

        // Check explicit Blade component aliases first. Alias registrations like
        // Blade::component('components.buttons.light-button', 'light-button') or
        // their config-driven equivalents override the directory convention, so
        // the alias map wins when there's a hit.
        if !component_name.contains("::") {
            if let Some(aliased) = self.component_aliases.get(component_name) {
                let aliased_path = aliased.replace('.', "/");
                for view_path in &self.view_paths {
                    let mut full_path = self.root.join(view_path).join(&aliased_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
                if paths.is_empty() {
                    let mut full_path = self.root.join("resources/views").join(&aliased_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
                return paths;
            }
        }

        // Handle package components (e.g., "courier::alert")
        let (namespace, actual_component) = if let Some(pos) = component_name.find("::") {
            let namespace = &component_name[..pos];
            let component = &component_name[pos + 2..];
            (Some(namespace), component)
        } else {
            (None, component_name)
        };

        // Component name uses dots: "forms.input" -> "forms/input.blade.php"
        let component_path = actual_component.replace('.', "/");

        if let Some(ns) = namespace {
            // Markdown mail components are hardcoded in Laravel's
            // ComponentTagCompiler: `<x-mail::message>` maps straight to view
            // `mail::message`, and at render time `Markdown` points the `mail`
            // namespace at `{path}/html` for each configured path — the
            // published `resources/views/vendor/mail` first, then the
            // framework's bundled views. There is no `components/` segment.
            // Pushed first so the published path is `paths.first()` — the
            // diagnostic reports that as the "Expected at:" location.
            if ns == "mail" {
                let mut published = self
                    .root
                    .join("resources/views/vendor/mail/html")
                    .join(&component_path);
                published.set_extension("blade.php");
                paths.push(published);
                let mut framework = self
                    .root
                    .join("vendor/laravel/framework/src/Illuminate/Mail/resources/views/html")
                    .join(&component_path);
                framework.set_extension("blade.php");
                paths.push(framework);
            }

            // Anonymous component path (Blade::anonymousComponentPath): the
            // registered directory IS the components directory, so resolve
            // directly with no `components/` segment.
            if let Some(dir) = self.anonymous_component_paths.get(ns) {
                push_component_file_candidates(&mut paths, dir.join(&component_path));
            }

            // Anonymous component namespace (Blade::anonymousComponentNamespace):
            // the registered directory is relative to each view path.
            if let Some(dir) = self.anonymous_component_namespaces.get(ns) {
                for view_path in &self.view_paths {
                    let base = self.root.join(view_path).join(dir);
                    push_component_file_candidates(&mut paths, base.join(&component_path));
                }
            }

            // Flux ships anonymous Blade components under the `flux` prefix.
            // Beyond any registration discovered from its service provider, fall
            // back to Flux's conventional locations: the app-published
            // `resources/views/flux/`, the package source `vendor/livewire/flux`,
            // and Flux Pro `vendor/livewire/flux-pro`.
            if ns == "flux" {
                push_component_file_candidates(
                    &mut paths,
                    self.root.join("resources/views/flux").join(&component_path),
                );
                push_component_file_candidates(
                    &mut paths,
                    self.root
                        .join("vendor/livewire/flux/stubs/resources/views/flux")
                        .join(&component_path),
                );
                push_component_file_candidates(
                    &mut paths,
                    self.root
                        .join("vendor/livewire/flux-pro/stubs/resources/views/flux")
                        .join(&component_path),
                );
            }

            // Package component - check package view path first
            if let Some(package_view_path) = self.view_namespaces.get(ns) {
                // Anonymous package component: {package_views}/components/{component}.blade.php
                push_component_file_candidates(
                    &mut paths,
                    package_view_path.join("components").join(&component_path),
                );
            }

            // Also check component namespace (Blade::componentNamespace)
            if let Some(php_namespace) = self.component_namespaces.get(ns) {
                // Convert component name to PascalCase class path
                // "alert" -> "Alert.php", "alert-box" -> "AlertBox.php"
                let class_name = kebab_to_pascal_case(&component_path.replace('/', "\\"));
                let class_path = format!("{}/{}.php", php_namespace.replace('\\', "/"), class_name);
                // Try common locations for package classes
                paths.push(self.root.join("vendor").join(&class_path));
                paths.push(
                    self.root
                        .join("app/View/Components")
                        .join(&class_name)
                        .with_extension("php"),
                );
            }

            // Check vendor published components: resources/views/vendor/{namespace}/components/
            push_component_file_candidates(
                &mut paths,
                self.root
                    .join("resources/views/vendor")
                    .join(ns)
                    .join("components")
                    .join(&component_path),
            );
        } else {
            // Regular component - check each component path
            for (_namespace, base_path) in &self.component_paths {
                let mut full_path = self.root.join(base_path).join(&component_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }

            // If no component paths found, use default within view paths
            if paths.is_empty() {
                for view_path in &self.view_paths {
                    let mut full_path = self
                        .root
                        .join(view_path)
                        .join("components")
                        .join(&component_path);
                    full_path.set_extension("blade.php");
                    paths.push(full_path);
                }
            }
        }

        paths
    }

    /// Resolve a Livewire component name to file path
    pub fn resolve_livewire_path(&self, component_name: &str) -> Option<PathBuf> {
        let livewire_base = self.livewire_path.as_ref()?;

        // Convert component name to PascalCase path
        // "user-profile" -> "UserProfile.php"
        // "admin.dashboard" -> "Admin/Dashboard.php"

        let parts: Vec<&str> = component_name.split('.').collect();
        let mut path = self.root.join(livewire_base);

        for (i, part) in parts.iter().enumerate() {
            let pascal_case = kebab_to_pascal_case(part);

            if i == parts.len() - 1 {
                // Last part becomes the PHP file
                path.push(format!("{}.php", pascal_case));
            } else {
                // Other parts are directories
                path.push(pascal_case);
            }
        }

        Some(path)
    }
}

/// Rewrite a Flux single-colon component tag name (`flux:button`,
/// `flux:icon.arrow-right`) into the `flux::` namespace form the component
/// resolver understands. Returns `None` for names that aren't Flux tags or are
/// already namespaced (`flux::button` arrives pre-normalized).
pub fn normalize_flux_tag_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("flux:")?;
    if rest.starts_with(':') {
        return None;
    }
    Some(format!("flux::{rest}"))
}

/// Push the three file shapes Laravel accepts for an anonymous component at
/// `base` (the component's path under its directory, *without* extension),
/// mirroring `ComponentTagCompiler`'s guess order:
///   1. `{base}.blade.php`            — flat file
///   2. `{base}/index.blade.php`      — directory-index convention
///   3. `{base}/{last}.blade.php`     — directory-self convention
///      (`<x-ns::button>` → `button/button.blade.php`)
fn push_component_file_candidates(paths: &mut Vec<PathBuf>, base: PathBuf) {
    let mut direct = base.clone();
    direct.set_extension("blade.php");
    paths.push(direct);
    paths.push(base.join("index.blade.php"));
    if let Some(last) = base.file_name().and_then(|s| s.to_str()) {
        let self_named = format!("{last}.blade.php");
        paths.push(base.join(self_named));
    }
}

/// All candidate file paths that could back a Blade component tag, in
/// priority order. This is the **single source of truth** shared by
/// goto-definition and the "component not found" diagnostic, so the two can
/// never disagree about whether a component resolves (issue #69).
///
/// Layers, in order:
///   1. [`LaravelConfigData::resolve_component_path`] — conventional,
///      aliased, icon, anonymous-path/namespace, package-view, vendor-publish,
///      and the *naive* class-namespace guesses.
///   2. The conventional class-backed component file
///      (`app/View/Components/<Pascal>.php`).
///   3. **PSR-4 class-based `Blade::componentNamespace` components.** Layer 1
///      only emits a guessed `vendor/<Namespace>/...` path that ignores how
///      Composer actually lays packages out on disk, so namespaced class
///      components (`<x-filament::badge>`, `<x-mail::message>`) never matched.
///      Here we walk the registered PHP namespace to its real source
///      directory via the autoload map and append the class file path.
///   4. **Explicit class-backed registrations** —
///      `Blade::component('tag', Class::class)` in any provider, facade or
///      instance form (Laravel core registers `dynamic-component` this way).
///      The tag maps straight to the registered class's resolved file.
///
/// `autoload` supplies the project's PSR-4 prefix map (see
/// [`crate::composer_autoload::ComposerAutoload`]). The function does **not**
/// touch the filesystem itself — callers decide existence (cached async check
/// for the live server, direct `Path::exists` in tests).
pub fn component_candidate_paths(
    name: &str,
    config: &LaravelConfigData,
    autoload: &crate::composer_autoload::ComposerAutoload,
) -> Vec<PathBuf> {
    let mut candidates = config.resolve_component_path(name);

    // Conventional class-backed component (non-namespaced names only). A
    // namespaced tag like `flux:button` or `pkg::badge` would produce an
    // invalid `app/View/Components/Flux:button.php` candidate — illegal on
    // Windows and a wasted `stat` on POSIX. Namespaced forms resolve via the
    // PSR-4 `componentNamespace` block below instead, so skip them here.
    if !name.contains(':') {
        // A name that can't form a safe relative path yields no candidate.
        candidates.extend(
            crate::component_declaration_locator::conventional_class_file_path(name, config),
        );
    }

    // Explicit class-backed registration: Blade::component('tag', Class::class)
    // in any provider (facade or instance form). Laravel core registers
    // `dynamic-component` → Illuminate\View\DynamicComponent this way.
    if let Some(class_file) = config.class_component_files.get(name) {
        candidates.push(class_file.clone());
    }

    // PSR-4 class-based componentNamespace resolution.
    if let Some((namespace, component)) = name.split_once("::") {
        if let Some(php_namespace) = config.component_namespaces.get(namespace) {
            // `forms.text-input` → relative class path `Forms/TextInput.php`,
            // matching the FQCN `<php_namespace>\Forms\TextInput`. Each
            // `\`-delimited segment must be PascalCased independently, since
            // `kebab_to_pascal_case` only splits on `-`.
            let class_name = component
                .replace('.', "\\")
                .split('\\')
                .map(kebab_to_pascal_case)
                .collect::<Vec<_>>()
                .join("\\");
            let mut rel = PathBuf::new();
            for segment in class_name.split('\\') {
                rel.push(segment);
            }
            rel.set_extension("php");

            for dir in autoload.resolve_namespace_dirs(php_namespace) {
                candidates.push(dir.join(&rel));
            }
        }
    }

    candidates
}

/// Type of file that contains a view reference
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum FileReferenceType {
    Controller,
    BladeTemplate,
    LivewireComponent,
    Route,
}

/// Classified symbol under the cursor — the payload `Backend::references`
/// (and later `Backend::rename`) hands across the Salsa actor boundary.
///
/// We never raw-shape-match: a position only counts as a reference to the
/// requested symbol when (a) the parser tagged the position as that pattern
/// kind AND (b) the carried name matches. Random PHP strings that happen to
/// share the shape are not returned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SymbolRefData {
    View(String),
    Route(String),
    Config(String),
    Translation(String),
    Env(String),
    Component(String),
    Livewire(String),
    Middleware(String),
    Binding(String),
    /// A PHP class referenced by fully-qualified name from a Blade `@use`
    /// directive. Keyed by the FQCN as written, with any `\\` collapsed, so
    /// `@use('App\Models\Flight')` and `@use('App\\Models\\Flight')` agree.
    ///
    /// Blade `@use` is currently the only producer: PHP `use` statements are
    /// not indexed as class references, so find-references on a class reports
    /// its Blade imports, not every import project-wide.
    Class(String),
    /// An Eloquent magic member (accessor / column / relationship / scope /
    /// dynamic finder) or a plain class member, keyed by its inheritance-
    /// resolved declaring class FQCN + member name. Unlike the literal kinds
    /// above — whose name is a raw string the parser tagged — this key is
    /// produced by the M3 resolver, so a trait-shared member keys once and
    /// every inheriting model's usages collapse to the same entry.
    MagicMember {
        fqcn: String,
        member: String,
    },
}

/// Location of a single parser-classified reference. Generic across pattern
/// kinds — `Backend::references` converts these into LSP `Location`s.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceLocationData {
    pub file_path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// View reference location data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewReferenceLocationData {
    /// The file that contains the reference
    pub file_path: PathBuf,
    /// The line number where the reference occurs (0-based)
    pub line: u32,
    /// The character position where the reference starts (0-based)
    pub character: u32,
    /// The type of file containing the reference
    pub reference_type: FileReferenceType,
    /// The view name being referenced
    pub view_name: String,
    /// Whether this is a route view (Route::view or response()->view)
    pub is_route_view: bool,
}

/// Middleware registration data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct MiddlewareRegistrationData {
    /// The middleware alias (e.g., "auth")
    pub alias: String,
    /// Fully qualified class name
    pub class_name: String,
    /// Resolved file path of the middleware class
    pub file_path: Option<PathBuf>,
    /// Source file where the alias is defined
    pub source_file: Option<PathBuf>,
    /// Line number in source file (0-based)
    pub source_line: Option<usize>,
    /// Priority: 0=framework, 1=package, 2=module, 3=app
    pub priority: u8,
}

/// Binding type enum for transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum BindingTypeData {
    Bind,
    Singleton,
    Scoped,
    Alias,
}

/// Container binding data for transfer across async boundaries
#[derive(Debug, Clone, serde::Serialize)]
pub struct BindingRegistrationData {
    /// The abstract/alias name
    pub abstract_name: String,
    /// Fully qualified concrete class name
    pub concrete_class: String,
    /// Resolved file path of the concrete class
    pub file_path: Option<PathBuf>,
    /// Binding type
    pub binding_type: BindingTypeData,
    /// Source file where the binding is defined
    pub source_file: Option<PathBuf>,
    /// Line number in source file (0-based)
    pub source_line: Option<usize>,
    /// Priority: 0=framework, 1=package, 2=module, 3=app
    pub priority: u8,
}

/// A resolved macro/mixin registration for transfer across async boundaries and
/// for the live query path. Keyed in the actor registry on
/// `(receiver_fqcn, macro_name)`; the `decl_file`/`decl_line` point at the
/// definition site (the registered closure, or the mixin method).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MacroRegistrationData {
    /// Resolved Macroable host FQCN (e.g. "Illuminate\\Support\\Str").
    pub receiver_fqcn: String,
    /// The registered macro/method name (e.g. "uuid7").
    pub macro_name: String,
    /// Definition site file.
    pub decl_file: PathBuf,
    /// 0-based definition line.
    pub decl_line: u32,
    /// Priority: 0=framework, 1=package, 2=module, 3=app (higher wins on key collision).
    pub priority: u8,
}

/// One provider file's own registration contribution — the macros, bindings,
/// and facade aliases parsed from exactly that file — in sorted, comparable
/// form. The save path snapshots this before and after a provider save: a
/// non-empty diff with an empty class-surface diff is the body-only
/// registration edit that previously never rippled to dependent call sites
/// (#255).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderRegistrationsData {
    /// `(receiver host FQCN, macro name)` pairs, sorted.
    pub macros: Vec<(String, String)>,
    /// `(abstract name, concrete FQCN)` pairs, sorted.
    pub bindings: Vec<(String, String)>,
    /// `(alias token, target FQCN)` pairs, sorted. Only `bootstrap/app.php`
    /// (`withAliases`) and `config/app.php` (`aliases`) contribute here.
    pub aliases: Vec<(String, String)>,
}

/// The reverse-index keys whose dependents a provider registration diff must
/// re-resolve — the save path feeds these into the same blast radius a class
/// surface change uses (#255). Empty when nothing changed.
///
/// Emitted per changed (added/removed/retargeted) entry, keyed on what the
/// dependent call sites actually recorded in `MagicDependencyIndex`:
///
/// - **macro**: the receiver host FQCN (`Illuminate\Support\Str`) — every
///   call site records the resolved receiver as an attempt, so this finds
///   sites in both directions (macro added: the previously-failed sites;
///   macro removed/renamed: the previously-resolved sites).
/// - **binding**: the `binding:<abstract>` attempt key
///   ([`crate::magic_dependency_index::BINDING_DEP_PREFIX`]) from both sides
///   of the diff — every string-keyed container site (`app('key')`, mapped
///   zero-arg helpers) records it resolved-or-not, so a BRAND-NEW binding
///   reaches the sites that previously resolved to nothing — plus the
///   concrete FQCN from both sides, which finds sites already referencing
///   the target directly.
/// - **facade alias**: the `alias:<token>` attempt key
///   ([`crate::magic_dependency_index::ALIAS_DEP_PREFIX`]) from both sides of the
///   diff — every global-alias facade site (`Auth::check()`) records it
///   resolved-or-not, so an alias RETARGET reaches the OLD target's sites even on
///   the first save of a session, when the empty baseline makes the diff see only
///   the new target added (#267) — plus the target FQCN from both sides, which
///   finds any site referencing the aliased class directly (a `use`-import or
///   type-hint recording).
/// - **the provider's own path**: macro classifications record the macro's
///   declaration file as a dependency ([`crate::member_resolver`]), which for
///   inline `::macro()` registrations is the registering provider itself.
pub fn registration_ripple_keys(
    before: &ProviderRegistrationsData,
    after: &ProviderRegistrationsData,
    provider_path: &Path,
) -> Vec<String> {
    fn changed<'a>(
        a: &'a [(String, String)],
        b: &'a [(String, String)],
    ) -> impl Iterator<Item = &'a (String, String)> {
        let sa: std::collections::HashSet<&(String, String)> = a.iter().collect();
        let sb: std::collections::HashSet<&(String, String)> = b.iter().collect();
        sa.symmetric_difference(&sb)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
    }

    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    keys.extend(changed(&before.macros, &after.macros).map(|(host, _)| host.clone()));
    keys.extend(changed(&before.bindings, &after.bindings).flat_map(
        |(abstract_name, concrete)| {
            [
                format!(
                    "{}{abstract_name}",
                    crate::magic_dependency_index::BINDING_DEP_PREFIX
                ),
                concrete.clone(),
            ]
        },
    ));
    keys.extend(
        changed(&before.aliases, &after.aliases).flat_map(|(token, target)| {
            [
                crate::magic_dependency_index::alias_dep_key(token),
                target.clone(),
            ]
        }),
    );
    if keys.is_empty() {
        return Vec::new();
    }
    keys.insert(provider_path.to_string_lossy().into_owned());
    keys.into_iter().collect()
}

/// Pairs the class-hierarchy index (FQCN → file) with the in-actor container
/// binding registry (binding key → concrete FQCN) behind the
/// [`crate::member_resolver::ClassFileResolver`] seam, so the live query path
/// (find-references fallback, hover, rename) types `app('key')` / `resolve('key')`
/// receivers the same way the build pass does — without materializing the full
/// class-file map a `SnapshotResolver` would need.
struct ContainerAwareResolver<'a> {
    index: &'a crate::class_hierarchy_index::ClassHierarchyIndex,
    bindings: &'a HashMap<String, BindingRegistrationData>,
    singletons: &'a HashMap<String, BindingRegistrationData>,
    /// The merged facade alias map (token → facade FQCN), so the live query path
    /// resolves facade receivers (`Auth::check()`) to their implementation the
    /// same way the build pass does.
    facade_aliases: Arc<HashMap<String, String>>,
    /// The macro registry — `(receiver_fqcn, macro_name)` → definition site —
    /// so the live query path classifies a runtime-registered macro/mixin member
    /// the same way the build pass does.
    macros: Arc<HashMap<(String, String), MacroRegistrationData>>,
}

impl crate::member_resolver::ClassFileResolver for ContainerAwareResolver<'_> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        crate::member_resolver::ClassFileResolver::class_file(self.index, fqcn)
    }
    fn binding_concrete(&self, key: &str) -> Option<String> {
        // Bindings win over singletons on key collision (mirrors
        // `handle_get_binding_by_name`); the concrete is normalized to the
        // leading-backslash-free form the class index keys on.
        self.bindings
            .get(key)
            .or_else(|| self.singletons.get(key))
            .map(|b| b.concrete_class.trim_start_matches('\\').to_string())
    }
    fn facade_aliases(&self) -> std::borrow::Cow<'_, HashMap<String, String>> {
        std::borrow::Cow::Borrowed(&self.facade_aliases)
    }
    fn macro_target(&self, receiver_fqcn: &str, name: &str) -> Option<(PathBuf, u32)> {
        self.macros
            .get(&(receiver_fqcn.to_string(), name.to_string()))
            .map(|m| (m.decl_file.clone(), m.decl_line))
    }
    fn has_macro_host(&self, receiver_fqcn: &str) -> bool {
        self.macros.keys().any(|(host, _)| host == receiver_fqcn)
    }
    fn implementers_of(&self, interface_fqcn: &str) -> Vec<String> {
        self.index.implementers_of(interface_fqcn).to_vec()
    }
}

/// Package view namespace data for transfer across async boundaries
/// From: $this->loadViewsFrom(__DIR__.'/../resources/views', 'courier')
#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewNamespaceData {
    /// The namespace prefix (e.g., "courier")
    pub namespace: String,
    /// Resolved view path
    pub view_path: Option<PathBuf>,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=module, 3=app
    pub priority: u8,
}

/// Blade component registration data for transfer across async boundaries
/// From: Blade::component('package-alert', AlertComponent::class)
#[derive(Debug, Clone, serde::Serialize)]
pub struct BladeComponentRegData {
    /// Component tag name (e.g., "package-alert")
    pub tag_name: String,
    /// Full class name
    pub class_name: String,
    /// Resolved file path of the component class
    pub file_path: Option<PathBuf>,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=module, 3=app
    pub priority: u8,
}

/// Component namespace registration data for transfer across async boundaries
/// From: Blade::componentNamespace('Nightshade\\Views\\Components', 'nightshade')
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComponentNamespaceData {
    /// Namespace prefix (e.g., "nightshade")
    pub prefix: String,
    /// PHP namespace (e.g., "Nightshade\\Views\\Components")
    pub php_namespace: String,
    /// Source file where registered
    pub source_file: PathBuf,
    /// Line number in source file
    pub source_line: u32,
    /// Priority: 0=framework, 1=package, 2=module, 3=app
    pub priority: u8,
}

// ============================================================================
// Salsa-based Data Transfer Types (for new incremental parsing)
// ============================================================================

/// Parsed environment variable data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedEnvVarData {
    /// Variable name
    pub name: String,
    /// Variable value
    pub value: String,
    /// Line number (0-indexed)
    pub line: u32,
    /// Column of variable name
    pub column: u32,
    /// Column where value starts
    pub value_column: u32,
    /// Whether commented out
    pub is_commented: bool,
    /// Priority (0=.env.example, 1=.env.local, 2=.env)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Parsed middleware data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedMiddlewareData {
    /// Middleware alias
    pub alias: String,
    /// Full class name
    pub class_name: String,
    /// Resolved file path
    pub file_path: Option<PathBuf>,
    /// Line in source file
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Parsed binding data from Salsa (for transfer across async boundaries)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedBindingData {
    /// Abstract name or interface
    pub abstract_name: String,
    /// Concrete class name
    pub concrete_class: String,
    /// Resolved file path
    pub file_path: Option<PathBuf>,
    /// Binding type
    pub binding_type: BindingTypeEnum,
    /// Line in source file
    pub source_line: u32,
    /// Priority (0=framework, 1=package, 2=module, 3=app)
    pub priority: u8,
    /// Source file path
    pub source_file: PathBuf,
}

/// Entry in the sorted position index for fast lookup
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PositionEntry {
    line: u32,
    column: u32,
    end_column: u32,
    pattern: PatternAtPosition,
}

/// All parsed patterns for a file - plain data for transfer
/// Uses Rc for efficient cloning when building the position index
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ParsedPatternsData {
    pub views: Vec<Arc<ViewReferenceData>>,
    /// Inertia page references (`inertia()`, `Inertia::render()`,
    /// `Route::inertia()`) — issue #10. Like `feature_refs`/`route_refs`, these
    /// are extracted in `handle_get_patterns` rather than as a `ParsedPatterns`
    /// Salsa field (that struct is at the 12-element tuple-Hash cap).
    ///
    /// `#[serde(default)]` so disk-cache entries written by older builds (which
    /// lacked this field) deserialize with an empty list rather than failing
    /// the whole entry; the next edit re-runs extraction and repopulates.
    #[serde(default)]
    pub inertia_refs: Vec<Arc<InertiaReferenceData>>,
    pub components: Vec<Arc<ComponentReferenceData>>,
    pub directives: Vec<Arc<DirectiveReferenceData>>,
    pub env_refs: Vec<Arc<EnvReferenceData>>,
    pub config_refs: Vec<Arc<ConfigReferenceData>>,
    pub livewire_refs: Vec<Arc<LivewireReferenceData>>,
    pub middleware_refs: Vec<Arc<MiddlewareReferenceData>>,
    pub translation_refs: Vec<Arc<TranslationReferenceData>>,
    pub asset_refs: Vec<Arc<AssetReferenceData>>,
    pub binding_refs: Vec<Arc<BindingReferenceData>>,
    pub route_refs: Vec<Arc<RouteReferenceData>>,
    /// Curated Laravel helper-function identifiers (`route`, `view`, `config`,
    /// `auth`, `app`, `session`, `cache`) at their name span — drives the
    /// helper hover card (#58). Like `route_refs`, extracted in
    /// `handle_get_patterns` rather than as a `ParsedPatterns` Salsa field
    /// (that struct is at the 12-element tuple-Hash cap).
    ///
    /// `#[serde(default)]` so disk-cache entries written by older builds (which
    /// lacked this field) deserialize with an empty list rather than failing
    /// the whole entry; the next edit re-runs extraction and repopulates.
    #[serde(default)]
    pub helper_refs: Vec<Arc<HelperReferenceData>>,
    pub url_refs: Vec<Arc<UrlReferenceData>>,
    pub action_refs: Vec<Arc<ActionReferenceData>>,
    pub feature_refs: Vec<Arc<FeatureReferenceData>>,
    /// Eloquent / DB query builder chains extracted in the same PHP parse pass
    /// as route/url/action/feature refs (see [`crate::query_chain::extractor`]).
    /// Stored alongside the other patterns rather than as a `ParsedPatterns`
    /// field because that struct is at Salsa's 12-element tuple-Hash cap.
    ///
    /// `#[serde(default)]` so disk-cache entries written by older builds (which
    /// lacked this field) deserialize with an empty chains list rather than
    /// failing the whole entry. The next file edit re-runs extraction and
    /// populates chains properly.
    #[serde(default)]
    pub chains: Vec<Arc<crate::query_chain::BuilderChain>>,
    /// Property-form member accesses (`$user->email`, `$this->profile`)
    /// captured for the magic-member semantic index (M2). Like `chains`,
    /// stored here rather than as a `ParsedPatterns` Salsa field because that
    /// struct is at its 12-element tuple-Hash cap.
    ///
    /// `#[serde(default)]` so older disk-cache entries (written before this
    /// field existed) deserialize with an empty list; the next edit re-runs
    /// extraction and repopulates.
    #[serde(default)]
    pub member_access_refs: Vec<Arc<MemberAccessReferenceData>>,
    /// Whether this (Blade) file is a Volt component — captured once at parse
    /// time (the source is already in hand) so the magic-build's Blade pass can
    /// route Volt vs. controller-rendered resolution without re-reading the
    /// file. Critical on projects with large published Blade sets (e.g. Flux's
    /// ~58k FontAwesome icon templates): without it the pass would open every
    /// one just to check the Volt signature. Always `false` for `.php` files.
    #[serde(default)]
    pub is_volt: bool,
    /// Blade `@foreach`/`@forelse` loops in this file — item variable, iterable
    /// expression, and line range. Lets the magic-build type a loop variable
    /// (`@foreach($users as $user) … {{ $user->email }}`) from its iterable's
    /// element type without re-reading the file at build time. Captured at parse
    /// (source in hand). Empty for `.php` files.
    #[serde(default)]
    pub blade_loops: Vec<BladeLoopVar>,
    /// M1 single-parse capture: the file's own-source resolution context —
    /// per-site receiver recipes, view-render plans, and the Volt surface —
    /// compiled once at parse so the magic-member resolve passes never re-read
    /// or re-parse this file. `None` for files with nothing to resolve (icon
    /// Blade templates, vendor files): a single tag byte, so pattern-free files
    /// pay ~zero. `member_context.sites` is positionally parallel to
    /// `member_access_refs` when present. `#[serde(default)]` keeps older
    /// disk-cache entries decodable in principle, but the growth of this struct
    /// is guarded by the `pattern_disk_cache` SCHEMA_VERSION bump (10 → 11) —
    /// bincode is non-self-describing, so stale slim entries re-parse.
    #[serde(default)]
    pub member_context: Option<Box<MemberContextData>>,
    /// Class FQCNs imported by Blade `@use` directives. Derived from
    /// `directives` by [`class_refs_from_directives`] rather than extracted
    /// separately — the directive already carries the name and its in-quote
    /// span. Stored here rather than as a `ParsedPatterns` Salsa field because
    /// that struct is at its 12-element tuple-Hash cap. Empty for `.php` files.
    ///
    /// `#[serde(default)]` so older disk-cache entries deserialize with an
    /// empty list; the `pattern_disk_cache` SCHEMA_VERSION bump makes them
    /// re-parse anyway.
    #[serde(default)]
    pub class_refs: Vec<Arc<ClassReferenceData>>,
    /// 100% derived from the Vec fields above and read by exactly one caller,
    /// `find_at_position`, for exactly one file at a time — the one the
    /// cursor is currently in. Built LAZILY on that call's first invocation
    /// rather than eagerly at parse time: a large project parses/restores
    /// tens of thousands of `ParsedPatternsData`, and eagerly sorting every
    /// one of them (most of which the cursor never visits in a session) was
    /// wasted CPU and ~23MB of Vec allocations that outlive their only reader.
    ///
    /// Skipped during (de)serialization — restoring from the on-disk cache
    /// leaves this empty, and the next `find_at_position` call on a restored
    /// entry builds it on demand, same as a freshly parsed one. We don't
    /// persist it because (a) it duplicates data already in the Vec fields
    /// above, (b) rebuilding is O(n log n) and fast, and (c)
    /// PatternAtPosition's Arc fields would deserialize as independent
    /// allocations, which is wasteful.
    #[serde(skip)]
    sorted_positions: std::sync::OnceLock<Vec<PositionEntry>>,
}

/// A pattern found at a specific cursor position
/// Uses Rc for cheap cloning (just increments reference count)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PatternAtPosition {
    View(Arc<ViewReferenceData>),
    /// An Inertia page reference (`inertia()`, `Inertia::render()`,
    /// `Route::inertia()`) under the cursor — issue #10.
    Inertia(Arc<InertiaReferenceData>),
    Component(Arc<ComponentReferenceData>),
    Directive(Arc<DirectiveReferenceData>),
    /// A class FQCN at an import site — a PHP `use` statement, or the name
    /// inside a Blade `@use`.
    ///
    /// In a Blade file the enclosing `Directive` entry starts earlier on the
    /// line and so wins `find_at_position`; its classifier arm yields the same
    /// `SymbolRef::Class`, so both routes agree. This variant is what a cursor
    /// on a PHP `use` resolves through, where no directive covers it.
    Class(Arc<ClassReferenceData>),
    EnvRef(Arc<EnvReferenceData>),
    ConfigRef(Arc<ConfigReferenceData>),
    Livewire(Arc<LivewireReferenceData>),
    Middleware(Arc<MiddlewareReferenceData>),
    Translation(Arc<TranslationReferenceData>),
    Asset(Arc<AssetReferenceData>),
    Binding(Arc<BindingReferenceData>),
    Route(Arc<RouteReferenceData>),
    /// A curated global helper-function identifier (`route`, `view`, `config`,
    /// `auth`, `app`, `session`, `cache`) under the cursor. Carries the helper
    /// name (via `HelperReferenceData.name`, per the issue's `{ name }` intent)
    /// plus the position the index and `pattern_range_at` need — matching the
    /// `Arc<…ReferenceData>` shape every sibling variant uses.
    HelperIdentifier(Arc<HelperReferenceData>),
    Url(Arc<UrlReferenceData>),
    Action(Arc<ActionReferenceData>),
    Feature(Arc<FeatureReferenceData>),
    MemberAccess(Arc<MemberAccessReferenceData>),
}

impl ParsedPatternsData {
    /// Compute the sorted (line, column) position index from the pattern
    /// vectors. Pure function of `&self` — this is what `find_at_position`
    /// runs as the `OnceLock` initializer on its first call for a file, and
    /// what `build_position_index` runs to force-populate it eagerly.
    fn compute_position_index(&self) -> Vec<PositionEntry> {
        let mut entries = Vec::new();

        // Add all patterns to the index
        for comp in &self.components {
            entries.push(PositionEntry {
                line: comp.line,
                column: comp.column,
                end_column: comp.end_column,
                pattern: PatternAtPosition::Component(comp.clone()),
            });
        }

        for lw in &self.livewire_refs {
            entries.push(PositionEntry {
                line: lw.line,
                column: lw.column,
                end_column: lw.end_column,
                pattern: PatternAtPosition::Livewire(lw.clone()),
            });
        }

        for dir in &self.directives {
            // `@use` names a class, and `class_refs` already carries that name
            // at its exact span. Emitting a directive entry too would shadow it
            // — the directive starts at column 0, so it wins `find_at_position`
            // for every cursor inside the call — and a single directive entry
            // cannot distinguish the members of a group import. Skipping it
            // makes class refs the one route for `@use`, in Blade and PHP alike.
            if dir.name == "use" {
                continue;
            }
            entries.push(PositionEntry {
                line: dir.line,
                column: dir.column,
                end_column: dir.end_column,
                pattern: PatternAtPosition::Directive(dir.clone()),
            });
        }

        for view in &self.views {
            entries.push(PositionEntry {
                line: view.line,
                column: view.column,
                end_column: view.end_column,
                pattern: PatternAtPosition::View(view.clone()),
            });
        }

        for inertia in &self.inertia_refs {
            entries.push(PositionEntry {
                line: inertia.line,
                column: inertia.column,
                end_column: inertia.end_column,
                pattern: PatternAtPosition::Inertia(inertia.clone()),
            });
        }

        for env in &self.env_refs {
            entries.push(PositionEntry {
                line: env.line,
                column: env.column,
                end_column: env.end_column,
                pattern: PatternAtPosition::EnvRef(env.clone()),
            });
        }

        for config in &self.config_refs {
            entries.push(PositionEntry {
                line: config.line,
                column: config.column,
                end_column: config.end_column,
                pattern: PatternAtPosition::ConfigRef(config.clone()),
            });
        }

        for mw in &self.middleware_refs {
            entries.push(PositionEntry {
                line: mw.line,
                column: mw.column,
                end_column: mw.end_column,
                pattern: PatternAtPosition::Middleware(mw.clone()),
            });
        }

        for trans in &self.translation_refs {
            entries.push(PositionEntry {
                line: trans.line,
                column: trans.column,
                end_column: trans.end_column,
                pattern: PatternAtPosition::Translation(trans.clone()),
            });
        }

        for asset in &self.asset_refs {
            entries.push(PositionEntry {
                line: asset.line,
                column: asset.column,
                end_column: asset.end_column,
                pattern: PatternAtPosition::Asset(asset.clone()),
            });
        }

        for binding in &self.binding_refs {
            entries.push(PositionEntry {
                line: binding.line,
                column: binding.column,
                end_column: binding.end_column,
                pattern: PatternAtPosition::Binding(binding.clone()),
            });
        }

        for route in &self.route_refs {
            entries.push(PositionEntry {
                line: route.line,
                column: route.column,
                end_column: route.end_column,
                pattern: PatternAtPosition::Route(route.clone()),
            });
        }

        for helper in &self.helper_refs {
            entries.push(PositionEntry {
                line: helper.line,
                column: helper.column,
                end_column: helper.end_column,
                pattern: PatternAtPosition::HelperIdentifier(helper.clone()),
            });
        }

        for url in &self.url_refs {
            entries.push(PositionEntry {
                line: url.line,
                column: url.column,
                end_column: url.end_column,
                pattern: PatternAtPosition::Url(url.clone()),
            });
        }

        for action in &self.action_refs {
            entries.push(PositionEntry {
                line: action.line,
                column: action.column,
                end_column: action.end_column,
                pattern: PatternAtPosition::Action(action.clone()),
            });
        }

        for feature in &self.feature_refs {
            entries.push(PositionEntry {
                line: feature.line,
                column: feature.column,
                end_column: feature.end_column,
                pattern: PatternAtPosition::Feature(feature.clone()),
            });
        }

        for member in &self.member_access_refs {
            entries.push(PositionEntry {
                line: member.line,
                column: member.column,
                end_column: member.end_column,
                pattern: PatternAtPosition::MemberAccess(member.clone()),
            });
        }

        for class_ref in &self.class_refs {
            entries.push(PositionEntry {
                line: class_ref.line,
                column: class_ref.column,
                end_column: class_ref.end_column,
                pattern: PatternAtPosition::Class(class_ref.clone()),
            });
        }

        // Sort by (line, column) for efficient binary search
        entries.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.column.cmp(&b.column)));

        entries
    }

    /// Force-build (or rebuild) the sorted position index right now,
    /// overwriting anything already cached in `sorted_positions`. Nothing in
    /// production calls this any more: `find_at_position` builds the index
    /// lazily on its own first call, so it's paid only for files the cursor
    /// actually visits. Kept for callers (mostly tests) that want the index
    /// populated up front rather than on first lookup.
    pub fn build_position_index(&mut self) {
        let entries = self.compute_position_index();
        self.sorted_positions = std::sync::OnceLock::from(entries);
    }

    /// Find a pattern at the given cursor position (line, column).
    /// Lazily builds `sorted_positions` on the first call for this
    /// `ParsedPatternsData` (see the field's doc comment), then uses binary
    /// search for O(log n) lookup to find the line, and a linear scan within
    /// the line for the matching column range.
    pub fn find_at_position(&self, line: u32, column: u32) -> Option<PatternAtPosition> {
        let sorted_positions = self
            .sorted_positions
            .get_or_init(|| self.compute_position_index());

        if sorted_positions.is_empty() {
            return None;
        }

        // Binary search to find the first entry on or after target line
        let start_idx = sorted_positions.partition_point(|e| e.line < line);

        // Scan entries on this line
        for entry in &sorted_positions[start_idx..] {
            // Stop when we've passed the target line
            if entry.line > line {
                break;
            }

            // Check if cursor is within this pattern's column range
            if column >= entry.column && column <= entry.end_column {
                return Some(entry.pattern.clone());
            }
        }

        None
    }
}

// ============================================================================
// Actor Pattern - For async integration
// ============================================================================

/// A facade receiver (`\Auth`) resolved to its bound concrete class — the
/// goto/hover target shared by both handlers. Goto jumps to `file`/`decl_line`;
/// hover renders a class-definition card from the file (FQCN header + class
/// signature snippet), the same way the Livewire/class hover does.
#[derive(Debug, Clone)]
pub struct FacadeReceiverTarget {
    pub fqcn: String,
    pub file: PathBuf,
    pub decl_line: u32,
}

/// Requests that can be sent to the Salsa actor
/// The answer to one backing-class resolution for a Blade template (#339,
/// item 7): the plain-`.php` files that back it, and each of those files'
/// current source, plus the template itself when it declares its component
/// inline.
///
/// Both halves come from the same pair of memoized queries in one actor
/// round-trip, so a caller that needs only the paths (the item 1 up-walk, which
/// calls this once per ancestor) never pays for a second request that would
/// recompute the same lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BladeBackingResolutionData {
    /// Backing `.php` paths that exist and could be read, in precedence order.
    pub files: Vec<PathBuf>,
    /// `(path, source)` for each entry of `files`, plus the Blade template
    /// itself when it carries an inline component class.
    pub sources: Vec<(PathBuf, String)>,
}

pub enum SalsaRequest {
    /// Update or create a file in the database
    UpdateFile {
        path: PathBuf,
        version: i32,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Get parsed patterns for a file
    GetPatterns {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<ParsedPatternsData>>>,
    },
    /// Get parsed loop blocks for a Blade file
    GetLoopBlocks {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>>>,
    },
    /// Get parsed @php block assignments for a Blade file
    GetPhpAssignments {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<(String, String)>>>>,
    },
    /// Get the document-symbol tree for a file (drives textDocument/documentSymbol)
    GetDocumentSymbols {
        path: PathBuf,
        reply: oneshot::Sender<Option<Arc<Vec<crate::document_symbols::SymbolEntry>>>>,
    },
    /// Resolve a `$this->X` member access in a Livewire component PHP file
    /// (auto-registers the file as a Salsa input via mtime-based invalidation).
    ResolveLivewireMember {
        path: PathBuf,
        member: String,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Remove a file from the database
    RemoveFile {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Record that an editor buffer for this path is open, so a `didClose`
    /// for an earlier buffer of the same path cannot hand the live one back
    /// to the loader — see [`SalsaActor::acquire_external_php_ownership`].
    AcquireExternalPhpOwnership {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },

    /// Hand a client-pushed path's text back to the backing-class loader once
    /// its last buffer closes, evicting nothing — see
    /// [`SalsaActor::release_external_php_ownership`].
    ReleaseExternalPhpOwnership {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },

    /// Tell the actor to update its per-category file lists in
    /// response to a filesystem event from the language client. The
    /// path is classified against the roots captured at the last
    /// `register_project_files` call; if it falls under a known root,
    /// it's added to (or removed from) that category's list.
    ///
    /// Returns the assigned `FileCategory` as a tuple-stringified
    /// label, or `None` if the path didn't match any project root
    /// (the actor silently ignored it). Used for logging — the
    /// watcher handler isn't required to do anything with the value.
    UpdateProjectFileList {
        path: PathBuf,
        op: FileListOp,
        reply: oneshot::Sender<Option<&'static str>>,
    },

    /// Rebuild the symbol_index from the current pattern_cache. Sent
    /// once after warming completes so the first find-references query
    /// is fast. Replies with the total entry count for logging.
    ///
    /// Watcher events don't need to send this — they incrementally
    /// update the index via `mark_dirty` from inside the relevant
    /// handlers, processed lazily on next query.
    BuildSymbolIndex { reply: oneshot::Sender<usize> },

    /// Wipe every in-memory reference index for a cold reindex (pattern
    /// cache, symbol index, class hierarchy, per-file LRU caches, class-files
    /// snapshot). See `SalsaHandle::clear_reindex_state`.
    ClearReindexState { reply: oneshot::Sender<()> },

    // === Config Management ===
    /// Register configuration files for the project
    RegisterConfigFiles {
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
        reply: oneshot::Sender<()>,
    },
    /// Update a specific configuration file
    UpdateConfigFile {
        path: PathBuf,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Get the current Laravel configuration
    GetLaravelConfig {
        reply: oneshot::Sender<Option<LaravelConfigData>>,
    },

    // === Blade backing-class resolution (#339, item 7) ===
    /// Read the actor database's query body-execution counters. Exists so a
    /// test driving the real LSP handler can prove the memo was hit, which a
    /// return value alone cannot show.
    QueryRunCounts {
        reply: oneshot::Sender<QueryRunCountsData>,
    },
    /// Replace the render-index snapshot the backing-class queries read.
    /// Bumps the Salsa revision, so only send this when the index has actually
    /// changed — see `Backend::pending_render_index_snapshot`.
    SetRenderIndex {
        /// `ViewVarIndex::generation()` this snapshot was taken at. The actor
        /// drops a snapshot older than the one it holds, so two concurrent
        /// pushes cannot leave it serving the loser's data.
        generation: u64,
        entries: Vec<(String, PathBuf)>,
        reply: oneshot::Sender<()>,
    },
    /// Resolve the PHP class(es) backing a Blade template, memoized against
    /// the render index and each backing file's content.
    BladeBackingClassResolution {
        blade_path: PathBuf,
        /// The template's own Laravel view name, when it has one.
        view_name: Option<String>,
        /// Livewire-convention class paths the caller already resolved.
        livewire_paths: Vec<PathBuf>,
        /// The live editor buffer for `blade_path`, when the document is open.
        /// `None` falls back to the file on disk.
        live_blade_text: Option<String>,
        reply: oneshot::Sender<BladeBackingResolutionData>,
    },

    // === Reference Finding ===
    /// Register project files for reference finding
    /// Scans directories and registers all PHP/Blade files
    RegisterProjectFiles {
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
        /// Every `vendor/` PHP file, from the shared vendor walk (issue #371).
        /// Handed in rather than re-walked here: this actor runs on its own
        /// thread and cannot reach the server's cached
        /// [`crate::vendor_index::VendorIndex`].
        vendor_files: Vec<PathBuf>,
        reply: oneshot::Sender<()>,
    },
    /// Find all references to a specific view across the project
    FindViewReferences {
        view_name: String,
        reply: oneshot::Sender<Vec<ViewReferenceLocationData>>,
    },
    /// Every Blade template that RENDERS one of these components — the reverse
    /// edge the item-1 up-walk climbs (#339). `component_names` are matched
    /// against `<x-…>` tags (`ParsedPatternsData::components`) and
    /// `livewire_names` against `<livewire:…>` tags
    /// (`ParsedPatternsData::livewire_refs`), because a partial is used through
    /// either syntax and indexing only the first would leave the other silent.
    ///
    /// Replies with the rendering `.blade.php` paths, sorted and deduped.
    FilesRenderingComponent {
        component_names: Vec<String>,
        livewire_names: Vec<String>,
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
    /// Find all references to a classified symbol across the project.
    /// Iterates `ProjectFiles` and filters parser-classified patterns by name —
    /// never matches by raw string shape.
    FindReferences {
        symbol: SymbolRefData,
        include_declaration: bool,
        reply: oneshot::Sender<Vec<ReferenceLocationData>>,
    },
    /// Count references for a symbol straight from the inverted index — the
    /// cheap, lazy primitive behind code-lens `resolve` (#59). Unlike
    /// `FindReferences` it does no dirty-refresh / project walk; it's a direct
    /// `symbol_index` lookup returning the occurrence count.
    CountSymbolReferences {
        symbol: SymbolRefData,
        reply: oneshot::Sender<usize>,
    },
    /// Return every project file path the actor currently has registered.
    /// Used by the warming task to compute which files to parse out-of-band.
    ListProjectFiles {
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
    /// Bulk-import a batch of pre-parsed `ParsedPatternsData` into the
    /// actor's pattern cache. The warming task uses this to push the
    /// results of parallel out-of-actor parsing back into the cache in
    /// one shot, instead of paying the per-file actor round-trip cost.
    BulkImportPatterns {
        entries: Vec<(PathBuf, Arc<ParsedPatternsData>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Bulk-import class-hierarchy nodes parsed out-of-actor during warming.
    /// Each entry's nodes replace any existing entry for that path. Replies
    /// with the total class count after import (for logging).
    BulkImportHierarchy {
        entries: Vec<(PathBuf, Vec<crate::class_hierarchy_index::ClassNode>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Snapshot the `fqcn → declaring file` map for the out-of-actor
    /// magic-member index build (M4).
    SnapshotClassFiles {
        reply: oneshot::Sender<Arc<std::collections::HashMap<String, PathBuf>>>,
    },
    /// Snapshot the `binding key → concrete FQCN` map for the same out-of-actor
    /// build, so `app('key')` / `resolve('key')` receivers resolve to their
    /// bound class while indexing.
    SnapshotBindings {
        reply: oneshot::Sender<Arc<std::collections::HashMap<String, String>>>,
    },
    /// Snapshot the facade alias map — token → facade FQCN (`Auth` →
    /// `Illuminate\Support\Facades\Auth`) — merged from the built-in seed,
    /// `config/app.php`'s `aliases`, and `bootstrap/app.php`'s `withAliases`.
    /// Mirrors [`Self::SnapshotBindings`]: the facade receiver path resolves a
    /// static-call token to its facade FQCN against this owned copy.
    SnapshotFacadeAliases {
        reply: oneshot::Sender<Arc<std::collections::HashMap<String, String>>>,
    },
    /// Snapshot the macro registry — `(receiver_fqcn, macro_name)` → `(decl_file,
    /// decl_line)` — for the same out-of-actor build, so a runtime-registered
    /// macro/mixin member (`Str::uuid7()`) classifies while indexing exactly as
    /// it does on the live query path. Mirrors [`Self::SnapshotBindings`].
    SnapshotMacros {
        reply: oneshot::Sender<Arc<std::collections::HashMap<(String, String), (PathBuf, u32)>>>,
    },
    /// Snapshot the registered service-provider paths in the deterministic merge
    /// order (`sorted_sp_files`): lexicographically ascending, which is what
    /// makes an equal-priority collision resolve to the smallest path in BOTH
    /// registries. Test-observability for the #255 Bug B guard — asserts the sort
    /// directly, so a reverted (raw-`HashMap`) `sorted_sp_files` fails reliably
    /// rather than by chance seed (#267).
    SnapshotSortedProviderPaths {
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
    /// One provider file's `(before, after)` registration contribution
    /// (macros / bindings / facade aliases), for the save path's registration
    /// diff (#255). `before` is the actor-kept BASELINE — the contribution as
    /// of the last save transaction — NOT the live inputs: the did_change
    /// debounce eagerly overwrites `salsa_sp_files` / `config_files` on every
    /// typing pause, so a snapshot of the live inputs taken at save time
    /// already holds the edited text and would diff empty. A path with no
    /// baseline yields the empty default (the first save of a session
    /// over-ripples that provider's keys once — the fail-safe direction).
    /// `fresh_text`, when given, marks a save transaction: the saved buffer is
    /// re-registered first (the App rescan a provider save queues is
    /// asynchronous), `after` reads the fresh registration, and the baseline
    /// advances to it. Without `fresh_text` this is a pure read — the
    /// baseline does not advance.
    FileProviderRegistrations {
        path: PathBuf,
        fresh_text: Option<String>,
        reply: oneshot::Sender<(ProviderRegistrationsData, ProviderRegistrationsData)>,
    },
    /// Snapshot the interface→implementors reverse map — `interface FQCN` →
    /// directly implementing class FQCNs — for the same out-of-actor build, so a
    /// contract-returning helper / method-return chain (`view()->make()->
    /// render()`) resolves to the concrete implementor while indexing exactly as
    /// it does on the live query path. Mirrors [`Self::SnapshotBindings`].
    SnapshotImplementers {
        reply: oneshot::Sender<Arc<std::collections::HashMap<String, Vec<String>>>>,
    },
    /// Snapshot every indexed class grouped by file, so warming can persist
    /// the hierarchy to the disk cache.
    SnapshotHierarchyNodes {
        reply: oneshot::Sender<
            std::collections::HashMap<PathBuf, Vec<Arc<crate::class_hierarchy_index::ClassNode>>>,
        >,
    },
    /// Surface signatures (`fqcn → u64`) for every class `path` declares.
    /// The save flow snapshots this *before* pushing the saved buffer into
    /// Salsa, then diffs against the re-parse to decide whether the edit
    /// could affect other files (incremental refresh, #80).
    FileClassSurfaces {
        path: PathBuf,
        reply: oneshot::Sender<std::collections::HashMap<String, u64>>,
    },
    /// Expand `seeds` to include every transitive descendant (subclasses,
    /// implementers, trait users) — the class-level blast radius of a
    /// surface change.
    ExpandClassDescendants {
        seeds: Vec<String>,
        reply: oneshot::Sender<std::collections::HashSet<String>>,
    },
    /// Export every magic-member entry grouped by usage file, for the
    /// incremental magic-cache re-save (#80).
    ExportMagicMembers {
        reply: oneshot::Sender<
            std::collections::HashMap<PathBuf, Vec<crate::symbol_index::MagicMemberEntry>>,
        >,
    },
    /// Bulk-import resolved magic-member occurrences into the symbol index
    /// (M4). Appends to each path's existing (literal-symbol) entries.
    BulkImportMagicMembers {
        entries: Vec<(PathBuf, Vec<crate::symbol_index::MagicMemberEntry>)>,
        reply: oneshot::Sender<usize>,
    },
    /// Re-index a single file's symbols after an edit (instant per-file half of
    /// the incremental refresh): drop the file's prior keys, re-insert its
    /// literal symbols from the current pattern cache, then insert the freshly
    /// resolved magic members. Keeps find-references on the edited file current
    /// without a project-wide rebuild.
    ReindexFileMagic {
        path: PathBuf,
        entries: Vec<crate::symbol_index::MagicMemberEntry>,
        reply: oneshot::Sender<()>,
    },
    /// find-references for the magic member under the cursor (M4): resolve the
    /// `member_access` site at `(line, column)` and return its indexed usages.
    FindMemberReferences {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Vec<ReferenceLocationData>>,
    },

    /// Resolve + classify the magic member at a cursor position for a hover
    /// card (M6). Returns the classification, not references.
    ResolveMagicMemberAt {
        path: PathBuf,
        line: u32,
        column: u32,
        /// The caller's already-cached per-root builder-method surface (real
        /// vendor signatures for `__call`-forwarded Eloquent/Query builder
        /// methods), so a builder-forwarded member like `orderByDesc` can get
        /// a real hover card instead of nothing. `None` when the caller
        /// couldn't build one (e.g. vendor/ absent).
        builder_index: Option<Arc<crate::laravel_introspector::BuilderMethodIndex>>,
        reply: oneshot::Sender<Option<MagicMemberHoverData>>,
    },

    /// Resolve a facade receiver token (`\Auth`) at a cursor to its bound
    /// concrete class location (`AuthManager` file + decl line).
    ResolveFacadeReceiverAt {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Option<FacadeReceiverTarget>>,
    },

    /// Resolve the magic member at a cursor for rename (M7) — method-backed
    /// kinds only; returns the declaring method to rewrite.
    ResolveMagicMemberRenameAt {
        path: PathBuf,
        line: u32,
        column: u32,
        reply: oneshot::Sender<Option<MagicMemberRenameData>>,
    },

    // === Service Provider Management ===
    /// Register the service provider registry from the existing analyzer
    RegisterServiceProviderRegistry {
        middleware_aliases: std::collections::HashMap<String, MiddlewareRegistrationData>,
        bindings: std::collections::HashMap<String, BindingRegistrationData>,
        singletons: std::collections::HashMap<String, BindingRegistrationData>,
        reply: oneshot::Sender<()>,
    },
    /// Get middleware by alias
    GetMiddlewareByAlias {
        alias: String,
        reply: oneshot::Sender<Option<MiddlewareRegistrationData>>,
    },
    /// Get binding by name
    GetBindingByName {
        name: String,
        reply: oneshot::Sender<Option<BindingRegistrationData>>,
    },
    /// Get view namespace by name (e.g., "courier" -> view path)
    GetViewNamespace {
        namespace: String,
        reply: oneshot::Sender<Option<ViewNamespaceData>>,
    },
    /// Get all view namespaces (for autocomplete)
    GetAllViewNamespaces {
        reply: oneshot::Sender<Vec<ViewNamespaceData>>,
    },
    /// Get a Blade component by tag name (e.g., "package-alert")
    GetBladeComponentReg {
        tag_name: String,
        reply: oneshot::Sender<Option<BladeComponentRegData>>,
    },
    /// Get all registered Blade components
    GetAllBladeComponentRegs {
        reply: oneshot::Sender<Vec<BladeComponentRegData>>,
    },
    /// Get component namespace by prefix (e.g., "nightshade")
    GetComponentNamespace {
        prefix: String,
        reply: oneshot::Sender<Option<ComponentNamespaceData>>,
    },
    /// Get all component namespaces
    GetAllComponentNamespaces {
        reply: oneshot::Sender<Vec<ComponentNamespaceData>>,
    },

    // === Salsa-based Environment Variable Management (New) ===
    /// Register a raw .env file for Salsa to parse
    RegisterEnvSource {
        path: PathBuf,
        text: String,
        priority: u8, // 0=.env.example, 1=.env.local, 2=.env
        reply: oneshot::Sender<()>,
    },
    /// Get a parsed env variable from Salsa
    GetParsedEnvVar {
        name: String,
        reply: oneshot::Sender<Option<ParsedEnvVarData>>,
    },
    /// Get all parsed env variables from Salsa
    GetAllParsedEnvVars {
        reply: oneshot::Sender<Vec<ParsedEnvVarData>>,
    },

    // === Salsa-based Translation Resolution (issue #293) ===
    /// Resolve one translation key in one locale through the Salsa cache
    ResolveTranslation {
        root: PathBuf,
        key: String,
        locale: String,
        /// Live `namespace -> lang dir` map for unpublished vendor
        /// translations. Passed per request rather than memoized: it never
        /// enters a query key, so it can neither defeat memoization of the
        /// common dotted path nor serve a stale namespaced resolution.
        vendor_map: Option<Arc<HashMap<String, PathBuf>>>,
        reply: oneshot::Sender<Option<ResolvedTranslationData>>,
    },
    /// Every locale that could define this key, APP_LOCALE first
    AvailableLocales {
        root: PathBuf,
        key: String,
        vendor_map: Option<Arc<HashMap<String, PathBuf>>>,
        reply: oneshot::Sender<Vec<String>>,
    },
    /// Push a lang file's authoritative text (editor buffer) into Salsa
    RegisterLangSource {
        path: PathBuf,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Drop a lang path's cached entry and its directory listing, so the next
    /// lookup re-reads disk. Covers external create, change and delete.
    InvalidateLangPath {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Drop a config path's cached text, so the next completion re-reads it.
    /// Covers external create, change and delete, and an in-editor edit.
    InvalidateConfigPath {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Locate a key's declaration inside one catalogue
    LocateTranslationKey {
        root: PathBuf,
        path: PathBuf,
        target: TranslationKeyTarget,
        reply: oneshot::Sender<Option<KeyLocationData>>,
    },
    /// Every translation key autocomplete should offer
    TranslationKeyCompletions {
        root: PathBuf,
        reply: oneshot::Sender<Vec<TranslationKeyCompletionData>>,
    },
    /// Every locale file declaring a translation key, for rename
    LocateKeyAcrossLocales {
        root: PathBuf,
        dotted_key: String,
        reply: oneshot::Sender<Vec<TranslationKeyLocationData>>,
    },
    /// The project's provider-registered translation namespace map
    VendorTranslationNamespaces {
        root: PathBuf,
        reply: oneshot::Sender<HashMap<String, PathBuf>>,
    },
    /// Drop everything derived from service providers
    InvalidateTranslationProviders { reply: oneshot::Sender<()> },

    /// Replace the host-registered extra translation-provider files (module
    /// providers from the `modules.paths` setting).
    SetTranslationProviderExtras {
        files: Vec<PathBuf>,
        reply: oneshot::Sender<()>,
    },
    /// Replace the configured module directories, in `modules.paths`
    /// glob-match order. The registration merge reads their order as the
    /// equal-priority tie-break rank.
    SetModuleDirs {
        dirs: Vec<PathBuf>,
        reply: oneshot::Sender<()>,
    },
    /// How many times the translation cache has touched disk
    LangDiskReads { reply: oneshot::Sender<usize> },

    // === Salsa-based Service Provider Management (New) ===
    /// Register a raw service provider file for Salsa to parse
    RegisterServiceProviderSource {
        path: PathBuf,
        text: String,
        priority: u8, // 0=framework, 1=package, 2=module, 3=app
        root_path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Get middleware from Salsa-parsed service providers
    GetParsedMiddleware {
        alias: String,
        reply: oneshot::Sender<Option<ParsedMiddlewareData>>,
    },
    /// Get all parsed middleware from Salsa
    GetAllParsedMiddleware {
        reply: oneshot::Sender<Vec<ParsedMiddlewareData>>,
    },
    /// Get binding from Salsa-parsed service providers
    GetParsedBinding {
        name: String,
        reply: oneshot::Sender<Option<ParsedBindingData>>,
    },
    /// Get all parsed bindings from Salsa
    GetAllParsedBindings {
        reply: oneshot::Sender<Vec<ParsedBindingData>>,
    },

    // === Cache-based Registration ===
    /// Register a middleware entry from disk cache
    RegisterCachedMiddleware {
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
        reply: oneshot::Sender<()>,
    },
    /// Register a binding entry from disk cache
    RegisterCachedBinding {
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
        reply: oneshot::Sender<()>,
    },

    /// Register multiple middleware entries from disk cache (batch)
    RegisterCachedMiddlewareBatch {
        entries: Vec<(String, String, Option<String>, Option<String>, u32)>, // (alias, class, class_file, source_file, line)
        reply: oneshot::Sender<()>,
    },
    /// Register multiple binding entries from disk cache (batch)
    RegisterCachedBindingBatch {
        entries: Vec<(String, String, String, Option<String>, Option<String>, u32)>, // (name, class, binding_type, class_file, source_file, line)
        reply: oneshot::Sender<()>,
    },

    /// Register Laravel config from disk cache (bypasses parsing).
    /// Boxed because `LaravelConfigData` is by far the largest payload of any
    /// `SalsaRequest` variant; keeping it inline bloats every message (see
    /// clippy::large_enum_variant).
    RegisterCachedConfig {
        config: Box<LaravelConfigData>,
        reply: oneshot::Sender<()>,
    },

    /// Shutdown the actor
    Shutdown,
}

/// Handle to communicate with the Salsa actor
#[derive(Clone)]
pub struct SalsaHandle {
    sender: mpsc::Sender<SalsaRequest>,
    /// Publication slot for the shared concurrent pattern cache. Reads and
    /// writes through it NEVER go through the actor's mpsc channel, which
    /// means they're never blocked behind a slow handler. See the comment on
    /// `SalsaActor::pattern_cache` for why that matters.
    ///
    /// Empty until the actor finishes its first project walk and publishes a
    /// table sized for the real file count (see
    /// `SalsaActor::size_and_publish_pattern_cache`). A `OnceLock` rather
    /// than a plain `Arc<DashMap>` handed out at `spawn()` because the
    /// capacity isn't knowable that early — and a `OnceLock` rather than a
    /// swappable `RwLock<Arc<DashMap>>` because a swap would silently split
    /// a long-lived caller (warming holds this `Arc` for minutes) from the
    /// table the actor is writing to. Published exactly once, so an `Arc`
    /// taken from here is the live table for the rest of the session.
    pattern_cache:
        Arc<std::sync::OnceLock<Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>>>,
    /// The server's open-document map (URI → (text, version)). `None` until
    /// `set_documents` publishes it — every existing `spawn()` call site
    /// (25+, mostly tests) still compiles unchanged; an unset map is
    /// treated as "no open buffers", i.e. the pre-fix behaviour. See
    /// `SalsaActor::documents` for the shared-allocation rationale and
    /// `bulk_import_patterns` for the one consumer that matters.
    documents: Arc<OnceLock<Arc<RwLock<HashMap<Url, (String, i32)>>>>>,
}

impl SalsaHandle {
    /// Borrow the shared pattern cache directly. The on-disk cache module
    /// uses this to pre-load entries before warming starts, and to read them
    /// back out after warming completes for persistence. The returned `Arc`
    /// is cheap to clone and points at the same `DashMap` instance the actor
    /// reads from in `handle_get_patterns` — for the rest of the session, not
    /// just for the moment of this call.
    ///
    /// `None` until the actor has walked a project and published the table
    /// (see `SalsaActor::size_and_publish_pattern_cache`). Callers that run
    /// before project registration — or in a test/tooling instance that never
    /// registers one — must handle that rather than assume a cache exists.
    pub fn pattern_cache(
        &self,
    ) -> Option<Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>> {
        self.pattern_cache.get().cloned()
    }

    /// Publish the server's open-document map into the salsa layer. Call
    /// once at startup, right after `SalsaActor::spawn()` — mirrors how
    /// `pattern_cache` above is a single shared `Arc` handed to both sides
    /// at construction. Kept as a post-construction setter (instead of a
    /// `spawn()` parameter) because `spawn()` has 25+ call sites, mostly
    /// tests, that don't have a documents map to hand it. A second call is
    /// a no-op — `OnceLock` refuses to overwrite — since only server
    /// startup should ever publish this.
    pub fn set_documents(&self, documents: Arc<RwLock<HashMap<Url, (String, i32)>>>) {
        if self.documents.set(documents).is_err() {
            debug!("SalsaHandle::set_documents called more than once; ignoring");
        }
    }

    /// Snapshot the paths of every currently open buffer as a `HashSet`,
    /// once, for O(1) membership checks. The sole consumer is
    /// `bulk_import_patterns`, which must not let a disk-parsed warm entry
    /// overwrite a path the user has open (and possibly edited) — see
    /// there. Called from async context (the warming task that calls
    /// `bulk_import_patterns` is itself a `tokio::spawn`ed future, not the
    /// actor thread), so a plain `.read().await` is correct here.
    async fn open_buffer_paths(&self) -> std::collections::HashSet<PathBuf> {
        match self.documents.get() {
            Some(documents) => documents
                .read()
                .await
                .keys()
                .filter_map(|uri| uri.to_file_path().ok())
                .collect(),
            None => std::collections::HashSet::new(),
        }
    }

    /// Update or create a file in the database
    pub async fn update_file(
        &self,
        path: PathBuf,
        version: i32,
        text: String,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateFile {
                path,
                version,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed patterns for a file
    /// Returns Arc for efficient sharing without cloning the entire data structure
    pub async fn get_patterns(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<ParsedPatternsData>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetPatterns {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed Blade loop blocks for a file.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_loop_blocks(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetLoopBlocks {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed `@php` block assignments for a Blade file.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_php_assignments(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<(String, String)>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetPhpAssignments {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get the document-symbol tree for a file. Powers textDocument/documentSymbol.
    /// Memoized — returns the same Arc on repeated calls until the file version changes.
    pub async fn get_document_symbols(
        &self,
        path: PathBuf,
    ) -> Result<Option<Arc<Vec<crate::document_symbols::SymbolEntry>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetDocumentSymbols {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve a `$this->X` member access in a Livewire component PHP file.
    /// Auto-registers the file as a Salsa input on first access, invalidates on mtime change.
    /// Read the actor database's query body-execution counters.
    pub async fn query_run_counts(&self) -> Result<QueryRunCountsData, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::QueryRunCounts { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Push a render-index snapshot into the actor. Invalidates the memoized
    /// backing-class queries, so send it only when the index has changed.
    pub async fn set_render_index(
        &self,
        generation: u64,
        entries: Vec<(String, PathBuf)>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SetRenderIndex {
                generation,
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve the PHP class(es) backing `blade_path`. See
    /// [`BladeBackingResolutionData`].
    pub async fn blade_backing_class_resolution(
        &self,
        blade_path: PathBuf,
        view_name: Option<String>,
        livewire_paths: Vec<PathBuf>,
        live_blade_text: Option<String>,
    ) -> Result<BladeBackingResolutionData, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BladeBackingClassResolution {
                blade_path,
                view_name,
                livewire_paths,
                live_blade_text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    pub async fn resolve_livewire_member(
        &self,
        path: PathBuf,
        member: String,
    ) -> Result<Option<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveLivewireMember {
                path,
                member,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Remove a file from the database
    pub async fn remove_file(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RemoveFile {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register an open editor buffer for `path`, the acquire half of
    /// [`SalsaHandle::release_external_php_ownership`]. Call it BEFORE the
    /// buffer's [`SalsaHandle::update_file`] push — see
    /// [`SalsaActor::acquire_external_php_ownership`] for what that ordering
    /// buys.
    pub async fn acquire_external_php_ownership(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::AcquireExternalPhpOwnership {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Release one open buffer's claim on `path`'s text. Once the last claim
    /// goes, the backing-class loader re-reads disk on its next resolution.
    /// Evicts nothing — see [`SalsaActor::release_external_php_ownership`] for
    /// why this is not [`SalsaHandle::remove_file`].
    pub async fn release_external_php_ownership(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ReleaseExternalPhpOwnership {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Build (or rebuild) the inverted symbol index from the current
    /// pattern cache. Called by the warming task once warming finishes
    /// so the first `find-references` query is O(1) rather than
    /// O(N files). Returns the total entry count for logging.
    pub async fn build_symbol_index(&self) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BuildSymbolIndex { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Wipe every in-memory reference index in one actor turn, for the
    /// `laravel.reindexProject` cold rebuild. Clears the pattern cache, the
    /// inverted symbol index, the class-hierarchy index, the per-file LRU
    /// caches, and the class-files snapshot. Doing it in a single message
    /// keeps the wipe atomic against concurrent actor reads, and emptying the
    /// pattern cache is what forces the following warming pass to re-parse
    /// every file instead of reusing a stale cached entry. The categorized
    /// file lists are left alone — `register_project_files` re-walks and
    /// replaces them next.
    pub async fn clear_reindex_state(&self) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ClearReindexState { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Update the per-category project file lists in response to a
    /// filesystem event. `Add` is for `Created` notifications, `Remove`
    /// for `Deleted`. Returns the category label the actor classified
    /// the path under (useful for logging), or `None` if the path
    /// didn't match any indexed project root.
    pub async fn update_project_file_list(
        &self,
        path: PathBuf,
        op: FileListOp,
    ) -> Result<Option<&'static str>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateProjectFileList {
                path,
                op,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Shutdown the actor gracefully
    pub async fn shutdown(&self) -> Result<(), &'static str> {
        self.sender
            .send(SalsaRequest::Shutdown)
            .await
            .map_err(|_| "Salsa actor already disconnected")
    }

    // === Config Methods ===

    /// Register configuration files for the project
    pub async fn register_config_files(
        &self,
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterConfigFiles {
                root_path,
                composer_json,
                view_config,
                livewire_config,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Update a specific configuration file
    pub async fn update_config_file(
        &self,
        path: PathBuf,
        text: String,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateConfigFile {
                path,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get the current Laravel configuration
    pub async fn get_laravel_config(&self) -> Result<Option<LaravelConfigData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetLaravelConfig { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Reference Finding Methods ===

    /// Register project files for reference finding
    /// Scans the provided directories and registers all PHP/Blade files with Salsa
    pub async fn register_project_files(
        &self,
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
        vendor_files: Vec<PathBuf>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterProjectFiles {
                root_path,
                controller_paths,
                view_paths,
                livewire_path,
                routes_path,
                vendor_files,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Find all references to a specific view across the project
    /// Returns cached results when possible, only scanning changed files
    pub async fn find_view_references(
        &self,
        view_name: String,
    ) -> Result<Vec<ViewReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindViewReferences {
                view_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Every Blade template that renders one of `component_names` (as
    /// `<x-…>`) or `livewire_names` (as `<livewire:…>`), sorted and deduped.
    /// Backs the item-1 up-walk from a partial to the component that rendered
    /// it (#339).
    pub async fn files_rendering_component(
        &self,
        component_names: Vec<String>,
        livewire_names: Vec<String>,
    ) -> Result<Vec<PathBuf>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FilesRenderingComponent {
                component_names,
                livewire_names,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Find all parser-classified references to a symbol across the project.
    pub async fn find_references(
        &self,
        symbol: SymbolRefData,
        include_declaration: bool,
    ) -> Result<Vec<ReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindReferences {
                symbol,
                include_declaration,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Count references for `symbol` directly from the inverted index (cheap;
    /// no project walk). Backs code-lens `resolve`.
    pub async fn count_symbol_references(
        &self,
        symbol: SymbolRefData,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::CountSymbolReferences {
                symbol,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Return every project file path the actor currently has registered.
    pub async fn list_project_files(&self) -> Result<Vec<PathBuf>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ListProjectFiles { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Bulk-import pre-parsed patterns into the shared pattern cache.
    ///
    /// **Does NOT go through the actor mpsc channel.** Earlier revisions
    /// routed this through the actor and we observed a 65-second stall
    /// per cold start on a 40k-file project — the actor's `blocking_recv`
    /// thread was not waking up when the warming task sent its message,
    /// and only un-stalled when an unrelated `did_open` arrived. We never
    /// fully pinned down the wake-up failure, but the architectural fix
    /// is correct regardless: pattern_cache writes are pure data ops and
    /// have no Salsa-mutable-db requirements, so they shouldn't be
    /// serialized through the actor's single-threaded request queue.
    ///
    /// This is now a tight synchronous loop of `DashMap::insert` calls.
    /// Real-world cost: ~7ms for 40,589 entries (per earlier bench).
    /// The `async fn` and `Result` shape is preserved for source
    /// compatibility with the existing call sites.
    ///
    /// Errors if the actor hasn't published the cache yet. Every caller is on
    /// the warming path, which only runs after project registration, so this
    /// should be unreachable — but dropping the batch loudly beats inserting
    /// it into a table nothing else can see.
    ///
    /// **Open buffers are skipped.** Entries here are parsed from DISK, so
    /// unconditionally inserting would silently overwrite the pattern-cache
    /// entry for a file the user has open with unsaved edits — goto/hover
    /// then serve stale positions in exactly the files being worked on,
    /// until the next keystroke re-parses it. The open path is read from
    /// `documents` (published via `set_documents`) into a `HashSet` ONCE,
    /// not per-entry, to stay inside the ~7ms budget above. A skipped path
    /// simply keeps whatever's already in the cache (or nothing, forcing a
    /// lazy re-parse from the live Salsa buffer on the next `get_patterns`
    /// call) — its own `did_open`/`did_change` path is what keeps its entry
    /// current.
    pub async fn bulk_import_patterns(
        &self,
        entries: Vec<(PathBuf, Arc<ParsedPatternsData>)>,
    ) -> Result<usize, &'static str> {
        let cache = self
            .pattern_cache
            .get()
            .ok_or("pattern cache not published — no project registered yet")?;
        let total = entries.len();
        let open_paths = self.open_buffer_paths().await;
        for (path, data) in entries {
            if open_paths.contains(&path) {
                continue;
            }
            cache.insert(path, (0, data));
        }
        Ok(total)
    }

    /// Bulk-import class-hierarchy nodes into the actor-owned index. Unlike
    /// `bulk_import_patterns` (which writes the shared cache directly), the
    /// hierarchy index lives inside the actor, so this round-trips through
    /// the request queue. Replies with the total class count after import.
    pub async fn bulk_import_hierarchy(
        &self,
        entries: Vec<(PathBuf, Vec<crate::class_hierarchy_index::ClassNode>)>,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BulkImportHierarchy {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the actor's `fqcn → declaring file` map. The magic-member
    /// index build (M4) runs in a parallel pass outside the actor and uses
    /// this owned copy to resolve receivers without borrowing the index.
    pub async fn snapshot_class_files(
        &self,
    ) -> Result<Arc<std::collections::HashMap<String, PathBuf>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotClassFiles { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the container-binding registry as a `binding key → concrete
    /// FQCN` map for the out-of-actor magic-member build. Mirrors
    /// [`Self::snapshot_class_files`]: the build pass resolves `app('key')`
    /// receivers against this owned copy without borrowing the actor.
    pub async fn snapshot_bindings(
        &self,
    ) -> Result<Arc<std::collections::HashMap<String, String>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotBindings { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the facade alias map — token → facade FQCN — for the facade
    /// receiver resolver. Mirrors [`Self::snapshot_bindings`]: the map merges
    /// the built-in seed with any user aliases from `config/app.php`'s
    /// `aliases` and `bootstrap/app.php`'s `withAliases`, user sources winning.
    pub async fn snapshot_facade_aliases(
        &self,
    ) -> Result<Arc<std::collections::HashMap<String, String>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotFacadeAliases { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the macro registry — `(receiver_fqcn, macro_name)` →
    /// `(decl_file, decl_line)` — for the out-of-actor magic-member build.
    /// Mirrors [`Self::snapshot_bindings`].
    pub async fn snapshot_macros(
        &self,
    ) -> Result<Arc<std::collections::HashMap<(String, String), (PathBuf, u32)>>, &'static str>
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotMacros { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the registered service-provider paths in `sorted_sp_files` merge
    /// order (lexicographically ascending). Test-observability for the #255 Bug B
    /// guard — the deterministic sort both registries merge in.
    pub async fn snapshot_sorted_provider_paths(&self) -> Result<Vec<PathBuf>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotSortedProviderPaths { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot the interface→implementors reverse map — `interface FQCN` →
    /// directly implementing class FQCNs — for the out-of-actor magic-member
    /// build. Mirrors [`Self::snapshot_bindings`].
    pub async fn snapshot_implementers(
        &self,
    ) -> Result<Arc<std::collections::HashMap<String, Vec<String>>>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotImplementers { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Snapshot every indexed class grouped by declaring file, so warming can
    /// persist the hierarchy to the disk cache (it survives a warm restart
    /// only if persisted — fresh parses are the sole other populator).
    /// Surface signatures for every class `path` currently declares (empty
    /// if the file is unknown to the hierarchy). Snapshot side of the
    /// save-time surface diff (#80).
    pub async fn file_class_surfaces(
        &self,
        path: PathBuf,
    ) -> Result<std::collections::HashMap<String, u64>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FileClassSurfaces {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// One provider file's registration contribution (macros / bindings /
    /// facade aliases), as `(before, after)` for the save path's registration
    /// diff (#255). `before` is the actor-kept baseline (last save
    /// transaction), insulated from the did_change debounce's eager input
    /// overwrite; `after` reads the current registration. Pass `fresh_text`
    /// on the save call: it re-registers the saved buffer first and advances
    /// the baseline. See [`SalsaRequest::FileProviderRegistrations`].
    pub async fn file_provider_registrations(
        &self,
        path: PathBuf,
        fresh_text: Option<String>,
    ) -> Result<(ProviderRegistrationsData, ProviderRegistrationsData), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FileProviderRegistrations {
                path,
                fresh_text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// `seeds` plus every transitive descendant — the class-level blast
    /// radius of a surface change (#80).
    pub async fn expand_class_descendants(
        &self,
        seeds: Vec<String>,
    ) -> Result<std::collections::HashSet<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ExpandClassDescendants {
                seeds,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Export every magic-member entry grouped by usage file — the live
    /// index contents, for the incremental magic-cache re-save (#80).
    pub async fn export_magic_members(
        &self,
    ) -> Result<
        std::collections::HashMap<PathBuf, Vec<crate::symbol_index::MagicMemberEntry>>,
        &'static str,
    > {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ExportMagicMembers { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    pub async fn snapshot_hierarchy_nodes(
        &self,
    ) -> Result<
        std::collections::HashMap<PathBuf, Vec<Arc<crate::class_hierarchy_index::ClassNode>>>,
        &'static str,
    > {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SnapshotHierarchyNodes { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Bulk-import resolved magic-member occurrences into the actor-owned
    /// symbol index (M4). Mirrors `bulk_import_hierarchy`; replies with the
    /// total magic-member entry count ingested.
    pub async fn bulk_import_magic_members(
        &self,
        entries: Vec<(PathBuf, Vec<crate::symbol_index::MagicMemberEntry>)>,
    ) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::BulkImportMagicMembers {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Re-index a single edited file's symbols (instant per-file refresh):
    /// evict its prior keys, re-add literals from the pattern cache, then insert
    /// the freshly resolved magic members.
    pub async fn reindex_file_magic(
        &self,
        path: PathBuf,
        entries: Vec<crate::symbol_index::MagicMemberEntry>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ReindexFileMagic {
                path,
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// find-references for the magic member under the cursor (M4). The actor
    /// resolves the `member_access` site at `(line, column)` to its declaring
    /// class + member, then returns every indexed usage of that key. Empty
    /// when the cursor isn't on a resolvable magic member.
    pub async fn find_member_references(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Vec<ReferenceLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::FindMemberReferences {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve + classify the magic member at `(line, column)` for a hover card
    /// (M6). `Ok(None)` when the position isn't a resolvable magic member.
    ///
    /// `builder_index`: the caller's cached per-root builder-method surface
    /// (see `Backend::get_builder_method_index`), passed through so a
    /// `__call`-forwarded Eloquent/Query builder method (`orderByDesc`, …)
    /// can render a real card. `None` when the caller has none available or
    /// doesn't want the fallback (goto/rename/references never do).
    pub async fn resolve_magic_member_at(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
        builder_index: Option<Arc<crate::laravel_introspector::BuilderMethodIndex>>,
    ) -> Result<Option<MagicMemberHoverData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveMagicMemberAt {
                path,
                line,
                column,
                builder_index,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve a facade receiver token at `(line, column)` to its bound concrete
    /// class location (`AuthManager` file + 0-based decl line), or `None` when
    /// the cursor isn't on a resolvable facade receiver.
    pub async fn resolve_facade_receiver_at(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Option<FacadeReceiverTarget>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveFacadeReceiverAt {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Resolve the magic member at `(line, column)` for rename (M7). `Ok(None)`
    /// unless it's a method-backed magic member (relationship / scope /
    /// accessor / dynamic finder).
    pub async fn resolve_magic_member_rename_at(
        &self,
        path: PathBuf,
        line: u32,
        column: u32,
    ) -> Result<Option<MagicMemberRenameData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveMagicMemberRenameAt {
                path,
                line,
                column,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Service Provider Methods ===

    /// Register the service provider registry from the existing analyzer
    pub async fn register_service_provider_registry(
        &self,
        middleware_aliases: std::collections::HashMap<String, MiddlewareRegistrationData>,
        bindings: std::collections::HashMap<String, BindingRegistrationData>,
        singletons: std::collections::HashMap<String, BindingRegistrationData>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterServiceProviderRegistry {
                middleware_aliases,
                bindings,
                singletons,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get middleware by alias
    pub async fn get_middleware_by_alias(
        &self,
        alias: String,
    ) -> Result<Option<MiddlewareRegistrationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetMiddlewareByAlias {
                alias,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get binding by name
    pub async fn get_binding_by_name(
        &self,
        name: String,
    ) -> Result<Option<BindingRegistrationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetBindingByName {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get view namespace by name (for resolving package::view syntax)
    pub async fn get_view_namespace(
        &self,
        namespace: String,
    ) -> Result<Option<ViewNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetViewNamespace {
                namespace,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all view namespaces (for autocomplete)
    pub async fn get_all_view_namespaces(&self) -> Result<Vec<ViewNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllViewNamespaces { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get a Blade component registration by tag name
    pub async fn get_blade_component_reg(
        &self,
        tag_name: String,
    ) -> Result<Option<BladeComponentRegData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetBladeComponentReg {
                tag_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all registered Blade components
    pub async fn get_all_blade_component_regs(
        &self,
    ) -> Result<Vec<BladeComponentRegData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllBladeComponentRegs { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get component namespace by prefix (for resolving <x-package::component>)
    pub async fn get_component_namespace(
        &self,
        prefix: String,
    ) -> Result<Option<ComponentNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetComponentNamespace {
                prefix,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all component namespaces
    pub async fn get_all_component_namespaces(
        &self,
    ) -> Result<Vec<ComponentNamespaceData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllComponentNamespaces { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Salsa-based Environment Variable Methods (New - Phase 1) ===

    /// Register a raw .env file for Salsa to parse
    /// This replaces the old EnvFileCache by having Salsa do the parsing
    pub async fn register_env_source(
        &self,
        path: PathBuf,
        text: String,
        priority: u8,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterEnvSource {
                path,
                text,
                priority,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get a parsed env variable from Salsa
    /// Returns the highest-priority variable if multiple files define the same var
    pub async fn get_parsed_env_var(
        &self,
        name: String,
    ) -> Result<Option<ParsedEnvVarData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedEnvVar {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed env variables from Salsa (merged by priority)
    pub async fn get_all_parsed_env_vars(&self) -> Result<Vec<ParsedEnvVarData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedEnvVars { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Salsa-based Translation Methods (issue #293) ===

    /// Resolve a translation key in one locale through the Salsa cache.
    ///
    /// `vendor_map` is the live `namespace -> lang dir` map from
    /// `vendor_translations`; pass `None` when the project registers no
    /// unpublished package translations.
    pub async fn resolve_translation(
        &self,
        root: PathBuf,
        key: String,
        locale: String,
        vendor_map: Option<Arc<HashMap<String, PathBuf>>>,
    ) -> Result<Option<ResolvedTranslationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::ResolveTranslation {
                root,
                key,
                locale,
                vendor_map,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Every locale that could define `key`, APP_LOCALE first. Never empty —
    /// falls back to `["en"]` on a project that defines no translations.
    pub async fn available_locales(
        &self,
        root: PathBuf,
        key: String,
        vendor_map: Option<Arc<HashMap<String, PathBuf>>>,
    ) -> Result<Vec<String>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::AvailableLocales {
                root,
                key,
                vendor_map,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Push a lang file's authoritative text (an editor buffer) into Salsa, so
    /// an unsaved edit is reflected by the next resolution.
    pub async fn register_lang_source(
        &self,
        path: PathBuf,
        text: String,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterLangSource {
                path,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Locate a translation key's declaration inside one catalogue.
    pub async fn locate_translation_key(
        &self,
        root: PathBuf,
        path: PathBuf,
        target: TranslationKeyTarget,
    ) -> Result<Option<KeyLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::LocateTranslationKey {
                root,
                path,
                target,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Every translation key autocomplete should offer for this project.
    pub async fn translation_key_completions(
        &self,
        root: PathBuf,
    ) -> Result<Vec<TranslationKeyCompletionData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::TranslationKeyCompletions {
                root,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Every locale file declaring `dotted_key`, for building a rename edit.
    pub async fn locate_key_across_locales(
        &self,
        root: PathBuf,
        dotted_key: String,
    ) -> Result<Vec<TranslationKeyLocationData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::LocateKeyAcrossLocales {
                root,
                dotted_key,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// The project's provider-registered `namespace -> lang dir` map.
    pub async fn vendor_translation_namespaces(
        &self,
        root: PathBuf,
    ) -> Result<HashMap<String, PathBuf>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::VendorTranslationNamespaces {
                root,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register module service-provider files (from the `modules.paths`
    /// setting) as first-party translation-namespace providers.
    pub async fn set_translation_provider_extras(
        &self,
        files: Vec<PathBuf>,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SetTranslationProviderExtras {
                files,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Hand the actor the configured module directories in `modules.paths`
    /// order, so the registration merge can break an equal-priority tie by
    /// module rank instead of by provider path.
    pub async fn set_module_dirs(&self, dirs: Vec<PathBuf>) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::SetModuleDirs {
                dirs,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Drop everything derived from service providers after one changed.
    pub async fn invalidate_translation_providers(&self) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::InvalidateTranslationProviders { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// How many times the translation cache has touched disk this session.
    ///
    /// Instrumentation for the cache-warmth tests: compare the count before and
    /// after a second resolution of the same key — an equal count is the proof
    /// that Salsa served it.
    pub async fn lang_disk_reads(&self) -> Result<usize, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::LangDiskReads { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Drop a lang path's cached entry after an external create, change or
    /// delete, so the next resolution re-reads disk.
    pub async fn invalidate_lang_path(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::InvalidateLangPath {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Drop a config path's cached text after a create, change or delete, so
    /// the next completion re-reads it.
    ///
    /// The counterpart to the cache added in #349: completion resolves the
    /// preview locale from `config/app.php` once per session, which without
    /// this call would keep answering with the pre-edit locale.
    pub async fn invalidate_config_path(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::InvalidateConfigPath {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Salsa-based Service Provider Methods (New - Phase 1) ===

    /// Register a raw service provider file for Salsa to parse
    /// This replaces the old ServiceProviderRegistry by having Salsa do the parsing
    pub async fn register_service_provider_source(
        &self,
        path: PathBuf,
        text: String,
        priority: u8,
        root_path: PathBuf,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterServiceProviderSource {
                path,
                text,
                priority,
                root_path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get middleware by alias from Salsa-parsed service providers
    /// Returns the highest-priority middleware if multiple providers define the same alias
    pub async fn get_parsed_middleware(
        &self,
        alias: String,
    ) -> Result<Option<ParsedMiddlewareData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedMiddleware {
                alias,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed middleware from Salsa (merged by priority)
    pub async fn get_all_parsed_middleware(
        &self,
    ) -> Result<Vec<ParsedMiddlewareData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedMiddleware { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get binding by name from Salsa-parsed service providers
    /// Returns the highest-priority binding if multiple providers define the same name
    pub async fn get_parsed_binding(
        &self,
        name: String,
    ) -> Result<Option<ParsedBindingData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetParsedBinding {
                name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get all parsed bindings from Salsa (merged by priority)
    pub async fn get_all_parsed_bindings(&self) -> Result<Vec<ParsedBindingData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetAllParsedBindings { reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    // === Cache-based Registration Methods ===

    /// Register a middleware entry from disk cache
    pub async fn register_cached_middleware(
        &self,
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedMiddleware {
                alias,
                class,
                class_file,
                source_file,
                line,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register a binding entry from disk cache
    pub async fn register_cached_binding(
        &self,
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedBinding {
                name,
                class,
                binding_type,
                class_file,
                source_file,
                line,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register multiple middleware entries from disk cache (batch - single round-trip)
    pub async fn register_cached_middleware_batch(
        &self,
        entries: Vec<(String, String, Option<String>, Option<String>, u32)>, // (alias, class, class_file, source_file, line)
    ) -> Result<(), &'static str> {
        if entries.is_empty() {
            return Ok(());
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedMiddlewareBatch {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register multiple binding entries from disk cache (batch - single round-trip)
    pub async fn register_cached_binding_batch(
        &self,
        entries: Vec<(String, String, String, Option<String>, Option<String>, u32)>, // (name, class, binding_type, class_file, source_file, line)
    ) -> Result<(), &'static str> {
        if entries.is_empty() {
            return Ok(());
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedBindingBatch {
                entries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Register Laravel config from disk cache (bypasses parsing)
    pub async fn register_cached_config(
        &self,
        config: LaravelConfigData,
    ) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RegisterCachedConfig {
                config: Box::new(config),
                reply: reply_tx,
            })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx
            .await
            .map_err(|_| "Salsa actor dropped reply channel")
    }
}

/// Absolute root directories captured at `register_project_files` time.
/// Used by the file-watcher handler to classify newly-created paths
/// into the right per-category list without re-doing the directory
/// walk. Each `*_roots` field is a list of absolute paths because some
/// project layouts have multiple roots (e.g. multi-themed view paths).
///
/// Stored on `SalsaActor` (not passed in per request) because watcher
/// notifications arrive asynchronously and need consistent state to
/// classify against.
#[derive(Default, Debug, Clone)]
struct ProjectRootPaths {
    controller_roots: Vec<PathBuf>,
    view_roots: Vec<PathBuf>,
    livewire_root: Option<PathBuf>,
    routes_root: Option<PathBuf>,
    vendor_root: Option<PathBuf>,
}

impl ProjectRootPaths {
    /// Classify an absolute path into the file-category list it
    /// belongs to. Order matters here: vendor wins over views even
    /// though a published `vendor/<pkg>/resources/views/foo.blade.php`
    /// technically lives under both `vendor/` AND a view path; we
    /// treat it as vendor because that's where its source-of-truth
    /// content lives. Returns `None` for paths outside every known
    /// root (build artifacts, .git, dotfiles, etc.).
    fn classify(&self, path: &Path) -> Option<FileCategory> {
        if let Some(root) = &self.vendor_root {
            if path.starts_with(root) {
                return Some(FileCategory::Vendor);
            }
        }
        if let Some(root) = &self.livewire_root {
            if path.starts_with(root) {
                return Some(FileCategory::Livewire);
            }
        }
        for root in &self.controller_roots {
            if path.starts_with(root) {
                return Some(FileCategory::Controller);
            }
        }
        if let Some(root) = &self.routes_root {
            if path.starts_with(root) {
                return Some(FileCategory::Route);
            }
        }
        for root in &self.view_roots {
            if path.starts_with(root) {
                return Some(FileCategory::View);
            }
        }
        None
    }
}

/// Discriminant returned by `ProjectRootPaths::classify` so the
/// watcher-update path can pick the right `Vec<PathBuf>` to mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileCategory {
    Controller,
    View,
    Livewire,
    Route,
    Vendor,
}

impl FileCategory {
    /// Short label for logging — keeps log lines readable without
    /// pulling in a full Debug derive at the call site.
    fn label(self) -> &'static str {
        match self {
            FileCategory::Controller => "controller",
            FileCategory::View => "view",
            FileCategory::Livewire => "livewire",
            FileCategory::Route => "route",
            FileCategory::Vendor => "vendor",
        }
    }
}

/// Operation for `SalsaRequest::UpdateProjectFileList`. `Add` is sent
/// on a `Created` filesystem event; `Remove` on `Deleted`. There's no
/// "Change" variant because a change to an already-listed file
/// doesn't affect the list — only its contents change, and those flow
/// through `update_file` separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileListOp {
    Add,
    Remove,
}

/// Where the text currently installed in `files[path]` came from, and
/// therefore who is responsible for replacing it.
///
/// The backing-class loader ([`SalsaActor::ensure_external_php_source_loaded`])
/// reads a file from disk whenever a Blade template resolves `$this->member`,
/// which is a request-path read of a file somebody may be editing. The rule
/// this enum encodes is the narrowest one that keeps that safe: **the loader
/// only ever overwrites text it loaded itself.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalPhpText {
    /// The loader read it from disk, at this mtime. It is the owner, so it
    /// re-reads once the mtime advances.
    LoadedFromDisk(std::time::SystemTime),
    /// A client push installed it — `textDocument/didOpen`/`didChange` with an
    /// editor buffer, or `workspace/didChangeWatchedFiles` with the watcher's
    /// own fresh read. Both push again when their source changes, so the
    /// pusher owns invalidation for this path and the loader must not read
    /// disk over it: a live buffer would lose the user's unsaved edits, and a
    /// watched file is already being kept current by the watcher.
    ///
    /// The promise to push again lasts exactly as long as the pusher does.
    /// The watcher never goes away, so a watched file holds this state for the
    /// session; an editor buffer does, so `textDocument/didClose` hands the
    /// path back via [`SalsaActor::release_external_php_ownership`] once the
    /// path's last open buffer goes. Closing a buffer whose edits were
    /// DISCARDED writes nothing to disk and fires no watcher event, so without
    /// that release this state would outlive every source able to correct it.
    PushedByClient,
}

/// The Salsa actor that owns the database and runs on a dedicated thread
pub struct SalsaActor {
    db: LaravelDatabase,
    receiver: mpsc::Receiver<SalsaRequest>,
    /// Map from path to SourceFile for efficient lookups and updates
    files: HashMap<PathBuf, SourceFile>,
    /// Concurrent map of converted pattern data, SHARED with the SalsaHandle.
    /// Key: file path, Value: (file version, cached patterns wrapped in Arc).
    ///
    /// **Architectural note:** this is intentionally NOT routed through the
    /// actor's mpsc channel. The previous LRU-inside-actor design had a real
    /// production-pathological behaviour: warming would send a single
    /// `BulkImportPatterns` message and the actor's `blocking_recv()` thread
    /// would not get woken up until some unrelated LSP request (typically a
    /// `did_open`) arrived. On a 40k-file project the result was a 65-second
    /// stall every cold start. We never fully tracked down the wake-up
    /// failure (looked like a tokio mpsc + blocking_recv interaction), but
    /// bypassing the actor for cache writes side-steps the problem entirely
    /// AND yields a more correct architecture: pattern_cache reads/writes
    /// are pure data ops with no Salsa-mutable-db requirements, so there's
    /// no reason they should serialize through the actor.
    ///
    /// DashMap is lock-free for reads and uses per-shard locks for writes
    /// (`(available_parallelism * 4).next_power_of_two()` shards by default —
    /// 64 on a typical 10-to-16-core dev machine), so contention between the
    /// actor's read path and warming's bulk insert is negligible. There is no
    /// cap: it holds exactly one entry per indexed file (app + vendor), so its
    /// size is whatever the project's file count makes it — on a large project
    /// that's tens of thousands of entries, several hundred MB total. Wins
    /// like dropping vendor `member_access_refs` at parse time (see
    /// `pattern_indexer`'s vendor gate) shrink the PER-ENTRY cost; nothing
    /// here bounds the ENTRY COUNT.
    ///
    /// Replaced exactly once, by `size_and_publish_pattern_cache`, with a
    /// table pre-sized for the project's real file count — dashmap 6.x has no
    /// `reserve`, so growing the table means building a new one. That happens
    /// before the table is ever published to a `SalsaHandle`, so no other
    /// holder can be left pointing at the discarded one.
    pattern_cache: Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    /// The slot every `SalsaHandle` reads `pattern_cache` out of. Filled by
    /// `size_and_publish_pattern_cache` once the first project walk knows how
    /// big the table should be; see `SalsaHandle::pattern_cache`.
    pattern_cache_slot:
        Arc<std::sync::OnceLock<Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>>>,
    /// The server's open-document map (URI → (text, version)), published
    /// once via `SalsaHandle::set_documents` after `spawn()` returns. SAME
    /// `Arc<OnceLock<_>>` allocation as `SalsaHandle::documents` — see there
    /// for why this exists and why it's a `OnceLock` rather than a
    /// constructor parameter. Read with `blocking_read()` in
    /// `BulkImportPatterns`: this struct lives on the actor's dedicated
    /// `std::thread::spawn` thread (`run`, below), never polled by the
    /// tokio runtime, so a blocking read is the correct (and only
    /// available) primitive here.
    documents: Arc<OnceLock<Arc<RwLock<HashMap<Url, (String, i32)>>>>>,
    /// LRU cache of parsed Blade loop blocks, keyed by file path + text
    /// revision ([`SalsaActor::text_revision`]). Salsa already memoizes the
    /// underlying query, but caching the Arc avoids re-walking the query graph
    /// on every diagnostic / completion request.
    loop_blocks_cache: LruCache<PathBuf, (u64, Arc<Vec<crate::blade_loops::BladeLoopBlock>>)>,
    /// LRU cache of parsed `@php ... @endphp` block assignments, keyed by file
    /// path + text revision.
    php_assignments_cache: LruCache<PathBuf, (u64, Arc<Vec<(String, String)>>)>,
    /// LRU cache of document-symbol trees keyed by file path. Stores the text
    /// revision alongside the cached Arc so a mismatch triggers a recompute
    /// via the memoized Salsa query.
    document_symbols_cache:
        LruCache<PathBuf, (u64, Arc<Vec<crate::document_symbols::SymbolEntry>>)>,
    /// Records where the text currently installed in `files[path]` came from,
    /// for external PHP files registered as Salsa inputs — Livewire component
    /// classes, and the backing classes of a Blade template (#339, item 7).
    ///
    /// [`SalsaActor::ensure_external_php_source_loaded`] reads this to decide
    /// whether a disk re-read is due. An absent entry means "this actor never
    /// installed text for that path", which is the only state that permits an
    /// unconditional read.
    external_php_text: HashMap<PathBuf, ExternalPhpText>,
    /// How many open editor buffers currently claim each path. Incremented by
    /// `textDocument/didOpen` and decremented by `didClose`; the
    /// `PushedByClient` stamp above is handed back only when the count reaches
    /// zero. Absent means zero.
    ///
    /// Counting — rather than one flag per path — is what makes the release
    /// safe against out-of-order handlers. tower-lsp drives up to four
    /// notification handlers concurrently, so the `didOpen` of a REOPENED
    /// buffer can reach this actor before the `didClose` that preceded it at
    /// the client. Increments and decrements commute, so the pair settles at
    /// one whichever order it arrives in, and the live buffer keeps its
    /// ownership; a flag would read "closed" and hand a live buffer back to
    /// the loader, which then overwrites the shared `SourceFile` text every
    /// other query reads.
    ///
    /// Kept by the open/close edges alone: `RemoveFile` deliberately does not
    /// clear it, because deleting a path on disk does not close its buffer.
    external_php_open_buffers: HashMap<PathBuf, u32>,
    /// Monotonic version counter for external PHP SourceFiles (incremented per disk re-read).
    /// Monotonic stamp for the text currently installed in `files[path]`,
    /// bumped by EVERY writer, and the key the three per-file LRU caches
    /// below compare on.
    ///
    /// `SourceFile::version` cannot serve that purpose: `handle_update_file`
    /// stamps the LSP document version onto it while the backing-class loader
    /// stamped a counter of its own, so two independent sequences — both
    /// starting near zero, both climbing — wrote one field. Where they
    /// collided, a cache hit returned blocks parsed from the other writer's
    /// text: same number, different source. One counter, one namespace, no
    /// collision possible.
    text_revision: u64,
    /// Per-file view of [`Self::text_revision`]. Absent means "never written
    /// through a bumping site", which reads as revision 0.
    file_text_revisions: HashMap<PathBuf, u64>,

    // === Config Management ===
    /// Project root path
    config_root: Option<PathBuf>,
    /// Configuration files tracked by Salsa
    config_files: HashMap<PathBuf, ConfigFile>,
    /// Config file version counter (incremented on changes)
    config_version: i32,
    /// Cached Laravel config data (version, data)
    config_cache: Option<(i32, LaravelConfigData)>,
    /// Per-path registration BASELINE for the save path's diff (#255): the
    /// contribution as of the last save transaction
    /// ([`SalsaRequest::FileProviderRegistrations`] with `fresh_text`).
    /// Deliberately NOT updated by the did_change debounce's eager
    /// re-registration — that insulation is what keeps the pre-save side of
    /// the diff pre-edit. Missing entry = empty default (first save
    /// over-ripples once; fail-safe).
    registration_baselines: HashMap<PathBuf, ProviderRegistrationsData>,

    // === Reference Finding ===
    /// The render index as a Salsa input, created on first use (#339, item 7).
    render_index: Option<RenderIndex>,
    /// Monotonic version for the render-index input.
    render_index_version: i32,
    /// The `ViewVarIndex` generation the installed render index was built from.
    /// Guards `handle_set_render_index` against an out-of-order push.
    render_index_generation: u64,
    /// Project files input for reference finding
    project_files: Option<ProjectFiles>,
    /// Version counter for project files
    project_files_version: i32,
    /// Categorized file lists for quick lookup
    controller_files: Vec<PathBuf>,
    view_files: Vec<PathBuf>,
    livewire_files: Vec<PathBuf>,
    route_files: Vec<PathBuf>,
    /// Vendor `*.php` and `*.blade.php` files. Composer packages can ship
    /// Livewire components, routes, controllers, views, and translations
    /// just like user code — find-references and goto-definition both
    /// need to see them. We index everything under `vendor/` and rely on
    /// the warming-stage filters (`.json.php` skip, 256KB size cap) to
    /// drop the auto-generated noise.
    vendor_files: Vec<PathBuf>,
    /// Every non-vendor `*.php` / `*.blade.php` in the project (app/, database/,
    /// tests/, config/, resources/, routes/, …). The categorized lists above
    /// only cover controllers + Blade views + routes, which is enough for the
    /// view/route/livewire navigation features but misses the broad source the
    /// magic-member reverse index needs — a `$user->email` usage can live in any
    /// model, service, job, action, or Volt `.php` page. This bucket feeds the
    /// warm parse so those usages are indexed, not just files the user happens
    /// to open. Excludes vendor (covered separately) and noise dirs.
    source_files: Vec<PathBuf>,

    /// Root directories captured from the most recent
    /// `register_project_files` call. We retain them so the file-watcher
    /// handler can classify newly-created paths into the right
    /// per-category list (controllers, views, livewire, routes,
    /// vendor) by checking which root the path falls under.
    ///
    /// All paths here are absolute, so prefix matching against an
    /// incoming absolute event path is a straightforward
    /// `path.starts_with(prefix)`.
    project_root_paths: ProjectRootPaths,

    /// Inverted symbol index — turns find-references from O(N files)
    /// into O(1) hash lookup. Built at warming completion via a
    /// `BuildSymbolIndex` message; kept fresh thereafter via the
    /// `mark_dirty` / `take_dirty` pattern (see `symbol_index.rs`).
    symbol_index: crate::symbol_index::SymbolIndex,

    /// Reverse "which Blade files render component X" index, over the same
    /// `ParsedPatternsData` the pattern cache already holds. Kept fresh with
    /// the same deferred `mark_dirty` / drain shape as `symbol_index`; see
    /// `component_usage_index.rs` for why the ancestor walk cannot afford the
    /// linear scan it replaces.
    component_usage_index: crate::component_usage_index::ComponentUsageIndex,

    /// Project-wide class-hierarchy + member index. Populated at warming from
    /// the same parse that feeds the pattern cache; powers structural code
    /// lenses (implementations / usages / overrides / parent) and cross-file
    /// inheritance resolution. See `class_hierarchy_index.rs`.
    class_hierarchy_index: crate::class_hierarchy_index::ClassHierarchyIndex,
    /// Cached class→file map handed to `snapshot_class_files`, shared by `Arc`
    /// so the hot edit path and the debounced rebuild don't re-clone the whole
    /// map every call. Set to `None` whenever the hierarchy's FQCN→file mapping
    /// actually changes (see the invalidation at each mutation site); a typical
    /// method-body edit leaves it intact so the next snapshot is O(1).
    class_files_snapshot: Option<Arc<HashMap<String, PathBuf>>>,

    // === Service Provider Registry ===
    /// Cached middleware aliases from service provider analysis
    sp_middleware_aliases: HashMap<String, MiddlewareRegistrationData>,
    /// Cached bindings from service provider analysis
    sp_bindings: HashMap<String, BindingRegistrationData>,
    /// Cached singletons from service provider analysis
    sp_singletons: HashMap<String, BindingRegistrationData>,
    /// Cached view namespaces from loadViewsFrom() calls
    sp_view_namespaces: HashMap<String, ViewNamespaceData>,
    /// Cached Blade component registrations from Blade::component() calls
    sp_blade_components: HashMap<String, BladeComponentRegData>,
    /// Cached component namespace registrations from Blade::componentNamespace() calls
    sp_component_namespaces: HashMap<String, ComponentNamespaceData>,

    // === Salsa-based Environment Variable Tracking (New) ===
    /// Env files registered with Salsa for incremental parsing
    salsa_env_files: HashMap<PathBuf, EnvFile>,
    /// Version counter for env files
    salsa_env_version: i32,

    // === Salsa-based Translation Tracking (issue #293) ===
    /// Lang catalogues and directory listings, memoized through Salsa.
    translations: TranslationCache,

    // === Salsa-based Service Provider Tracking (New) ===
    /// Service provider files registered with Salsa for incremental parsing
    salsa_sp_files: HashMap<PathBuf, ServiceProviderFile>,

    /// Configured module directories in `modules.paths` glob-match order —
    /// the rank the registration merge breaks equal-priority ties on. Empty
    /// when the modular-monolith feature is off.
    module_dirs: Vec<PathBuf>,
    /// Version counter for service provider files
    salsa_sp_version: i32,
    /// Project root for service provider resolution
    salsa_sp_root: Option<PathBuf>,
}

/// Bootstrap capacity for the pattern cache, used only until
/// [`SalsaActor::size_and_publish_pattern_cache`] sizes it for real (see that
/// method and [`SalsaActor::spawn`]).
const PATTERN_CACHE_INITIAL_CAPACITY: usize = 1024;

/// Headroom added on top of the discovered file count when
/// [`SalsaActor::size_and_publish_pattern_cache`] sizes the pattern cache —
/// covers files that appear afterwards without forcing an immediate rehash
/// (a `composer update`, new app files created mid-session).
const PATTERN_CACHE_CAPACITY_PADDING: usize = 1000;

/// One provider registration's standing in the merge performed by
/// [`SalsaActor::handle_get_laravel_config`]: its tier priority
/// (`0=framework, 1=package, 2=module, 3=app`) and, for a module provider,
/// its 1-based rank in `modules.paths` order (`0` = not a module provider).
/// Ordered as a plain tuple, which IS the documented rule.
type MergeRank = (u8, usize);

impl SalsaActor {
    /// Spawn the actor on a dedicated thread and return a handle for communication
    pub fn spawn() -> SalsaHandle {
        let (tx, rx) = mpsc::channel(256);

        // The pattern cache is published to handles through this slot rather
        // than handed out here, because `spawn()` runs before any project is
        // known — the LSP server is constructed at this point, before
        // `initialize` carries a workspace root — so there's no file count to
        // size a table against yet. `handle_register_project_files` fills the
        // slot once its walk knows the count. A short-lived test/tooling
        // instance that never registers a project simply never fills it.
        let pattern_cache_slot: Arc<
            std::sync::OnceLock<Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>>,
        > = Arc::new(std::sync::OnceLock::new());
        let pattern_cache_slot_for_actor = pattern_cache_slot.clone();

        // Published post-construction via `SalsaHandle::set_documents` (the
        // server's `documents` map isn't available yet here — see the field
        // doc comment on `SalsaActor::documents`). Both structs below share
        // this SAME `Arc<OnceLock<_>>`, so a single `set()` through either
        // handle publishes it to both at once.
        let documents: Arc<OnceLock<Arc<RwLock<HashMap<Url, (String, i32)>>>>> =
            Arc::new(OnceLock::new());
        let documents_for_actor = documents.clone();

        std::thread::spawn(move || {
            let mut actor = SalsaActor::new(rx, pattern_cache_slot_for_actor, documents_for_actor);

            // Pre-warm query cache on actor thread (background)
            // This runs before any file parsing requests arrive,
            // moving the ~200ms compilation cost to startup
            crate::queries::prewarm_query_cache();

            actor.run();
        });

        SalsaHandle {
            sender: tx,
            pattern_cache: pattern_cache_slot,
            documents,
        }
    }

    /// Build the actor's initial state.
    ///
    /// Extracted from [`Self::spawn`] so the struct literal has exactly one
    /// home and the in-crate test module can construct an actor to drive
    /// `&mut self` methods directly — the only way to assert on `files` and
    /// `external_php_text`, which no `SalsaHandle` message exposes (#364).
    /// `spawn` still owns the threading; this owns the fields.
    fn new(
        receiver: mpsc::Receiver<SalsaRequest>,
        pattern_cache_slot: Arc<
            std::sync::OnceLock<Arc<dashmap::DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>>,
        >,
        documents: Arc<OnceLock<Arc<RwLock<HashMap<Url, (String, i32)>>>>>,
    ) -> Self {
        SalsaActor {
            db: LaravelDatabase::new(),
            receiver,
            // Pre-allocate with reasonable capacity to avoid early reallocations
            files: HashMap::with_capacity(64),
            // Bootstrap table, big enough for the handful of `didOpen`
            // buffers an editor may send before registration finishes.
            // Replaced with a correctly-sized one — carrying those
            // entries over — by `size_and_publish_pattern_cache`.
            pattern_cache: Arc::new(dashmap::DashMap::with_capacity(
                PATTERN_CACHE_INITIAL_CAPACITY,
            )),
            pattern_cache_slot,
            documents,
            loop_blocks_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
            php_assignments_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
            document_symbols_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
            external_php_text: HashMap::with_capacity(64),
            external_php_open_buffers: HashMap::with_capacity(64),
            text_revision: 0,
            file_text_revisions: HashMap::with_capacity(64),
            // Config management
            config_root: None,
            config_files: HashMap::with_capacity(4),
            config_version: 0,
            config_cache: None,
            registration_baselines: HashMap::new(),
            // Reference finding
            render_index: None,
            render_index_version: 0,
            render_index_generation: 0,
            project_files: None,
            project_files_version: 0,
            controller_files: Vec::new(),
            view_files: Vec::new(),
            livewire_files: Vec::new(),
            route_files: Vec::new(),
            vendor_files: Vec::new(),
            source_files: Vec::new(),
            project_root_paths: ProjectRootPaths::default(),
            symbol_index: crate::symbol_index::SymbolIndex::default(),
            component_usage_index: crate::component_usage_index::ComponentUsageIndex::default(),
            class_hierarchy_index: crate::class_hierarchy_index::ClassHierarchyIndex::default(),
            class_files_snapshot: None,
            // Service provider registry
            sp_middleware_aliases: HashMap::new(),
            sp_bindings: HashMap::new(),
            sp_singletons: HashMap::new(),
            sp_view_namespaces: HashMap::new(),
            sp_blade_components: HashMap::new(),
            sp_component_namespaces: HashMap::new(),
            // Salsa-based env tracking
            salsa_env_files: HashMap::with_capacity(4),
            salsa_env_version: 0,
            // Salsa-based translation tracking (issue #293)
            translations: TranslationCache::default(),
            // Salsa-based service provider tracking
            salsa_sp_files: HashMap::with_capacity(32),
            module_dirs: Vec::new(),
            salsa_sp_version: 0,
            salsa_sp_root: None,
        }
    }

    /// Synchronous counterpart to `SalsaHandle::open_buffer_paths`, for the
    /// dead-fallback `BulkImportPatterns` message handler below. This runs
    /// inside `run()`, on the actor's dedicated `std::thread::spawn` thread
    /// — never polled by the tokio runtime — so `blocking_read()` is the
    /// correct primitive: there's no `.await` available (this isn't an
    /// async fn) and, unlike on a tokio worker thread, nothing here can
    /// deadlock the runtime by blocking.
    fn open_buffer_paths_blocking(&self) -> std::collections::HashSet<PathBuf> {
        match self.documents.get() {
            Some(documents) => documents
                .blocking_read()
                .keys()
                .filter_map(|uri| uri.to_file_path().ok())
                .collect(),
            None => std::collections::HashSet::new(),
        }
    }

    /// Main event loop - process requests until shutdown
    fn run(&mut self) {
        while let Some(request) = self.receiver.blocking_recv() {
            match request {
                SalsaRequest::UpdateFile {
                    path,
                    version,
                    text,
                    reply,
                } => {
                    self.handle_update_file(path, version, text);
                    let _ = reply.send(());
                }
                SalsaRequest::GetPatterns { path, reply } => {
                    let result = self.handle_get_patterns(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetLoopBlocks { path, reply } => {
                    let result = self.handle_get_loop_blocks(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetPhpAssignments { path, reply } => {
                    let result = self.handle_get_php_assignments(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetDocumentSymbols { path, reply } => {
                    let result = self.handle_get_document_symbols(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::QueryRunCounts { reply } => {
                    let _ = reply.send(self.db.query_run_counts().snapshot());
                }
                SalsaRequest::SetRenderIndex {
                    generation,
                    entries,
                    reply,
                } => {
                    self.handle_set_render_index(generation, entries);
                    let _ = reply.send(());
                }
                SalsaRequest::BladeBackingClassResolution {
                    blade_path,
                    view_name,
                    livewire_paths,
                    live_blade_text,
                    reply,
                } => {
                    let result = self.handle_blade_backing_class_resolution(
                        &blade_path,
                        view_name,
                        livewire_paths,
                        live_blade_text,
                    );
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveLivewireMember {
                    path,
                    member,
                    reply,
                } => {
                    let result = self.handle_resolve_livewire_member(&path, &member);
                    let _ = reply.send(result);
                }
                SalsaRequest::RemoveFile { path, reply } => {
                    self.files.remove(&path);
                    self.invalidate_file_caches(&path);
                    self.file_text_revisions.remove(&path);
                    self.external_php_text.remove(&path);
                    // Drop from the inverted index too. Doing this
                    // synchronously (rather than via mark_dirty) is
                    // correct: there's no future state to refresh
                    // to — the file is gone.
                    self.symbol_index.remove_file(&path);
                    self.component_usage_index.remove_file(&path);
                    if self.class_hierarchy_index.contains_file(&path) {
                        self.class_hierarchy_index.remove_file(&path);
                        self.class_files_snapshot = None; // hierarchy changed
                    }
                    let _ = reply.send(());
                }
                SalsaRequest::AcquireExternalPhpOwnership { path, reply } => {
                    self.acquire_external_php_ownership(&path);
                    let _ = reply.send(());
                }
                SalsaRequest::ReleaseExternalPhpOwnership { path, reply } => {
                    self.release_external_php_ownership(&path);
                    let _ = reply.send(());
                }

                SalsaRequest::UpdateProjectFileList { path, op, reply } => {
                    let result = self.handle_update_project_file_list(path, op);
                    let _ = reply.send(result);
                }

                SalsaRequest::BuildSymbolIndex { reply } => {
                    // Full rebuild from current pattern_cache. Cheap on
                    // a freshly-warmed project (~50ms for 60k entries
                    // because we're just iterating a DashMap and
                    // pushing into HashMaps — no parsing). Clear first
                    // so we start from a known state.
                    self.symbol_index.clear();
                    // Cloned so the iteration below borrows the Arc, not `self` —
                    // `insert_file` needs `&mut self.symbol_index` at the same time.
                    let cache = Arc::clone(&self.pattern_cache);
                    for entry in cache.iter() {
                        let path = entry.key();
                        let (_, ref patterns) = *entry.value();
                        self.symbol_index.insert_file(path, patterns);
                    }
                    let count = self.symbol_index.entry_count();
                    let _ = reply.send(count);
                }

                SalsaRequest::ClearReindexState { reply } => {
                    // Cold-reindex wipe. Emptying pattern_cache is the load-
                    // bearing part: the warming pass skips any path already in
                    // it, so a stale entry would otherwise never be re-parsed.
                    // The rest are dropped so no since-deleted file's symbols,
                    // hierarchy node, or cached parse can survive the rebuild.
                    self.pattern_cache.clear();
                    self.symbol_index.clear();
                    // Re-queued rather than merely cleared: the view files
                    // still exist, and an emptied index that nothing refills
                    // would answer every ancestor walk with silence.
                    self.component_usage_index.clear();
                    for path in &self.view_files.clone() {
                        self.component_usage_index.mark_dirty(path);
                    }
                    self.class_hierarchy_index.clear();
                    self.loop_blocks_cache.clear();
                    self.php_assignments_cache.clear();
                    self.document_symbols_cache.clear();
                    self.class_files_snapshot = None;
                    let _ = reply.send(());
                }

                // === Config Handlers ===
                SalsaRequest::RegisterConfigFiles {
                    root_path,
                    composer_json,
                    view_config,
                    livewire_config,
                    reply,
                } => {
                    self.handle_register_config_files(
                        root_path,
                        composer_json,
                        view_config,
                        livewire_config,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::UpdateConfigFile { path, text, reply } => {
                    self.handle_update_config_file(path, text);
                    let _ = reply.send(());
                }
                SalsaRequest::GetLaravelConfig { reply } => {
                    let result = self.handle_get_laravel_config();
                    let _ = reply.send(result);
                }

                // === Reference Finding Handlers ===
                SalsaRequest::RegisterProjectFiles {
                    root_path,
                    controller_paths,
                    view_paths,
                    livewire_path,
                    routes_path,
                    vendor_files,
                    reply,
                } => {
                    self.handle_register_project_files(
                        root_path,
                        controller_paths,
                        view_paths,
                        livewire_path,
                        routes_path,
                        vendor_files,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::FindViewReferences { view_name, reply } => {
                    let result = self.handle_find_view_references(&view_name);
                    let _ = reply.send(result);
                }
                SalsaRequest::FilesRenderingComponent {
                    component_names,
                    livewire_names,
                    reply,
                } => {
                    let result =
                        self.handle_files_rendering_component(&component_names, &livewire_names);
                    let _ = reply.send(result);
                }
                SalsaRequest::FindReferences {
                    symbol,
                    include_declaration,
                    reply,
                } => {
                    let result = self.handle_find_references(&symbol, include_declaration);
                    let _ = reply.send(result);
                }
                SalsaRequest::CountSymbolReferences { symbol, reply } => {
                    // Direct inverted-index lookup — no dirty-refresh or project
                    // walk (code-lens resolve must stay cheap on large files).
                    let count = self.symbol_index.find(&symbol).len();
                    let _ = reply.send(count);
                }
                SalsaRequest::ListProjectFiles { reply } => {
                    // User code (the whole non-vendor source bucket, which
                    // supersets the categorized controller/view/livewire/route
                    // lists) is chained first so it parses first when the
                    // semaphore frees up; vendor is tailed last. Deduplicated —
                    // the categorized lists overlap `source_files`, and an
                    // absolute view path could fall outside the project root.
                    let mut seen = std::collections::HashSet::new();
                    let paths: Vec<PathBuf> = self
                        .source_files
                        .iter()
                        .chain(self.controller_files.iter())
                        .chain(self.view_files.iter())
                        .chain(self.livewire_files.iter())
                        .chain(self.route_files.iter())
                        .chain(self.vendor_files.iter())
                        .filter(|p| seen.insert((*p).clone()))
                        .cloned()
                        .collect();
                    let _ = reply.send(paths);
                }
                // NOTE: BulkImportPatterns is intentionally kept as a no-op
                // fallback in case any code path still sends it. The real
                // bulk import now writes directly to the shared
                // pattern_cache via SalsaHandle::bulk_import_patterns
                // (which does NOT round-trip through this actor channel).
                // See SalsaActor::pattern_cache for the architectural why.
                // Guarded the same way as that real path (open buffers
                // skipped) so the two can't drift apart if this fallback
                // ever does get exercised.
                SalsaRequest::BulkImportPatterns { entries, reply } => {
                    let total = entries.len();
                    let open_paths = self.open_buffer_paths_blocking();
                    for (path, data) in entries {
                        if open_paths.contains(&path) {
                            continue;
                        }
                        self.pattern_cache.insert(path, (0, data));
                    }
                    let _ = reply.send(total);
                }
                SalsaRequest::BulkImportHierarchy { entries, reply } => {
                    for (path, nodes) in entries {
                        // remove_file first so a re-warm refreshes cleanly.
                        self.class_hierarchy_index.remove_file(&path);
                        self.class_hierarchy_index.insert_file(&path, nodes);
                    }
                    // Bulk restore always changes the mapping — drop the cache.
                    self.class_files_snapshot = None;
                    let _ = reply.send(self.class_hierarchy_index.class_count());
                }
                SalsaRequest::SnapshotClassFiles { reply } => {
                    // Build once, then hand out cheap `Arc` clones until the
                    // hierarchy's FQCN→file mapping changes (invalidated below).
                    if self.class_files_snapshot.is_none() {
                        let map = self.class_hierarchy_index.fqcn_file_map();
                        self.class_files_snapshot = Some(Arc::new(map));
                    }
                    let snapshot = self.class_files_snapshot.clone().unwrap_or_default();
                    let _ = reply.send(snapshot);
                }
                SalsaRequest::SnapshotBindings { reply } => {
                    // Bindings number in the dozens, not thousands, so build
                    // fresh each call — no cache to invalidate on service-
                    // provider edits. Singletons first, then plain binds, so a
                    // plain bind wins on key collision (mirrors the precedence
                    // in `handle_get_binding_by_name`). Concrete FQCNs are
                    // normalized to the leading-backslash-free form the class
                    // index keys on.
                    let mut map: std::collections::HashMap<String, String> =
                        std::collections::HashMap::new();
                    for (key, reg) in self.sp_singletons.iter().chain(self.sp_bindings.iter()) {
                        map.insert(
                            key.clone(),
                            reg.concrete_class.trim_start_matches('\\').to_string(),
                        );
                    }
                    let _ = reply.send(Arc::new(map));
                }
                SalsaRequest::SnapshotFacadeAliases { reply } => {
                    let _ = reply.send(self.build_facade_alias_snapshot());
                }
                SalsaRequest::SnapshotMacros { reply } => {
                    // Reduce the registration data to the (decl_file, decl_line)
                    // the build-pass resolver needs; priority merging already
                    // happened in `build_macro_registry`.
                    let registry = self.build_macro_registry();
                    let mut map: std::collections::HashMap<(String, String), (PathBuf, u32)> =
                        std::collections::HashMap::with_capacity(registry.len());
                    for (key, data) in registry.iter() {
                        map.insert(key.clone(), (data.decl_file.clone(), data.decl_line));
                    }
                    let _ = reply.send(Arc::new(map));
                }
                SalsaRequest::SnapshotSortedProviderPaths { reply } => {
                    // Route through the real `sorted_sp_files` so a reverted sort
                    // surfaces here as raw-HashMap order — the whole point of the
                    // Bug B guard (#267).
                    let paths = self
                        .sorted_sp_files()
                        .into_iter()
                        .map(|sp_file| sp_file.path(&self.db).clone())
                        .collect();
                    let _ = reply.send(paths);
                }
                SalsaRequest::FileProviderRegistrations {
                    path,
                    fresh_text,
                    reply,
                } => {
                    // `before` comes from the baseline, never the live inputs
                    // — the did_change debounce has usually already overwritten
                    // those with the edited text by the time a save runs, which
                    // would diff empty and leave dependents stale (#255).
                    let before = self
                        .registration_baselines
                        .get(&path)
                        .cloned()
                        .unwrap_or_default();
                    let is_save = fresh_text.is_some();
                    if let Some(text) = fresh_text {
                        if self.salsa_sp_files.contains_key(&path) {
                            // Only re-register a provider the actor already
                            // knows — a brand-new provider file is the App
                            // rescan's job.
                            let priority = self.salsa_sp_files[&path].priority(&self.db);
                            if let Some(root) = self.salsa_sp_root.clone() {
                                self.handle_register_service_provider_source(
                                    path.clone(),
                                    text,
                                    priority,
                                    root,
                                );
                            }
                        } else if self
                            .config_root
                            .as_ref()
                            .is_some_and(|root| path == root.join("config/app.php"))
                        {
                            // config/app.php `aliases` live in the SEPARATE
                            // `config_files` input the provider re-registration
                            // above never touches — without this the post-save
                            // alias snapshot reads the same entry as the
                            // baseline and an alias edit never ripples (#255).
                            self.handle_update_config_file(path.clone(), text);
                        }
                    }
                    let after = self.handle_file_provider_registrations(&path);
                    // Advance the baseline only on a save transaction, and only
                    // for paths that carry (or carried) a contribution — every
                    // .php save lands here, and the untracked majority must not
                    // grow the map.
                    if is_save
                        && (after != ProviderRegistrationsData::default()
                            || self.registration_baselines.contains_key(&path))
                    {
                        self.registration_baselines.insert(path, after.clone());
                    }
                    let _ = reply.send((before, after));
                }
                SalsaRequest::SnapshotImplementers { reply } => {
                    // A cheap clone of the interface→implementors reverse map the
                    // build-pass resolver consults for contract→concrete chains;
                    // it changes only when a class's `implements` clause changes,
                    // so building fresh each call is fine (it numbers in the
                    // hundreds, not the tens of thousands).
                    let _ = reply.send(Arc::new(self.class_hierarchy_index.implementers_map()));
                }
                SalsaRequest::SnapshotHierarchyNodes { reply } => {
                    let _ = reply.send(self.class_hierarchy_index.nodes_by_file());
                }
                SalsaRequest::FileClassSurfaces { path, reply } => {
                    let _ = reply.send(self.class_hierarchy_index.file_surfaces(&path));
                }
                SalsaRequest::ExpandClassDescendants { seeds, reply } => {
                    let _ = reply.send(self.class_hierarchy_index.expand_with_descendants(&seeds));
                }
                SalsaRequest::ExportMagicMembers { reply } => {
                    let _ = reply.send(self.symbol_index.magic_members_by_file());
                }
                SalsaRequest::BulkImportMagicMembers { entries, reply } => {
                    // Append-only: `build_symbol_index` already inserted this
                    // path's literal-symbol keys, and `insert_magic_members`
                    // extends `by_file` rather than overwriting, so the two
                    // coexist and evict together via `remove_file`.
                    let mut count = 0usize;
                    for (path, members) in entries {
                        count += members.len();
                        self.symbol_index.insert_magic_members(&path, &members);
                    }
                    let _ = reply.send(count);
                }
                SalsaRequest::ReindexFileMagic {
                    path,
                    entries,
                    reply,
                } => {
                    // Evict the file's prior keys (literals + magic), then
                    // rebuild: literals from the current pattern cache + the
                    // freshly resolved magic members. `remove_file` clears both
                    // kinds, so re-inserting literals here keeps them alive.
                    self.symbol_index.remove_file(&path);
                    // Cloned so the lookup borrows the Arc, not `self` — `insert_file`
                    // needs `&mut self.symbol_index` while the Ref guard is alive.
                    let cache = Arc::clone(&self.pattern_cache);
                    if let Some(cached) = cache.get(&path) {
                        let (_, ref patterns) = *cached;
                        self.symbol_index.insert_file(&path, patterns);
                    }
                    self.symbol_index.insert_magic_members(&path, &entries);
                    let _ = reply.send(());
                }
                SalsaRequest::FindMemberReferences {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_find_member_references(&path, line, column);
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveMagicMemberAt {
                    path,
                    line,
                    column,
                    builder_index,
                    reply,
                } => {
                    let result = self.handle_resolve_magic_member_at(
                        &path,
                        line,
                        column,
                        builder_index.as_deref(),
                    );
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveFacadeReceiverAt {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_resolve_facade_receiver_at(&path, line, column);
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveMagicMemberRenameAt {
                    path,
                    line,
                    column,
                    reply,
                } => {
                    let result = self.handle_resolve_magic_member_rename_at(&path, line, column);
                    let _ = reply.send(result);
                }

                // === Service Provider Handlers ===
                SalsaRequest::RegisterServiceProviderRegistry {
                    middleware_aliases,
                    bindings,
                    singletons,
                    reply,
                } => {
                    self.handle_register_service_provider_registry(
                        middleware_aliases,
                        bindings,
                        singletons,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::GetMiddlewareByAlias { alias, reply } => {
                    let result = self.handle_get_middleware_by_alias(&alias);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetBindingByName { name, reply } => {
                    let result = self.handle_get_binding_by_name(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetViewNamespace { namespace, reply } => {
                    let result = self.handle_get_view_namespace(&namespace);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllViewNamespaces { reply } => {
                    let result = self.handle_get_all_view_namespaces();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetBladeComponentReg { tag_name, reply } => {
                    let result = self.handle_get_blade_component_reg(&tag_name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllBladeComponentRegs { reply } => {
                    let result = self.handle_get_all_blade_component_regs();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetComponentNamespace { prefix, reply } => {
                    let result = self.handle_get_component_namespace(&prefix);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllComponentNamespaces { reply } => {
                    let result = self.handle_get_all_component_namespaces();
                    let _ = reply.send(result);
                }

                // === Salsa-based Environment Variable Handlers (New) ===
                SalsaRequest::RegisterEnvSource {
                    path,
                    text,
                    priority,
                    reply,
                } => {
                    self.handle_register_env_source(path, text, priority);
                    let _ = reply.send(());
                }
                SalsaRequest::GetParsedEnvVar { name, reply } => {
                    let result = self.handle_get_parsed_env_var(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedEnvVars { reply } => {
                    let result = self.handle_get_all_parsed_env_vars();
                    let _ = reply.send(result);
                }
                SalsaRequest::ResolveTranslation {
                    root,
                    key,
                    locale,
                    vendor_map,
                    reply,
                } => {
                    let result = self.handle_resolve_translation(
                        &root,
                        &key,
                        &locale,
                        vendor_map.as_deref(),
                    );
                    let _ = reply.send(result);
                }
                SalsaRequest::AvailableLocales {
                    root,
                    key,
                    vendor_map,
                    reply,
                } => {
                    let result = self.handle_available_locales(&root, &key, vendor_map.as_deref());
                    let _ = reply.send(result);
                }
                SalsaRequest::RegisterLangSource { path, text, reply } => {
                    self.handle_register_lang_source(path, text);
                    let _ = reply.send(());
                }
                SalsaRequest::InvalidateLangPath { path, reply } => {
                    self.handle_invalidate_lang_path(&path);
                    let _ = reply.send(());
                }
                SalsaRequest::InvalidateConfigPath { path, reply } => {
                    self.handle_invalidate_config_path(&path);
                    let _ = reply.send(());
                }
                SalsaRequest::LocateTranslationKey {
                    root,
                    path,
                    target,
                    reply,
                } => {
                    let result = self
                        .translations
                        .locate_key(&mut self.db, &root, &path, &target);
                    let _ = reply.send(result);
                }
                SalsaRequest::TranslationKeyCompletions { root, reply } => {
                    let result = self.translations.completion_keys(&mut self.db, &root);
                    let _ = reply.send(result);
                }
                SalsaRequest::LocateKeyAcrossLocales {
                    root,
                    dotted_key,
                    reply,
                } => {
                    let result = self.translations.locate_key_across_locales(
                        &mut self.db,
                        &root,
                        &dotted_key,
                    );
                    let _ = reply.send(result);
                }
                SalsaRequest::VendorTranslationNamespaces { root, reply } => {
                    let result = self.translations.vendor_namespaces(&mut self.db, &root);
                    let _ = reply.send(result);
                }
                SalsaRequest::InvalidateTranslationProviders { reply } => {
                    self.translations.invalidate_providers();
                    let _ = reply.send(());
                }
                SalsaRequest::SetTranslationProviderExtras { files, reply } => {
                    self.translations.set_extra_provider_files(files);
                    let _ = reply.send(());
                }
                SalsaRequest::SetModuleDirs { dirs, reply } => {
                    // The merge tie-break reads these, and the merged config
                    // is memoized — a changed module list has to invalidate it.
                    self.module_dirs = dirs;
                    self.config_cache = None;
                    let _ = reply.send(());
                }
                SalsaRequest::LangDiskReads { reply } => {
                    let _ = reply.send(self.translations.disk_reads());
                }

                // === Salsa-based Service Provider Handlers (New) ===
                SalsaRequest::RegisterServiceProviderSource {
                    path,
                    text,
                    priority,
                    root_path,
                    reply,
                } => {
                    self.handle_register_service_provider_source(path, text, priority, root_path);
                    let _ = reply.send(());
                }
                SalsaRequest::GetParsedMiddleware { alias, reply } => {
                    let result = self.handle_get_parsed_middleware(&alias);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedMiddleware { reply } => {
                    let result = self.handle_get_all_parsed_middleware();
                    let _ = reply.send(result);
                }
                SalsaRequest::GetParsedBinding { name, reply } => {
                    let result = self.handle_get_parsed_binding(&name);
                    let _ = reply.send(result);
                }
                SalsaRequest::GetAllParsedBindings { reply } => {
                    let result = self.handle_get_all_parsed_bindings();
                    let _ = reply.send(result);
                }

                // === Cache-based Registration Handlers ===
                SalsaRequest::RegisterCachedMiddleware {
                    alias,
                    class,
                    class_file,
                    source_file,
                    line,
                    reply,
                } => {
                    self.handle_register_cached_middleware(
                        alias,
                        class,
                        class_file,
                        source_file,
                        line,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedBinding {
                    name,
                    class,
                    binding_type,
                    class_file,
                    source_file,
                    line,
                    reply,
                } => {
                    self.handle_register_cached_binding(
                        name,
                        class,
                        binding_type,
                        class_file,
                        source_file,
                        line,
                    );
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedMiddlewareBatch { entries, reply } => {
                    for (alias, class, class_file, source_file, line) in entries {
                        self.handle_register_cached_middleware(
                            alias,
                            class,
                            class_file,
                            source_file,
                            line,
                        );
                    }
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedBindingBatch { entries, reply } => {
                    for (name, class, binding_type, class_file, source_file, line) in entries {
                        self.handle_register_cached_binding(
                            name,
                            class,
                            binding_type,
                            class_file,
                            source_file,
                            line,
                        );
                    }
                    let _ = reply.send(());
                }
                SalsaRequest::RegisterCachedConfig { config, reply } => {
                    // Set config directly from cache, bypassing parsing
                    self.config_root = Some(config.root.clone());
                    self.config_cache = Some((self.config_version, *config));
                    tracing::info!("📋 Registered cached Laravel config");
                    let _ = reply.send(());
                }

                SalsaRequest::Shutdown => {
                    break;
                }
            }
        }
    }

    /// Handle file update - create or update the SourceFile
    fn handle_update_file(&mut self, path: PathBuf, version: i32, text: String) {
        // Invalidate caches for this file - will be recomputed on next request
        self.invalidate_file_caches(&path);
        // Mark for re-indexing on next find-references query. We don't
        // re-index eagerly here because (a) most file edits are
        // followed by more edits before any query runs, and (b) the
        // new patterns aren't parsed until something asks for them
        // via get_patterns anyway. Lazy refresh amortizes both costs.
        self.symbol_index.mark_dirty(&path);
        // A Blade edit can add or delete an `<x-…>` / `<livewire:…>` tag, so
        // the reverse usage index has to re-read this file before its next
        // answer (no-op for non-Blade paths).
        self.component_usage_index.mark_dirty(&path);

        self.bump_text_revision(&path);
        // Hand ownership of this path's text to the pusher. Without it
        // `ensure_external_php_source_loaded` sees no entry, decides a reload
        // is due, and overwrites an unsaved buffer with the last-saved bytes
        // the moment a Blade hover resolves its backing class. Same rule
        // `did_change_watched_files` already applies from the other side: the
        // buffer wins over disk.
        self.mark_pushed_by_client(&path);

        if let Some(file) = self.files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(version);
            file.set_text(&mut self.db).to(text);
        } else {
            // Create new file
            let file = SourceFile::new(&self.db, path.clone(), version, text);
            self.files.insert(path, file);
        }
    }

    /// Stamp a new revision for `path`'s text and return it. Every write into
    /// `self.files` calls this; see [`SalsaActor::text_revision`] for why the
    /// caches cannot key on `SourceFile::version` instead.
    fn bump_text_revision(&mut self, path: &Path) -> u64 {
        self.text_revision = self.text_revision.wrapping_add(1);
        self.file_text_revisions
            .insert(path.to_path_buf(), self.text_revision);
        self.text_revision
    }

    /// The revision of the text currently installed for `path`. Zero for a
    /// file no bumping writer has touched.
    fn text_revision_of(&self, path: &Path) -> u64 {
        self.file_text_revisions.get(path).copied().unwrap_or(0)
    }

    /// Drop every per-file actor cache entry for `path`. Called by every
    /// writer of `files[path]`'s text — `pattern_cache` in particular is
    /// checked without any version comparison, so a stale entry there is
    /// served forever rather than merely once.
    fn invalidate_file_caches(&mut self, path: &Path) {
        self.pattern_cache.remove(path);
        self.loop_blocks_cache.pop(path);
        self.php_assignments_cache.pop(path);
        self.document_symbols_cache.pop(path);
    }

    /// Record that `path`'s Salsa text was installed by a client push, so the
    /// backing-class loader never reads disk over it.
    ///
    /// Deliberately infallible. The predecessor stamped `path`'s disk mtime
    /// and silently did nothing when `metadata()` failed — leaving a pushed
    /// (possibly unsaved) buffer with no entry at all, which the loader reads
    /// as "never loaded, read disk unconditionally": precisely the clobber the
    /// stamp exists to prevent. Ownership is not a fact about the filesystem,
    /// so it must not be recorded through a fallible filesystem call.
    fn mark_pushed_by_client(&mut self, path: &Path) {
        self.external_php_text
            .insert(path.to_path_buf(), ExternalPhpText::PushedByClient);
    }

    /// Count one more open editor buffer for `path`, driven by
    /// `textDocument/didOpen`. Nothing else: the text and its
    /// `PushedByClient` stamp arrive with the buffer's own `update_file`
    /// push, and this only records that a buffer is holding the path so
    /// [`SalsaActor::release_external_php_ownership`] knows when the last one
    /// lets go.
    ///
    /// **`did_open` must enqueue this BEFORE its `update_file` push.** A
    /// `didClose` for a previous buffer of the same path can be in flight
    /// concurrently and lands somewhere in this sequence. Acquire-first leaves
    /// every landing point safe: before the acquire it releases the earlier
    /// buffer's stamp, which the push then re-installs; between or after, it
    /// decrements to a non-zero count and drops nothing. Push-first opens one
    /// window where the release lands after the stamp with the count still at
    /// zero — a live buffer left unowned, which is the defect the release
    /// exists to prevent, reached from the other side.
    fn acquire_external_php_ownership(&mut self, path: &Path) {
        *self
            .external_php_open_buffers
            .entry(path.to_path_buf())
            .or_insert(0) += 1;
    }

    /// Hand `path`'s text back to the loader once its LAST open buffer goes —
    /// the release edge matching [`SalsaActor::mark_pushed_by_client`]'s
    /// acquire, driven by `textDocument/didClose`.
    ///
    /// Dropping the entry restores the "nobody installed text here" state,
    /// the one [`SalsaActor::ensure_external_php_source_loaded`] reads as
    /// "read disk unconditionally". That is the whole point: a buffer can be
    /// closed with its edits DISCARDED, which writes nothing to disk and so
    /// fires no `did_change_watched_files`. Without a release the pusher's
    /// promise to push again outlives the pusher, and the loader keeps
    /// serving text that exists neither on disk nor in any open buffer.
    ///
    /// A path with buffers left is NOT handed back. tower-lsp runs
    /// notification handlers concurrently, so this close can arrive after the
    /// `didOpen` of a buffer that reopened the same path — a revert, or Zed's
    /// multibuffer lifecycle. The count is what distinguishes the two: it
    /// still reads one, so the reopened buffer keeps its stamp. Releasing
    /// there would leave a live buffer unowned, and the loader's next read
    /// does not merely answer from disk — it writes disk text into the shared
    /// `SourceFile` every per-file query reads, so a hover in one file would
    /// silently revert another file's buffer text.
    ///
    /// With no count at all the stamp goes. That is the state a
    /// `didChangeWatchedFiles` push leaves behind — ownership with no buffer
    /// to close it — and the safe direction besides: every divergence falls
    /// toward consulting disk, never toward serving a buffer nobody holds.
    ///
    /// Infallible and idempotent: `didClose` fires for every closed document,
    /// most of which never enter either map, and removing an absent key is a
    /// no-op.
    ///
    /// Deliberately NOT `RemoveFile`. The `SourceFile` input, the per-file
    /// caches and the resolved magic-member entries all stay; only ownership
    /// changes, and the next loader read replaces the text. See the comment in
    /// `Backend::did_close` for why eviction on close is the wrong tool.
    fn release_external_php_ownership(&mut self, path: &Path) {
        if let Some(open_buffers) = self.external_php_open_buffers.get_mut(path) {
            // A present key is always at least one: the acquire inserts at
            // one, and the branch below removes the key rather than leaving a
            // zero behind. So this cannot underflow.
            *open_buffers -= 1;
            if *open_buffers > 0 {
                return;
            }
            self.external_php_open_buffers.remove(path);
        }
        self.external_php_text.remove(path);

        // Drop the TEXT that stamp was protecting, not merely the claim on it.
        // Releasing ownership alone un-blocks the LOADER, and the loader is not
        // the only reader: `handle_get_patterns`, `handle_get_document_symbols`,
        // `handle_get_loop_blocks` and `handle_get_php_assignments` all read
        // `files[path]` directly, and `pattern_cache` is checked with NO version
        // comparison — so an entry derived from the discarded buffer is served
        // forever rather than merely once. Find-references answering out of it
        // names a symbol that exists in no file at all.
        //
        // Removing the input restores the same "nobody installed text here"
        // state the line above restores for ownership, for every reader at
        // once: `ensure_file_registered` finds the slot vacant and reads disk,
        // and `ensure_external_php_source_loaded` reloads through its own
        // containment guard. This stays LAZY — nothing is read at close time;
        // the re-read lands on whichever query asks first.
        //
        // Deliberately NOT `RemoveFile`: the symbol index, the reverse
        // component-usage index, the class-hierarchy index and the resolved
        // magic-member entries are all left standing. Those are what `did_close`
        // refuses to evict, and nothing here touches them.
        self.files.remove(path);
        self.invalidate_file_caches(path);

        // The three deferred indexes are readers too, and neither of the lines
        // above reaches them: they answer `find_references` and the code lenses
        // out of their own maps, refreshed lazily from whatever `mark_dirty`
        // queued. A query run WHILE the buffer was open drains that queue, so
        // the index is left holding the buffer's literals with the flag already
        // cleared — a `view('…')` the discarded buffer introduced would keep
        // answering find-references forever, pointing at a file that never
        // contained it.
        //
        // Re-queueing is what `handle_update_file` does for any other change of
        // a path's text, and this is one: the text just went from the buffer's
        // back to disk's. It is NOT eviction — the drain runs
        // `remove_literal_entries` + `insert_file`, which deliberately keeps
        // the resolved magic-member entries that only a warm or save pass can
        // rebuild, and which are the whole reason `did_close` refuses
        // `RemoveFile`.
        self.symbol_index.mark_dirty(path);
        self.component_usage_index.mark_dirty(path);
    }

    /// Handle a Blade loop-blocks query. Memoized via Salsa + actor LRU.
    fn handle_get_loop_blocks(
        &mut self,
        path: &PathBuf,
    ) -> Option<Arc<Vec<crate::blade_loops::BladeLoopBlock>>> {
        let file = self.files.get(path)?;
        let version = self.text_revision_of(path);

        // Cache hit on matching revision
        if let Some((cached_version, cached)) = self.loop_blocks_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        // Cache miss / stale - call Salsa tracked query (memoized at the Salsa layer too)
        let blocks = parse_blade_loop_blocks(&self.db, *file);
        let arc = Arc::new(blocks);
        self.loop_blocks_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle resolving a `$this->X` member access in a Livewire component PHP file.
    /// Auto-registers the file in Salsa, invalidates on mtime change.
    /// Replace the render-index snapshot, unless it is older than the one
    /// already held.
    ///
    /// Every call bumps the Salsa revision, which invalidates the
    /// backing-class memo — so the caller must not push a snapshot the actor
    /// already holds. `Backend::pending_render_index_snapshot` is that gate,
    /// and skipping there saves the round-trip as well as the write.
    ///
    /// **The generation check is what makes concurrent pushes safe.** Two
    /// tasks can snapshot generations 5 and 6 and then reach this actor in
    /// either order, because each awaits a channel round-trip in between. The
    /// caller's own gate cannot fix that — it only advances a counter, while
    /// the data that ends up installed is decided here. So ordering is the
    /// actor's problem, and the actor solves it by refusing to go backwards:
    /// the winner is always the newest generation, whatever order they arrive
    /// in. `>=` rather than `>` because an equal generation carries identical
    /// entries, and re-installing them would drop the memo for nothing.
    fn handle_set_render_index(&mut self, generation: u64, entries: Vec<(String, PathBuf)>) {
        if self.render_index.is_some() && generation <= self.render_index_generation {
            return;
        }
        self.render_index_generation = generation;
        self.render_index_version = self.render_index_version.wrapping_add(1);
        match self.render_index {
            Some(index) => {
                index
                    .set_version(&mut self.db)
                    .to(self.render_index_version);
                index.set_entries(&mut self.db).to(entries);
            }
            None => {
                self.render_index = Some(RenderIndex::new(
                    &self.db,
                    self.render_index_version,
                    entries,
                ));
            }
        }
    }

    /// The render-index input, created empty on first use so a resolution that
    /// runs before any snapshot arrives still answers (with no contributors)
    /// instead of failing.
    fn render_index_input(&mut self) -> RenderIndex {
        match self.render_index {
            Some(index) => index,
            None => {
                let index = RenderIndex::new(&self.db, self.render_index_version, Vec::new());
                self.render_index = Some(index);
                index
            }
        }
    }

    /// Resolve the PHP class(es) backing a Blade template (#339, item 7).
    ///
    /// The two memoized queries do the work; this handler only supplies their
    /// inputs. That split is deliberate: registering a `SourceFile` reads the
    /// filesystem, and a tracked query must stay pure.
    fn handle_blade_backing_class_resolution(
        &mut self,
        blade_path: &PathBuf,
        view_name: Option<String>,
        livewire_paths: Vec<PathBuf>,
        live_blade_text: Option<String>,
    ) -> BladeBackingResolutionData {
        let index = self.render_index_input();
        let candidates = blade_backing_class_files(&self.db, index, view_name, livewire_paths);

        // Existence filtering happens HERE, not in the query: a candidate that
        // cannot be read as a Salsa input (absent file, or an MFC component
        // directory) contributes nothing and is dropped, exactly as the old
        // `is_file()` guard dropped it.
        let inputs: Vec<SourceFile> = candidates
            .iter()
            .filter_map(|path| self.ensure_external_php_source_loaded(path))
            .collect();
        let files: Vec<PathBuf> = inputs
            .iter()
            .map(|file| file.path(&self.db).clone())
            .collect();

        let inline = self.ensure_blade_source_registered(blade_path, live_blade_text);
        let sources = blade_backing_class_sources(&self.db, inputs, inline);
        BladeBackingResolutionData { files, sources }
    }

    /// Register the Blade template itself as a Salsa input so the inline
    /// SFC / Volt arm of [`blade_backing_class_sources`] reads it through the
    /// same memoized path as the backing classes.
    ///
    /// `live_text` (the open editor buffer) wins over disk, because the
    /// `did_change` → Salsa hop is debounced and an inline component's members
    /// must resolve against what the user is typing right now, not what landed
    /// 250 ms ago. The text is only written when it actually differs, so a
    /// resolution on an unedited buffer leaves the memo intact.
    fn ensure_blade_source_registered(
        &mut self,
        path: &PathBuf,
        live_text: Option<String>,
    ) -> Option<SourceFile> {
        let Some(text) = live_text else {
            return self.ensure_external_php_source_loaded(path);
        };
        match self.files.get(path).copied() {
            Some(file) => {
                if *file.text(&self.db) != text {
                    self.bump_text_revision(path);
                    // The live buffer supersedes disk for this file too, so
                    // the loader must not read it back out from under us.
                    self.mark_pushed_by_client(path);
                    self.invalidate_file_caches(path);
                    file.set_text(&mut self.db).to(text);
                }
                Some(file)
            }
            None => {
                self.bump_text_revision(path);
                self.mark_pushed_by_client(path);
                let file = SourceFile::new(&self.db, path.clone(), 0, text);
                self.files.insert(path.clone(), file);
                Some(file)
            }
        }
    }

    fn handle_resolve_livewire_member(&mut self, path: &PathBuf, member: &str) -> Option<String> {
        let file = self.ensure_external_php_source_loaded(path)?;
        resolve_livewire_member_type(&self.db, file, member.to_string())
    }

    /// Register an external PHP file as a Salsa input, reloading it from disk
    /// whenever its mtime advances — but only while this loader owns the
    /// path's text. A path a client pushed (see [`ExternalPhpText`]) is served
    /// from Salsa untouched, so an unsaved buffer is never overwritten with
    /// its last-saved bytes.
    ///
    /// Returns the cached `SourceFile` handle, or `None` when an unowned path
    /// is unreadable — which is also how a non-existent path and a directory
    /// (a Livewire v4 MFC's component dir) are dropped from backing-class
    /// resolution.
    ///
    /// **Containment (issue #364).** Every path reaching here is contained by
    /// construction today — render-index candidates come from the project's
    /// own directory walk, Livewire candidates from a resolver that gates each
    /// segment through `naming::is_safe_path_segment` — but this function is a
    /// read primitive whose result is *emitted* as a goto-definition target
    /// (`handle_blade_backing_class_resolution` maps it into
    /// `BladeBackingResolutionData::files`). A guard that lives only in the
    /// callers is a guard a future caller can forget, which is how #294 and
    /// both rounds of #348 happened. The guard therefore lives at the
    /// primitive, and it is split across the two branches because they ask
    /// different questions — see the comments on each. Both branches gate
    /// against the candidate's owning module where it has one, so a module
    /// symlinked in from a composer path repository keeps resolving.
    fn ensure_external_php_source_loaded(&mut self, path: &PathBuf) -> Option<SourceFile> {
        // Root unknown: refuse before any state is read, mutated, or stat'd.
        // Containment cannot be decided without a root, and this function's
        // failure mode must be closed.
        let root = self.config_root.clone()?;

        // Gate against the candidate's OWNING MODULE where it has one, falling
        // back to the project root — the same choice
        // `livewire_namespaces::contained_class_path` makes for the
        // registrations that MINT these paths, and the same
        // `config::owning_module` lookup this file already uses for provider
        // rank.
        //
        // `config::expand_module_dirs` admits a module directory whose real
        // target sits outside the project ON PURPOSE: that is the composer
        // path-repository layout. Gating this read against the root alone
        // dropped every backing class inside such a module — silently, since
        // there is no "component not found" diagnostic and the only symptom is
        // goto and hover quietly doing nothing.
        //
        // The swap does not loosen the guard, and for a module path it
        // TIGHTENS it: a candidate lexically under a module must canonicalize
        // inside that module, so one reaching into a sibling module or into
        // bare `app/` is refused even though it is inside the root.
        // `owning_module` collapses `..` before its prefix test, so a
        // traversing path cannot elect itself a laxer gate.
        let gate = crate::config::owning_module(&self.module_dirs, path)
            .map(|(_, dir)| dir.to_path_buf())
            .unwrap_or(root);

        // Ownership is checked BEFORE the filesystem, so a client-pushed path
        // is served from Salsa whether or not it can be stat'd at this instant.
        // Ordering the stat first would drop an unsaved buffer out of
        // backing-class resolution whenever its file is momentarily absent —
        // a branch switch, a stash, an `artisan make:*` regeneration.
        if self.external_php_text.get(path) == Some(&ExternalPhpText::PushedByClient) {
            // This branch reads no disk — but the path it hands back is emitted
            // to the client as a location to open, so containment still has to
            // hold. `path_within_root_emit_safe` is the guard shaped for that:
            // its lexical pre-gate refuses an out-of-root candidate with no
            // `stat` probe (#145), and its `None` arm admits a *genuinely
            // absent* in-root path — precisely the unsaved-buffer case this
            // branch exists to protect (#361) — while refusing a dangling
            // under-root symlink and every non-`NotFound` lstat error.
            //
            // The fail-closed read guard below cannot serve this branch: it
            // refuses anything it cannot canonicalize, which is exactly the
            // momentarily-absent buffer, so using it here would reintroduce
            // #361. The read branch keeps the full fail-closed guard; this is
            // an addition to a branch that guard never covered, not a
            // substitution for it.
            if !path_within_root_emit_safe(path, &gate) {
                return None;
            }
            if let Some(file) = self.files.get(path).copied() {
                return Some(file);
            }
            // Marked as pushed, yet the input is gone. Nothing is left to
            // protect, so fall through and load from disk rather than answer
            // `None`. `RemoveFile` clears both together, so this is a
            // belt-and-braces arm, not a reachable steady state.
        }

        // The disk branch reads real bytes, so containment must be PROVEN.
        // `canonical_within_root_registration` is documented for exactly this
        // shape — a path minted from discovered source data and then read: it
        // keeps the lexical pre-gate (no out-of-root existence oracle, #145)
        // and fails closed on anything it cannot canonicalize, including a
        // dangling under-root symlink (#134/#155).
        //
        // Both filesystem calls below go through `real`, the VERIFIED canonical
        // path the guard returns. Re-deriving it from `path` would let a
        // symlink swapped between guard and read hand back a target the guard
        // never approved. `path` stays the key for `self.files` and
        // `external_php_text`, so the callers' own lookups still resolve.
        let real = canonical_within_root_registration(path, &gate)?;

        let current_mtime = std::fs::metadata(&real).ok()?.modified().ok()?;

        let needs_reload = match self.external_php_text.get(path) {
            Some(ExternalPhpText::LoadedFromDisk(prev_mtime)) => {
                *prev_mtime != current_mtime || !self.files.contains_key(path)
            }
            // Either never loaded, or the pushed-but-vanished case above.
            _ => true,
        };

        if needs_reload {
            let text = std::fs::read_to_string(&real).ok()?;
            self.bump_text_revision(path);
            // This write replaces the text every per-file cache was populated
            // from. `pattern_cache` compares nothing on lookup — a hit is
            // assumed current — so an entry left behind here would answer
            // goto and completion out of the previous text indefinitely.
            self.invalidate_file_caches(path);

            if let Some(existing) = self.files.get(path) {
                existing.set_text(&mut self.db).to(text);
            } else {
                let file = SourceFile::new(&self.db, path.clone(), 0, text);
                self.files.insert(path.clone(), file);
            }
            self.external_php_text
                .insert(path.clone(), ExternalPhpText::LoadedFromDisk(current_mtime));
        }

        self.files.get(path).copied()
    }

    /// Handle a Blade @php-assignments query. Memoized via Salsa + actor LRU.
    fn handle_get_php_assignments(&mut self, path: &PathBuf) -> Option<Arc<Vec<(String, String)>>> {
        let file = self.files.get(path)?;
        let version = self.text_revision_of(path);

        if let Some((cached_version, cached)) = self.php_assignments_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        let assignments = parse_blade_php_assignments(&self.db, *file);
        let arc = Arc::new(assignments);
        self.php_assignments_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle a document-symbol query. Memoized via Salsa + actor LRU.
    fn handle_get_document_symbols(
        &mut self,
        path: &PathBuf,
    ) -> Option<Arc<Vec<crate::document_symbols::SymbolEntry>>> {
        let file = self.files.get(path)?;
        let version = self.text_revision_of(path);

        if let Some((cached_version, cached)) = self.document_symbols_cache.get(path) {
            if *cached_version == version {
                return Some(Arc::clone(cached));
            }
        }

        let symbols = extract_document_symbols(&self.db, *file);
        let arc = Arc::new(symbols);
        self.document_symbols_cache
            .put(path.clone(), (version, Arc::clone(&arc)));
        Some(arc)
    }

    /// Handle pattern query - parse file and extract patterns
    /// Uses cached data if version matches, otherwise converts and caches
    /// Returns Arc for efficient sharing without cloning the entire data structure
    fn handle_get_patterns(&mut self, path: &PathBuf) -> Option<Arc<ParsedPatternsData>> {
        let start = Instant::now();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // CHECK pattern_cache FIRST — before touching `self.files`. The
        // cache is the fast path for the vast majority of queries:
        // warming + disk cache populate it for every indexed file, and
        // `handle_update_file` removes the entry when content changes.
        // So a cache hit always means the entry is current (no version
        // check needed). This is the lookup that lets us skip
        // `ensure_file_registered` during project-file registration —
        // we don't need a Salsa SourceFile input to serve cached
        // patterns.
        //
        // DashMap::get returns a Ref guard holding a shard lock; clone the
        // Arc out and let the guard drop at the end of this statement, so
        // it's never held across the `&mut self` work below.
        let hit = self.pattern_cache.get(path).map(|entry| {
            let (_, cached_data) = entry.value();
            Arc::clone(cached_data)
        });
        if let Some(data) = hit {
            debug!("✅ Cache HIT for {} ({:?})", file_name, start.elapsed());
            return Some(data);
        }

        // Cache miss. We need to parse via the Salsa-tracked function,
        // which requires a `SourceFile` input. Lazily create it now if
        // it doesn't exist yet — this is where the file-read cost we
        // skipped at registration time finally lands. The cost is paid
        // once per file, only when something queries it past the cache.
        self.ensure_file_registered(path);

        let file = self.files.get(path)?;
        let version = file.version(&self.db);

        let parse_start = Instant::now();
        let patterns = parse_file_patterns(&self.db, *file);
        let parse_time = parse_start.elapsed();

        // Convert Salsa types to plain data types for transfer
        // Note: Cache intermediate interned values to avoid double lookups
        // Wrap in Rc for cheap cloning when building position index
        let views = patterns
            .views(&self.db)
            .iter()
            .map(|v| {
                let name = v.name(&self.db);
                Arc::new(ViewReferenceData {
                    name: name.name(&self.db).clone(),
                    line: v.line(&self.db),
                    column: v.column(&self.db),
                    end_column: v.end_column(&self.db),
                    is_route_view: v.is_route_view(&self.db),
                    is_property_site: v.is_property_site(&self.db),
                })
            })
            .collect();

        let components = patterns
            .components(&self.db)
            .iter()
            .map(|c| {
                let name = c.name(&self.db);
                let tag = c.tag_name(&self.db);
                Arc::new(ComponentReferenceData {
                    name: name.name(&self.db).clone(),
                    tag_name: tag.name(&self.db).clone(),
                    line: c.line(&self.db),
                    column: c.column(&self.db),
                    end_column: c.end_column(&self.db),
                })
            })
            .collect();

        // Annotated because `class_refs_from_directives(&directives)` below
        // borrows this as a slice, which otherwise leaves the collect's target
        // type unconstrained.
        let directives: Vec<Arc<DirectiveReferenceData>> = patterns
            .directives(&self.db)
            .iter()
            .map(|d| {
                let name = d.name(&self.db);
                Arc::new(DirectiveReferenceData {
                    name: name.name(&self.db).clone(),
                    arguments: d.arguments(&self.db).clone(),
                    line: d.line(&self.db),
                    column: d.column(&self.db),
                    end_column: d.end_column(&self.db),
                    string_column: d.string_column(&self.db),
                    string_end_column: d.string_end_column(&self.db),
                })
            })
            .collect();

        let env_refs = patterns
            .env_refs(&self.db)
            .iter()
            .map(|e| {
                let name = e.name(&self.db);
                Arc::new(EnvReferenceData {
                    name: name.name(&self.db).clone(),
                    has_fallback: e.has_fallback(&self.db),
                    line: e.line(&self.db),
                    column: e.column(&self.db),
                    end_column: e.end_column(&self.db),
                })
            })
            .collect();

        let config_refs = patterns
            .config_refs(&self.db)
            .iter()
            .map(|c| {
                let key = c.key(&self.db);
                Arc::new(ConfigReferenceData {
                    key: key.key(&self.db).clone(),
                    line: c.line(&self.db),
                    column: c.column(&self.db),
                    end_column: c.end_column(&self.db),
                })
            })
            .collect();

        let livewire_refs = patterns
            .livewire_refs(&self.db)
            .iter()
            .map(|lw| {
                let name = lw.name(&self.db);
                Arc::new(LivewireReferenceData {
                    name: name.name(&self.db).clone(),
                    line: lw.line(&self.db),
                    column: lw.column(&self.db),
                    end_column: lw.end_column(&self.db),
                })
            })
            .collect();

        let middleware_refs = patterns
            .middleware_refs(&self.db)
            .iter()
            .map(|mw| {
                let name = mw.name(&self.db);
                Arc::new(MiddlewareReferenceData {
                    name: name.name(&self.db).clone(),
                    line: mw.line(&self.db),
                    column: mw.column(&self.db),
                    end_column: mw.end_column(&self.db),
                })
            })
            .collect();

        let translation_refs = patterns
            .translation_refs(&self.db)
            .iter()
            .map(|t| {
                let key = t.key(&self.db);
                Arc::new(TranslationReferenceData {
                    key: key.key(&self.db).clone(),
                    line: t.line(&self.db),
                    column: t.column(&self.db),
                    end_column: t.end_column(&self.db),
                })
            })
            .collect();

        let asset_refs = patterns
            .asset_refs(&self.db)
            .iter()
            .map(|a| {
                let path = a.path(&self.db);
                Arc::new(AssetReferenceData {
                    path: path.path(&self.db).clone(),
                    helper_type: a.helper_type(&self.db),
                    line: a.line(&self.db),
                    column: a.column(&self.db),
                    end_column: a.end_column(&self.db),
                })
            })
            .collect();

        let binding_refs = patterns
            .binding_refs(&self.db)
            .iter()
            .map(|b| {
                let name = b.name(&self.db);
                Arc::new(BindingReferenceData {
                    name: name.name(&self.db).clone(),
                    is_class_reference: b.is_class_reference(&self.db),
                    line: b.line(&self.db),
                    column: b.column(&self.db),
                    end_column: b.end_column(&self.db),
                })
            })
            .collect();

        // Parse route, url, action patterns directly (not cached in Salsa to keep field count under 12)
        // Uses single-pass extraction - query is cached globally so this is fast
        use crate::parser::{language_php, parse_php};
        use crate::queries::extract_all_php_patterns;

        let text = file.text(&self.db);
        let mut route_refs = Vec::new();
        let mut helper_refs: Vec<Arc<HelperReferenceData>> = Vec::new();
        let mut url_refs = Vec::new();
        let mut action_refs = Vec::new();
        let mut feature_refs = Vec::new();
        let mut inertia_refs: Vec<Arc<InertiaReferenceData>> = Vec::new();
        let mut member_access_refs: Vec<Arc<MemberAccessReferenceData>> = Vec::new();
        let mut chains: Vec<Arc<crate::query_chain::BuilderChain>> = Vec::new();

        // Skip the full-file PHP parse for Blade files — same rationale as
        // in parse_file_patterns above. Blade-embedded route/url/action/
        // feature extraction is handled by the `is_blade` block below.
        let path_is_blade = file
            .path(&self.db)
            .to_string_lossy()
            .ends_with(".blade.php");
        // Parse the full-file PHP tree ONCE and keep it — the M1 capture below
        // reuses it (no second tree-sitter pass) to compile per-site receiver
        // recipes. `None` for Blade (parsing a `.blade.php` as PHP is
        // pathologically slow — see `pattern_indexer`).
        let php_tree = if !path_is_blade {
            parse_php(text).ok()
        } else {
            None
        };
        if let Some(tree) = &php_tree {
            {
                let lang = language_php();

                if let Ok(php_patterns) = extract_all_php_patterns(tree, text, &lang) {
                    for r in php_patterns.route_calls {
                        route_refs.push(Arc::new(RouteReferenceData {
                            name: r.route_name.to_string(),
                            line: r.row as u32,
                            column: r.column as u32,
                            end_column: r.end_column as u32,
                        }));
                    }

                    for h in php_patterns.helper_identifiers {
                        helper_refs.push(Arc::new(HelperReferenceData {
                            name: h.name.to_string(),
                            line: h.row as u32,
                            column: h.column as u32,
                            end_column: h.end_column as u32,
                        }));
                    }

                    for u in php_patterns.url_calls {
                        url_refs.push(Arc::new(UrlReferenceData {
                            path: u.url_path.to_string(),
                            line: u.row as u32,
                            column: u.column as u32,
                            end_column: u.end_column as u32,
                        }));
                    }

                    for a in php_patterns.action_calls {
                        action_refs.push(Arc::new(ActionReferenceData {
                            action: a.action_name.to_string(),
                            line: a.row as u32,
                            column: a.column as u32,
                            end_column: a.end_column as u32,
                        }));
                    }

                    for f in php_patterns.feature_calls {
                        feature_refs.push(Arc::new(FeatureReferenceData {
                            feature_name: f.feature_name.to_string(),
                            method_name: f.method_name.to_string(),
                            is_class_reference: f.is_class_reference,
                            line: f.row as u32,
                            column: f.column as u32,
                            end_column: f.end_column as u32,
                        }));
                    }

                    for p in php_patterns.inertia_pages {
                        inertia_refs.push(Arc::new(InertiaReferenceData {
                            name: p.page_name.to_string(),
                            line: p.row as u32,
                            column: p.column as u32,
                            end_column: p.end_column as u32,
                        }));
                    }

                    // Property-form member-access sites (`$user->email`).
                    // Captured raw here (M2); the receiver-resolution fields
                    // stay at their `None`/`Unresolved` defaults until M3.
                    // Blade-embedded member access is intentionally deferred —
                    // resolving Blade-scope receivers is M3 work.
                    for m in php_patterns.member_accesses {
                        member_access_refs.push(Arc::new(MemberAccessReferenceData {
                            member: m.member.to_string(),
                            receiver: m.receiver.to_string(),
                            receiver_byte_start: m.receiver_byte_start,
                            receiver_byte_end: m.receiver_byte_end,
                            is_nullsafe: m.is_nullsafe,
                            form: m.form,
                            line: m.row as u32,
                            column: m.column as u32,
                            end_column: m.end_column as u32,
                            declaring_fqcn: None,
                            kind: None,
                            confidence: Confidence::Unresolved,
                        }));
                    }
                }

                // Extract Eloquent / DB query builder chains from the same
                // parsed tree. No second parse — we reuse the `tree` already
                // produced above for route/url/action/feature extraction.
                for chain in crate::query_chain::extract_chains(tree, text) {
                    chains.push(Arc::new(chain));
                }

                // Keep the class-hierarchy index current for this file. The
                // on-demand parse path (did_open / edits / cache misses) is the
                // ONLY populator for files warming skipped because they were
                // already cached — without this, an open/edited model's own
                // class is absent from the hierarchy, so magic-member
                // resolution (`$this->email` → its declaring class) fails.
                let nodes = crate::class_hierarchy_index::classes_from_tree(path, tree, text);
                // Invalidate the cached class→file snapshot only when this
                // file's set of declared FQCNs actually changed — a method-body
                // edit leaves it intact, keeping the next snapshot O(1).
                let mapping_changed = self.class_hierarchy_index.fqcns_changed(path, &nodes);
                self.class_hierarchy_index.remove_file(path);
                if !nodes.is_empty() {
                    self.class_hierarchy_index.insert_file(path, nodes);
                }
                if mapping_changed {
                    self.class_files_snapshot = None;
                }
            }
        } // end if let Some(tree) = &php_tree  (non-Blade full-file PHP)

        // Blade-embedded PHP: extract route/url/action/feature from every
        // `{{ }}` / `{!! !!}` / `@php` region. Mirrors the Salsa-cached
        // extraction in parse_file_patterns for the kinds that aren't
        // stored in ParsedPatterns. Without this, route('home') inside a
        // Blade nav menu is invisible to find-references.
        if path_is_blade {
            use crate::blade_embedded_php::{adjust_inner_position, extract_php_regions};
            let lang_php = language_php();
            for region in extract_php_regions(text) {
                let wrapped = format!("<?php {}", region.content);
                let Ok(snippet_tree) = parse_php(&wrapped) else {
                    continue;
                };
                let Ok(snippet_patterns) =
                    extract_all_php_patterns(&snippet_tree, &wrapped, &lang_php)
                else {
                    continue;
                };
                for r in snippet_patterns.route_calls {
                    let (line, col) = adjust_inner_position(
                        r.row as u32,
                        r.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        r.row as u32,
                        r.end_column as u32,
                        region.row,
                        region.column,
                    );
                    route_refs.push(Arc::new(RouteReferenceData {
                        name: r.route_name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for h in snippet_patterns.helper_identifiers {
                    let (line, col) = adjust_inner_position(
                        h.row as u32,
                        h.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        h.row as u32,
                        h.end_column as u32,
                        region.row,
                        region.column,
                    );
                    helper_refs.push(Arc::new(HelperReferenceData {
                        name: h.name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for u in snippet_patterns.url_calls {
                    let (line, col) = adjust_inner_position(
                        u.row as u32,
                        u.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        u.row as u32,
                        u.end_column as u32,
                        region.row,
                        region.column,
                    );
                    url_refs.push(Arc::new(UrlReferenceData {
                        path: u.url_path.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for a in snippet_patterns.action_calls {
                    let (line, col) = adjust_inner_position(
                        a.row as u32,
                        a.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        a.row as u32,
                        a.end_column as u32,
                        region.row,
                        region.column,
                    );
                    action_refs.push(Arc::new(ActionReferenceData {
                        action: a.action_name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for f in snippet_patterns.feature_calls {
                    let (line, col) = adjust_inner_position(
                        f.row as u32,
                        f.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        f.row as u32,
                        f.end_column as u32,
                        region.row,
                        region.column,
                    );
                    feature_refs.push(Arc::new(FeatureReferenceData {
                        feature_name: f.feature_name.to_string(),
                        method_name: f.method_name.to_string(),
                        is_class_reference: f.is_class_reference,
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }
                for p in snippet_patterns.inertia_pages {
                    let (line, col) = adjust_inner_position(
                        p.row as u32,
                        p.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        p.row as u32,
                        p.end_column as u32,
                        region.row,
                        region.column,
                    );
                    inertia_refs.push(Arc::new(InertiaReferenceData {
                        name: p.page_name.to_string(),
                        line,
                        column: col,
                        end_column: end_col,
                    }));
                }

                // Property-form member accesses inside this Blade region
                // (`{{ $user->email }}`). Positions are mapped to outer-file
                // coords; byte ranges stay snippet-local (Blade resolution uses
                // the receiver text + view-variable inference, not a whole-file
                // PHP parse).
                for m in snippet_patterns.member_accesses {
                    let (line, col) = adjust_inner_position(
                        m.row as u32,
                        m.column as u32,
                        region.row,
                        region.column,
                    );
                    let (_, end_col) = adjust_inner_position(
                        m.row as u32,
                        m.end_column as u32,
                        region.row,
                        region.column,
                    );
                    member_access_refs.push(Arc::new(MemberAccessReferenceData {
                        member: m.member.to_string(),
                        receiver: m.receiver.to_string(),
                        receiver_byte_start: m.receiver_byte_start,
                        receiver_byte_end: m.receiver_byte_end,
                        is_nullsafe: m.is_nullsafe,
                        form: m.form,
                        line,
                        column: col,
                        end_column: end_col,
                        declaring_fqcn: None,
                        kind: None,
                        confidence: Confidence::Unresolved,
                    }));
                }

                // Eloquent / DB query builder chains inside this Blade-
                // embedded PHP region. Snippet-local byte ranges produced by
                // the extractor reference the `<?php `-wrapped source; shift
                // each range back into outer-file coordinates so the cursor
                // resolver can find them by LSP byte offset.
                use crate::blade_embedded_php::PHP_WRAPPER_PREFIX_LEN;
                for mut chain in crate::query_chain::extract_chains(&snippet_tree, &wrapped) {
                    crate::query_chain::extractor::shift_chain_byte_ranges(
                        &mut chain,
                        region.byte_offset,
                        PHP_WRAPPER_PREFIX_LEN as usize,
                    );
                    chains.push(Arc::new(chain));
                }
            }
        }

        // Capture member accesses inside `@foreach` iterables (`$this->entities`)
        // — directive args the region loop above doesn't reach.
        if path_is_blade {
            for m in blade_loop_iterable_accesses(text) {
                member_access_refs.push(Arc::new(m));
            }
        }

        // Class FQCNs this file imports — Blade `@use` scanned from source, PHP
        // `use` off the tree already parsed for the M1 capture. Same helper
        // `pattern_indexer` uses, so the two constructors emit identical
        // entries.
        let class_refs = class_refs_for(path, php_tree.as_ref(), text);

        let mut data = ParsedPatternsData {
            views,
            inertia_refs,
            components,
            directives,
            class_refs,
            env_refs,
            config_refs,
            livewire_refs,
            middleware_refs,
            translation_refs,
            asset_refs,
            binding_refs,
            route_refs,
            helper_refs,
            url_refs,
            action_refs,
            feature_refs,
            chains,
            member_access_refs,
            // Captured here (source in hand) so the magic-build Blade pass
            // routes Volt vs. controller-rendered resolution without re-reading.
            is_volt: path_is_blade
                && crate::livewire_resolver::source_contains_volt_signature(text),
            blade_loops: if path_is_blade {
                blade_loop_vars(text)
            } else {
                Vec::new()
            },
            // Populated below once `data` is in hand — capture needs the final
            // `member_access_refs` (parallel `sites`) and reuses the PHP tree.
            member_context: None,
            sorted_positions: std::sync::OnceLock::new(),
        };

        // M1 single-parse capture: compile this file's own-source resolution
        // context now, while the tree/source are in hand, so the whole-project
        // magic build never re-reads or re-parses it. Non-vendor only — the
        // build passes all skip vendor, so capturing there would be wasted
        // memory. `.php` reuses `php_tree`; Blade compiles from receiver-text
        // snippets + front-matter (there's no full-file PHP tree for Blade).
        let is_vendor = path.components().any(|c| c.as_os_str() == "vendor");
        if !is_vendor {
            data.member_context = crate::member_capture::capture_member_context(
                path,
                text,
                php_tree.as_ref(),
                &data.member_access_refs,
                data.is_volt,
            )
            .map(Box::new);
        }

        // Win 2: `sorted_positions` is built lazily on first `find_at_position`
        // call, not here — this parse runs on every `handle_get_patterns`
        // request (diagnostics, symbols, hovers, …), most of which never
        // look up a cursor position at all.

        // Wrap in Arc for efficient sharing
        let data = Arc::new(data);

        // Cache the Arc for future requests (cheap Arc::clone on cache hit)
        self.pattern_cache
            .insert(path.clone(), (version, Arc::clone(&data)));

        let total_time = start.elapsed();
        debug!(
            "🔄 Cache MISS for {} - parse: {:?}, total: {:?}, middleware_count: {}",
            file_name,
            parse_time,
            total_time,
            data.middleware_refs.len()
        );

        Some(data)
    }

    // === Config Handlers ===

    /// Handle config file registration
    fn handle_register_config_files(
        &mut self,
        root_path: PathBuf,
        composer_json: Option<String>,
        view_config: Option<String>,
        livewire_config: Option<String>,
    ) {
        self.config_root = Some(root_path.clone());
        self.config_version += 1;
        self.config_cache = None; // Invalidate cache

        // Register composer.json
        if let Some(text) = composer_json {
            let path = root_path.join("composer.json");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }

        // Register config/view.php
        if let Some(text) = view_config {
            let path = root_path.join("config/view.php");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }

        // Register config/livewire.php
        if let Some(text) = livewire_config {
            let path = root_path.join("config/livewire.php");
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }
    }

    /// Handle config file update
    fn handle_update_config_file(&mut self, path: PathBuf, text: String) {
        self.config_version += 1;
        self.config_cache = None; // Invalidate cache

        if let Some(file) = self.config_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.config_version);
            file.set_text(&mut self.db).to(text);
        } else {
            // Create new file
            let file = ConfigFile::new(&self.db, path.clone(), self.config_version, text);
            self.config_files.insert(path, file);
        }
    }

    /// Handle get Laravel config request
    fn handle_get_laravel_config(&mut self) -> Option<LaravelConfigData> {
        let root = self.config_root.clone()?;

        // Check cache first
        if let Some((cached_version, ref cached_data)) = self.config_cache {
            if cached_version == self.config_version {
                return Some(cached_data.clone());
            }
        }

        // Get config files
        let composer = self.config_files.get(&root.join("composer.json")).copied();
        let view_config = self
            .config_files
            .get(&root.join("config/view.php"))
            .copied();
        let livewire_config = self
            .config_files
            .get(&root.join("config/livewire.php"))
            .copied();

        // Use Salsa query to build config
        let config_ref = build_laravel_config(
            &self.db,
            root.clone(),
            composer,
            view_config,
            livewire_config,
        );

        // THE MERGE RULE, for all five registries below: the highest
        // [`MergeRank`] wins — tier priority first (the app boots last, so
        // app > module > package > framework), then `modules.paths` rank, and
        // a full tie goes to the later provider in the deterministic
        // lexicographic order (last-wins). Every map carries its winner's
        // rank while merging and drops it once the winner is settled; none of
        // them restates the rule locally.
        let mut view_namespaces: HashMap<String, (MergeRank, PathBuf)> = HashMap::new();
        let mut component_namespaces: HashMap<String, (MergeRank, String)> = HashMap::new();
        let mut anonymous_component_paths: HashMap<String, (MergeRank, PathBuf)> = HashMap::new();
        let mut anonymous_component_namespaces: HashMap<String, (MergeRank, String)> =
            HashMap::new();
        let mut class_component_files: HashMap<String, (MergeRank, PathBuf)> = HashMap::new();

        /// Does `candidate` take the key from the current holder? The single
        /// comparison every merge below routes through, so the five registries
        /// cannot drift into five rules again (#354 item 4).
        fn wins(existing: Option<&MergeRank>, candidate: MergeRank) -> bool {
            existing.is_none_or(|held| candidate >= *held)
        }

        if let Some(sp_root) = self.salsa_sp_root.as_ref() {
            // Lexicographic provider order (`sorted_sp_files`) so the
            // last-wins tie-break below is deterministic (#255).
            for sp_file in self.sorted_sp_files() {
                // `modules.paths` rank of the module that owns this provider,
                // read through the one shared ownership lookup — a module's
                // provider path sorts lexicographically, which is NOT the
                // configured order the docs promise (#354 item 3).
                let module_rank =
                    crate::config::owning_module(&self.module_dirs, sp_file.path(&self.db))
                        .map_or(0, |(rank, _)| rank);
                let parsed = parse_service_provider_source(&self.db, sp_file, sp_root.clone());

                for vn in parsed.view_namespaces(&self.db) {
                    let ns = vn.namespace(&self.db).namespace(&self.db).clone();
                    let rank = (vn.priority(&self.db), module_rank);
                    if let Some(path) = vn.view_path(&self.db).clone() {
                        if wins(view_namespaces.get(&ns).map(|(r, _)| r), rank) {
                            view_namespaces.insert(ns, (rank, path));
                        }
                    }
                }

                for cn in parsed.component_namespaces(&self.db) {
                    let prefix = cn.prefix(&self.db).namespace(&self.db).clone();
                    let rank = (cn.priority(&self.db), module_rank);
                    if wins(component_namespaces.get(&prefix).map(|(r, _)| r), rank) {
                        component_namespaces
                            .insert(prefix, (rank, cn.php_namespace(&self.db).clone()));
                    }
                }

                // Blade::anonymousComponentPath
                for acp in parsed.anonymous_component_paths(&self.db) {
                    let prefix = acp.prefix(&self.db).namespace(&self.db).clone();
                    let rank = (acp.priority(&self.db), module_rank);
                    if wins(anonymous_component_paths.get(&prefix).map(|(r, _)| r), rank) {
                        anonymous_component_paths
                            .insert(prefix, (rank, acp.directory(&self.db).clone()));
                    }
                }

                // Blade::anonymousComponentNamespace
                for acn in parsed.anonymous_component_namespaces(&self.db) {
                    let prefix = acn.prefix(&self.db).namespace(&self.db).clone();
                    let rank = (acn.priority(&self.db), module_rank);
                    if wins(
                        anonymous_component_namespaces.get(&prefix).map(|(r, _)| r),
                        rank,
                    ) {
                        anonymous_component_namespaces
                            .insert(prefix, (rank, acn.directory(&self.db).clone()));
                    }
                }

                // Blade::component('tag', Class::class), either form/order
                for bc in parsed.blade_components(&self.db) {
                    let Some(file) = bc.file_path(&self.db).clone() else {
                        continue;
                    };
                    let tag = bc.tag_name(&self.db).name(&self.db).clone();
                    let rank = (bc.priority(&self.db), module_rank);
                    if wins(class_component_files.get(&tag).map(|(r, _)| r), rank) {
                        class_component_files.insert(tag, (rank, file));
                    }
                }
            }
        }

        // Also include any from the legacy cache — fallback-only, so it
        // never displaces a live parse (priority 0 on insert).
        for (ns, data) in &self.sp_view_namespaces {
            if let Some(path) = &data.view_path {
                view_namespaces
                    .entry(ns.clone())
                    .or_insert_with(|| ((0, 0), path.clone()));
            }
        }
        for (prefix, data) in &self.sp_component_namespaces {
            component_namespaces
                .entry(prefix.clone())
                .or_insert_with(|| ((0, 0), data.php_namespace.clone()));
        }
        for (tag, data) in &self.sp_blade_components {
            if let Some(file) = &data.file_path {
                class_component_files
                    .entry(tag.clone())
                    .or_insert_with(|| ((data.priority, 0), file.clone()));
            }
        }

        // Livewire v4 registers an anonymous component path for every entry
        // of config('livewire.component_namespaces') at boot (`layouts` and
        // `pages` by default) — a config-driven loop no provider parse can
        // see. Merged last so explicit Blade::anonymousComponentPath
        // registrations win. Not gated on `has_livewire`: that flag only
        // sees direct composer.json requires, while Livewire commonly
        // arrives transitively (Flux, Filament, MaryUI); the loader
        // self-gates on the config files existing.
        for (ns, dir) in crate::config::livewire_component_namespaces(&root) {
            anonymous_component_paths.entry(ns).or_insert(((0, 0), dir));
        }

        // Convert to data transfer type
        let root = config_ref.root(&self.db).clone();
        let component_aliases = crate::config::load_component_aliases(&root);
        let icon_aliases = crate::config::scan_vendor_for_icon_sets(&root);
        let data = LaravelConfigData {
            root,
            view_paths: config_ref.view_paths(&self.db).clone(),
            component_paths: config_ref.component_paths(&self.db).clone(),
            livewire_path: config_ref.livewire_path(&self.db).clone(),
            has_livewire: config_ref.has_livewire(&self.db),
            view_namespaces: view_namespaces
                .into_iter()
                .map(|(ns, (_rank, path))| (ns, path))
                .collect(),
            component_namespaces: component_namespaces
                .into_iter()
                .map(|(prefix, (_rank, php_ns))| (prefix, php_ns))
                .collect(),
            anonymous_component_paths: anonymous_component_paths
                .into_iter()
                .map(|(prefix, (_rank, dir))| (prefix, dir))
                .collect(),
            anonymous_component_namespaces: anonymous_component_namespaces
                .into_iter()
                .map(|(prefix, (_rank, dir))| (prefix, dir))
                .collect(),
            component_aliases,
            icon_aliases,
            class_component_files: class_component_files
                .into_iter()
                .map(|(tag, (_rank, file))| (tag, file))
                .collect(),
        };

        // Cache the result
        self.config_cache = Some((self.config_version, data.clone()));

        Some(data)
    }

    // === Reference Finding Handlers ===

    /// Add or remove a path in the appropriate per-category file list
    /// based on the classification of its absolute path against the
    /// roots captured at `register_project_files` time. Returns the
    /// category label that was mutated, or `None` if the path didn't
    /// match any project root (silently dropped).
    ///
    /// Idempotency: `Add` is a no-op if the path is already in the
    /// list; `Remove` is a no-op if it isn't. This matters because
    /// LSP filesystem-event delivery isn't deduplicated — an atomic
    /// write can produce two `Created` events for the same final
    /// path, and we shouldn't end up with duplicates in `vendor_files`.
    fn handle_update_project_file_list(
        &mut self,
        path: PathBuf,
        op: FileListOp,
    ) -> Option<&'static str> {
        let category = self.project_root_paths.classify(&path)?;
        let list = match category {
            FileCategory::Controller => &mut self.controller_files,
            FileCategory::View => &mut self.view_files,
            FileCategory::Livewire => &mut self.livewire_files,
            FileCategory::Route => &mut self.route_files,
            FileCategory::Vendor => &mut self.vendor_files,
        };
        match op {
            FileListOp::Add => {
                if !list.contains(&path) {
                    list.push(path.clone());
                }
                // Invariant 3 of `component_usage_index`: every `view_files`
                // mutation queues or drops the path, or the walk cannot see it.
                self.component_usage_index.mark_dirty(&path);
            }
            FileListOp::Remove => {
                list.retain(|p| p != &path);
                self.component_usage_index.remove_file(&path);
            }
        }
        Some(category.label())
    }

    /// Handle project files registration
    /// Scans directories and registers all PHP/Blade files with Salsa
    fn handle_register_project_files(
        &mut self,
        root_path: PathBuf,
        controller_paths: Vec<PathBuf>,
        view_paths: Vec<PathBuf>,
        livewire_path: Option<PathBuf>,
        routes_path: PathBuf,
        vendor_files: Vec<PathBuf>,
    ) {
        use walkdir::WalkDir;

        self.project_files_version += 1;

        // Clear existing file lists
        self.controller_files.clear();
        self.view_files.clear();
        // Rebuilt below as the walk re-discovers each view file.
        self.component_usage_index.clear();
        self.livewire_files.clear();
        self.route_files.clear();
        self.vendor_files.clear();
        self.source_files.clear();

        // Capture the absolute roots we're about to walk so the file-
        // watcher handler can classify Created/Deleted events back into
        // the right category list without re-walking. View paths are
        // already absolute (config layer resolves them); the others are
        // relative and need joining to `root_path`.
        let vendor_root = root_path.join("vendor");
        self.project_root_paths = ProjectRootPaths {
            controller_roots: controller_paths.iter().map(|p| root_path.join(p)).collect(),
            view_roots: view_paths.clone(),
            livewire_root: livewire_path.clone(),
            routes_root: Some(root_path.join(&routes_path)),
            vendor_root: if vendor_root.is_dir() {
                Some(vendor_root)
            } else {
                None
            },
        };

        // Scan controller directories
        for controller_path in &controller_paths {
            let full_path = root_path.join(controller_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(ext) = entry.path().extension() {
                            if ext == "php" {
                                let path = entry.path().to_path_buf();
                                self.controller_files.push(path);
                                // No ensure_file_registered: deferred to
                                // first cache miss in handle_get_patterns.
                                // See the comment on handle_get_patterns
                                // for the architectural why.
                            }
                        }
                    }
                }
            }
        }

        // Scan view directories (for Blade files)
        for view_path in &view_paths {
            let full_path = root_path.join(view_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(file_name) = entry.path().file_name() {
                            if file_name.to_string_lossy().ends_with(".blade.php") {
                                let path = entry.path().to_path_buf();
                                self.component_usage_index.mark_dirty(&path);
                                self.view_files.push(path);
                                // Salsa input deferred to first cache miss.
                            }
                        }
                    }
                }
            }
        }

        // Scan Livewire directory
        if let Some(lw_path) = &livewire_path {
            let full_path = root_path.join(lw_path);
            if full_path.exists() {
                for entry in WalkDir::new(&full_path)
                    .into_iter()
                    .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                    .filter_map(|e| e.ok())
                {
                    if entry.file_type().is_file() {
                        if let Some(ext) = entry.path().extension() {
                            if ext == "php" {
                                let path = entry.path().to_path_buf();
                                self.livewire_files.push(path);
                                // Salsa input deferred to first cache miss.
                            }
                        }
                    }
                }
            }
        }

        // Scan routes directory
        let full_routes_path = root_path.join(&routes_path);
        if full_routes_path.exists() {
            for entry in WalkDir::new(&full_routes_path)
                .into_iter()
                .filter_entry(|e| e.file_name().to_str().map(|s| s != ".git").unwrap_or(true))
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file() {
                    if let Some(ext) = entry.path().extension() {
                        if ext == "php" {
                            let path = entry.path().to_path_buf();
                            self.route_files.push(path);
                            // Salsa input deferred to first cache miss.
                        }
                    }
                }
            }
        }

        // Vendor files come from the shared vendor walk (issue #371) instead
        // of a fifth independent `WalkDir` over the Composer tree. The caller
        // filters `.git` out for us — this pass never descended into it — and
        // the set is otherwise identical: every `*.php` and `*.blade.php`,
        // unbounded depth. `.blade.php` needs no separate test because its
        // extension is already `php`.
        //
        // Composer packages can declare Livewire components, routes,
        // controllers, views and translations, so all of them are indexed; the
        // warming-stage filters (skip `*.json.php` data files, drop anything
        // >256KB) keep tree-sitter away from pathological auto-generated
        // content. Salsa inputs are deferred to the first cache miss — see
        // `handle_get_patterns` for the architectural why.
        self.vendor_files = vendor_files;

        // Scan the whole project (minus vendor + noise dirs) for every
        // `*.php` / `*.blade.php`. The categorized scans above cover the
        // navigation features; this broad bucket feeds the magic-member reverse
        // index, whose usages can live in any model / service / job / action /
        // Volt page — not just controllers and Blade views.
        self.source_files = collect_source_files(&root_path);

        // Create the ProjectFiles input
        self.project_files = Some(ProjectFiles::new(
            &self.db,
            self.project_files_version,
            self.controller_files.clone(),
            self.view_files.clone(),
            self.livewire_files.clone(),
            self.route_files.clone(),
        ));

        // The buckets above are the only file-count signal in the process,
        // and this is the moment they're complete — size the pattern cache
        // from them before anything bulk-inserts into it.
        self.size_and_publish_pattern_cache();
    }

    /// Number of distinct files the project walk discovered. Supersets the
    /// categorized buckets (`source_files` covers all non-vendor code, and the
    /// controller/view/livewire/route lists overlap it), so the union is
    /// deduplicated — on borrowed paths, which costs a hash per entry and no
    /// `PathBuf` allocations.
    fn project_file_count(&self) -> usize {
        let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        self.source_files
            .iter()
            .chain(self.controller_files.iter())
            .chain(self.view_files.iter())
            .chain(self.livewire_files.iter())
            .chain(self.route_files.iter())
            .chain(self.vendor_files.iter())
            .filter(|p| seen.insert(p.as_path()))
            .count()
    }

    /// Replace the bootstrap pattern cache with one sized for the project just
    /// walked, then publish it to every [`SalsaHandle`].
    ///
    /// dashmap 6.x has no `reserve` — growing a `DashMap`'s table means
    /// building a new one — so this runs BEFORE the table is shared, and only
    /// ever once. Both halves of that matter:
    ///
    /// * **Before sharing.** The disk-cache load and warming's bulk import
    ///   hold their `Arc` for the whole indexing pass; if the table could be
    ///   swapped underneath them they'd read one map while the actor wrote to
    ///   another. Sizing first and publishing second makes that unreachable
    ///   rather than merely unlikely.
    /// * **Only once.** A mid-session re-registration (a project-root change)
    ///   leaves the existing table in place: [`PATTERN_CACHE_CAPACITY_PADDING`]
    ///   absorbs ordinary growth, and organic per-shard rehashing beyond that
    ///   is far cheaper than the alternative of a swap.
    ///
    /// Any entries already present — an editor may send `didOpen` for open
    /// buffers before registration finishes — are migrated, not dropped.
    fn size_and_publish_pattern_cache(&mut self) {
        if self.pattern_cache_slot.get().is_some() {
            return;
        }

        let target_capacity = self
            .project_file_count()
            .saturating_add(PATTERN_CACHE_CAPACITY_PADDING);
        if self.pattern_cache.capacity() < target_capacity {
            let sized = dashmap::DashMap::with_capacity(target_capacity);
            for entry in self.pattern_cache.iter() {
                sized.insert(entry.key().clone(), entry.value().clone());
            }
            self.pattern_cache = Arc::new(sized);
        }

        let _ = self.pattern_cache_slot.set(Arc::clone(&self.pattern_cache));
    }

    /// Ensure a file is registered with Salsa (read from disk if needed)
    ///
    /// **No containment guard here, deliberately** (#364 sibling-site audit),
    /// and the reason differs from [`Self::ensure_external_php_source_loaded`]
    /// next door. Every call site passes the *request's own*
    /// `textDocument.uri` — the document the client already has open and is
    /// asking about (`handle_get_patterns`,
    /// `handle_find_magic_member_references`, `hover_for_magic_member`,
    /// `handle_resolve_facade_receiver_at`, `handle_magic_member_rename_data`).
    /// That is not a path minted by joining project-derived text onto a
    /// directory, so there is no traversal to fence: the client supplied the
    /// path, holds the file open, and `did_open`/`did_change` already register
    /// arbitrary client paths through `handle_update_file`. The text read here
    /// is parsed for the answer and the path itself is never emitted as a new
    /// navigation target.
    ///
    /// Gating it on the project root would also be a behaviour change, not a
    /// hardening: a file legitimately open outside the workspace root would
    /// stop answering hover and goto entirely.
    fn ensure_file_registered(&mut self, path: &PathBuf) {
        use std::collections::hash_map::Entry;
        // Use entry API to avoid double lookup
        if let Entry::Vacant(entry) = self.files.entry(path.clone()) {
            if let Ok(text) = std::fs::read_to_string(path) {
                let file = SourceFile::new(&self.db, path.clone(), 0, text);
                entry.insert(file);
            }
        }
    }

    /// The Blade templates that render any of `component_names` (`<x-…>`) or
    /// `livewire_names` (`<livewire:…>`) — one step UP the usage graph, for the
    /// item-1 walk from an anonymous partial to the Livewire component that
    /// rendered it (#339).
    ///
    /// Both tag families are read, from the per-file `ParsedPatternsData` the
    /// parse pass already produced: `<x-save-button>` lands in `components` and
    /// `<livewire:save-button>` in `livewire_refs`, and a partial is reachable
    /// through either. Matching is on the parser-classified NAME, never on raw
    /// text, exactly as [`Self::handle_find_view_references`] matches its
    /// directives.
    ///
    /// Only `.blade.php` files are returned. A plain `.php` parent (a class
    /// rendering a tag from a heredoc) is not a rendering ancestor for this
    /// purpose: the `$this` inside the partial belongs to the component whose
    /// TEMPLATE contains the tag, and that template is what the walk needs.
    ///
    /// Sorted and deduped, because the walk takes the first match and an
    /// unsorted result would flap between two parents that both qualify.
    fn handle_files_rendering_component(
        &mut self,
        component_names: &[String],
        livewire_names: &[String],
    ) -> Vec<PathBuf> {
        if component_names.is_empty() && livewire_names.is_empty() {
            return Vec::new();
        }
        self.refresh_component_usage_index();
        self.component_usage_index
            .find(component_names, livewire_names)
    }

    /// Fold every queued Blade file into the reverse component-usage index.
    ///
    /// Empty on the hot path, which is the entire point: only project
    /// registration, an edit, or a watcher event queues anything, so an
    /// ancestor walk visiting a hundred nodes drains once and then answers
    /// each of the hundred lookups out of a `HashMap`. The drain itself reads
    /// through `handle_get_patterns`, so a warmed file costs a cache hit
    /// rather than a parse.
    fn refresh_component_usage_index(&mut self) {
        for path in self.component_usage_index.take_pending() {
            match self.handle_get_patterns(&path) {
                Some(patterns) => self.component_usage_index.insert_file(&path, &patterns),
                // Unreadable now — drop whatever it contributed before rather
                // than serving entries for a file we can no longer confirm.
                None => self.component_usage_index.remove_file(&path),
            }
        }
    }

    /// Handle find view references request
    fn handle_find_view_references(&mut self, view_name: &str) -> Vec<ViewReferenceLocationData> {
        let mut references = Vec::new();

        // Search controller files
        for path in &self.controller_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Controller,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search view files (for @extends, @include directives)
        for path in &self.view_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for directive in &patterns.directives {
                    if directive.name == "extends" || directive.name == "include" {
                        // Extract view name from directive arguments
                        if let Some(ref args) = directive.arguments {
                            let extracted = extract_view_from_args(args);
                            if extracted.as_deref() == Some(view_name) {
                                references.push(ViewReferenceLocationData {
                                    file_path: path.clone(),
                                    line: directive.line,
                                    character: directive.column,
                                    reference_type: FileReferenceType::BladeTemplate,
                                    view_name: view_name.to_string(),
                                    is_route_view: false,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Search Livewire files
        for path in &self.livewire_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::LivewireComponent,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search route files
        for path in &self.route_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Route,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        // Search vendor files — package controllers/views often call
        // `view(...)` against published view namespaces, and find-view-
        // references should surface those alongside user code.
        // FileReferenceType::Controller is used as a catch-all
        // category: there's no `Vendor` variant on the existing enum,
        // and adding one would ripple through every consumer for a
        // cosmetic distinction we don't actually use. The file_path is
        // what matters for navigation.
        for path in &self.vendor_files.clone() {
            if let Some(patterns) = self.handle_get_patterns(path) {
                for view_ref in &patterns.views {
                    if view_ref.name == view_name {
                        references.push(ViewReferenceLocationData {
                            file_path: path.clone(),
                            line: view_ref.line,
                            character: view_ref.column,
                            reference_type: FileReferenceType::Controller,
                            view_name: view_ref.name.clone(),
                            is_route_view: view_ref.is_route_view,
                        });
                    }
                }
            }
        }

        references
    }

    // Cap on the number of dirty files we'll synchronously re-parse
    // inside a single `find_references` call. Past this threshold the
    // actor would block long enough to cause Zed to time out and reset
    // the LSP connection (observed crossing into the tens of seconds at
    // 10k+ dirty entries). When we cross the cap we drop the dirty set
    // on the floor and serve the current index — slightly stale, but
    // alive. Affected files re-index naturally on next save or on the
    // next warming pass.
    //
    // Sized to be comfortably larger than any single bulk import (full
    // vendor parse is ~120 files; a hot edit session typically has tens
    // of dirty files) but small enough that the worst case fits in a
    // tower-lsp request budget.
    const DIRTY_REFRESH_CAP: usize = 1000;

    /// Generic find-references engine. Walks every registered project file and
    /// pulls parser-classified patterns from Salsa for each — matching only
    /// when both the kind and the name agree with `symbol`. The
    /// `include_declaration` flag is honoured for kinds where the parser
    /// distinguishes declaration from usage (currently a no-op since the
    /// parser doesn't tag declarations; reserved for future use).
    ///
    /// Defensive: if the dirty set has more than [`Self::DIRTY_REFRESH_CAP`]
    /// entries (it can blow up to 11k+ on `workspace/didChangeWatchedFiles`
    /// bursts at Zed startup), we skip the per-file re-parse entirely.
    /// Re-parsing thousands of files serially before a single query
    /// freezes the actor long enough that Zed times out the LSP and
    /// resets the connection — a stale-but-live answer beats a dead
    /// server every time.
    /// Resolve the magic member under the cursor and return its indexed usages
    /// (M4). The cursor-side resolution runs here, not in
    /// `classify_pattern_at_cursor`, because it needs the live parse tree plus
    /// the actor-owned class-hierarchy index.
    fn handle_find_member_references(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Vec<ReferenceLocationData> {
        // Primary: the reverse index is already a position→symbol map. If the
        // click lands on a resolved usage (PHP `$this->status`, Blade
        // `$post->status`, Volt `$this->entities`, …) the index knows which
        // symbol that position belongs to — return its references directly. No
        // receiver re-resolution, and (unlike the fallback below) this works in
        // Blade, where re-parsing the whole template as PHP can't locate nodes.
        let indexed = self.symbol_index.references_at(path, line, column);
        if !indexed.is_empty() {
            return indexed;
        }

        // Fallback: live resolution for usages not yet in the index — e.g. a
        // usage typed since the last save-time magic refresh. Resolves the
        // receiver from the cursor file's own PHP AST.
        let Some(patterns) = self.handle_get_patterns(path) else {
            return Vec::new();
        };
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => return Vec::new(),
        };
        let Some(project_root) = self.config_root.clone() else {
            return Vec::new();
        };

        // In-memory source for the cursor file (reflects unsaved edits).
        self.ensure_file_registered(path);
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let text = file.text(&self.db).clone();

        let Ok(tree) = crate::parser::parse_php(&text) else {
            return Vec::new();
        };
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        // Model magic member: resolve the receiver node to its class and key on
        // that. Needs the receiver node (located by byte range — valid for PHP;
        // Blade-embedded refs may not locate, which is fine — the component
        // fallback below is text-based).
        let classviews = crate::member_resolver::ClassViewCache::new();
        let resolver = self.container_aware_resolver();
        if let Some(receiver) = tree
            .root_node()
            .descendant_for_byte_range(member_ref.receiver_byte_start, member_ref.receiver_byte_end)
        {
            if let Some(resolved) = crate::member_resolver::resolve_and_classify(
                receiver,
                &member_ref.member,
                member_ref.form,
                bytes,
                &aliases,
                &resolver,
                &classviews,
                &project_root,
                None, // find-references never wants a builder-method fallback —
                // vendor-forwarded methods aren't renameable/referenceable.
                None, // query-time path — no dependency recording
            ) {
                // find-references threshold: HIGH + MEDIUM.
                if matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
                    return self.symbol_index.find(&SymbolRefData::MagicMember {
                        fqcn: resolved.declaring_fqcn,
                        member: member_ref.member.clone(),
                    });
                }
            }
        }

        // Component-member fallback: `$this->member` in a Livewire/Volt
        // component. The component is often an anonymous class (no FQCN), so it's
        // keyed under a synthetic per-component id shared across its `.php` and
        // `.blade.php`. Text-based, so it works even when the receiver node above
        // didn't locate (Blade template clicks).
        if member_ref.receiver.trim() == "$this" {
            if let Some(key) = crate::view_var_index::volt_component_key(path, &text) {
                return self.symbol_index.find(&SymbolRefData::MagicMember {
                    fqcn: key,
                    member: member_ref.member.clone(),
                });
            }
        }
        Vec::new()
    }

    /// Resolve + classify the magic member at a position for a hover card (M6).
    /// Mirrors the live-resolution path of `handle_find_member_references`, but
    /// returns the classification (kind + declaring class + a declaration link)
    /// rather than references. Gated to HIGH/MEDIUM confidence — we never guess.
    /// Scoped to Eloquent-model magic members (a resolvable declaring FQCN);
    /// component `$this->` members are out of scope for M6.1.
    fn handle_resolve_magic_member_at(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
        builder_index: Option<&crate::laravel_introspector::BuilderMethodIndex>,
    ) -> Option<MagicMemberHoverData> {
        let patterns = self.handle_get_patterns(path)?;
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => {
                info!(
                    "🪄 hover_for_magic_member: no MemberAccess pattern at {:?}:{line}:{column}",
                    path
                );
                return None;
            }
        };
        info!(
            "🪄 hover_for_magic_member: member='{}' form={:?} builder_index={}",
            member_ref.member,
            member_ref.form,
            builder_index.is_some()
        );
        let project_root = self.config_root.clone()?;

        self.ensure_file_registered(path);
        let file = self.files.get(path)?;
        let text = file.text(&self.db).clone();

        let tree = crate::parser::parse_php(&text).ok()?;
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        let classviews = crate::member_resolver::ClassViewCache::new();
        let resolver = self.container_aware_resolver();
        let receiver = tree.root_node().descendant_for_byte_range(
            member_ref.receiver_byte_start,
            member_ref.receiver_byte_end,
        )?;
        // Classify the member; HIGH/MEDIUM only (mirrors find-references). If the
        // member doesn't classify but the receiver still resolves to a model, it
        // may be a plain DB column the source-only ClassView can't see (not in
        // `$casts`) — mark it tentative and let the main side confirm it against
        // migrations/DB.
        let (declaring_fqcn, kind, confidence, tentative) =
            match crate::member_resolver::resolve_and_classify(
                receiver,
                &member_ref.member,
                member_ref.form,
                bytes,
                &aliases,
                &resolver,
                &classviews,
                &project_root,
                builder_index,
                None, // query-time path — no dependency recording
            ) {
                Some(r) if matches!(r.confidence, Confidence::High | Confidence::Medium) => {
                    (r.declaring_fqcn, r.kind, r.confidence, false)
                }
                Some(r) => {
                    info!(
                        "🪄 hover_for_magic_member: classified as {:?} but confidence {:?} too low, dropping",
                        r.kind, r.confidence
                    );
                    return None;
                }
                // An unclassified CALL can't be a column — the tentative-column
                // fallback below is a property-read concept.
                None if member_ref.form.is_call() => {
                    info!(
                        "🪄 hover_for_magic_member: '{}' did not classify at all (call-form, no column fallback)",
                        member_ref.member
                    );
                    return None;
                }
                None => {
                    let (fqcn, confidence) = crate::member_resolver::resolve_expression_type(
                        receiver,
                        bytes,
                        &aliases,
                        &resolver,
                        &classviews,
                        &project_root,
                    )?;
                    if !matches!(confidence, Confidence::High | Confidence::Medium) {
                        return None;
                    }
                    (fqcn, MagicMemberKind::Column, confidence, true)
                }
            };

        // A macro/mixin member's definition site is the registered closure (or
        // mixin method), carried in the macro registry — NOT a method on the
        // declaring (Macroable host) class, which is typically a vendor class
        // with no such method. Look it up directly; no end line (the registry
        // stores only the definition's start).
        if kind == MagicMemberKind::Macro {
            let (decl_file, decl_line) = self
                .build_macro_registry()
                .get(&(declaring_fqcn.clone(), member_ref.member.clone()))
                .map(|m| (Some(m.decl_file.clone()), Some(m.decl_line)))
                .unwrap_or((None, None));
            return Some(MagicMemberHoverData {
                declaring_fqcn,
                member: member_ref.member.clone(),
                kind,
                confidence,
                decl_file,
                decl_line,
                decl_end_line: None,
                tentative: false,
                builder_signature: None,
                builder_summary: None,
            });
        }

        // A builder-forwarded method (`Model::orderByDesc(...)`, `->where(...)`):
        // no real declaration site in the project — `declaring_fqcn` already
        // names the real vendor class (`classify_against`'s fallback set it to
        // the matched `ParsedMethod::source_class`), and the signature/summary
        // come straight from `builder_index`, not a file+line snippet. Look the
        // method back up by name (a second, cheap linear scan over the ~30-200
        // entry merged surface) to pull those strings — `ResolvedMemberAccess`
        // has no room to carry them through the classify step itself.
        if kind == MagicMemberKind::BuilderMethod {
            let (builder_signature, builder_summary) = builder_index
                .and_then(|index| {
                    index
                        .merged_surface()
                        .into_iter()
                        .find(|m| m.name == member_ref.member)
                })
                .map(|m| (Some(m.signature.clone()), m.summary.clone()))
                .unwrap_or((None, None));
            info!(
                "🪄 hover_for_magic_member: '{}' classified as BuilderMethod (declaring={}), signature_found={}",
                member_ref.member,
                declaring_fqcn,
                builder_signature.is_some()
            );
            return Some(MagicMemberHoverData {
                declaring_fqcn,
                member: member_ref.member.clone(),
                kind,
                confidence,
                decl_file: None,
                decl_line: None,
                decl_end_line: None,
                tentative: false,
                builder_signature,
                builder_summary,
            });
        }

        // Locate the declaration in the declaring class. A method-backed member
        // (relationship / scope / accessor / finder) yields both start+end lines
        // so the hover can show its source; a property (column / plain) yields
        // just the start line for the link; otherwise fall back to the class's
        // own start line.
        let (decl_file, decl_line, decl_end_line) =
            match self.class_hierarchy_index.get(&declaring_fqcn) {
                Some(node) => {
                    let candidates = crate::hover::candidate_method_names(kind, &member_ref.member);
                    if let Some(m) = node.methods.iter().find(|m| candidates.contains(&m.name)) {
                        (
                            Some(node.file_path.clone()),
                            Some(m.start_line),
                            Some(m.end_line),
                        )
                    } else if let Some(p) =
                        node.properties.iter().find(|p| p.name == member_ref.member)
                    {
                        (Some(node.file_path.clone()), Some(p.start_line), None)
                    } else if kind == MagicMemberKind::FacadeMethod {
                        // The concrete doesn't declare this member — it forwards
                        // via `__call`/`@mixin` (e.g. `AuthManager` → `Guard::check`).
                        // Chase the real declaration from source; fall back to the
                        // concrete class line only when the chase comes up empty.
                        match crate::facade_resolver::facade_method_decl(
                            &declaring_fqcn,
                            &member_ref.member,
                            &project_root,
                        ) {
                            // Chased to the real declaration — carry its end line
                            // so the hover slices the full method (signature +
                            // docblock + body), matching Intelephense's depth.
                            Some((f, start, end)) => (Some(f), Some(start), Some(end)),
                            None => (Some(node.file_path.clone()), Some(node.start_line), None),
                        }
                    } else {
                        (Some(node.file_path.clone()), Some(node.start_line), None)
                    }
                }
                // A facade's concrete may be a vendor class absent from the
                // hierarchy index — still chase its declaration from disk.
                None if kind == MagicMemberKind::FacadeMethod => {
                    crate::facade_resolver::facade_method_decl(
                        &declaring_fqcn,
                        &member_ref.member,
                        &project_root,
                    )
                    // Carry the end line through the vendor-not-in-index branch
                    // too, so its hover gets the same full-declaration snippet.
                    .map(|(f, start, end)| (Some(f), Some(start), Some(end)))
                    .unwrap_or((None, None, None))
                }
                None => (None, None, None),
            };

        Some(MagicMemberHoverData {
            declaring_fqcn,
            member: member_ref.member.clone(),
            kind,
            confidence,
            decl_file,
            decl_line,
            decl_end_line,
            tentative,
            builder_signature: None,
            builder_summary: None,
        })
    }

    /// Resolve a facade *receiver* at a cursor (`\Auth` in `\Auth::check()`) to
    /// its bound concrete class location. The position index only marks the
    /// method-name token, so this is the goto/hover path when the cursor sits on
    /// the receiver itself: find the enclosing `scoped_call_expression`, confirm
    /// the cursor is on its `scope`, resolve the facade to the concrete it
    /// proxies (`AuthManager`), and locate that class's declaration line.
    fn handle_resolve_facade_receiver_at(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Option<FacadeReceiverTarget> {
        let project_root = self.config_root.clone()?;
        self.ensure_file_registered(path);
        let file = self.files.get(path)?;
        let text = file.text(&self.db).clone();
        let tree = crate::parser::parse_php(&text).ok()?;
        let bytes = text.as_bytes();

        // The node under the cursor, then the static call it belongs to.
        let point = tree_sitter::Point {
            row: line as usize,
            column: column as usize,
        };
        let node = tree.root_node().descendant_for_point_range(point, point)?;
        let mut call = node;
        while call.kind() != "scoped_call_expression" {
            call = call.parent()?;
        }
        let scope = call.child_by_field_name("scope")?;
        // Only fire on the receiver — when the cursor is on the method name the
        // normal member-access goto handles it.
        if node.start_byte() < scope.start_byte() || node.end_byte() > scope.end_byte() {
            return None;
        }

        let raw = scope.utf8_text(bytes).ok()?;
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);
        let resolver = self.container_aware_resolver();
        let (concrete, _) = crate::member_resolver::resolve_facade_receiver(
            scope,
            raw,
            bytes,
            &aliases,
            &resolver,
            &project_root,
        )?;

        // Locate the concrete class's own declaration line.
        let class_file =
            crate::class_locator::find_php_class_file_in_app_or_vendor(&concrete, &project_root)?;
        let class_src = std::fs::read_to_string(&class_file).ok()?;
        let class_tree = crate::parser::parse_php(&class_src).ok()?;
        let structure = crate::laravel_introspector::walker::extract_php_structure_from_tree(
            &class_tree,
            class_src.as_bytes(),
        );
        let short = concrete.rsplit('\\').next().unwrap_or(&concrete);
        let decl_line = structure
            .structures
            .iter()
            .find(|s| s.name == short)
            .map(|s| s.start_line)
            .unwrap_or(0);
        Some(FacadeReceiverTarget {
            fqcn: concrete,
            file: class_file,
            decl_line,
        })
    }

    /// Resolve the magic member at a position for rename (M7). Only
    /// method-backed kinds (relationship / scope / accessor / dynamic finder)
    /// qualify — a column/plain member returns `None` (renaming a DB column is a
    /// migration concern). Returns the declaring method name + file so the
    /// caller can rewrite the declaration (transformed) alongside the call
    /// sites. HIGH/MEDIUM confidence only.
    fn handle_resolve_magic_member_rename_at(
        &mut self,
        path: &PathBuf,
        line: u32,
        column: u32,
    ) -> Option<MagicMemberRenameData> {
        let patterns = self.handle_get_patterns(path)?;
        let member_ref = match patterns.find_at_position(line, column) {
            Some(PatternAtPosition::MemberAccess(m)) => m,
            _ => return None,
        };
        let project_root = self.config_root.clone()?;

        self.ensure_file_registered(path);
        let file = self.files.get(path)?;
        let text = file.text(&self.db).clone();

        let tree = crate::parser::parse_php(&text).ok()?;
        let bytes = text.as_bytes();
        let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, &text);

        let classviews = crate::member_resolver::ClassViewCache::new();
        let resolver = self.container_aware_resolver();
        let receiver = tree.root_node().descendant_for_byte_range(
            member_ref.receiver_byte_start,
            member_ref.receiver_byte_end,
        )?;
        let resolved = crate::member_resolver::resolve_and_classify(
            receiver,
            &member_ref.member,
            member_ref.form,
            bytes,
            &aliases,
            &resolver,
            &classviews,
            &project_root,
            None, // rename never wants a builder-method fallback — a
            // vendor-forwarded method has no declaration here to rewrite.
            None, // query-time path — no dependency recording
        )?;
        if !matches!(resolved.confidence, Confidence::High | Confidence::Medium) {
            return None;
        }
        // Only method-backed kinds rename. A column/plain member can't, and a
        // dynamic finder is EXPLICITLY excluded — `whereEmail` has no declared
        // method to rewrite (it's `__call` sugar over the column; renaming the
        // column is the real operation). Relying on the candidate-method
        // lookup below to miss would make finder renameability an accident of
        // `candidate_method_names`' behavior.
        if !matches!(
            resolved.kind,
            MagicMemberKind::Relationship | MagicMemberKind::Scope | MagicMemberKind::Accessor
        ) {
            return None;
        }

        // Find the declaring method (its real name + file) via the kind-aware
        // candidate names — the same mapping the hover uses.
        let node = self.class_hierarchy_index.get(&resolved.declaring_fqcn)?;
        let candidates = crate::hover::candidate_method_names(resolved.kind, &member_ref.member);
        let method = node.methods.iter().find(|m| candidates.contains(&m.name))?;

        Some(MagicMemberRenameData {
            fqcn: resolved.declaring_fqcn,
            member: member_ref.member.clone(),
            kind: resolved.kind,
            method_name: method.name.clone(),
            decl_file: node.file_path.clone(),
        })
    }

    fn handle_find_references(
        &mut self,
        symbol: &SymbolRefData,
        _include_declaration: bool,
    ) -> Vec<ReferenceLocationData> {
        // Refresh any files whose patterns may have drifted since
        // their entries were last indexed (edits via `didChange`,
        // watcher Created/Changed events, etc.). The dirty set is
        // populated by `handle_update_file` and any other mutator;
        // here we drain it and re-index the affected paths exactly
        // once per query.
        //
        // Borrow note: `handle_get_patterns` takes `&mut self`, and
        // we can't hold a `&mut` on `self.symbol_index` across that
        // call. So `take_dirty` clones the paths out FIRST (releasing
        // the borrow), then we iterate them serially.
        // Magic members are never refreshed by this drain — `insert_file`
        // re-adds only *literal* patterns; magic entries come from the separate
        // resolution pass (warm / save). So the (potentially multi-second)
        // re-parse below can't change a magic-member result — skip straight to
        // the O(1) lookup for them.
        if matches!(symbol, SymbolRefData::MagicMember { .. }) {
            return self.symbol_index.find(symbol);
        }

        let start = std::time::Instant::now();
        let dirty = self.symbol_index.take_dirty();
        let dirty_count = dirty.len();
        if dirty_count > Self::DIRTY_REFRESH_CAP {
            // Safety valve. The dirty set has historically blown up to
            // 11k+ entries during a single warm session (likely from
            // bulk `workspace/didChangeWatchedFiles` events on Zed
            // startup), and re-parsing all of them serially before a
            // single find-references query freezes the actor for tens
            // of seconds — long enough that Zed gives up and resets the
            // connection. When we cross this threshold we skip the
            // refresh entirely and serve the cached index as-is. The
            // result may be slightly stale (entries from files that
            // were edited but not yet reflected in the index), but a
            // partially-stale rename UI is dramatically better than
            // a hung server. The dirty paths are dropped (not
            // re-queued), so the staleness is bounded to "until the
            // affected file is re-saved or re-indexed by warming".
            tracing::warn!(
                "⚠️  find_references: dirty set has {} entries (cap {}), \
                 SKIPPING refresh for {:?} — results may be stale. \
                 This typically means a watched-files burst (e.g. Zed \
                 startup) flooded the index; affected files re-index \
                 on next save.",
                dirty_count,
                Self::DIRTY_REFRESH_CAP,
                symbol
            );
            // Intentional: do NOT re-queue. Re-queuing would just hit
            // this branch again on the next query.
        } else if !dirty.is_empty() {
            tracing::debug!(
                "find_references: refreshing {} dirty file(s) before query for {:?}",
                dirty_count,
                symbol
            );
            for path in dirty {
                // Literal-only eviction: re-parsing restores literals via
                // `insert_file`, but magic members are resolved only by the
                // warm/save passes. A full `remove_file` here would drop this
                // file's magic entries with nothing to restore them until the
                // next save — silently zeroing magic-member counts. Preserve
                // them.
                self.symbol_index.remove_literal_entries(&path);
                if let Some(patterns) = self.handle_get_patterns(&path) {
                    self.symbol_index.insert_file(&path, &patterns);
                }
            }
        }
        let refresh_elapsed = start.elapsed();

        // O(1) lookup — the hot path the whole index exists for.
        let find_start = std::time::Instant::now();
        let results = self.symbol_index.find(symbol);
        let find_elapsed = find_start.elapsed();
        tracing::debug!(
            "find_references: {:?} → {} result(s) (refresh {} dirty in {:?}, lookup {:?})",
            symbol,
            results.len(),
            dirty_count,
            refresh_elapsed,
            find_elapsed,
        );
        results
    }

    /// Build the facade alias snapshot — token → facade FQCN — merging three
    /// sources in ascending precedence: the built-in seed
    /// (`default_facade_aliases`), `config/app.php`'s `aliases` array, then
    /// `bootstrap/app.php`'s `withAliases([...])`. A later source overrides an
    /// earlier one's token (a user `'Auth' => Custom::class` wins over the
    /// default) and adds new tokens. Built fresh each call — the sources number
    /// in the dozens, with no cache to invalidate on a config/bootstrap edit
    /// (mirrors the `SnapshotBindings` rationale).
    fn build_facade_alias_snapshot(&self) -> Arc<HashMap<String, String>> {
        let mut map = crate::facade_resolver::default_facade_aliases();

        // config/app.php 'aliases' (legacy) — overrides the seed.
        if let Some(root) = self.config_root.as_ref() {
            if let Some(file) = self.config_files.get(&root.join("config/app.php")) {
                for (token, fqcn) in crate::config::parse_facade_aliases(file.text(&self.db)) {
                    map.insert(token, fqcn);
                }
            }
        }

        // bootstrap/app.php withAliases (Laravel 11+) — overrides both above.
        if let Some(root) = self.salsa_sp_root.as_ref().or(self.config_root.as_ref()) {
            let bootstrap = root.join("bootstrap/app.php");
            if let Some(file) = self.salsa_sp_files.get(&bootstrap) {
                let text = file.text(&self.db);
                if let Ok(tree) = parse_php(text) {
                    for (token, fqcn) in extract_with_aliases(&tree, text) {
                        map.insert(token, fqcn);
                    }
                }
            }
        }

        Arc::new(map)
    }

    /// The Salsa-parsed provider files in lexicographic path order — the
    /// deterministic merge order for the registry builders. `salsa_sp_files`
    /// is a `HashMap` with unspecified iteration order; merging in that order
    /// made an equal-priority key collision resolve to whichever provider the
    /// map happened to yield first, flipping across LSP restarts (#255).
    /// Combined with the builders' keep-first rule on equal priority, sorting
    /// here makes the winner the provider with the lexicographically smallest
    /// path — stable across restarts.
    fn sorted_sp_files(&self) -> Vec<ServiceProviderFile> {
        let mut entries: Vec<(&PathBuf, &ServiceProviderFile)> =
            self.salsa_sp_files.iter().collect();
        entries.sort_unstable_by_key(|(path, _)| *path);
        entries.into_iter().map(|(_, file)| *file).collect()
    }

    /// Build the macro registry — `(receiver_fqcn, macro_name)` → registration —
    /// by merging every Salsa-parsed service provider's `macros`, highest
    /// priority winning on key collision (framework=0 < package=1 < module=2 <
    /// app=3). Built
    /// fresh each call from the tracked-query outputs (mirrors
    /// [`Self::build_facade_alias_snapshot`]); macros number in the dozens-to-
    /// hundreds, with no cache to invalidate on a provider edit.
    ///
    /// Providers merge in lexicographic path order ([`Self::sorted_sp_files`]),
    /// so an equal-priority key collision deterministically resolves to the
    /// provider with the smallest path (#255).
    fn build_macro_registry(&self) -> Arc<HashMap<(String, String), MacroRegistrationData>> {
        let mut map: HashMap<(String, String), MacroRegistrationData> = HashMap::new();
        let Some(root) = self.salsa_sp_root.as_ref() else {
            return Arc::new(map);
        };
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for m in parsed.macros(&self.db) {
                let key = (
                    m.receiver_fqcn(&self.db).name(&self.db).clone(),
                    m.macro_name(&self.db).name(&self.db).clone(),
                );
                let data = MacroRegistrationData {
                    receiver_fqcn: key.0.clone(),
                    macro_name: key.1.clone(),
                    decl_file: m.decl_file(&self.db).clone(),
                    decl_line: m.decl_line(&self.db),
                    priority: m.priority(&self.db),
                };
                match map.get(&key) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        map.insert(key, data);
                    }
                }
            }
        }
        Arc::new(map)
    }

    /// One provider file's own registration contribution, sorted for the
    /// save path's pre/post diff (#255). Uniform across the three registries:
    /// macros and bindings parse from the file's `ServiceProviderFile` input;
    /// the facade-alias sources are `bootstrap/app.php` (`withAliases`, also a
    /// provider input) and `config/app.php` (`aliases`, a config input).
    /// A path the actor doesn't know yields the empty (default) contribution.
    fn handle_file_provider_registrations(&self, path: &Path) -> ProviderRegistrationsData {
        let mut out = ProviderRegistrationsData::default();
        if let (Some(root), Some(sp_file)) = (
            self.salsa_sp_root.as_ref(),
            self.salsa_sp_files.get(path).copied(),
        ) {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for m in parsed.macros(&self.db) {
                out.macros.push((
                    m.receiver_fqcn(&self.db).name(&self.db).clone(),
                    m.macro_name(&self.db).name(&self.db).clone(),
                ));
            }
            for binding in parsed.bindings(&self.db) {
                out.bindings.push((
                    binding.abstract_name(&self.db).name(&self.db).clone(),
                    binding
                        .concrete_class(&self.db)
                        .trim_start_matches('\\')
                        .to_string(),
                ));
            }
            if *path == root.join("bootstrap/app.php") {
                let text = sp_file.text(&self.db);
                if let Ok(tree) = parse_php(text) {
                    out.aliases.extend(extract_with_aliases(&tree, text));
                }
            }
        }
        if let Some(root) = self.config_root.as_ref() {
            if *path == root.join("config/app.php") {
                if let Some(file) = self.config_files.get(path) {
                    out.aliases
                        .extend(crate::config::parse_facade_aliases(file.text(&self.db)));
                }
            }
        }
        out.macros.sort_unstable();
        out.bindings.sort_unstable();
        out.aliases.sort_unstable();
        out
    }

    // === Service Provider Handlers ===

    /// Handle service provider registry registration
    fn handle_register_service_provider_registry(
        &mut self,
        middleware_aliases: HashMap<String, MiddlewareRegistrationData>,
        bindings: HashMap<String, BindingRegistrationData>,
        singletons: HashMap<String, BindingRegistrationData>,
    ) {
        self.sp_middleware_aliases = middleware_aliases;
        self.sp_bindings = bindings;
        self.sp_singletons = singletons;
    }

    /// Handle get middleware by alias
    fn handle_get_middleware_by_alias(&self, alias: &str) -> Option<MiddlewareRegistrationData> {
        self.sp_middleware_aliases
            .get(middleware_base_alias(alias))
            .cloned()
    }

    /// Handle get binding by name
    fn handle_get_binding_by_name(&self, name: &str) -> Option<BindingRegistrationData> {
        // Check bindings first, then singletons
        self.sp_bindings
            .get(name)
            .cloned()
            .or_else(|| self.sp_singletons.get(name).cloned())
    }

    /// A container-aware [`ClassFileResolver`](crate::member_resolver::ClassFileResolver)
    /// over the actor's class index + binding registry, for the live query path
    /// (find-references fallback, hover, rename).
    fn container_aware_resolver(&self) -> ContainerAwareResolver<'_> {
        ContainerAwareResolver {
            index: &self.class_hierarchy_index,
            bindings: &self.sp_bindings,
            singletons: &self.sp_singletons,
            facade_aliases: self.build_facade_alias_snapshot(),
            macros: self.build_macro_registry(),
        }
    }

    /// Handle get view namespace by name (queries Salsa-parsed service providers)
    fn handle_get_view_namespace(&self, namespace: &str) -> Option<ViewNamespaceData> {
        // First check the legacy cache
        if let Some(data) = self.sp_view_namespaces.get(namespace) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ViewNamespaceData> = None;

        // Lexicographic provider order — deterministic equal-priority winner (#255).
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for vn in parsed.view_namespaces(&self.db) {
                if vn.namespace(&self.db).namespace(&self.db) == namespace {
                    let data = ViewNamespaceData {
                        namespace: vn.namespace(&self.db).namespace(&self.db).clone(),
                        view_path: vn.view_path(&self.db).clone(),
                        source_file: vn.source_file(&self.db).clone(),
                        source_line: vn.source_line(&self.db),
                        priority: vn.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all view namespaces
    fn handle_get_all_view_namespaces(&self) -> Vec<ViewNamespaceData> {
        let mut merged: HashMap<String, ViewNamespaceData> = self.sp_view_namespaces.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            // Lexicographic provider order — deterministic equal-priority
            // winner per key (#255); output order stays map-arbitrary.
            for sp_file in self.sorted_sp_files() {
                let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
                for vn in parsed.view_namespaces(&self.db) {
                    let ns = vn.namespace(&self.db).namespace(&self.db).clone();
                    let data = ViewNamespaceData {
                        namespace: ns.clone(),
                        view_path: vn.view_path(&self.db).clone(),
                        source_file: vn.source_file(&self.db).clone(),
                        source_line: vn.source_line(&self.db),
                        priority: vn.priority(&self.db),
                    };

                    match merged.get(&ns) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(ns, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle get Blade component registration by tag name
    fn handle_get_blade_component_reg(&self, tag_name: &str) -> Option<BladeComponentRegData> {
        // First check the legacy cache
        if let Some(data) = self.sp_blade_components.get(tag_name) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<BladeComponentRegData> = None;

        // Lexicographic provider order — deterministic equal-priority winner (#255).
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for bc in parsed.blade_components(&self.db) {
                if bc.tag_name(&self.db).name(&self.db) == tag_name {
                    let data = BladeComponentRegData {
                        tag_name: bc.tag_name(&self.db).name(&self.db).clone(),
                        class_name: bc.class_name(&self.db).clone(),
                        file_path: bc.file_path(&self.db).clone(),
                        source_file: bc.source_file(&self.db).clone(),
                        source_line: bc.source_line(&self.db),
                        priority: bc.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all Blade component registrations
    fn handle_get_all_blade_component_regs(&self) -> Vec<BladeComponentRegData> {
        let mut merged: HashMap<String, BladeComponentRegData> = self.sp_blade_components.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            // Lexicographic provider order — deterministic equal-priority
            // winner per key (#255); output order stays map-arbitrary.
            for sp_file in self.sorted_sp_files() {
                let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
                for bc in parsed.blade_components(&self.db) {
                    let tag = bc.tag_name(&self.db).name(&self.db).clone();
                    let data = BladeComponentRegData {
                        tag_name: tag.clone(),
                        class_name: bc.class_name(&self.db).clone(),
                        file_path: bc.file_path(&self.db).clone(),
                        source_file: bc.source_file(&self.db).clone(),
                        source_line: bc.source_line(&self.db),
                        priority: bc.priority(&self.db),
                    };

                    match merged.get(&tag) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(tag, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle get component namespace by prefix
    fn handle_get_component_namespace(&self, prefix: &str) -> Option<ComponentNamespaceData> {
        // First check the legacy cache
        if let Some(data) = self.sp_component_namespaces.get(prefix) {
            return Some(data.clone());
        }

        // Then query Salsa-parsed service providers
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ComponentNamespaceData> = None;

        // Lexicographic provider order — deterministic equal-priority winner (#255).
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for cn in parsed.component_namespaces(&self.db) {
                if cn.prefix(&self.db).namespace(&self.db) == prefix {
                    let data = ComponentNamespaceData {
                        prefix: cn.prefix(&self.db).namespace(&self.db).clone(),
                        php_namespace: cn.php_namespace(&self.db).clone(),
                        source_file: cn.source_file(&self.db).clone(),
                        source_line: cn.source_line(&self.db),
                        priority: cn.priority(&self.db),
                    };
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle get all component namespaces
    fn handle_get_all_component_namespaces(&self) -> Vec<ComponentNamespaceData> {
        let mut merged: HashMap<String, ComponentNamespaceData> =
            self.sp_component_namespaces.clone();

        if let Some(root) = self.salsa_sp_root.as_ref() {
            // Lexicographic provider order — deterministic equal-priority
            // winner per key (#255); output order stays map-arbitrary.
            for sp_file in self.sorted_sp_files() {
                let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
                for cn in parsed.component_namespaces(&self.db) {
                    let pfx = cn.prefix(&self.db).namespace(&self.db).clone();
                    let data = ComponentNamespaceData {
                        prefix: pfx.clone(),
                        php_namespace: cn.php_namespace(&self.db).clone(),
                        source_file: cn.source_file(&self.db).clone(),
                        source_line: cn.source_line(&self.db),
                        priority: cn.priority(&self.db),
                    };

                    match merged.get(&pfx) {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => {
                            merged.insert(pfx, data);
                        }
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    // === Salsa-based Environment Variable Handlers (New) ===

    /// Handle registering a raw env file for Salsa to parse
    fn handle_register_env_source(&mut self, path: PathBuf, text: String, priority: u8) {
        use salsa::Setter;
        self.salsa_env_version += 1;

        if let Some(file) = self.salsa_env_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.salsa_env_version);
            file.set_text(&mut self.db).to(text);
            file.set_priority(&mut self.db).to(priority);
        } else {
            // Create new file
            let file = EnvFile::new(
                &self.db,
                path.clone(),
                self.salsa_env_version,
                text,
                priority,
            );
            self.salsa_env_files.insert(path, file);
        }
    }

    // === Salsa-based Translation Handlers (issue #293) ===

    /// Resolve one translation key in one locale through the Salsa cache.
    fn handle_resolve_translation(
        &mut self,
        root: &Path,
        key: &str,
        locale: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
    ) -> Option<ResolvedTranslationData> {
        self.translations
            .resolve(&mut self.db, root, key, locale, vendor_map)
    }

    /// Every locale that could define `key`, this project's APP_LOCALE first.
    fn handle_available_locales(
        &mut self,
        root: &Path,
        key: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
    ) -> Vec<String> {
        let app_locale = self.app_locale();
        self.translations
            .locales(&mut self.db, root, key, vendor_map, app_locale.as_deref())
    }

    /// The project's `APP_LOCALE`, served from the Salsa env cache.
    ///
    /// Reproduces `config::read_env_value(root, "APP_LOCALE")` exactly rather
    /// than taking whatever the env layer ranks highest: that helper reads
    /// **`.env` only**, its line-anchored regex never matches a commented line,
    /// and it discards an empty value. Hence the three filters — priority 2 is
    /// `.env` (1 is `.env.local`, 0 is `.env.example`). Without them, locale
    /// *ordering* would silently change on any project whose `.env.local` or
    /// `.env.example` sets a locale its `.env` does not.
    fn app_locale(&self) -> Option<String> {
        self.handle_get_parsed_env_var("APP_LOCALE")
            .filter(|var| !var.is_commented && var.priority == 2 && !var.value.is_empty())
            .map(|var| var.value)
    }

    /// Push a lang catalogue's authoritative text into Salsa.
    fn handle_register_lang_source(&mut self, path: PathBuf, text: String) {
        self.translations.register(&mut self.db, path, text);
    }

    /// Drop a lang path's cached entry so the next lookup re-reads disk.
    fn handle_invalidate_lang_path(&mut self, path: &Path) {
        self.translations.invalidate(path);
    }

    /// Drop a config path's cached text so the next completion re-reads disk.
    fn handle_invalidate_config_path(&mut self, path: &Path) {
        self.translations.invalidate_config(path);
    }

    /// Whether `candidate` replaces `current` as the merged declaration of a
    /// key. The one rule both env merges below resolve ties with.
    ///
    /// The file-priority ladder decides first: `.env` (2) outranks
    /// `.env.local` (1) outranks `.env.example` (0). Every declaration *inside*
    /// one file carries that file's priority and so always ties, and there an
    /// active declaration outranks a commented one — `# KEY=old` above
    /// `KEY=new` is one declaration and one comment, not two candidates, and
    /// keeping the first line seen let the comment answer for the key. Beyond
    /// that the first line seen still wins.
    ///
    /// A commented declaration in a higher-priority file still outranks an
    /// active one below it: commenting a key out in `.env` is how a project
    /// turns it off, and the ladder is what gives `.env` the last word.
    fn env_var_supersedes(candidate: &ParsedEnvVarData, current: &ParsedEnvVarData) -> bool {
        match candidate.priority.cmp(&current.priority) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => current.is_commented && !candidate.is_commented,
            std::cmp::Ordering::Less => false,
        }
    }

    /// Handle getting a parsed env variable by name from Salsa
    fn handle_get_parsed_env_var(&self, name: &str) -> Option<ParsedEnvVarData> {
        // Find the variable with the highest priority
        let mut best: Option<ParsedEnvVarData> = None;

        for env_file in self.salsa_env_files.values() {
            let parsed_vars = parse_env_source(&self.db, *env_file);
            for var in parsed_vars {
                if var.name(&self.db).name(&self.db) == name {
                    let data = ParsedEnvVarData {
                        name: var.name(&self.db).name(&self.db).clone(),
                        value: var.value(&self.db).clone(),
                        line: var.line(&self.db),
                        column: var.column(&self.db),
                        value_column: var.value_column(&self.db),
                        is_commented: var.is_commented(&self.db),
                        priority: var.priority(&self.db),
                        source_file: var.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if !Self::env_var_supersedes(&data, existing) => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed env variables from Salsa
    fn handle_get_all_parsed_env_vars(&self) -> Vec<ParsedEnvVarData> {
        use std::collections::HashMap;

        // Merge variables by name, higher priority wins
        let mut merged: HashMap<String, ParsedEnvVarData> = HashMap::new();

        for env_file in self.salsa_env_files.values() {
            let parsed_vars = parse_env_source(&self.db, *env_file);
            for var in parsed_vars {
                let name = var.name(&self.db).name(&self.db).clone();
                let data = ParsedEnvVarData {
                    name: name.clone(),
                    value: var.value(&self.db).clone(),
                    line: var.line(&self.db),
                    column: var.column(&self.db),
                    value_column: var.value_column(&self.db),
                    is_commented: var.is_commented(&self.db),
                    priority: var.priority(&self.db),
                    source_file: var.source_file(&self.db).clone(),
                };

                match merged.get(&name) {
                    Some(existing) if !Self::env_var_supersedes(&data, existing) => {}
                    _ => {
                        merged.insert(name, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    // === Salsa-based Service Provider Handlers (New) ===

    /// Handle registering a raw service provider file for Salsa to parse
    fn handle_register_service_provider_source(
        &mut self,
        path: PathBuf,
        text: String,
        priority: u8,
        root_path: PathBuf,
    ) {
        use salsa::Setter;
        self.salsa_sp_version += 1;
        self.salsa_sp_root = Some(root_path);

        // The Laravel config's namespace maps (view namespaces, component
        // namespaces, and anonymous-component paths/namespaces) are derived by
        // parsing these service-provider files. Registering or updating one can
        // therefore change the config, so the memoized config_cache must be
        // dropped — otherwise `get_laravel_config` keeps serving the config that
        // was built before this provider was known, and namespaced components
        // resolve as "not found". Bumping salsa_sp_version alone is not enough;
        // config_cache is keyed on config_version, which this doesn't touch.
        self.config_cache = None;

        if let Some(file) = self.salsa_sp_files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(self.salsa_sp_version);
            file.set_text(&mut self.db).to(text);
            file.set_priority(&mut self.db).to(priority);
        } else {
            // Create new file
            let file = ServiceProviderFile::new(
                &self.db,
                path.clone(),
                self.salsa_sp_version,
                text,
                priority,
            );
            self.salsa_sp_files.insert(path, file);
        }
    }

    /// Handle getting middleware by alias from Salsa-parsed service providers
    ///
    /// Strips parameters from the alias before matching — `auth:sanctum` and
    /// `throttle:60,1` resolve to the `auth` and `throttle` aliases respectively.
    fn handle_get_parsed_middleware(&self, alias: &str) -> Option<ParsedMiddlewareData> {
        let base_alias = middleware_base_alias(alias);
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ParsedMiddlewareData> = None;

        // Lexicographic provider order — deterministic equal-priority winner (#255).
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for mw in parsed.middleware(&self.db) {
                if mw.alias(&self.db).name(&self.db) == base_alias {
                    let data = ParsedMiddlewareData {
                        alias: mw.alias(&self.db).name(&self.db).clone(),
                        class_name: mw.class_name(&self.db).clone(),
                        file_path: mw.file_path(&self.db).clone(),
                        source_line: mw.source_line(&self.db),
                        priority: mw.priority(&self.db),
                        source_file: mw.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed middleware from Salsa
    fn handle_get_all_parsed_middleware(&self) -> Vec<ParsedMiddlewareData> {
        let root = match self.salsa_sp_root.as_ref() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut merged: HashMap<String, ParsedMiddlewareData> = HashMap::new();

        // Lexicographic provider order — deterministic equal-priority
        // winner per key (#255); output order stays map-arbitrary.
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for mw in parsed.middleware(&self.db) {
                let alias = mw.alias(&self.db).name(&self.db).clone();
                let data = ParsedMiddlewareData {
                    alias: alias.clone(),
                    class_name: mw.class_name(&self.db).clone(),
                    file_path: mw.file_path(&self.db).clone(),
                    source_line: mw.source_line(&self.db),
                    priority: mw.priority(&self.db),
                    source_file: mw.source_file(&self.db).clone(),
                };

                match merged.get(&alias) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        merged.insert(alias, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle getting a binding by name from Salsa-parsed service providers
    fn handle_get_parsed_binding(&self, name: &str) -> Option<ParsedBindingData> {
        let root = self.salsa_sp_root.as_ref()?;
        let mut best: Option<ParsedBindingData> = None;

        // Lexicographic provider order — deterministic equal-priority winner
        // (#255); mirrors [`Self::handle_get_all_parsed_bindings`].
        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for binding in parsed.bindings(&self.db) {
                if binding.abstract_name(&self.db).name(&self.db) == name {
                    let data = ParsedBindingData {
                        abstract_name: binding.abstract_name(&self.db).name(&self.db).clone(),
                        concrete_class: binding.concrete_class(&self.db).clone(),
                        file_path: binding.file_path(&self.db).clone(),
                        binding_type: binding.binding_type(&self.db),
                        source_line: binding.source_line(&self.db),
                        priority: binding.priority(&self.db),
                        source_file: binding.source_file(&self.db).clone(),
                    };
                    // Keep the one with highest priority
                    match &best {
                        Some(existing) if existing.priority >= data.priority => {}
                        _ => best = Some(data),
                    }
                }
            }
        }

        best
    }

    /// Handle getting all parsed bindings from Salsa
    ///
    /// Providers merge in lexicographic path order ([`Self::sorted_sp_files`]),
    /// so an equal-priority key collision deterministically resolves to the
    /// provider with the smallest path (#255) — mirrors
    /// [`Self::build_macro_registry`].
    fn handle_get_all_parsed_bindings(&self) -> Vec<ParsedBindingData> {
        let root = match self.salsa_sp_root.as_ref() {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut merged: HashMap<String, ParsedBindingData> = HashMap::new();

        for sp_file in self.sorted_sp_files() {
            let parsed = parse_service_provider_source(&self.db, sp_file, root.clone());
            for binding in parsed.bindings(&self.db) {
                let name = binding.abstract_name(&self.db).name(&self.db).clone();
                let data = ParsedBindingData {
                    abstract_name: name.clone(),
                    concrete_class: binding.concrete_class(&self.db).clone(),
                    file_path: binding.file_path(&self.db).clone(),
                    binding_type: binding.binding_type(&self.db),
                    source_line: binding.source_line(&self.db),
                    priority: binding.priority(&self.db),
                    source_file: binding.source_file(&self.db).clone(),
                };

                match merged.get(&name) {
                    Some(existing) if existing.priority >= data.priority => {}
                    _ => {
                        merged.insert(name, data);
                    }
                }
            }
        }

        merged.into_values().collect()
    }

    /// Handle registering a middleware entry from disk cache
    fn handle_register_cached_middleware(
        &mut self,
        alias: String,
        class: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) {
        // Store in the simple registry (same as register_service_provider_registry)
        self.sp_middleware_aliases.insert(
            alias.clone(),
            MiddlewareRegistrationData {
                alias,
                class_name: class,
                file_path: class_file.map(PathBuf::from),
                source_file: source_file.map(PathBuf::from),
                source_line: Some(line as usize),
                priority: 3, // Cache entries are app tier — the highest
            },
        );
    }

    /// Handle registering a binding entry from disk cache
    fn handle_register_cached_binding(
        &mut self,
        name: String,
        class: String,
        binding_type: String,
        class_file: Option<String>,
        source_file: Option<String>,
        line: u32,
    ) {
        let bt = match binding_type.as_str() {
            "singleton" => BindingTypeData::Singleton,
            "scoped" => BindingTypeData::Scoped,
            "alias" => BindingTypeData::Alias,
            _ => BindingTypeData::Bind,
        };

        // Store in the simple registry
        self.sp_bindings.insert(
            name.clone(),
            BindingRegistrationData {
                abstract_name: name,
                concrete_class: class,
                file_path: class_file.map(PathBuf::from),
                binding_type: bt,
                source_file: source_file.map(PathBuf::from),
                source_line: Some(line as usize),
                priority: 3, // Cache entries are app tier — the highest
            },
        );
    }
}

/// Extract view name from directive arguments (e.g., "('layouts.app')" -> "layouts.app")
fn extract_view_from_args(args: &str) -> Option<String> {
    let trimmed = args.trim().trim_matches('(').trim_matches(')').trim();
    let unquoted = trimmed.trim_matches('\'').trim_matches('"');
    if !unquoted.is_empty() && !unquoted.contains(',') {
        Some(unquoted.to_string())
    } else {
        None
    }
}

/// Directories never worth walking for project source: dependency trees, VCS
/// metadata, and runtime/cache output. `vendor` is excluded here because it's
/// scanned separately (with its own size/noise filters at warm time).
const SKIP_SCAN_DIRS: &[&str] = &["vendor", "node_modules", ".git", "storage", ".cache"];

/// Collect every non-vendor `*.php` / `*.blade.php` under `root`, skipping
/// dependency and runtime dirs. Feeds the magic-member reverse index, whose
/// usages can live in any source file — not just controllers and Blade views.
/// (`.blade.php` is included because it also ends with `.php`.)
pub fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    use walkdir::WalkDir;
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|s| !SKIP_SCAN_DIRS.contains(&s))
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".php") {
            out.push(entry.path().to_path_buf());
        }
    }
    out
}

/// Derive a file's class references from its imports.
///
/// Blade templates contribute their `@use` directives, scanned from the source
/// because the class names live inside string literals; `.php` files contribute
/// their `use` statements off `php_tree` — the full-file parse both constructors
/// already hold for the M1 capture, so this costs no extra pass. `path` selects
/// the branch, so a `.php` file containing the *text* `@use('…')` inside a
/// string is never mistaken for a Blade import.
///
/// A Blade file's embedded PHP regions are scanned for `use` statements too.
/// In practice that means a Volt single-file component's `<?php … ?>` front
/// matter, which the Blade-directive scan cannot see — leaving it out would let
/// a class rename miss a Volt component's imports entirely.
///
/// Shared by both `ParsedPatternsData` constructors (`handle_get_patterns` and
/// `pattern_indexer::parse_owned_with_hierarchy`) so a file's class refs never
/// depend on which one built it.
pub fn class_refs_for(
    path: &Path,
    php_tree: Option<&tree_sitter::Tree>,
    text: &str,
) -> Vec<Arc<ClassReferenceData>> {
    let mut out: Vec<Arc<ClassReferenceData>> = Vec::new();

    if path.to_string_lossy().ends_with(".blade.php") {
        for import in crate::query_chain::use_aliases::blade_use_imports(text) {
            let (line, column) = byte_line_col(text, import.name.0);
            let (_, end_column) = byte_line_col(text, import.name.1);
            out.push(Arc::new(ClassReferenceData {
                name: import.fqcn,
                line,
                column,
                end_column,
            }));
        }
        // A Blade file's embedded PHP regions — chiefly a Volt single-file
        // component's `<?php … ?>` front matter, which carries real PHP `use`
        // statements. Skipping them would let a class rename miss a Volt
        // component's imports entirely.
        let prefix = crate::blade_embedded_php::PHP_WRAPPER_PREFIX_LEN;
        for region in crate::blade_embedded_php::extract_php_regions(text) {
            let wrapped = format!("<?php {}", region.content);
            let Ok(tree) = crate::parser::parse_php(&wrapped) else {
                continue;
            };
            for i in crate::query_chain::use_aliases::php_use_class_refs(&tree, &wrapped) {
                let (line, column) = crate::blade_embedded_php::adjust_inner_position(
                    i.line,
                    i.column,
                    region.row,
                    region.column,
                );
                // `adjust_inner_position` only un-shifts the wrapper on row 0;
                // the end column needs the same treatment to stay paired.
                let end_column = if i.line == 0 {
                    region.column + i.end_column.saturating_sub(prefix)
                } else {
                    i.end_column
                };
                out.push(Arc::new(ClassReferenceData {
                    name: i.fqcn,
                    line,
                    column,
                    end_column,
                }));
            }
        }
    } else if let Some(tree) = php_tree {
        out.extend(
            crate::query_chain::use_aliases::php_use_class_refs(tree, text)
                .into_iter()
                .map(|i| {
                    Arc::new(ClassReferenceData {
                        name: i.fqcn,
                        line: i.line,
                        column: i.column,
                        end_column: i.end_column,
                    })
                }),
        );
    }

    out.sort_by_key(|c| (c.line, c.column));
    out
}

/// 0-based (line, **byte** column) of `offset` in `text`.
///
/// Deliberately not `query_chain::byte_offset_to_position`, which counts chars
/// for the LSP wire format: every pattern position in this module is a byte
/// column, matching tree-sitter `Point`s.
fn byte_line_col(text: &str, offset: usize) -> (u32, u32) {
    let clamped = offset.min(text.len());
    let before = &text[..clamped];
    match before.rfind('\n') {
        Some(nl) => (
            before.matches('\n').count() as u32,
            (clamped - nl - 1) as u32,
        ),
        None => (0, clamped as u32),
    }
}

/// Append every parser-classified reference in `patterns` that matches `symbol`
/// into `out`. Only the pattern collection corresponding to the symbol kind is
/// scanned — a `SymbolRef::Route` never matches a coincidental config-key
/// string, etc. This is the "instance chain" enforcement point.
fn collect_matches_for_symbol(
    path: &Path,
    patterns: &ParsedPatternsData,
    symbol: &SymbolRefData,
    out: &mut Vec<ReferenceLocationData>,
) {
    let push = |out: &mut Vec<ReferenceLocationData>, line, column, end_column| {
        out.push(ReferenceLocationData {
            file_path: path.to_path_buf(),
            line,
            column,
            end_column,
        });
    };

    match symbol {
        SymbolRefData::View(name) => {
            for v in &patterns.views {
                if v.name == *name {
                    push(out, v.line, v.column, v.end_column);
                }
            }
            // Blade directives can reference views too (@include, @extends,
            // @component, @each). The parser stores the raw argument string;
            // unwrap to the contained view name before comparing.
            for d in &patterns.directives {
                if matches!(
                    d.name.as_str(),
                    "include" | "extends" | "component" | "each" | "includeIf" | "includeWhen"
                ) {
                    if let Some(args) = d.arguments.as_deref() {
                        if extract_view_from_args(args).as_deref() == Some(name.as_str()) {
                            push(out, d.line, d.string_column, d.string_end_column);
                        }
                    }
                }
            }
        }
        SymbolRefData::Route(name) => {
            for r in &patterns.route_refs {
                if r.name == *name {
                    push(out, r.line, r.column, r.end_column);
                }
            }
        }
        SymbolRefData::Class(fqcn) => {
            for c in &patterns.class_refs {
                if c.name == *fqcn {
                    push(out, c.line, c.column, c.end_column);
                }
            }
        }
        SymbolRefData::Config(key) => {
            for c in &patterns.config_refs {
                if c.key == *key {
                    push(out, c.line, c.column, c.end_column);
                }
            }
        }
        SymbolRefData::Translation(key) => {
            for t in &patterns.translation_refs {
                if t.key == *key {
                    push(out, t.line, t.column, t.end_column);
                }
            }
        }
        SymbolRefData::Env(name) => {
            for e in &patterns.env_refs {
                if e.name == *name {
                    push(out, e.line, e.column, e.end_column);
                }
            }
        }
        SymbolRefData::Component(name) => {
            for c in &patterns.components {
                if c.name == *name {
                    push(out, c.line, c.column, c.end_column);
                }
            }
        }
        SymbolRefData::Livewire(name) => {
            for l in &patterns.livewire_refs {
                if l.name == *name {
                    push(out, l.line, l.column, l.end_column);
                }
            }
        }
        SymbolRefData::Middleware(name) => {
            for m in &patterns.middleware_refs {
                if m.name == *name {
                    push(out, m.line, m.column, m.end_column);
                }
            }
        }
        SymbolRefData::Binding(name) => {
            for b in &patterns.binding_refs {
                if b.name == *name {
                    push(out, b.line, b.column, b.end_column);
                }
            }
        }
        // Magic members can't be matched by raw pattern scanning — a
        // `member_access_ref` only resolves to a `(declaring_fqcn, member)` key
        // through the M3 resolver (which needs the class-hierarchy index). They
        // are served from the resolved inverted index (`insert_magic_members`),
        // so this per-file scanner contributes nothing for them.
        SymbolRefData::MagicMember { .. } => {}
    }
}

#[cfg(test)]
mod tests;
