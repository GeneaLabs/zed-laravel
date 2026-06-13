//! Laravel Folio page-route discovery.
//!
//! [Folio](https://laravel.com/docs/folio) derives routes from the filesystem:
//! a Blade file at `resources/views/pages/users/[id].blade.php` becomes the
//! route `/users/{id}`. Because nothing calls `Route::` for these pages, the
//! conventional route discovery in [`crate::route_discovery`] never sees them.
//!
//! This module bridges that gap. It discovers the project's Folio mount
//! directories (the default `resources/views/pages`, plus any registered via
//! `Folio::path('...')` in a service provider), derives each page's URI from
//! its filename — honouring dynamic `[id]` and catch-all `[...slug]` segments
//! and the `index.blade.php` convention — and reads each page's explicit
//! `name('...')` route name. The named pages are then injected into the shared
//! [`crate::route_discovery::RouteIndex`] as ordinary [`RouteDefinition`]s, so
//! goto-definition, route-name completion, route-not-found diagnostics, and
//! find-references all surface Folio pages with no per-feature changes.

use std::path::{Path, PathBuf};

use lazy_static::lazy_static;
use regex::Regex;
use walkdir::WalkDir;

use crate::route_discovery::{normalize_path, RouteDefinition, RouteIndex, PRIORITY_APP};

/// Folio's default page directory, relative to the project root. Used when a
/// project enables Folio but never calls `Folio::path(...)` to relocate it.
pub const DEFAULT_FOLIO_MOUNT: &str = "resources/views/pages";

/// A discovered Folio mount: one page directory plus the URI and route-name
/// prefixes it contributes to every page beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolioMount {
    /// Absolute path to the mount's page directory.
    pub directory: PathBuf,
    /// URI prefix contributed by a chained `->uri('...')` (no surrounding
    /// slashes). Empty for the default mount.
    pub uri_prefix: String,
    /// Route-name prefix contributed by a chained `->name('...')`. Empty when
    /// the mount sets none.
    pub name_prefix: String,
}

/// A Folio page resolved to a route. `name` is `None` for pages without an
/// explicit `name('...')` call — those still have a URI but can't be reached
/// via `route('...')`, so only named pages reach the route index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolioRoute {
    /// Absolute path to the backing `.blade.php` page file.
    pub file: PathBuf,
    /// URI the page responds to, e.g. `/users/{id}`.
    pub uri: String,
    /// Fully-qualified route name (mount prefix + page name) when the page
    /// declares one.
    pub name: Option<String>,
}

