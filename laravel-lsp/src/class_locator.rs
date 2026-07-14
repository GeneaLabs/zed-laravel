//! Find a PHP class's source file anywhere under the project's `app/` tree.
//!
//! Used by the LSP to power hover, property completion, and goto-definition
//! for variables whose type resolves to a class name (e.g. `$form` →
//! `ContactForm` → `app/Livewire/Forms/ContactForm.php`).
//!
//! The strategy is intentionally simple: walk `app/**/*.php` and match by
//! basename. This avoids parsing `composer.json` PSR-4 mappings and works for
//! any standard Laravel layout (`app/Models/`, `app/Livewire/`, `app/Http/`,
//! `app/Livewire/Forms/`, `app/Services/`, etc.). The walker skips `vendor/`
//! and `node_modules/` (we never want to land in dependency code).
//!
//! Filesystem traversal is bounded by the `app/` directory depth, which is
//! typically modest (~tens of subdirs even in large apps). For projects with
//! atypical layouts (e.g. `src/` instead of `app/`), the caller can extend
//! the search roots.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use lru::LruCache;
use walkdir::WalkDir;

use crate::path_containment::path_within_root;

/// How long a *negative* (unresolved) walk result is trusted before it's
/// re-walked. Positive entries are revalidated by an `exists()` + containment
/// check on every hit — but a miss has nothing to re-stat, so a class created
/// after the miss was cached would stay invisible forever without a bound. This
/// matches the repo's existing file-existence cache TTL (5 min): the window
/// where a just-created model isn't yet resolvable, absent an explicit
/// [`invalidate_project`]. See the Fork-2 note on why the file watcher can't be
/// the sole invalidation trigger (its globs don't cover `app/Models/` etc.).
const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Upper bound on cached walk results. The key is a *basename* per project, so
/// the live set is small; the cap just guarantees the memo can't grow unbounded
/// over a long session. The LRU evicts the coldest basenames past this.
const LOCATOR_CACHE_CAP: usize = 4096;

/// A cached WalkDir result plus when it was recorded (for the negative-entry
/// TTL).
#[derive(Clone)]
struct CacheEntry {
    /// `Some(path)` = the walk found this file; `None` = the walk found nothing.
    path: Option<PathBuf>,
    /// When this entry was written. Only consulted for negative entries.
    cached_at: std::time::Instant,
}

/// Process-wide memo for the **basename WalkDir fallback only**, keyed by
/// `(canonical project root, simple class name, include_vendor)`.
///
/// # Why only the walk is cached
///
/// Resolution has two tiers. The first — Composer autoload + the cheap PSR-4
/// *shape* probes — is fast and, crucially, *precedence- and containment-correct
/// by construction* (it re-checks app-vs-vendor ordering and [`path_within_root`]
/// every call). Caching its positives would be a correctness trap: a cached
/// vendor hit couldn't be shadowed by a later app-side file, and a cached path
/// could bypass the containment guard on a since-swapped symlink. So we run that
/// tier live on every call and it stays free of stale precedence.
///
/// Only the **last-resort basename `WalkDir`** is memoized — that's the
/// expensive part (a full `app/`/`src/` walk, or the entire `vendor/` tree for
/// the app-or-vendor variant), and during the index build the same unresolvable
/// ancestor is walked once per referencing file. That walk runs *after* the
/// precedence-ordered tiers already failed, so its result carries no precedence
/// decision to go stale; the only freshness concerns are "file since deleted"
/// and "class since created", both handled below.
///
/// Freshness: a positive entry is revalidated on every hit with `exists()`
/// **and** [`path_within_root`] (so a deleted file *or* a since-swapped
/// out-of-root symlink falls back to a fresh walk); a negative entry expires
/// after [`NEGATIVE_TTL`], and both are dropped immediately by
/// [`invalidate_project`] on any watched `.php` create/delete.
///
/// Bounded `LruCache` behind a `Mutex` (lru mutates on read to reorder;
/// `spawn_blocking` workers share it, so the lock guards the O(1) get/put only).
type LocatorKey = (PathBuf, String, bool);
type LocatorCache = Mutex<LruCache<LocatorKey, CacheEntry>>;
fn locator_cache() -> &'static LocatorCache {
    static CACHE: OnceLock<LocatorCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(LOCATOR_CACHE_CAP).unwrap())))
}

