//! End-to-end handler coverage for Inertia goto-definition and completion
//! (issue #207 — follow-up from Holmes's review of #10 / PR #204).
//!
//! The Inertia feature (#10) resolves an Inertia page name to a JS/TS file under
//! `resources/js/Pages/` for goto-definition, and lists those pages for
//! completion. The pure path-math seam — `resolve_page_candidates`,
//! `resolve_existing_page`, `list_pages` — has thorough tempdir unit tests in
//! `inertia.rs`. But nothing drove the *real LSP request handlers* that wire
//! those seams to the wire protocol: `Backend::resolve_inertia_file` (the goto
//! arm) and `Backend::get_all_inertia_pages` (the completion arm). A regression
//! in those handler arms — a dropped containment guard, an inverted existence
//! check, a `None` for a page that *does* resolve, or a phantom target for one
//! that doesn't — would slip through every lower-level test.
//!
//! These tests drive the **real** handlers on a server built through the same
//! `tower_lsp::LspService` harness `flux_goto_def_handler.rs` and
//! `blade_var_rename_handler.rs` use. The two handlers read different state, and
//! the fixtures prime exactly what each one reads:
//!   - `resolve_inertia_file` resolves against `get_cached_config().root`, so the
//!     goto tests prime `cached_config` with a minimal root-only config. With it
//!     primed, `get_cached_config` never touches the (inert) Salsa actor; the
//!     document cache is left empty, so `file_exists_cached` falls through to a
//!     real `metadata` probe against the tempdir, exactly as the live server does.
//!   - `get_all_inertia_pages` lists against `root_path`, so the completion tests
//!     prime `root_path` instead.
//!
//! This is the first issue of its theme; future "no handler-harness test for X"
//! findings should expand this anchor rather than spawn near-duplicates.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::LspService;

/// A minimal Inertia page body. The markup is irrelevant to resolution and
/// listing (both operate purely on the *path*), but a real body keeps the
/// fixture honest.
const PAGE_BODY: &str = "<template><div>page</div></template>\n";

