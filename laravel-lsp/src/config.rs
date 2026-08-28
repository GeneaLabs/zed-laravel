//! Laravel project configuration utilities
//!
//! This module provides utilities for discovering Laravel projects
//! and working with Laravel naming conventions.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;

/// Does this directory look like the root of a Laravel project or package?
///
/// The three marker sets the extension has always recognised, unchanged:
///
/// - `composer.json` + `artisan` — a Laravel app
/// - `composer.json` + `app/` + `resources/` — a Laravel app
/// - `composer.json` + `src/` + `vendor/` — a Laravel package
///
/// Every signal except `vendor/` is committed to the repository, so a fresh
/// clone that has never run `composer install` still identifies correctly
/// through `artisan` or the `app/` + `resources/` pair. `vendor/` is only
/// consulted for the package shape, where nothing else distinguishes a package
/// checkout from any directory that happens to hold a `composer.json` and a
/// `src/`.
pub fn looks_like_laravel_project(dir: &Path) -> bool {
    if !dir.join("composer.json").exists() {
        return false;
    }
    dir.join("artisan").exists()
        || (dir.join("app").is_dir() && dir.join("resources").is_dir())
        || (dir.join("src").is_dir() && dir.join("vendor").is_dir())
}

/// Find the Laravel project root for a file.
///
/// Two strategies, chosen by whether the file sits inside the editor's
/// workspace:
///
/// **Inside the workspace** — walk *down* from `workspace_root` toward the
/// file and take the **outermost** directory that [`looks_like_laravel_project`].
/// In a modular monolith (composer-merge-plugin layouts, `app/{Parent}/{Module}/`
/// with per-module manifests merged into the workspace manifest) every module
/// matches the same markers as the workspace itself, so "first match walking
/// up" hands the whole server to a module directory — `.env` resolution, the
/// route/migration/command indexes, DB config, the vendor scan and the file
/// watchers all silently retarget a subdirectory (issue #286). Outermost-wins
/// cannot make that mistake, because the workspace root always matches before
/// any module nested inside it does.
///
/// The workspace root is the fence, and it matters: with `/` as the starting
/// point instead, one stray `composer.json` beside an `app/` and a `resources/`
/// in a home directory would silently re-root every project opened beneath it.
///
/// **Outside the workspace, or no workspace at all** — vendor files, globally
/// installed packages, a file opened by absolute path — fall back to walking
/// *up* from the file. There is no fence to walk down from, so the nearest
/// enclosing project wins, with one refinement from #286: a
/// `composer.json` + `app/` + `resources/` match that has no `vendor/` of its
/// own is only *tentative*, since that is exactly the shape of a merged module.
/// The walk continues and prefers any ancestor that is unambiguously a root,
/// returning the tentative match only when no stronger ancestor exists.
///
/// Returns `None` if no Laravel project root is found.
pub fn find_project_root(file_path: &Path, workspace_root: Option<&Path>) -> Option<PathBuf> {
    let mut start = file_path;
    if start.is_file() {
        start = start.parent()?;
    }

    if let Some(workspace_root) = workspace_root {
        if let Some(root) = outermost_project_from(workspace_root, start) {
            info!(
                "Found Laravel project root at {:?} (outermost match within workspace {:?})",
                root, workspace_root
            );
            return Some(root);
        }
    }

    find_project_root_upward(start)
}