/// Clear the walk-result memo.
///
/// Drops every entry (positive and negative) from the process-global
/// class-locator cache. Two production callers rely on this:
///
/// * The `laravel.reindexProject` command, which resets all process-global
///   memos before a cold rebuild so a stale walk result can't survive the
///   reindex.
/// * The isolation benchmarks/tests, which clear the memo between passes to
///   measure (or assert on) cold vs. warm behavior.
///
/// The memo is otherwise self-freshening (positive hits are revalidated with
/// `exists()` + [`path_within_root`]; negatives expire after [`NEGATIVE_TTL`];
/// watched `.php` create/delete calls [`invalidate_project`]), so this full
/// reset is only needed when the caller wants a guaranteed-clean slate.
pub fn reset_locator_cache() {
    locator_cache().lock().unwrap().clear();
}

/// Canonicalize the project root once per process (canonicalize is a syscall;
/// the root never changes during a session). Mirrors `ComposerAutoload`'s key
/// normalization so both caches agree on what "this project" means.
fn canonical_root(root: &Path) -> PathBuf {
    static ROOTS: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let roots = ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = roots.lock().expect("class_locator root cache poisoned");
    map.entry(root.to_path_buf())
        .or_insert_with(|| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
        .clone()
}

/// The basename WalkDir fallback, memoized. Runs `find_php_class_file_impl` only
/// on a cache miss and caches the result (positive or negative). Positive hits
/// are revalidated with `exists()` **and** [`path_within_root`]; negative hits
/// expire after [`NEGATIVE_TTL`].
///
/// This is the ONLY cached tier — callers run the precedence-correct
/// Composer/PSR-4 tiers live before reaching here.
fn cached_walk(class_name: &str, root: &Path, include_vendor: bool) -> Option<PathBuf> {
    let key = (canonical_root(root), class_name.to_string(), include_vendor);
    if let Some(hit) = locator_cache().lock().unwrap().get(&key).cloned() {
        match &hit.path {
            // Positive hit — trust it only while the file still exists AND still
            // resolves inside the project root (a swapped-in symlink could now
            // escape it). Re-applying the containment guard here mirrors the
            // uncached path exactly.
            Some(path) if path.exists() && path_within_root(path, root) => {
                return Some(path.clone())
            }
            // Stale positive (deleted, moved, or now-escaping) — re-walk.
            Some(_) => {}
            // Negative hit — reuse until the TTL lapses, then re-walk so a
            // since-created class becomes discoverable even without a watcher event.
            None if hit.cached_at.elapsed() < NEGATIVE_TTL => return None,
            None => {}
        }
    }
    walk_and_cache(&key, class_name, root, include_vendor)
}

/// Run the uncached WalkDir and store its result under `key`. Shared by
/// [`cached_walk`] (on a cache miss) and [`live_app_walk`] (the precedence
/// re-confirmation), so both compute and cache identically.
fn walk_and_cache(
    key: &LocatorKey,
    class_name: &str,
    root: &Path,
    include_vendor: bool,
) -> Option<PathBuf> {
    let resolved = find_php_class_file_impl(class_name, root, include_vendor);
    locator_cache().lock().unwrap().put(
        key.clone(),
        CacheEntry {
            path: resolved.clone(),
            cached_at: std::time::Instant::now(),
        },
    );
    resolved
}

/// A LIVE app-only walk that bypasses a cached app-*negative* (but refreshes the
/// cache with the result). Used only on the app-or-vendor precedence path: when
/// the cached app walk said "miss" and vendor would otherwise win, this
/// re-confirms app is genuinely empty right now — so a since-created app file
/// (in a directory the watcher doesn't cover, e.g. `app/Models/`, `Modules/…`)
/// shadows vendor exactly as a fully-live app-then-vendor walk would.
///
/// A cached app-*positive* is trusted as-is (still revalidated by exists +
/// containment) — only the negative is bypassed, and only in the rare branch
/// where a vendor positive is about to be returned, so the miss-walk perf win is
/// preserved for the common all-miss case.
///
/// ## Deliberate trade-off (accepted)
///
/// For a vendor-only, non-PSR-4 class this re-walks `app/` on *every* lookup
/// (the cached app-negative is intentionally bypassed here). This is chosen over
/// honoring the negative TTL because honoring it would reintroduce the
/// precedence bug where a stale app-negative lets vendor shadow a since-created
/// app file (issue: CON2/CON3 lineage). It is **not** a regression: the
/// pre-refactor resolver ran BOTH a live `app/` walk AND a live 31k-file
/// `vendor/` walk on every call, so "cached vendor positive + one live app-only
/// walk" is strictly faster than that baseline even in this worst case. Build
/// time is unaffected — Part 1's shared `ClassViewCache` already dedupes
/// resolution to once per FQCN per build.
fn live_app_walk(class_name: &str, root: &Path) -> Option<PathBuf> {
    let key = (canonical_root(root), class_name.to_string(), false);
    // Trust a still-valid cached app-positive without re-walking.
    if let Some(hit) = locator_cache().lock().unwrap().get(&key).cloned() {
        if let Some(path) = &hit.path {
            if path.exists() && path_within_root(path, root) {
                return Some(path.clone());
            }
        }
    }
    // Cached miss (or stale positive) — re-walk live and refresh the cache.
    walk_and_cache(&key, class_name, root, false)
}

/// Drop every cached walk result for `project_root`. Called by the file watcher
/// on a `.php` create/delete so a newly-added class (previously a cached miss)
/// becomes discoverable, and a moved/renamed one isn't served from a stale
/// positive.
pub fn invalidate_project(project_root: &Path) {
    let key_root = canonical_root(project_root);
    // `LruCache` has no `retain`; rebuild without this root's entries. The cache
    // is small (bounded) and invalidation is rare (a watched create/delete), so
    // the O(n) rebuild is fine.
    let mut cache = locator_cache().lock().unwrap();
    let keep: Vec<(LocatorKey, CacheEntry)> = cache
        .iter()
        .filter(|((root, _, _), _)| root != &key_root)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    cache.clear();
    for (k, v) in keep {
        cache.put(k, v);
    }
}

/// Locate the PHP source file for a given class name.
///
/// Searches the project's `app/` directory recursively for `<ClassName>.php`,
/// preferring files whose path segments match the class's namespace shape when
/// possible.
///
/// Returns the first matching file path, or `None` when the class can't be
/// found. Does not parse the file to verify the class name inside — relies on
/// Laravel's strong convention that file basename matches class name.
///
/// The precedence-ordered tiers (Composer autoload, PSR-4 shape) run live on
/// every call; only the final basename walk is memoized (see [`cached_walk`]).
pub fn find_php_class_file(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Composer autoload is the authoritative source for any FQCN with
    // a declared PSR-4 prefix. If it resolves the class — whether to
    // an app-side or vendor-side path — trust it. A vendor FQCN like
    // `CrossBibleInc\BibleModels\Models\Book` MUST route to vendor;
    // falling back to a basename walk under `app/` for such an FQCN
    // would land on the first same-named file (e.g.
    // `app/Nova/Filters/Book.php`), which is the wrong class.
    let autoload = crate::composer_autoload::ComposerAutoload::for_project(root);
    if let Some(path) = autoload.resolve(class_name) {
        return Some(path);
    }

    // Composer doesn't know the FQCN. Try the heuristic mappings for
    // projects without an installed.json (or for namespaces the user
    // hasn't declared in composer.json). Cheap App and vendor PSR-4
    // shape checks, no walking.
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, false) {
        return Some(path);
    }
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, true) {
        return Some(path);
    }

    // Last resort: basename walk under `app/` (and `src/`) — the expensive tier,
    // memoized. Vendor is intentionally not walked here; the app-or-vendor entry
    // point below handles inheritance walks that legitimately need vendor.
    cached_walk(class_name, root, false)
}