/// A `LaravelConfigData` rooted at `root` with every other field empty — the
/// "root only" minimal config the goto AC names. `resolve_inertia_file` only
/// reads `config.root` (Inertia page resolution is pure path math under
/// `resources/js/Pages/`), so none of the Blade/Livewire fields matter here.
fn inertia_config(root: PathBuf) -> LaravelConfigData {
    LaravelConfigData {
        root,
        view_paths: Vec::new(),
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// Build a backend for the **goto** handler (`resolve_inertia_file`) primed with
/// a root-only `cached_config`. `LspService::new` wires up a real `Client`,
/// `inner().clone()` hands back the `LaravelLanguageServer`, and we prime the one
/// piece of state the goto handler reads — the cached config (whose `root` it
/// resolves against). `inertia_default_ext` and `root_path` are left unprimed, so
/// `inertia_dominant_extension` resolves to `None` and the static
/// `PAGE_EXTENSIONS` priority order decides ambiguous matches.
async fn goto_backend(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.cached_config.write().await = Some(inertia_config(root.to_path_buf()));
    backend
}

/// Build a backend for the **completion** handler (`get_all_inertia_pages`)
/// primed with `root_path`. That handler lists pages against `root_path`, not the
/// cached config, so this is the only state it needs.
async fn completion_backend(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
}

/// Write `body` to `relpath` under `dir`, creating parent directories, and
/// return the absolute path written.
fn write_file(dir: &Path, relpath: &str, body: &str) -> PathBuf {
    let full = dir.join(relpath);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(&full, body).unwrap();
    full
}

#[tokio::test]
async fn inertia_page_resolves_to_vue_file() {
    // goto — happy path (.vue): `inertia('Dashboard')` → the real handler
    // resolves it to `resources/js/Pages/Dashboard.vue`.
    let dir = TempDir::new().unwrap();
    let expected = write_file(dir.path(), "resources/js/Pages/Dashboard.vue", PAGE_BODY);

    let backend = goto_backend(dir.path()).await;
    let resolved = backend
        .resolve_inertia_file("Dashboard")
        .await
        .expect("a page name with a backing .vue file resolves to its path");

    assert_eq!(
        resolved, expected,
        "goto-def lands on the .vue page under resources/js/Pages"
    );
}

#[tokio::test]
async fn inertia_nested_page_resolves_to_tsx_file() {
    // goto — nested page (.tsx): a `/`-nested name maps to nested directories:
    // `inertia('Auth/Login')` → `resources/js/Pages/Auth/Login.tsx`.
    let dir = TempDir::new().unwrap();
    let expected = write_file(dir.path(), "resources/js/Pages/Auth/Login.tsx", PAGE_BODY);

    let backend = goto_backend(dir.path()).await;
    let resolved = backend
        .resolve_inertia_file("Auth/Login")
        .await
        .expect("a nested page name resolves the '/' to a nested directory");

    assert_eq!(
        resolved, expected,
        "the nested segment maps to a nested path under resources/js/Pages"
    );
}

#[tokio::test]
async fn unresolvable_inertia_page_returns_none() {
    // goto — unresolvable returns None: no page file exists for `inertia('Ghost')`.
    // The handler must return `None` — never a phantom target for a missing file.
    let dir = TempDir::new().unwrap();
    // A real but unrelated page, to prove the `None` is about the missing target
    // and not an empty/unconfigured project.
    write_file(dir.path(), "resources/js/Pages/Dashboard.vue", PAGE_BODY);

    let backend = goto_backend(dir.path()).await;
    let resolved = backend.resolve_inertia_file("Ghost").await;

    assert!(
        resolved.is_none(),
        "an unresolvable page yields no target, got {resolved:?}"
    );
}

#[tokio::test]
async fn inertia_page_extension_priority_prefers_vue() {
    // goto — extension priority: both `Dashboard.vue` and `Dashboard.tsx` exist
    // and no dominant extension is primed (`inertia_default_ext` and `root_path`
    // are both unset, so `inertia_dominant_extension` is `None`). The handler must
    // fall back to the static `PAGE_EXTENSIONS` order, where `.vue` is first and
    // therefore wins.
    let dir = TempDir::new().unwrap();
    let expected = write_file(dir.path(), "resources/js/Pages/Dashboard.vue", PAGE_BODY);
    write_file(dir.path(), "resources/js/Pages/Dashboard.tsx", PAGE_BODY);

    let backend = goto_backend(dir.path()).await;
    let resolved = backend
        .resolve_inertia_file("Dashboard")
        .await
        .expect("an ambiguous page still resolves to a target");

    assert_eq!(
        resolved, expected,
        ".vue wins on the static PAGE_EXTENSIONS priority order when no dominant extension is set"
    );
    assert_eq!(
        resolved.extension().and_then(|e| e.to_str()),
        Some("vue"),
        "the resolved target carries the .vue extension"
    );
}

#[tokio::test]
async fn inertia_pages_are_listed_sorted_without_extension() {
    // completion — pages listed: `get_all_inertia_pages` returns every page as a
    // '/'-nested name without extension, sorted.
    let dir = TempDir::new().unwrap();
    write_file(dir.path(), "resources/js/Pages/Dashboard.vue", PAGE_BODY);
    write_file(dir.path(), "resources/js/Pages/Auth/Login.tsx", PAGE_BODY);

    let backend = completion_backend(dir.path()).await;
    let pages = backend.get_all_inertia_pages().await;

    assert_eq!(
        pages,
        vec!["Auth/Login".to_string(), "Dashboard".to_string()],
        "pages list sorted and without extension"
    );
}

#[tokio::test]
async fn inertia_completion_is_empty_without_pages_dir() {
    // completion — empty project: no `resources/js/Pages/` directory at all. The
    // handler must return an empty list, not error or panic.
    let dir = TempDir::new().unwrap();

    let backend = completion_backend(dir.path()).await;
    let pages = backend.get_all_inertia_pages().await;

    assert!(
        pages.is_empty(),
        "a project with no pages directory has no completions, got {pages:?}"
    );
}

#[tokio::test]
async fn inertia_completion_excludes_non_page_files() {
    // completion — extension filter: files under `resources/js/Pages/` whose
    // extension is not an Inertia page extension (`.php`, `.blade.php`) must be
    // excluded from the completion list.
    let dir = TempDir::new().unwrap();
    write_file(dir.path(), "resources/js/Pages/Dashboard.vue", PAGE_BODY);
    write_file(dir.path(), "resources/js/Pages/Legacy.php", "<?php\n");
    write_file(
        dir.path(),
        "resources/js/Pages/Old.blade.php",
        "<div></div>\n",
    );

    let backend = completion_backend(dir.path()).await;
    let pages = backend.get_all_inertia_pages().await;

    assert_eq!(
        pages,
        vec!["Dashboard".to_string()],
        "only the .vue page is a completion; .php and .blade.php are excluded"
    );
}