/// Walk down from `workspace_root` toward `start`, returning the first
/// directory on that path that [`looks_like_laravel_project`].
///
/// Returns `None` when `start` is not inside `workspace_root` (nothing to walk)
/// or when no directory along the way looks like a project — a monorepo whose
/// top-level `composer.json` only pins dev tooling, for instance, is skipped in
/// favour of the real app further down.
fn outermost_project_from(workspace_root: &Path, start: &Path) -> Option<PathBuf> {
    let relative = start.strip_prefix(workspace_root).ok()?;
    let mut candidate = workspace_root.to_path_buf();
    if looks_like_laravel_project(&candidate) {
        return Some(candidate);
    }
    for component in relative.components() {
        candidate.push(component);
        if looks_like_laravel_project(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Is `dir` the outermost Laravel project on its own path within
/// `workspace_root`?
///
/// The question every "may this nested directory be the root?" check actually
/// wants to ask. It reuses [`find_project_root`]'s own descent, so the answer
/// cannot drift from the rule that produced the root in the first place — and
/// unlike a marker-counting heuristic it consults no gitignored state, so it
/// returns the same answer before and after `composer install`.
///
/// Fails closed: a `dir` outside `workspace_root`, or one no descent reaches,
/// is not outermost. A wrong `true` re-roots the entire server; a wrong `false`
/// merely keeps the root already in use.
pub fn is_outermost_project(workspace_root: &Path, dir: &Path) -> bool {
    outermost_project_from(workspace_root, dir).as_deref() == Some(dir)
}

/// The pre-workspace-fence strategy: walk up from `start` and take the nearest
/// enclosing project, treating a vendor-less `composer.json` + `app/` +
/// `resources/` match as tentative. See [`find_project_root`] for when this
/// applies and why.
fn find_project_root_upward(start: &Path) -> Option<PathBuf> {
    let mut current = start;
    let mut tentative: Option<PathBuf> = None;

    loop {
        let has_composer = current.join("composer.json").exists();
        let has_artisan = current.join("artisan").exists();
        let has_app = current.join("app").is_dir();
        let has_resources = current.join("resources").is_dir();
        let has_src = current.join("src").is_dir();
        let has_vendor = current.join("vendor").is_dir();

        if has_composer && has_artisan {
            info!(
                "Found Laravel project root at {:?} (composer.json + artisan)",
                current
            );
            return Some(current.to_path_buf());
        }

        if has_composer && has_src && has_vendor {
            info!(
                "Found Laravel package root at {:?} (composer.json + src + vendor)",
                current
            );
            return Some(current.to_path_buf());
        }

        if has_composer && has_app && has_resources {
            if has_vendor {
                info!(
                    "Found Laravel project root at {:?} (composer.json + app + resources + vendor)",
                    current
                );
                return Some(current.to_path_buf());
            }
            if tentative.is_none() {
                tentative = Some(current.to_path_buf());
            }
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    if let Some(root) = &tentative {
        info!(
            "Found Laravel project root at {:?} (composer.json + app + resources; no stronger ancestor root)",
            root
        );
    }
    tentative
}

/// Read a single value out of a project's `.env`.
///
/// The one hardened `.env` reader in this codebase. The pattern is
/// deliberately horizontal (`[ \t]*`, `[^'"\n]*`) so a blank value
/// (`KEY=\n`) captures the empty string rather than swallowing the next
/// line — a naive multi-line-tolerant regex here previously leaked one
/// variable's value into another's, and any second copy of this logic risks
/// reintroducing that. Callers that need an env value go through this.
///
/// Returns `None` when the file is unreadable, the key is absent, or the value
/// is empty.
pub fn read_env_value(project_root: &Path, key: &str) -> Option<String> {
    let env_path = resolve_worktree_fallback(project_root, ".env");
    let content = std::fs::read_to_string(&env_path).ok()?;
    let pattern = format!(
        r#"(?m)^{}[ \t]*=[ \t]*['"]?([^'"\n]*)['"]?"#,
        regex::escape(key)
    );
    regex::Regex::new(&pattern)
        .ok()?
        .captures(&content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve a directory's real git "common dir" — the directory holding the
/// repository's shared object database and refs.
///
/// For an ordinary checkout this is just `<root>/.git`. For a **linked
/// worktree** (`git worktree add`), `.git` is a *file* (not a directory)
/// containing a `gitdir: <path>` pointer into the main checkout's
/// `.git/worktrees/<name>/` admin directory, which itself contains a
/// `commondir` file naming the actual shared `.git` directory (typically
/// `../..`, relative to that admin directory). Following both hops and
/// canonicalizing lands on the same path for every worktree of one repo,
/// linked or main.
fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let meta = fs::symlink_metadata(&dot_git).ok()?;

    let git_dir = if meta.is_dir() {
        dot_git
    } else {
        let contents = fs::read_to_string(&dot_git).ok()?;
        let pointer = contents.trim().strip_prefix("gitdir:")?.trim();
        let pointer_path = PathBuf::from(pointer);
        if pointer_path.is_absolute() {
            pointer_path
        } else {
            root.join(pointer_path)
        }
    };

    let commondir_file = git_dir.join("commondir");
    let common_dir = if commondir_file.is_file() {
        let contents = fs::read_to_string(&commondir_file).ok()?;
        let relative = PathBuf::from(contents.trim());
        if relative.is_absolute() {
            relative
        } else {
            git_dir.join(relative)
        }
    } else {
        git_dir
    };

    common_dir.canonicalize().ok()
}

/// True if `a` and `b` are two worktrees (linked or main) of the same git
/// repository.
///
/// Every worktree is a full checkout, so it carries its own `composer.json`
/// and `artisan` — indistinguishable, by [`find_project_root`]'s markers
/// alone, from a genuinely separate nested Laravel project. This check lets
/// callers tell the two apart: a discovered root that's just another
/// worktree of the project already open should never be treated as a
/// distinct project root.
pub fn is_same_git_repo(a: &Path, b: &Path) -> bool {
    match (git_common_dir(a), git_common_dir(b)) {
        (Some(dir_a), Some(dir_b)) => dir_a == dir_b,
        _ => false,
    }
}

/// The main worktree root for `path`'s repository — the checkout `git
/// worktree add` was run from, found by taking [`git_common_dir`]'s parent
/// (the shared `.git` directory always lives directly under the main
/// worktree). Returns `None` when `path` isn't a git repo at all, or is
/// already the main worktree itself (nothing to fall back to).
fn git_main_worktree_root(path: &Path) -> Option<PathBuf> {
    let main_root = git_common_dir(path)?.parent()?.to_path_buf();
    let path_canonical = path.canonicalize().ok()?;
    if path_canonical == main_root {
        None
    } else {
        Some(main_root)
    }
}

/// True if `path` is the MAIN worktree of its repository (not a linked one).
///
/// Only meaningful once the caller already knows `path` is *some* worktree
/// of a repo whose identity matters — e.g. after [`is_same_git_repo`]
/// confirmed two candidates share a repo, this tells them apart. Used to
/// prefer the main checkout over a linked worktree when both resolve the
/// same project: a linked worktree only gets what its branch happened to
/// have at creation time (issue: a model class added on `main` after a
/// Claude Code agent worktree branched off was invisible from inside that
/// worktree — `git worktree add` is a point-in-time snapshot, not a live
/// mirror), so once the active root drifts onto a linked worktree it should
/// self-correct back to main, never the other way around.
pub fn is_main_worktree(path: &Path) -> bool {
    git_main_worktree_root(path).is_none()
}

/// Resolve `relative` under `root`, falling back to the same relative path
/// under the main worktree root when `root` is a linked worktree and the
/// file isn't present locally.
///
/// A linked worktree only gets git-tracked files — `git worktree add` never
/// copies anything gitignored, so local dev config the project deliberately
/// keeps untracked (`.env`, `docker-compose.override.yml`) is simply absent
/// from a fresh worktree, even though the project it belongs to has one.
/// Falling back to the main worktree's copy is safe specifically *because*
/// the file is untracked: there's no tracked-file divergence between
/// branches to accidentally paper over, only a copy that was never there to
/// begin with.
///
/// Returns `root.join(relative)` unchanged when that exists, or when no
/// fallback applies (not a worktree, or absent everywhere) — every existing
/// caller's "file missing" handling keeps working as before.
pub fn resolve_worktree_fallback(root: &Path, relative: &str) -> PathBuf {
    let local = root.join(relative);
    if local.exists() {
        return local;
    }

    match git_main_worktree_root(root) {
        Some(main_root) => {
            let shared = main_root.join(relative);
            if shared.exists() {
                shared
            } else {
                local
            }
        }
        None => local,
    }
}

/// Load Blade component aliases from all known sources.
///
/// Three independent sources are merged into a single `HashMap<alias, view-dot-path>`,
/// in **priority order** (later sources override earlier ones):
///
/// 0. **Vendor packages** (weakest) — service-provider files under `vendor/`
///    that look like `*ServiceProvider*.php` and contain `Blade::component()` /
///    `$blade->component()` calls. Results are cached on disk and invalidated
///    when `composer.lock` changes.
/// 1. **Config-driven** — `config/component.php`'s `'aliases'` array. Common
///    convention for projects that register many aliases through a single
///    config-loop in their `AppServiceProvider`.
/// 2. **App service providers** (strongest) — `$blade->component($view, $alias)` and
///    `Blade::component($view, $alias)` invocations inside `app/Providers/*.php`.
///    Closest to runtime truth, wins on conflict.
///
/// All sources gracefully no-op when their respective files/dirs are absent.
pub fn load_component_aliases(root: &Path) -> HashMap<String, String> {
    let mut aliases = HashMap::new();

    // Source 0: Vendor packages (weakest priority).
    aliases.extend(scan_vendor_for_component_aliases(root));

    // Source 1: config/component.php (overrides vendor defaults).
    let config_path = root.join("config/component.php");
    if let Ok(source) = fs::read_to_string(&config_path) {
        parse_component_aliases(&source, &mut aliases);
    }

    // Source 2: app/Providers/*.php — direct $blade->component() / Blade::component() calls.
    let providers_dir = root.join("app/Providers");
    if providers_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&providers_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("php") {
                    continue;
                }
                let Ok(source) = fs::read_to_string(&path) else {
                    continue;
                };
                extract_provider_blade_aliases(&source, &mut aliases);
            }
        }
    }

    aliases
}

// ============================================================================
// Vendor scanning + on-disk cache
// ============================================================================

const VENDOR_ALIAS_CACHE_FILENAME: &str = "vendor_component_aliases.json";

/// Current cache schema version. Bump whenever the cache shape changes so
/// older cache files force a re-scan instead of silently returning stale data
/// for fields that didn't exist when the cache was written.
///
/// History:
///   v0 (implicit) — only `composer_lock_mtime_secs` + `aliases`.
///   v1 — added `icon_aliases` for blade-icons SVG resolution.
const VENDOR_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct VendorAliasCache {
    #[serde(default)]
    schema_version: u32,
    composer_lock_mtime_secs: u64,
    aliases: HashMap<String, String>,
    #[serde(default)]
    icon_aliases: HashMap<String, String>,
}

/// Walk `vendor/` for service providers that register Blade components, and
/// return the merged alias map. Results are cached to disk and only rebuilt
/// when `composer.lock` mtime changes — so the cost is paid once per
/// `composer install` / `composer update`, not on every LSP boot.
pub fn scan_vendor_for_component_aliases(root: &Path) -> HashMap<String, String> {
    let lock_mtime = composer_lock_mtime(root);

    // Cache hit: composer.lock hasn't changed AND the schema matches.
    if let Some(cached) = read_vendor_cache(root) {
        if lock_mtime > 0
            && cached.composer_lock_mtime_secs == lock_mtime
            && cached.schema_version == VENDOR_CACHE_SCHEMA_VERSION
        {
            return cached.aliases;
        }
    }

    let aliases = scan_vendor_uncached(root);
    let icon_aliases = scan_vendor_icons_uncached(root);

    if lock_mtime > 0 {
        write_vendor_cache(
            root,
            &VendorAliasCache {
                schema_version: VENDOR_CACHE_SCHEMA_VERSION,
                composer_lock_mtime_secs: lock_mtime,
                aliases: aliases.clone(),
                icon_aliases,
            },
        );
    }

    aliases
}

/// Scan vendor packages for **icon-set component registrations** (blade-icons
/// Factory pattern). Returns a map of full tag name (e.g., `"heroicon-o-clock"`)
/// to the absolute SVG file path.
///
/// blade-icons registers each icon dynamically at runtime via a loop over a
/// filesystem manifest, so static AST analysis can't extract the pairs. We
/// shortcut that by walking the manifest ourselves: any vendor package with the
/// blade-icons-shaped layout (`resources/svg/` directory + `config/blade-*.php`
/// declaring `'prefix' => '...'`) is treated as an icon set. Each SVG file
/// becomes a `<x-{prefix}-{filename-stem}>` registration.
///
/// Results are cached on disk alongside the component-alias map; invalidation
/// triggers on `composer.lock` mtime change.
pub fn scan_vendor_for_icon_sets(root: &Path) -> HashMap<String, String> {
    let lock_mtime = composer_lock_mtime(root);

    if let Some(cached) = read_vendor_cache(root) {
        if lock_mtime > 0
            && cached.composer_lock_mtime_secs == lock_mtime
            && cached.schema_version == VENDOR_CACHE_SCHEMA_VERSION
        {
            return cached.icon_aliases;
        }
    }

    let icon_aliases = scan_vendor_icons_uncached(root);

    // Refresh the unified cache. We re-scan component aliases too to keep
    // the cache coherent, since both maps share invalidation.
    if lock_mtime > 0 {
        let aliases = scan_vendor_uncached(root);
        write_vendor_cache(
            root,
            &VendorAliasCache {
                schema_version: VENDOR_CACHE_SCHEMA_VERSION,
                composer_lock_mtime_secs: lock_mtime,
                aliases,
                icon_aliases: icon_aliases.clone(),
            },
        );
    }

    icon_aliases
}

fn scan_vendor_icons_uncached(root: &Path) -> HashMap<String, String> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut icons = HashMap::new();

    // Vendor layout: vendor/{vendor-name}/{package-name}/...
    let Ok(vendor_entries) = fs::read_dir(&vendor) else {
        return icons;
    };
    for vendor_entry in vendor_entries.flatten() {
        let vendor_dir = vendor_entry.path();
        if !vendor_dir.is_dir() {
            continue;
        }
        let Ok(pkg_entries) = fs::read_dir(&vendor_dir) else {
            continue;
        };
        for pkg_entry in pkg_entries.flatten() {
            let pkg_dir = pkg_entry.path();
            if !pkg_dir.is_dir() {
                continue;
            }

            let svg_dir = pkg_dir.join("resources/svg");
            let config_dir = pkg_dir.join("config");
            if !svg_dir.is_dir() || !config_dir.is_dir() {
                continue;
            }

            let Some(prefix) = extract_prefix_from_blade_config_dir(&config_dir) else {
                continue;
            };

            walk_svg_dir_into(&svg_dir, &prefix, &mut icons);
        }
    }

    icons
}

/// Look for a `blade-*.php` config file in the directory and extract its
/// `'prefix' => 'NAME'` value. Returns None when no such file exists or no
/// prefix is declared.
fn extract_prefix_from_blade_config_dir(config_dir: &Path) -> Option<String> {
    let entries = fs::read_dir(config_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !filename.starts_with("blade-") || !filename.ends_with(".php") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(prefix) = scan_prefix_string(&source) {
            return Some(prefix);
        }
    }
    None
}

/// Find `'prefix' => 'value'` (or `"prefix" => "value"`) in a PHP source.
fn scan_prefix_string(source: &str) -> Option<String> {
    for key in ["'prefix'", "\"prefix\""] {
        let mut search_from = 0;
        while let Some(rel) = source[search_from..].find(key) {
            let pos = search_from + rel;
            let after = source[pos + key.len()..].trim_start();
            let Some(after_arrow) = after.strip_prefix("=>") else {
                search_from = pos + key.len();
                continue;
            };
            let after_arrow = after_arrow.trim_start();
            let quote = after_arrow.chars().next()?;
            if quote != '\'' && quote != '"' {
                search_from = pos + key.len();
                continue;
            }
            let body = &after_arrow[1..];
            let end = body.find(quote)?;
            return Some(body[..end].to_string());
        }
    }
    None
}

/// Walk an SVG directory and register each file with its `{prefix}-{name}` tag.
/// Nested directories produce dash-separated tag names (e.g., `outline/clock.svg`
/// under prefix `heroicon` becomes `heroicon-outline-clock`).
fn walk_svg_dir_into(svg_dir: &Path, prefix: &str, out: &mut HashMap<String, String>) {
    for entry in walkdir::WalkDir::new(svg_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("svg") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(svg_dir) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else {
            continue;
        };
        let Some(stem) = rel_str.strip_suffix(".svg") else {
            continue;
        };
        // Normalize directory separators to dashes so nested + flat layouts
        // both produce dashed tag names.
        let icon_name = stem.replace(std::path::MAIN_SEPARATOR, "-");
        let tag = format!("{}-{}", prefix, icon_name);
        let Some(abs_str) = path.to_str() else {
            continue;
        };
        out.insert(tag, abs_str.to_string());
    }
}

fn scan_vendor_uncached(root: &Path) -> HashMap<String, String> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut aliases = HashMap::new();

    for entry in walkdir::WalkDir::new(&vendor)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("php") {
            continue;
        }

        // Filename gate (cheap): only consider files whose name contains
        // "ServiceProvider". This covers ~99% of real Laravel package providers
        // and trims a ~50k-file vendor walk down to a few hundred parse candidates.
        let filename_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("ServiceProvider"))
            .unwrap_or(false);
        if !filename_matches {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };

        // Content gate (cheap substring): must look like a Blade component
        // registration. Avoids parsing files that happen to be named
        // *ServiceProvider* but register middleware, bindings, etc.
        let has_component_call =
            source.contains("Blade::component(") || source.contains("->component(");
        if !has_component_call {
            continue;
        }

        extract_provider_blade_aliases(&source, &mut aliases);
    }

    aliases
}