lazy_static! {
    /// `Folio::path('resources/views/folio')` — captures the directory string.
    static ref FOLIO_PATH_RE: Regex =
        Regex::new(r#"Folio::path\s*\(\s*['"]([^'"]+)['"]"#).unwrap();

    /// A chained `->uri('admin')` link. Searched within a single `Folio::path`
    /// statement.
    static ref FOLIO_URI_RE: Regex =
        Regex::new(r#"->\s*uri\s*\(\s*['"]([^'"]+)['"]"#).unwrap();

    /// A chained `->name('admin.')` link. Searched within a single `Folio::path`
    /// statement.
    static ref FOLIO_NAME_RE: Regex =
        Regex::new(r#"->\s*name\s*\(\s*['"]([^'"]+)['"]"#).unwrap();

    /// A page-level `name('users.show')` call (Folio's `Laravel\Folio\name`
    /// helper). The leading boundary class excludes `>`, `:` and `\` so it
    /// never matches the route-chain `->name(`, the static `::name(`, or the
    /// `use function Laravel\Folio\name;` import.
    static ref PAGE_NAME_RE: Regex =
        Regex::new(r#"(?:\A|[^A-Za-z0-9_>:\\])name\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
}

/// Whether the project uses Folio at all. Gates the page-directory walk so
/// non-Folio projects that merely happen to have a `pages/` directory never
/// pay for it. True when `composer.json` requires `laravel/folio`, or any
/// service provider references the `Folio::` facade.
pub fn folio_in_use(root: &Path) -> bool {
    if let Ok(composer) = std::fs::read_to_string(root.join("composer.json")) {
        if composer.contains("laravel/folio") {
            return true;
        }
    }
    provider_files(root)
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|c| c.contains("Folio::"))
}

/// Service-provider files that may register Folio mounts. Mirrors the
/// route-discovery convention of scanning `bootstrap/app.php` and
/// `app/Providers/*.php`.
fn provider_files(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![root.join("bootstrap/app.php")];
    let providers = root.join("app/Providers");
    if providers.exists() {
        for entry in WalkDir::new(&providers)
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
        {
            paths.push(entry.path().to_path_buf());
        }
    }
    paths
}

/// Discover the project's Folio mounts. Every explicit `Folio::path('...')`
/// (with its chained `->uri(...)` / `->name(...)`) becomes a mount; when none
/// are registered the default [`DEFAULT_FOLIO_MOUNT`] is used.
pub fn discover_folio_mounts(root: &Path) -> Vec<FolioMount> {
    let mut mounts = Vec::new();
    for file in provider_files(root) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        mounts.extend(parse_folio_mounts(&content, root));
    }

    if mounts.is_empty() {
        mounts.push(FolioMount {
            directory: root.join(DEFAULT_FOLIO_MOUNT),
            uri_prefix: String::new(),
            name_prefix: String::new(),
        });
    }
    mounts
}

/// Parse every `Folio::path(...)` statement out of one provider's source,
/// resolving each mount's directory (relative to `root`) and its chained URI /
/// name prefixes. Pulled out of [`discover_folio_mounts`] for direct testing.
fn parse_folio_mounts(content: &str, root: &Path) -> Vec<FolioMount> {
    let mut mounts = Vec::new();
    for m in FOLIO_PATH_RE.captures_iter(content) {
        let whole = m.get(0).unwrap();
        let rel = m.get(1).unwrap().as_str();

        // The chained `->uri(...)` / `->name(...)` links live between this
        // `Folio::path(` call and the statement terminator. Bound the search to
        // that statement so a later mount's prefixes don't bleed in.
        let stmt_start = whole.end();
        let stmt_end = content[stmt_start..]
            .find(';')
            .map(|i| stmt_start + i)
            .unwrap_or(content.len());
        let statement = &content[stmt_start..stmt_end];

        let uri_prefix = FOLIO_URI_RE
            .captures(statement)
            .map(|c| c[1].trim_matches('/').to_string())
            .unwrap_or_default();
        let name_prefix = FOLIO_NAME_RE
            .captures(statement)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        mounts.push(FolioMount {
            directory: root.join(rel),
            uri_prefix,
            name_prefix,
        });
    }
    mounts
}

/// Derive a Folio route URI from a page path relative to its mount directory
/// (e.g. `users/[id].blade.php`). Drops the `.blade.php` suffix and a trailing
/// `index` segment, rewrites `[id]` → `{id}` and `[...slug]` → `{slug}`, and
/// joins the rest with `/`. Returns the path segments WITHOUT a leading slash;
/// the empty string denotes the mount root.
pub fn derive_uri(relative: &str) -> String {
    let stem = relative
        .strip_suffix(".blade.php")
        .unwrap_or(relative)
        .replace('\\', "/");

    let raw: Vec<&str> = stem.split('/').filter(|s| !s.is_empty()).collect();
    let mut segments: Vec<String> = Vec::new();
    for (i, seg) in raw.iter().enumerate() {
        // A trailing `index` maps to its parent directory's URI.
        if *seg == "index" && i == raw.len() - 1 {
            continue;
        }
        segments.push(rewrite_segment(seg));
    }
    segments.join("/")
}

/// Rewrite a single filename segment's Folio placeholder syntax into a route
/// parameter: `[...slug]` (catch-all) and `[id]` both become `{...}`. Literal
/// segments pass through untouched.
fn rewrite_segment(segment: &str) -> String {
    if let Some(inner) = segment
        .strip_prefix("[...")
        .and_then(|s| s.strip_suffix(']'))
    {
        return format!("{{{}}}", inner);
    }
    if let Some(inner) = segment.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return format!("{{{}}}", inner);
    }
    segment.to_string()
}

/// Extract a page's explicit Folio route name from its source — the argument of
/// the `name('...')` helper. Returns `None` when the page declares none.
pub fn extract_page_name(content: &str) -> Option<String> {
    PAGE_NAME_RE
        .captures(content)
        .map(|c| c[1].trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Discover every Folio page across the project's mounts and resolve it to a
/// [`FolioRoute`]. Returns an empty vector when the project doesn't use Folio.
pub fn discover_folio_routes(root: &Path) -> Vec<FolioRoute> {
    if !folio_in_use(root) {
        return Vec::new();
    }

    let mut routes = Vec::new();
    for mount in discover_folio_mounts(root) {
        if !mount.directory.exists() {
            continue;
        }
        for entry in WalkDir::new(&mount.directory)
            .max_depth(12)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
        {
            let path = entry.path();
            if !is_blade_file(path) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&mount.directory) else {
                continue;
            };
            let derived = derive_uri(&relative.to_string_lossy());
            let uri = compose_uri(&mount.uri_prefix, &derived);

            let name = std::fs::read_to_string(path)
                .ok()
                .and_then(|c| extract_page_name(&c))
                .map(|page_name| compose_name(&mount.name_prefix, &page_name));

            routes.push(FolioRoute {
                file: path.to_path_buf(),
                uri,
                name,
            });
        }
    }
    routes
}

/// Inject every *named* Folio page into `index` as a [`RouteDefinition`], so
/// all existing route consumers (goto, completion, diagnostics, references)
/// resolve Folio routes by name. Goto lands at the top of the page file. A
/// conventional route of the same name wins, because [`RouteIndex::insert`]
/// keeps the equal-priority entry inserted first and this runs after the
/// conventional pass.
pub fn inject_folio_routes(root: &Path, index: &mut RouteIndex) {
    for route in discover_folio_routes(root) {
        index.source_files.insert(normalize_path(&route.file));
        if let Some(name) = route.name {
            index.insert(
                name,
                RouteDefinition {
                    file: route.file,
                    line: 0,
                    column: 0,
                    end_column: 0,
                    priority: PRIORITY_APP,
                    method: Some("get".to_string()),
                    uri: Some(route.uri),
                    action: None,
                },
            );
        }
    }
}

/// Join a mount's URI prefix and a page's derived URI into a leading-slash
/// absolute URI. An all-empty result (the mount root's `index.blade.php`)
/// collapses to `/`.
fn compose_uri(prefix: &str, derived: &str) -> String {
    let body = [prefix.trim_matches('/'), derived]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    format!("/{}", body)
}

/// Combine a mount's name prefix with a page's own name. The prefix is honoured
/// verbatim (Folio prefixes typically end in `.`).
fn compose_name(prefix: &str, page_name: &str) -> String {
    if prefix.is_empty() {
        page_name.to_string()
    } else {
        format!("{}{}", prefix, page_name)
    }
}

/// Whether `path` is a Blade page file (`*.blade.php`).
fn is_blade_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".blade.php"))
}

#[cfg(test)]
mod tests;