/// Same as [`find_php_class_file`] but ALSO searches `vendor/` so the
/// inheritance walker can pick up parent classes shipped by Laravel
/// packages (e.g. `OAuthAccessToken extends Laravel\Passport\Token`
/// — Token lives in `vendor/laravel/passport/src/Token.php`).
///
/// Slower than the app-only variant because vendor trees are huge. Use
/// it only for inheritance walking, where the search depth is bounded
/// (≤10 levels) and the result is cached behind ModelMetadata anyway.
/// app/-side definitions still win — they're checked first.
///
/// As with [`find_php_class_file`], the precedence tiers run live; only the
/// (here, vendor-spanning and thus most expensive) basename walk is memoized.
pub fn find_php_class_file_in_app_or_vendor(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Composer first (same reasoning as `find_php_class_file`). For
    // the inheritance walker this is normally enough — parent classes
    // declared in any installed package land via PSR-4.
    let autoload = crate::composer_autoload::ComposerAutoload::for_project(root);
    if let Some(path) = autoload.resolve(class_name) {
        return Some(path);
    }

    // Heuristic fallbacks for projects without installed.json.
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, false) {
        return Some(path);
    }
    if let Some(path) = find_php_class_file_by_fqcn(class_name, root, true) {
        return Some(path);
    }

    // Last resort: basename walk app/, then vendor/ — both memoized. app/ is
    // tried first so an app-side definition shadows a vendor one.
    if let Some(path) = cached_walk(class_name, root, false) {
        return Some(path);
    }

    // App walk missed (possibly a *cached* miss). Before letting vendor win, do a
    // live app re-walk that bypasses the app-negative cache — otherwise a stale
    // app-negative (whose file lives in a watcher-uncovered dir) would let vendor
    // wrongly shadow a since-created app file. Only pay this when vendor actually
    // resolves; vendor-walk positives are rare (non-PSR-4 classes), so the common
    // all-miss case keeps its cached-negative fast path.
    let vendor = cached_walk(class_name, root, true);
    if vendor.is_some() {
        if let Some(app) = live_app_walk(class_name, root) {
            return Some(app);
        }
    }
    vendor
}