fn composer_lock_mtime(root: &Path) -> u64 {
    fs::metadata(root.join("composer.lock"))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn vendor_cache_path(root: &Path) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let cache_base = crate::cache_root::cache_root()?;

    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());

    Some(
        cache_base
            .join(project_hash)
            .join(VENDOR_ALIAS_CACHE_FILENAME),
    )
}

fn read_vendor_cache(root: &Path) -> Option<VendorAliasCache> {
    let path = vendor_cache_path(root)?;
    let source = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&source).ok()
}

fn write_vendor_cache(root: &Path, cache: &VendorAliasCache) {
    let Some(path) = vendor_cache_path(root) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = fs::write(&path, json);
    }
}

/// Extract `$blade->component()` / `Blade::component()` alias registrations
/// from a service-provider PHP file. Inserts pairs into `aliases`. Calls with
/// non-literal arguments (e.g., `$blade->component($component, $alias)` in a
/// loop) produce no captures — those rely on the config-driven source.
fn extract_provider_blade_aliases(source: &str, aliases: &mut HashMap<String, String>) {
    use crate::parser::{language_php, parse_php};
    use crate::queries::extract_all_php_patterns;

    let Ok(tree) = parse_php(source) else {
        return;
    };
    let lang = language_php();
    let Ok(patterns) = extract_all_php_patterns(&tree, source, &lang) else {
        return;
    };

    for m in &patterns.blade_component_aliases {
        // Skip class-FQN-shaped views. PSR-4 class names start with an uppercase
        // letter; view dot-paths are kebab/snake-cased lowercase by convention.
        // Tree-sitter's PHP grammar splits strings at escape sequences, so a
        // literal like `'App\\View\\Components\\Alert'` can surface here with
        // only the leading segment captured — guarding on the first-char case
        // catches both that truncation and unescaped FQNs.
        let first_char_is_uppercase = m.view.chars().next().is_some_and(|c| c.is_uppercase());
        if first_char_is_uppercase || m.view.contains('\\') {
            continue;
        }
        aliases.insert(m.alias.to_string(), m.view.to_string());
    }
}

