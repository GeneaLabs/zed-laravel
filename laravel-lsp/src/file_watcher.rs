//! Server-initiated file watching via LSP `workspace/didChangeWatchedFiles`.
//!
//! Without this, external file changes (a `git pull`, a formatter run
//! outside Zed, another editor saving) leave our in-memory pattern
//! cache stale until the user restarts the LSP — and stale cache means
//! wrong find-references results, which is the single failure mode
//! users are least forgiving of.
//!
//! Design choices, in case they need revisiting later:
//!
//! 1. **Glob-scoped, not project-wide.** We only ask the client to
//!    notify us about files we actually index: the project's PSR-4
//!    source roots (from `composer.json` — `app/`, `src/`, any custom
//!    `Modules\` mapping), the configured view paths, the Livewire path
//!    (if any), `routes/`, `database/migrations/`, `vendor/`, the
//!    Inertia pages dir, and both translation lang roots (`lang/` and
//!    `resources/lang/`). The PSR-4 roots (M2) are what make an external
//!    edit to *any* first-party source dir — not just the hardcoded
//!    controllers path — converge the magic-member index. A
//!    `composer install` that rewrites thousands of files in `vendor/`
//!    still produces one debounced incremental batch, not a project-wide
//!    rebuild.
//!
//! 2. **Server-initiated dynamic registration.** Zed's view paths and
//!    Livewire path depend on the project's `config/view.php` and
//!    `config/livewire.php`, which we can't see during `initialize`
//!    (we haven't read them yet). So we declare the capability statically
//!    in `initialize` and send the actual `workspace/didChangeWatchedFiles`
//!    registration later, from `initialized` once the config is loaded.
//!
//! 3. **Lazy re-parse, eager invalidation.** When an event arrives, we
//!    remove the entry from `pattern_cache` and bump the Salsa file
//!    version. We do NOT spawn a re-parse — the next query that touches
//!    the file pays the parse cost lazily. Spreads work across user-
//!    driven queries instead of bunching it.
//!
//! 4. **Debounced magic-index convergence.** Per-event pattern-cache
//!    work is ~50µs (a DashMap remove + an actor message), too cheap to
//!    coalesce on its own. But since M2 the watched path *also* drives a
//!    dependency-tracked magic-member reconverge, which IS worth
//!    coalescing: a `git checkout` burst is collapsed into one debounced
//!    incremental batch (see `schedule_magic_rebuild` in `main.rs`).
//!
//! 5. **Open-document precedence.** If a file is currently open in the
//!    editor, its in-memory authoritative content is the editor buffer,
//!    NOT what's on disk. `textDocument/didChange` already handles those
//!    updates and pushes buffer text into Salsa. We skip
//!    watched-file events for open paths to avoid clobbering the
//!    buffer with disk content the user hasn't seen yet (race during
//!    external edit while file is open in Zed).

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, Registration,
    WatchKind,
};

/// One registration ID for our single file-watcher registration. Letting
/// the server send a future `client/unregisterCapability` with the same
/// ID would tear it down cleanly — we don't do that today, but the ID
/// is here in case we ever support config reloads that change the
/// watched globs.
pub const REGISTRATION_ID: &str = "laravel-lsp/file-watcher";

/// LSP method this registration is for.
pub const METHOD: &str = "workspace/didChangeWatchedFiles";