/// Heuristic FQCN → file path mapping for projects without an
/// `installed.json` (or where the user hasn't declared an autoload
/// entry in `composer.json`). Used as a *fallback* below the
/// Composer autoload step in the public lookup functions.
///
/// Mappings:
/// - `App\Models\User` → `app/Models/User.php` (or `src/Models/User.php`
///   for projects that use `src/` for app code)
/// - `Laravel\Passport\Token` → `vendor/laravel/passport/src/Token.php`
///   (lowercased vendor + package, then `src/`, then remaining
///   namespace segments). Misses for hyphenated package dirs —
///   Composer autoload (the step above) handles those correctly.
///
/// `search_vendor`: `true` consults the vendor heuristic only;
/// `false` the App heuristic only. Callers chain both.
///
/// Returns `None` if no candidate path exists on disk. Each candidate is gated
/// by the fail-closed [`path_within_root`] guard before the on-disk check, so an
/// FQCN whose `..` segments — or an under-root symlink in the constructed path —
/// would resolve outside the project root is refused rather than read (issue
/// #218, containment lineage #130 → #143 → #148 → #194 → #199 → #201 → #214).
fn find_php_class_file_by_fqcn(
    fqcn: &str,
    project_root: &Path,
    search_vendor: bool,
) -> Option<PathBuf> {
    let segments: Vec<&str> = fqcn.split('\\').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    let class_name = *segments.last().unwrap();
    let ns_segments = &segments[..segments.len() - 1];

    if !search_vendor {
        // App\Models\User → app/Models/User.php (and src/ alternative for
        // projects that use src/ for app code).
        if ns_segments.first().map(|s| s.to_ascii_lowercase()) == Some("app".to_string()) {
            let rest = &ns_segments[1..];
            for app_dir in ["app", "src"] {
                let mut path = project_root.join(app_dir);
                for seg in rest {
                    path = path.join(seg);
                }
                path = path.join(format!("{class_name}.php"));
                // An FQCN segment may carry `..` (or the joined path may cross
                // an under-root symlink), so the candidate can resolve outside
                // the project root. Refuse it with the fail-closed guard before
                // the read — a path that canonicalizes outside root (or can't be
                // proven in-root) is skipped.
                if !path_within_root(&path, project_root) {
                    continue;
                }
                if path.exists() {
                    return Some(path);
                }
            }
        }
        return None;
    }

    // Vendor convention: lowercase first two segments → package
    // directory; remaining segments are paths under `src/` (or under
    // the package root if `src/` doesn't exist for this package).
    if ns_segments.len() < 2 {
        return None;
    }
    let vendor = ns_segments[0].to_ascii_lowercase();
    let pkg = ns_segments[1].to_ascii_lowercase();
    let rest = &ns_segments[2..];

    for src_segment in ["src", ""] {
        let mut path = project_root.join("vendor").join(&vendor).join(&pkg);
        if !src_segment.is_empty() {
            path = path.join(src_segment);
        }
        for seg in rest {
            path = path.join(seg);
        }
        path = path.join(format!("{class_name}.php"));
        // Same containment guard as the app branch: an FQCN with `..` segments
        // (or an under-root symlink in the path) can escape the project root, so
        // gate the candidate with the fail-closed guard before the read.
        if !path_within_root(&path, project_root) {
            continue;
        }
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn find_php_class_file_impl(class_name: &str, root: &Path, search_vendor: bool) -> Option<PathBuf> {
    if class_name.is_empty() {
        return None;
    }
    let simple_name = class_name.rsplit('\\').next().unwrap_or(class_name);
    let target_filename = format!("{}.php", simple_name);

    let roots: Vec<PathBuf> = if search_vendor {
        vec![root.join("vendor")]
    } else {
        search_roots(root)
    };

    for app_root in roots {
        if !app_root.is_dir() {
            continue;
        }
        let walker = WalkDir::new(&app_root).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // When searching vendor itself, allow descent INTO vendor —
            // only skip nested vendor/.git/.node_modules dirs.
            if search_vendor {
                !matches!(name.as_ref(), "node_modules" | ".git")
            } else {
                !matches!(name.as_ref(), "vendor" | "node_modules" | ".git")
            }
        });
        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.file_name() == target_filename.as_str() {
                return Some(entry.into_path());
            }
        }
    }

    None
}