/// Extract `'alias' => 'view.path'` pairs from a PHP config file's source.
///
/// Scans the file for the `'aliases'` key and walks the inner array literal,
/// pulling out single-quoted alias/view pairs. Skips entries whose value is a
/// `Class::class` reference (those are PHP component classes, not view paths).
fn parse_component_aliases(source: &str, aliases: &mut HashMap<String, String>) {
    let Some(block) = php_array_block(source, "aliases") else {
        return;
    };

    for raw_line in block.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
        {
            continue;
        }

        let Some((alias, value)) = split_arrow_pair(line) else {
            continue;
        };

        // Skip ::class references — those point at PHP classes, not view paths.
        if value.contains("::class") {
            continue;
        }

        let Some(view_path) = unquote(value) else {
            continue;
        };
        let Some(alias_name) = unquote(alias) else {
            continue;
        };

        aliases.insert(alias_name.to_string(), view_path.to_string());
    }
}

// ============================================================================
// Facade aliases (config/app.php 'aliases')
// ============================================================================

/// Parse the `'aliases'` array from a `config/app.php` source into a
/// token → facade-FQCN map (`'Auth' => 'Illuminate\Support\Facades\Auth'`).
///
/// This is the legacy facade-alias registration: a `Facade::defaultAliases()`
/// override returned from `config/app.php`, each entry an `'Alias' =>
/// Class::class` pair. It is the **opposite** of [`parse_component_aliases`],
/// which *skips* `::class` values (those are Blade component classes); here a
/// `::class` value is exactly what we want — it names the facade class the
/// alias resolves to.
///
/// Values are read as written: a `::class` constant (`Auth::class`,
/// `Illuminate\Support\Facades\Auth::class`, or a leading-`\` form). We strip
/// the `::class` suffix and any leading `\`, taking the remaining FQCN
/// verbatim. config/app.php's `aliases` are written fully-qualified by Laravel
/// convention, so no `use`-import resolution is needed for this source (the
/// `bootstrap/app.php` `withAliases` source, parsed via tree-sitter, does
/// resolve imports). A non-`::class` value (a bare string, a computed
/// expression) is skipped — it can't name a facade class statically.
pub fn parse_facade_aliases(source: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let Some(block) = php_array_block(source, "aliases") else {
        return aliases;
    };

    for raw_line in block.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
        {
            continue;
        }

        let Some((alias, value)) = split_arrow_pair(line) else {
            continue;
        };
        let Some(alias_name) = unquote(alias) else {
            continue;
        };
        // Only `Class::class` values name a facade class; anything else is not
        // a static class reference.
        let Some(class_ref) = value.strip_suffix("::class") else {
            continue;
        };
        let fqcn = class_ref.trim().trim_start_matches('\\');
        if fqcn.is_empty() {
            continue;
        }
        aliases.insert(alias_name.to_string(), fqcn.to_string());
    }

    aliases
}