/// Build the glob patterns to watch for a given project. Globs are
/// absolute paths — Zed handles both absolute and relative globs, but
/// absolute removes any ambiguity about which workspace folder a
/// pattern is rooted in.
///
/// The set of globs covers exactly the directories that
/// `SalsaActor::handle_register_project_files` enumerates, so the
/// pattern_cache only ever holds entries for files we're also
/// watching.
///
/// `psr4_roots` are the project's first-party PSR-4 source roots
/// (`ComposerAutoload::project_source_roots`) — each gets a recursive
/// `**/*.php` glob so an external edit to any first-party source dir
/// converges the magic-member index (M2). A root whose recursive glob is
/// already emitted verbatim (an exact string match against an earlier
/// glob) is skipped; a merely-overlapping glob (`app/**/*.php` over the
/// fixed `app/Http/Controllers/**/*.php`) is left in place — duplicate
/// events collapse in the idempotent watched-files handler, so the
/// overlap is harmless.
/// A path rendered with forward slashes, for embedding in an LSP glob pattern.
///
/// LSP glob patterns are forward-slash by specification, while `Path::display`
/// emits the platform separator. Interpolating a joined path on Windows
/// therefore produced `C:\proj\resources\views/**/*.blade.php` — both
/// separators in one pattern. Beyond being wrong per spec, the mixed spelling
/// defeated the string-equality dedup that collapses duplicate PSR-4 roots, so
/// the same directory was registered twice (issue #292).
///
/// A literal backslash in a Unix filename would be rewritten here, but a glob
/// base is a directory path the LSP client will match with `/` regardless, so
/// the spec-correct spelling wins.
fn glob_base(path: &std::path::Path) -> String {
    path.display().to_string().replace('\\', "/")
}