/// Directories worth searching for class files. Standard Laravel uses `app/`;
/// some projects also use `src/` for libraries living alongside the app.
fn search_roots(root: &Path) -> Vec<PathBuf> {
    vec![root.join("app"), root.join("src")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Spin up a Laravel-shaped tempdir with the given (path, body)
    /// pairs. Paths are relative to the project root.
    fn project_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        for (relpath, body) in files {
            let full = dir.path().join(relpath);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, body).unwrap();
        }
        let root = dir.path().to_path_buf();
        (dir, root)
    }

    /// Serialize the cache-behavior tests. The `locator_cache` is process-global,
    /// and Rust runs tests in parallel — so a test that asserts on *cache
    /// persistence* (a cached negative still hiding a file, a `peek` on state)
    /// would race another test's `reset_locator_cache()`. Each such test holds
    /// this guard for its duration, then `reset_locator_cache()` starts it clean.
    /// Returns a guard; keep it bound (`let _g = ...;`) for the whole test.
    fn serial_cache_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Recover from a poisoned lock (a panicking serial test) so one failure
        // doesn't cascade into "poisoned" failures for every later serial test.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn fqcn_aware_lookup_prefers_namespace_shape_match() {
        // Mike's crossbible-vapor case: TWO files named Version.php live
        // in the project. The FQCN `App\Models\Version` should map to
        // `app/Models/Version.php`, NOT to `app/Nova/Filters/Version.php`
        // even though the latter is also a Version.php with the same
        // basename.
        let (_dir, root) = project_with_files(&[
            (
                "app/Models/Version.php",
                "<?php\nnamespace App\\Models;\nclass Version {}",
            ),
            (
                "app/Nova/Filters/Version.php",
                "<?php\nnamespace App\\Nova\\Filters;\nclass Version {}",
            ),
        ]);
        let path =
            find_php_class_file("App\\Models\\Version", &root).expect("should find the model");
        assert!(
            path.ends_with("app/Models/Version.php"),
            "should pick the namespace-matching file; got: {path:?}"
        );
    }

    #[test]
    fn fqcn_aware_lookup_falls_back_to_basename_when_no_shape_match() {
        // If only one Version.php exists in the project (no PSR-4 match
        // possible), the basename walk still finds it.
        let (_dir, root) =
            project_with_files(&[("app/SomeOtherPlace/Version.php", "<?php\nclass Version {}")]);
        let path = find_php_class_file("App\\Models\\Version", &root).expect("fallback walk");
        assert!(path.ends_with("app/SomeOtherPlace/Version.php"));
    }

    #[test]
    fn fqcn_lookup_routes_vendor_classes_to_psr4_path() {
        // `Laravel\Passport\Token` should resolve to the standard
        // Composer PSR-4 path. Note: only `find_php_class_file_in_app_or_vendor`
        // searches vendor — `find_php_class_file` stays app-side only.
        let (_dir, root) = project_with_files(&[(
            "vendor/laravel/passport/src/Token.php",
            "<?php\nnamespace Laravel\\Passport;\nclass Token {}",
        )]);
        let path = find_php_class_file_in_app_or_vendor("Laravel\\Passport\\Token", &root)
            .expect("vendor PSR-4 lookup");
        assert!(path.ends_with("vendor/laravel/passport/src/Token.php"));
    }

    #[test]
    fn fqcn_lookup_app_class_shadows_vendor_match() {
        // Both an app/-side and a vendor/-side file with the same FQCN
        // exist? App wins (matches PSR-4 autoload behavior).
        let (_dir, root) = project_with_files(&[
            (
                "app/Models/Token.php",
                "<?php\nnamespace App\\Models;\nclass Token {}",
            ),
            (
                "vendor/laravel/passport/src/Token.php",
                "<?php\nnamespace Laravel\\Passport;\nclass Token {}",
            ),
        ]);
        let path = find_php_class_file_in_app_or_vendor("App\\Models\\Token", &root).unwrap();
        assert!(
            path.ends_with("app/Models/Token.php"),
            "App\\Models\\Token should resolve to the project file; got {path:?}"
        );
    }

    #[test]
    fn find_php_class_file_routes_vendor_fqcn_via_composer_autoload() {
        // Phase 5.12: the dotted-walker hands `find_php_class_file` a
        // vendor FQCN (Phase 5.11 made related_model store FQCNs).
        // Composer autoload knows the real PSR-4 mapping, including for
        // hyphenated package dirs. We must trust it even when the
        // lookup is "app-side" — falling back to a basename walk under
        // app/ for a vendor FQCN finds a same-named app file (e.g.
        // app/Nova/Filters/Book.php), which is the wrong class.
        let installed = r#"{
            "packages": [
                {
                    "name": "crossbibleinc/bible-models",
                    "autoload": {
                        "psr-4": { "CrossBibleInc\\BibleModels\\": "src/" }
                    },
                    "install-path": "../crossbibleinc/bible-models"
                }
            ]
        }"#;
        let (_dir, root) = project_with_files(&[
            ("vendor/composer/installed.json", installed),
            (
                "vendor/crossbibleinc/bible-models/src/Models/Book.php",
                "<?php\nnamespace CrossBibleInc\\BibleModels\\Models;\nclass Book {}",
            ),
            (
                // Same-basename app file — must NOT be picked.
                "app/Nova/Filters/Book.php",
                "<?php\nnamespace App\\Nova\\Filters;\nclass Book {}",
            ),
        ]);
        let path = find_php_class_file("CrossBibleInc\\BibleModels\\Models\\Book", &root)
            .expect("Composer autoload should route the vendor FQCN");
        assert!(
            path.ends_with("vendor/crossbibleinc/bible-models/src/Models/Book.php"),
            "vendor FQCN must resolve to the vendor file via Composer autoload; got {path:?}"
        );
    }

    #[test]
    fn bare_class_name_with_no_namespace_still_uses_basename_walk() {
        // `Foo` (no namespace) doesn't have a PSR-4 shape — should fall
        // through to basename walking.
        let (_dir, root) = project_with_files(&[("app/Services/Foo.php", "<?php\nclass Foo {}")]);
        let path = find_php_class_file("Foo", &root).expect("bare-name walk");
        assert!(path.ends_with("app/Services/Foo.php"));
    }

    // ─── Walk-fallback cache (Part 2) ─────────────────────────────────────
    //
    // Only the last-resort basename WalkDir is memoized — the Composer/PSR-4
    // tiers run live every call. So these tests use *shapeless* names (no
    // `App\` PSR-4 prefix that the fast path would resolve), which fall through
    // to the cached walk. `reset_locator_cache()` at the top of each isolates
    // it from the process-global cache other tests populate.

    /// Peek a cache entry under the lock without disturbing LRU order much.
    fn peek(root: &Path, name: &str, vendor: bool) -> Option<Option<PathBuf>> {
        let key = (canonical_root(root), name.to_string(), vendor);
        locator_cache()
            .lock()
            .unwrap()
            .get(&key)
            .map(|e| e.path.clone())
    }

    #[test]
    fn cached_walk_miss_is_reused_as_negative_entry() {
        let _g = serial_cache_guard();
        reset_locator_cache();
        // `Ghost` has no namespace → no PSR-4 shape → falls through to the walk,
        // which finds nothing and records a negative entry.
        let (_dir, root) = project_with_files(&[("app/Services/Real.php", "<?php\nclass Real {}")]);
        assert!(find_php_class_file("Ghost", &root).is_none());
        assert_eq!(
            peek(&root, "Ghost", false),
            Some(None),
            "the walk miss should be recorded as a negative cache entry"
        );
        assert!(find_php_class_file("Ghost", &root).is_none());
    }

    #[test]
    fn cached_walk_positive_revalidates_after_file_deleted() {
        let _g = serial_cache_guard();
        reset_locator_cache();
        // Shapeless name → resolved by the walk and cached as a positive.
        let (_dir, root) =
            project_with_files(&[("app/Services/Gadget.php", "<?php\nclass Gadget {}")]);
        let found = find_php_class_file("Gadget", &root).expect("walk resolves once");
        assert_eq!(peek(&root, "Gadget", false), Some(Some(found.clone())));
        std::fs::remove_file(&found).unwrap();
        // The cached positive points at a now-missing file — revalidation
        // (`exists()`) must reject it and re-walk to None.
        assert!(
            find_php_class_file("Gadget", &root).is_none(),
            "a deleted file must not be served from a stale positive cache entry"
        );
    }

    #[test]
    fn cached_walk_positive_revalidates_containment_on_hit() {
        // CON3: a positive hit is re-checked with `path_within_root`, not just
        // `exists()`. Seed the cache with an entry whose file exists but resolves
        // OUTSIDE the project root (an escaping symlink target) — it must be
        // rejected and re-walked, not served.
        let _g = serial_cache_guard();
        reset_locator_cache();
        let (_dir, root) = project_with_files(&[("app/Services/Real.php", "<?php\nclass Real {}")]);

        // A real file living outside the project root.
        let outside_dir = TempDir::new().unwrap();
        let outside = outside_dir.path().join("Escapee.php");
        std::fs::write(&outside, "<?php\nclass Escapee {}").unwrap();

        // Manually seed a positive cache entry pointing at the out-of-root file.
        let key = (canonical_root(&root), "Escapee".to_string(), false);
        locator_cache().lock().unwrap().put(
            key,
            CacheEntry {
                path: Some(outside.clone()),
                cached_at: std::time::Instant::now(),
            },
        );

        // The file exists, so a bare `exists()` check would serve it — but the
        // containment guard must reject an out-of-root path and re-walk (which
        // finds nothing inside the project).
        assert!(
            find_php_class_file("Escapee", &root).is_none(),
            "an out-of-root cached path must be rejected by the containment re-check, not served"
        );
    }

    #[test]
    fn fast_path_precedence_re_evaluated_live_app_shadows_vendor() {
        // CON2: because the Composer/PSR-4 tiers are NOT cached, a later app-side
        // file shadows an earlier vendor resolution with no invalidation needed.
        //
        // First call: only the vendor file exists, so `App\Widgets\Panel`'s app
        // PSR-4 shape misses and the app-or-vendor lookup resolves to vendor.
        let _g = serial_cache_guard();
        reset_locator_cache();
        let (dir, root) = project_with_files(&[(
            "vendor/acme/widgets/src/Panel.php",
            "<?php\nnamespace App\\Widgets;\nclass Panel {}",
        )]);
        // No app file yet: the vendor PSR-4 heuristic (lowercased vendor/pkg)
        // won't match `App\Widgets\...`, so this falls to the vendor walk.
        let first =
            find_php_class_file_in_app_or_vendor("App\\Widgets\\Panel", &root).expect("resolves");
        assert!(
            first.to_string_lossy().contains("vendor/"),
            "with no app file, resolution lands in vendor; got {first:?}"
        );

        // Now create the app-side PSR-4 file for the SAME FQCN.
        let app_file = dir.path().join("app/Widgets/Panel.php");
        std::fs::create_dir_all(app_file.parent().unwrap()).unwrap();
        std::fs::write(&app_file, "<?php\nnamespace App\\Widgets;\nclass Panel {}").unwrap();

        // The next call must return the APP file: the live PSR-4 shape tier
        // (`App\` → `app/`) runs before the cached walk and shadows it. No
        // invalidate_project call — precedence is simply never cached.
        let second =
            find_php_class_file_in_app_or_vendor("App\\Widgets\\Panel", &root).expect("resolves");
        assert!(
            second.ends_with("app/Widgets/Panel.php"),
            "the live app PSR-4 tier must shadow the earlier vendor result; got {second:?}"
        );
    }

    #[test]
    fn walk_tier_precedence_app_shadows_vendor_after_cached_vendor_hit() {
        // The residual precedence bug: an FQCN that reaches the WALK tier (no
        // `App\` prefix, not PSR-4-resolvable) resolves to vendor and caches an
        // app-NEGATIVE. Then an app-side file is created in a watcher-uncovered
        // dir (no invalidate_project). A fully-live app-then-vendor walk would
        // now return APP — the cached app-negative must NOT let vendor shadow it.
        let _g = serial_cache_guard();
        reset_locator_cache();

        // `Modules\Blog\Thing`: first namespace segment is `modules` (not `app`),
        // so the app PSR-4 shape misses. The vendor file is placed under a
        // package dir that does NOT match the `vendor/modules/blog` heuristic
        // shape, so the vendor PSR-4 heuristic misses too — BOTH tiers fall
        // through to `cached_walk`, which is exactly what we must exercise.
        let (dir, root) = project_with_files(&[(
            "vendor/acme/legacy/src/Thing.php",
            "<?php\nnamespace Modules\\Blog;\nclass Thing {}",
        )]);

        // Call 1: app-walk misses (caches an app-negative), vendor-walk finds
        // Thing.php by basename → resolves to vendor.
        let first = find_php_class_file_in_app_or_vendor("Modules\\Blog\\Thing", &root)
            .expect("vendor walk resolves");
        assert!(
            first.to_string_lossy().contains("vendor/"),
            "with no app file, the walk lands in vendor; got {first:?}"
        );

        // Create the app-side file in an UNWATCHED dir (app/Blog — the file
        // watcher covers only Controllers/routes/migrations/views/livewire/
        // vendor/Inertia, never app/Blog). NO invalidate_project call.
        let app_file = dir.path().join("app/Blog/Thing.php");
        std::fs::create_dir_all(app_file.parent().unwrap()).unwrap();
        std::fs::write(&app_file, "<?php\nnamespace Modules\\Blog;\nclass Thing {}").unwrap();

        // Call 2: the cached app-negative must NOT let the cached vendor positive
        // win. A live app re-walk finds the new app file, which shadows vendor —
        // matching a fully-live app-then-vendor walk.
        let second =
            find_php_class_file_in_app_or_vendor("Modules\\Blog\\Thing", &root).expect("resolves");
        assert!(
            second.ends_with("app/Blog/Thing.php"),
            "a since-created app file must shadow the cached vendor result (live app re-walk); got {second:?}"
        );

        // And it self-heals the cache: a third call returns app from the now-
        // positive app cache entry without another live walk.
        let third =
            find_php_class_file_in_app_or_vendor("Modules\\Blog\\Thing", &root).expect("resolves");
        assert!(third.ends_with("app/Blog/Thing.php"));
    }

    #[test]
    fn walk_tier_all_miss_stays_cached_no_extra_walk() {
        // Perf guard: when BOTH app and vendor walks miss, the live app re-walk
        // must NOT fire (vendor is None), so the cached-negative fast path is
        // preserved for the common repeated-miss case. We assert the negative is
        // cached and reused.
        let _g = serial_cache_guard();
        reset_locator_cache();
        let (_dir, root) = project_with_files(&[("app/Services/Real.php", "<?php\nclass Real {}")]);

        assert!(find_php_class_file_in_app_or_vendor("Modules\\Blog\\Ghost", &root).is_none());
        // Both tiers cached negative.
        assert_eq!(peek(&root, "Modules\\Blog\\Ghost", false), Some(None));
        assert_eq!(peek(&root, "Modules\\Blog\\Ghost", true), Some(None));
        // Repeat still None, served from cache.
        assert!(find_php_class_file_in_app_or_vendor("Modules\\Blog\\Ghost", &root).is_none());
    }

    #[test]
    fn invalidate_project_lets_newly_created_walk_class_resolve() {
        let _g = serial_cache_guard();
        reset_locator_cache();
        // Shapeless miss cached; create the file; invalidate; now it resolves.
        let (dir, root) = project_with_files(&[("app/Services/Real.php", "<?php\nclass Real {}")]);
        assert!(find_php_class_file("Fresh", &root).is_none());

        std::fs::write(
            dir.path().join("app/Services/Fresh.php"),
            "<?php\nclass Fresh {}",
        )
        .unwrap();

        // Before invalidation the cached negative still hides it.
        assert!(find_php_class_file("Fresh", &root).is_none());

        invalidate_project(&root);
        let found = find_php_class_file("Fresh", &root)
            .expect("after invalidation the newly created class resolves");
        assert!(found.ends_with("app/Services/Fresh.php"));
    }

    #[test]
    fn negative_walk_entry_expires_after_ttl() {
        let _g = serial_cache_guard();
        reset_locator_cache();
        // The Fork-2 fallback: a cached walk miss must stop hiding a class
        // created after it, even without a watcher event. Back-date the entry.
        let (dir, root) = project_with_files(&[("app/Services/Real.php", "<?php\nclass Real {}")]);
        assert!(find_php_class_file("Later", &root).is_none());

        std::fs::write(
            dir.path().join("app/Services/Later.php"),
            "<?php\nclass Later {}",
        )
        .unwrap();
        let key = (canonical_root(&root), "Later".to_string(), false);
        locator_cache().lock().unwrap().put(
            key,
            CacheEntry {
                path: None,
                cached_at: std::time::Instant::now() - (NEGATIVE_TTL + Duration::from_secs(1)),
            },
        );

        let found = find_php_class_file("Later", &root)
            .expect("an expired negative entry must re-resolve and find the new class");
        assert!(found.ends_with("app/Services/Later.php"));
    }

    #[test]
    fn invalidate_project_only_clears_its_own_root() {
        let _g = serial_cache_guard();
        reset_locator_cache();
        // Invalidating one project must not evict another project's entries.
        let (_dir_a, root_a) = project_with_files(&[("app/Services/A.php", "<?php\nclass A {}")]);
        let (_dir_b, root_b) = project_with_files(&[("app/Services/B.php", "<?php\nclass B {}")]);
        find_php_class_file("A", &root_a).expect("A resolves via walk");
        find_php_class_file("B", &root_b).expect("B resolves via walk");

        invalidate_project(&root_a);

        assert!(
            peek(&root_b, "B", false).is_some(),
            "project B's cache entry must survive project A's invalidation"
        );
        assert!(
            peek(&root_a, "A", false).is_none(),
            "project A's entry must be gone after its invalidation"
        );
    }
}