// ============================================================================
// Livewire component namespaces (Livewire v4)
// ============================================================================

/// Anonymous component namespaces Livewire v4 registers at boot.
///
/// Livewire v4's service provider loops over
/// `config('livewire.component_namespaces')` — defaulting to
/// `['layouts' => resource_path('views/layouts'), 'pages' =>
/// resource_path('views/pages')]` — calling
/// `Blade::anonymousComponentPath($location, $namespace)` for each entry,
/// which is what makes `<x-layouts::app>` resolve to
/// `resources/views/layouts/app.blade.php`. The registration is config-driven
/// and runs in a loop, so provider parsing never sees it; reconstruct it from
/// the config instead: the app's `config/livewire.php` when it defines the
/// key, else the package's own config (Livewire merges vendor defaults
/// underneath the app's). Livewire v3 has no such key, so this is a no-op
/// there.
pub fn livewire_component_namespaces(root: &Path) -> Vec<(String, PathBuf)> {
    let candidates = [
        root.join("config/livewire.php"),
        root.join("vendor/livewire/livewire/config/livewire.php"),
    ];
    for config_path in candidates {
        let Ok(source) = fs::read_to_string(&config_path) else {
            continue;
        };
        // A present key is authoritative even when its array is empty —
        // `'component_namespaces' => []` is how an app *disables* Livewire's
        // defaults (Laravel's config merge replaces the array wholesale).
        // Only a missing key falls through to the vendor defaults.
        if let Some(parsed) = parse_livewire_component_namespaces(&source, root) {
            return parsed;
        }
    }
    Vec::new()
}