pub fn build_watchers(
    root: &Path,
    view_paths: &[PathBuf],
    livewire_path: Option<&Path>,
    psr4_roots: &[PathBuf],
) -> Vec<FileSystemWatcher> {
    // Watch creates, changes, and deletes — all three matter for
    // keeping the pattern cache aligned with disk. The LSP spec's
    // default if `kind` is omitted is also "all three (7)", so we
    // could leave it None; we pass it explicitly for clarity.
    let kind = Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete);

    // 5 fixed (controllers, routes, migrations, vendor php + blade) + 4 Inertia
    // page-extension globs + 6 lang-catalogue globs (3 per lang root) + 1
    // optional livewire + 2 per view path + 1 per PSR-4 source root.
    let mut watchers = Vec::with_capacity(16 + 2 * view_paths.len() + psr4_roots.len());

    // Controllers — current default path. If a project moves them, we
    // miss those changes until a future improvement makes this glob
    // configurable. Acceptable for v1. (The PSR-4 roots below usually
    // cover `app/` too, but this stays as a floor for a project whose
    // `composer.json` we couldn't read.)
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!(
            "{}/app/Http/Controllers/**/*.php",
            root.display()
        )),
        kind,
    });

    // Routes.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/routes/**/*.php", glob_base(root))),
        kind,
    });

    // Migrations — feed the migration index (goto-definition for columns and
    // tables). New/renamed/edited migrations change column definitions.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!(
            "{}/database/migrations/**/*.php",
            root.display()
        )),
        kind,
    });

    // View paths. We watch `.blade.php` first as the primary case, then
    // bare `.php` for the rare anonymous-component-in-PHP-only style.
    // Some projects configure multiple view paths (e.g., themed apps);
    // we register one pair per configured path.
    for view_path in view_paths {
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.blade.php", glob_base(view_path))),
            kind,
        });
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.php", glob_base(view_path))),
            kind,
        });
    }

    // Livewire path, when the project uses it. v3 vs v2 differ in
    // location; the config layer already resolved which one applies.
    if let Some(lw) = livewire_path {
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.php", glob_base(lw))),
            kind,
        });
    }

    // Vendor packages. We index everything composer-installed (see
    // SalsaActor::vendor_files) — the watcher needs matching globs so
    // changes from `composer install`, `composer update`, or a local
    // package symlink edit invalidate the right entries. Two globs
    // cover PHP source and Blade views; the `.json.php` data-file
    // skip lives in the warming filter, not at the watcher layer.
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/vendor/**/*.php", glob_base(root))),
        kind,
    });
    watchers.push(FileSystemWatcher {
        glob_pattern: GlobPattern::String(format!("{}/vendor/**/*.blade.php", glob_base(root))),
        kind,
    });

    // Inertia page files (issue #10). Inertia "views" are JS/TS files under
    // `resources/js/Pages/`, not Blade — so a page created/deleted outside the
    // editor (a `git pull`, another tool) must invalidate the file-existence
    // cache event-driven, not just when the 5-second TTL lapses. One glob per
    // supported extension: Zed's matcher doesn't reliably expand brace
    // alternation (`{vue,tsx,...}`), and per-extension mirrors the explicit
    // blade/php pairing above.
    let pages_dir = crate::inertia::pages_dir(root);
    for ext in crate::inertia::PAGE_EXTENSIONS {
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(format!("{}/**/*.{}", glob_base(&pages_dir), ext)),
            kind,
        });
    }

    // Translation catalogues (issue #293). Until the translation layer was
    // routed through Salsa, every lookup re-read these files from disk, so no
    // glob was needed — nothing was cached, so nothing could go stale. Now that
    // they are cached, an external edit (a `git pull`, a branch switch,
    // `php artisan lang:publish`) must reach `did_change_watched_files` or the
    // session would keep serving the pre-change translation.
    //
    // Both lang roots are watched, not just `lang/`: the resolver searches
    // `lang/` (Laravel 9+) *and* `resources/lang/` (Laravel 8 and earlier), and
    // watching only the first would leave every Laravel-8-layout project
    // serving stale translations — the exact hover/diagnostics divergence
    // issue #288 exists to close.
    //
    // Three globs per root: `**/*.php` for locale directories (which also
    // covers `vendor/`), `*.json` for the top-level text catalogues Laravel
    // reads as `{lang_root}/{locale}.json`, and an explicit `vendor/**/*.php`
    // for published package translations. The last overlaps the first; the
    // watched-files handler is idempotent, so duplicate events collapse.
    for lang_root in crate::translation_lookup::project_lang_roots(root) {
        let base = glob_base(&lang_root);
        for pattern in [
            format!("{base}/**/*.php"),
            format!("{base}/*.json"),
            format!("{base}/vendor/**/*.php"),
        ] {
            watchers.push(FileSystemWatcher {
                glob_pattern: GlobPattern::String(pattern),
                kind,
            });
        }
    }

    // First-party PSR-4 source roots (M2). Each gets a recursive `**/*.php`
    // glob (which also matches `.blade.php`) so an external edit to any
    // first-party source dir — `app/`, `src/`, a custom `Modules\` layout —
    // reaches the watched-files handler and converges the magic-member index.
    // Skip a root whose glob exactly duplicates one already emitted; a merely
    // overlapping glob is left in place (the handler is idempotent).
    let mut existing: std::collections::HashSet<String> = watchers
        .iter()
        .filter_map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => Some(s.clone()),
            GlobPattern::Relative(_) => None,
        })
        .collect();
    for src_root in psr4_roots {
        let glob = format!("{}/**/*.php", glob_base(src_root));
        // Skip a glob already emitted — whether by a fixed watcher or an
        // earlier PSR-4 root this call (defensive: `project_source_roots`
        // already dedups, but two identical roots must never double-register).
        if !existing.insert(glob.clone()) {
            continue;
        }
        watchers.push(FileSystemWatcher {
            glob_pattern: GlobPattern::String(glob),
            kind,
        });
    }

    watchers
}

/// Build the full registration payload for `client/registerCapability`.
/// One registration covers all globs in a single batch — Zed processes
/// them as one watcher set rather than N independent watchers.
pub fn build_registration(
    root: &Path,
    view_paths: &[PathBuf],
    livewire_path: Option<&Path>,
    psr4_roots: &[PathBuf],
) -> Registration {
    let watchers = build_watchers(root, view_paths, livewire_path, psr4_roots);
    let opts = DidChangeWatchedFilesRegistrationOptions { watchers };
    Registration {
        id: REGISTRATION_ID.to_string(),
        method: METHOD.to_string(),
        register_options: serde_json::to_value(opts).ok(),
    }
}

#[cfg(test)]
mod tests;
