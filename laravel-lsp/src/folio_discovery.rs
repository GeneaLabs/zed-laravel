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
    /// The head of a `Folio::path(` call. The argument expression — a bare
    /// string literal, or a `resource_path(...)` / `base_path(...)` helper
    /// wrapping one (the form `php artisan folio:install` scaffolds) — is parsed
    /// by balancing parens from the match end (see [`resolve_folio_path`]), so
    /// helper-call forms are handled, not only bare literals.
    static ref FOLIO_PATH_HEAD_RE: Regex = Regex::new(r#"Folio::path\s*\("#).unwrap();

    /// A chained `->uri('admin')` link. Searched within a single `Folio::path`
    /// statement.
    static ref FOLIO_URI_RE: Regex =
        Regex::new(r#"->\s*uri\s*\(\s*['"]([^'"]+)['"]"#).unwrap();

    /// A chained `->name('admin.')` link. Searched within a single `Folio::path`
    /// statement.
    static ref FOLIO_NAME_RE: Regex =
        Regex::new(r#"->\s*name\s*\(\s*['"]([^'"]+)['"]"#).unwrap();

    /// A page-level `name('users.show')` call (Folio's `Laravel\Folio\name`
    /// helper). The leading boundary class excludes `>`, `:`, `\`, `(` and `[`
    /// so it never matches the route-chain `->name(`, the static `::name(`, the
    /// `use function Laravel\Folio\name;` import, or a `name(` nested inside
    /// another call/index expression (e.g. `usort($a, name('cmp'))`).
    static ref PAGE_NAME_RE: Regex =
        Regex::new(r#"(?:\A|[^A-Za-z0-9_>:\\(\[])name\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
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
            .follow_links(false)
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
/// resolving each mount's directory (relative to `root`, containment-checked)
/// and its chained URI / name prefixes. Pulled out of [`discover_folio_mounts`]
/// for direct testing.
fn parse_folio_mounts(content: &str, root: &Path) -> Vec<FolioMount> {
    let bytes = content.as_bytes();
    let mut mounts = Vec::new();
    for head in FOLIO_PATH_HEAD_RE.find_iter(content) {
        // `head.end()` is one past the opening `(`. Balance parens to find the
        // argument's close, so a wrapped helper (`resource_path('...')`) is
        // captured whole rather than truncated at its inner `)`.
        let open = head.end() - 1;
        let Some(close) = matching_paren(bytes, open) else {
            continue;
        };
        let arg = &content[open + 1..close];
        let Some(directory) = resolve_folio_path(arg, root) else {
            // Unparseable argument (a variable, an unsupported helper), or a
            // path that escapes the project root (`Folio::path('/etc')`,
            // `Folio::path('../../x')`) — a traversal vector we refuse to walk.
            // Skip; the default-mount fallback in [`discover_folio_mounts`]
            // still applies when nothing else parses.
            continue;
        };

        // The chained `->uri(...)` / `->name(...)` links live between this
        // call's close paren and the statement terminator. Bound the search to
        // that statement so a later mount's prefixes don't bleed in.
        let stmt_start = close + 1;
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
            directory,
            uri_prefix,
            name_prefix,
        });
    }
    mounts
}

/// Find the index of the `)` that closes the `(` at byte `open`, balancing
/// nested parens and skipping over single/double-quoted string contents (so a
/// `)` inside a path literal never ends the scan early).
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Parse a single- or double-quoted PHP string literal, returning its inner
/// content. `None` for anything that isn't a bare quoted literal (e.g. a
/// variable or a concatenation we can't resolve statically).
fn parse_literal(expr: &str) -> Option<String> {
    let expr = expr.trim();
    let bytes = expr.as_bytes();
    let &first = bytes.first()?;
    if (first == b'\'' || first == b'"') && bytes.len() >= 2 && bytes[bytes.len() - 1] == first {
        return Some(expr[1..expr.len() - 1].to_string());
    }
    None
}

/// If `expr` is a `<name>('literal')` or `<name>()` call, return the inner
/// literal (the empty string for the no-arg form). `None` when `expr` isn't
/// that helper or its argument isn't a static string.
fn helper_call(expr: &str, name: &str) -> Option<String> {
    let rest = expr.strip_prefix(name)?.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
    if inner.is_empty() {
        return Some(String::new());
    }
    parse_literal(inner)
}

/// Resolve a `Folio::path(...)` argument to an absolute mount directory that is
/// guaranteed to stay under `root`. Handles the bare string literal and the
/// `resource_path('...')` / `base_path('...')` helper-wrapped forms that
/// `php artisan folio:install` scaffolds.
///
/// Returns `None` when the argument isn't a static path, or when it resolves
/// outside the project: Folio mounts are walked and every `.blade.php` beneath
/// them is read into the route index, so a mount escaping the opened project
/// (`Folio::path('/etc')`, `Folio::path('../../x')`) is a path-traversal vector
/// and is refused. Mirrors the containment convention in
/// [`crate::route_discovery`]'s `resolve_path_argument`.
fn resolve_folio_path(expr: &str, root: &Path) -> Option<PathBuf> {
    let expr = expr.trim();
    let candidate = if let Some(sub) = helper_call(expr, "resource_path") {
        root.join("resources").join(sub.trim_start_matches('/'))
    } else if let Some(sub) = helper_call(expr, "base_path") {
        root.join(sub.trim_start_matches('/'))
    } else {
        let literal = parse_literal(expr)?;
        let p = Path::new(&literal);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };

    let normalized = normalize_path(&candidate);
    normalized
        .starts_with(normalize_path(root))
        .then_some(normalized)
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

/// The fully-qualified route name a `.blade.php` Folio page owns in `index`, if
/// any. A reverse lookup of the in-memory route index that
/// [`inject_folio_routes`] populated during the route-index build: it finds the
/// entry whose definition file is this page. Folio pages are the only
/// `.blade.php`-backed route definitions, so the suffix filter isolates them
/// from a same-named conventional route that shadows the page (keep-first
/// insertion keeps the conventional entry, whose file is a `routes/*.php`).
///
/// Find-references / rename use this to resolve a page's route name straight
/// from the already-built index instead of re-walking the project filesystem on
/// every request — the walk happened once, inside `spawn_blocking`, when the
/// index was built ([`crate::route_discovery::build_route_index`]). The returned
/// name carries the mount's `->name(...)` prefix, since that's the key the index
/// is stored under, so it reaches every `route('...')` usage of the page.
pub fn folio_name_for_file(index: &RouteIndex, file: &Path) -> Option<String> {
    let normalized = normalize_path(file);
    index.routes.iter().find_map(|(name, def)| {
        (def.file.to_str().is_some_and(|s| s.ends_with(".blade.php"))
            && normalize_path(&def.file) == normalized)
            .then(|| name.clone())
    })
}

/// Whether the cursor at (`line`, `character`) sits on a Folio page's own
/// `name('...')` helper call (the argument string, quotes included).
///
/// Powers find-references and rename triggered from *inside* a Folio page: the
/// page lives outside `routes/` and its bare `name(...)` helper isn't tagged by
/// the PHP parser, so the caller pairs this with [`folio_name_for_file`] to
/// recover the route name. Pure over `content` — no filesystem access — so the
/// caller decides how to source the page text (editor buffer or disk).
pub fn cursor_on_page_name(content: &str, line: u32, character: u32) -> bool {
    match page_name_span(content) {
        Some((name_line, start_col, end_col)) => {
            line == name_line && character >= start_col && character <= end_col
        }
        None => false,
    }
}

/// Source span of a Folio page's `name('...')` argument string, including the
/// surrounding quotes, as `(line, start_column, end_column)`. Used to decide
/// whether a cursor is on the call. `None` when the page declares no name.
fn page_name_span(content: &str) -> Option<(u32, u32, u32)> {
    let inner = PAGE_NAME_RE.captures(content)?.get(1)?;
    // Widen one byte each side to cover the enclosing quotes.
    let start = inner.start().saturating_sub(1);
    let end = (inner.end() + 1).min(content.len());
    let start_pos = crate::query_chain::byte_offset_to_position(content, start);
    let end_pos = crate::query_chain::byte_offset_to_position(content, end);
    // The `name('...')` argument is single-line; anchor to its start line.
    Some((start_pos.line, start_pos.character, end_pos.character))
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
            // Don't traverse symlinks: a link out of the mount would otherwise
            // let the walk read `.blade.php` files outside the opened project.
            .follow_links(false)
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