/// Returns `None` when the `component_namespaces` key is absent from the
/// source, `Some(entries)` (possibly empty) when it is present.
fn parse_livewire_component_namespaces(
    source: &str,
    root: &Path,
) -> Option<Vec<(String, PathBuf)>> {
    let mut namespaces = Vec::new();
    let block = php_array_block(source, "component_namespaces")?;

    for raw_line in block.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
        {
            continue;
        }

        let Some((key, value)) = split_arrow_pair(line) else {
            continue;
        };
        let Some(namespace) = unquote(key) else {
            continue;
        };
        let Some(path) = resolve_php_path_expression(value, root) else {
            continue;
        };
        namespaces.push((namespace.to_string(), path));
    }

    Some(namespaces)
}

/// Resolve a dotted config key (`mary.prefix`) to its string value the way
/// Laravel would at boot: the app's `config/{file}.php` wins; otherwise the
/// registering package's own bundled `config/{file}.php` default (packages
/// `mergeConfigFrom` their config under the same key). The package config is
/// located by walking up from the provider file to the nearest `config/`
/// sibling, stopping at the project root. Returns `None` when neither
/// defines the key — PHP's `config()` would yield null there.
pub fn resolve_config_string_for_package(
    root: &Path,
    dotted_key: &str,
    provider_path: &Path,
) -> Option<String> {
    let (file, key) = dotted_key.split_once('.')?;
    let config_file = format!("{file}.php");

    // App override.
    if let Ok(source) = fs::read_to_string(root.join("config").join(&config_file)) {
        if let Some(value) = php_top_level_string_value(&source, key) {
            return Some(value);
        }
    }

    // Package default: provider at `<pkg>/src/FooServiceProvider.php` →
    // `<pkg>/config/{file}.php`.
    let mut dir = provider_path.parent();
    while let Some(d) = dir {
        let candidate = d.join("config").join(&config_file);
        if candidate.exists() {
            let source = fs::read_to_string(&candidate).ok()?;
            return php_top_level_string_value(&source, key);
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }

    None
}

/// Find the string value of a **top-level** key in a PHP config file
/// (`return ['prefix' => 'mary-', ...]`), ignoring same-named keys nested
/// inside sub-arrays. Handles plain string literals and the
/// `env('NAME', 'default')` form (the default is taken — .env overrides are
/// out of static reach).
pub fn php_top_level_string_value(source: &str, key: &str) -> Option<String> {
    let return_pos = source.find("return")?;
    let open_rel = source[return_pos..].find('[')?;
    let block_start = return_pos + open_rel + 1;

    let mut depth: i32 = 1;
    let mut block_end = None;
    for (idx, ch) in source[block_start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    block_end = Some(block_start + idx);
                    break;
                }
            }
            _ => {}
        }
    }
    let block = &source[block_start..block_end?];

    let needle_sq = format!("'{key}'");
    let needle_dq = format!("\"{key}\"");
    let mut rel_depth: i32 = 0;
    for line in block.lines() {
        let trimmed = line.trim();
        if rel_depth == 0 && (trimmed.starts_with(&needle_sq) || trimmed.starts_with(&needle_dq)) {
            let (_, value) = split_arrow_pair(trimmed)?;
            if let Some(literal) = unquote(value) {
                return Some(literal.to_string());
            }
            // env('NAME', 'default') → the default argument.
            if let Some(rest) = value.strip_prefix("env(") {
                let inner = rest.rsplit_once(')')?.0;
                let default = inner.split_once(',')?.1.trim();
                return unquote(default).map(str::to_string);
            }
            return None;
        }
        for ch in line.chars() {
            match ch {
                '[' => rel_depth += 1,
                ']' => rel_depth -= 1,
                _ => {}
            }
        }
    }

    None
}

/// Resolve a PHP path expression from a config value to an absolute path.
/// Handles the Laravel path helpers (`resource_path('x')`, `base_path('x')`,
/// `app_path('x')`) and plain string literals (absolute, or root-relative).
fn resolve_php_path_expression(value: &str, root: &Path) -> Option<PathBuf> {
    let value = value.trim();

    for (helper, base) in [
        ("resource_path", Some("resources")),
        ("base_path", None),
        ("app_path", Some("app")),
    ] {
        if let Some(rest) = value.strip_prefix(helper) {
            let inner = rest.trim().strip_prefix('(')?.rsplit_once(')')?.0;
            let arg = unquote(inner.trim()).unwrap_or("");
            let mut path = root.to_path_buf();
            if let Some(base) = base {
                path.push(base);
            }
            if !arg.is_empty() {
                path.push(arg);
            }
            return Some(path);
        }
    }

    let literal = unquote(value)?;
    let path = PathBuf::from(literal);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(literal)
    })
}

/// Find the contents of the PHP array literal assigned to `key` in a config
/// source: `'{key}' => [ ... ]`. Walks character-by-character to the matching
/// close bracket so entries from sibling top-level config keys are never
/// picked up. Returns the text between the brackets.
fn php_array_block<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let key_pos = source
        .find(&format!("'{key}'"))
        .or_else(|| source.find(&format!("\"{key}\"")))?;

    let after_key = &source[key_pos..];
    let open_bracket_rel = after_key.find('[')?;

    let block_start = key_pos + open_bracket_rel + 1;
    let mut depth: i32 = 1;
    for (idx, ch) in source[block_start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[block_start..block_start + idx]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a PHP array entry like `'alias' => 'view.path',` into (key, value).
fn split_arrow_pair(line: &str) -> Option<(&str, &str)> {
    let arrow_pos = line.find("=>")?;
    let key = line[..arrow_pos].trim();
    let after_arrow = line[arrow_pos + 2..].trim();
    // Strip trailing comma if present
    let value = after_arrow.trim_end_matches(',').trim();
    Some((key, value))
}

/// Extract the contents of a single- or double-quoted PHP string literal.
fn unquote(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    if bytes[bytes.len() - 1] != quote {
        return None;
    }
    Some(&input[1..input.len() - 1])
}

/// Convert kebab-case to PascalCase
///
/// Used for converting Livewire component names to class names.
/// Examples:
/// - "user-profile" -> "UserProfile"
/// - "admin-dashboard" -> "AdminDashboard"
pub fn kebab_to_pascal_case(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

// ============================================================================
// Modular-monolith support (`modules.paths` LSP setting)
// ============================================================================

/// Expand the configured module-directory patterns against the project root.
///
/// Patterns are relative to `root`; a `*` segment matches every child
/// directory (one level, no recursion), any other segment must match
/// literally. Results keep the pattern order (ascending config-merge
/// precedence); within one `*` expansion children are sorted for
/// determinism; a directory matched by several patterns keeps its first
/// position. Only existing directories are returned.
///
/// A PATTERN escaping the root (a `..` segment) is rejected — that is a
/// configuration error, not a deliberate layout. Symlinked results are
/// deliberately followed and NOT containment-checked, by the
/// configured-vs-discovered split `path_containment` encodes: these paths
/// come from the user's own `modules.paths` setting, and composer path
/// repositories legitimately symlink local packages to targets outside the
/// repository — refusing them would break local package development. Paths
/// DISCOVERED under a module (the provider-class walk) are gated instead.
///
/// Each pattern logs its match count, and a pattern matching nothing warns:
/// a typo'd glob (`app/**` matches only a directory literally named `**` —
/// this expansion has no recursive wildcard) is otherwise indistinguishable
/// from a working one.
pub fn expand_module_dirs(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for pattern in patterns {
        let mut current: Vec<PathBuf> = vec![root.to_path_buf()];
        for segment in pattern.split('/').filter(|s| !s.is_empty()) {
            if segment == ".." {
                current.clear();
                break;
            }
            let mut next = Vec::new();
            if segment == "*" {
                for dir in &current {
                    let Ok(entries) = fs::read_dir(dir) else {
                        continue;
                    };
                    let mut children: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect();
                    children.sort();
                    next.extend(children);
                }
            } else {
                for dir in &current {
                    let candidate = dir.join(segment);
                    if candidate.is_dir() {
                        next.push(candidate);
                    }
                }
            }
            current = next;
        }
        let mut matched = 0usize;
        for dir in current {
            if dir != *root && seen.insert(dir.clone()) {
                out.push(dir);
                matched += 1;
            }
        }
        if matched == 0 {
            tracing::warn!(
                "modules.paths pattern {pattern:?} matched no directories — \
                 a typo'd glob is a silent no-op (note: `*` matches one \
                 level; there is no recursive `**`)"
            );
        } else {
            tracing::debug!("modules.paths pattern {pattern:?} matched {matched} directories");
        }
    }

    out
}

/// Service-provider files of the configured module directories, discovered
/// through each module's own `composer.json`: the classes its
/// `extra.laravel.providers` array names are the providers Laravel actually
/// boots (via the merged manifests), so THAT list — not a filename
/// convention — decides what gets indexed. A `*ServiceProvider.php` file the
/// manifest doesn't name is not a booted provider and is not indexed; a
/// provider the manifest names under any filename is.
///
/// Each named FQCN resolves to a file through the module manifest's
/// `autoload.psr-4` mapping (longest matching prefix wins), falling back to
/// a bounded walk for `{ClassBasename}.php` inside the module when no PSR-4
/// prefix matches. A module without a `composer.json`, without the `extra`
/// entry, or whose entries don't resolve on disk simply contributes nothing.
pub fn module_provider_files(module_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for module_dir in module_dirs {
        out.extend(composer_declared_providers(module_dir));
    }
    out
}

/// The provider files one module's `composer.json` declares. See
/// [`module_provider_files`].
fn composer_declared_providers(module_dir: &Path) -> Vec<PathBuf> {
    let Ok(manifest) = std::fs::read_to_string(module_dir.join("composer.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&manifest) else {
        return Vec::new();
    };
    let Some(providers) = json
        .pointer("/extra/laravel/providers")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let psr4: Vec<(String, String)> = json
        .pointer("/autoload/psr-4")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(prefix, dir)| Some((prefix.clone(), dir.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    providers
        .iter()
        .filter_map(|v| v.as_str())
        .filter_map(|fqcn| resolve_provider_class_file(module_dir, &psr4, fqcn))
        .collect()
}

/// Resolve one provider FQCN to a file inside `module_dir`: PSR-4 first
/// (longest matching prefix), then a bounded `{Basename}.php` walk.
fn resolve_provider_class_file(
    module_dir: &Path,
    psr4: &[(String, String)],
    fqcn: &str,
) -> Option<PathBuf> {
    let fqcn = fqcn.trim_start_matches('\\');
    let mut best: Option<(usize, PathBuf)> = None;
    for (prefix, dir) in psr4 {
        let prefix_trimmed = prefix.trim_end_matches('\\');
        // Composer matches PSR-4 prefixes at a NAMESPACE boundary — prefix
        // `App\Legal\ContractManagement` must not match FQCN
        // `App\Legal\ContractManagementSupport\X`. After stripping a
        // non-empty prefix the remainder therefore has to start with `\`;
        // only the empty catch-all prefix (`"": "src/"`) takes the FQCN
        // whole.
        let Some(rest) = fqcn.strip_prefix(prefix_trimmed) else {
            continue;
        };
        let rest = if prefix_trimmed.is_empty() {
            rest
        } else {
            match rest.strip_prefix('\\') {
                Some(r) => r,
                None => continue,
            }
        };
        if rest.is_empty() {
            continue;
        }
        let candidate = module_dir
            .join(dir)
            .join(format!("{}.php", rest.replace('\\', "/")));
        // The manifest's `autoload.psr-4` value is DISCOVERED data: an
        // absolute path replaces the join base entirely and `..` segments
        // walk out, so the candidate is gated against the module dir
        // before it is ever probed — lexically first (no out-of-root
        // existence oracle), then canonicalized (#228 convention).
        if !crate::path_containment::path_within_root_lexical(&candidate, module_dir) {
            continue;
        }
        if candidate.is_file() && best.as_ref().is_none_or(|(len, _)| prefix.len() > *len) {
            best = Some((prefix.len(), candidate));
        }
    }
    if let Some((_, path)) = best {
        return Some(path);
    }

    let basename = fqcn.rsplit('\\').next()?;
    let file_name = format!("{basename}.php");
    // `follow_links(true)` matches `expand_module_dirs`'s `is_dir()`
    // behaviour, so a symlinked module looks the same to both. These paths
    // are DISCOVERED rather than configured, so each yielded entry is gated
    // (#228 convention) — against the module dir, which is itself trusted
    // configuration: a symlink INSIDE the module escaping it is refused,
    // while a module that is itself a symlinked composer path repository
    // keeps working.
    walkdir::WalkDir::new(module_dir)
        .follow_links(true)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "vendor" && name != "node_modules"
        })
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .find(|p| {
            p.is_file()
                && p.file_name().and_then(|n| n.to_str()) == Some(file_name.as_str())
                && crate::path_containment::path_within_root_walk_entry(p, module_dir)
        })
}

/// All files that contribute to the config group `group` (the top-level
/// `config('group.…')` key), in **descending merge precedence** — the file
/// whose value wins at runtime FIRST: each module's `config/{group}.php` in
/// reverse module order (the last-merged module wins), then the project
/// `config/{group}.php` as the fallback. Mirrors the runtime pattern where
/// a module service provider `array_replace_recursive`s its config files
/// over the existing repository state. Only existing files are returned.
///
/// This helper owns precedence, not just discovery: consumers iterate the
/// returned order as-is and take the first hit — none of them re-reverses,
/// so a new consumer cannot silently invert the rule by forgetting a
/// `.rev()`.
pub fn config_group_files(root: &Path, module_dirs: &[PathBuf], group: &str) -> Vec<PathBuf> {
    let file_name = format!("{group}.php");
    module_dirs
        .iter()
        .rev()
        .map(|m| m.join("config").join(&file_name))
        .chain(std::iter::once(root.join("config").join(&file_name)))
        .filter(|p| p.is_file())
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
