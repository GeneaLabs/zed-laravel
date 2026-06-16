//! Tests for the root-containment guard in
//! `LaravelLanguageServer::create_view_location_from_salsa` (issue #148,
//! extending #130).
//!
//! View go-to-definition resolves a `view('ns::name')` reference through
//! `resolve_view_path`, which honours `loadViewsFrom`-style namespaces. A
//! `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')`-style registration can map a
//! namespace to an absolute directory that escapes the project root, so a
//! `pkg::card` reference would otherwise hand the LSP client a `LocationLink`
//! pointing outside the root. The guard re-checks containment against
//! `config.root` — via the same `path_within_root` the slot-navigation, Folio
//! cursor, and blade-var rename flows use — before building the link. These
//! tests drive the private async method directly by building the server through
//! `tower_lsp::LspService` and reaching its inner value with `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{LaravelConfigData, ViewReferenceData};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::{lsp_types::Url, LspService};

/// The Blade view that backs `pkg::card`. Its contents are irrelevant to
/// resolution — only that the file exists on disk, so a `None` result can only
/// come from the containment guard, never a missing file.
const CARD_VIEW: &str = "<div>card</div>\n";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call its
/// private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A minimal config rooted at `root` that registers `pkg` as a `loadViewsFrom`
/// -style view namespace pointing at `namespace_dir`. With this, `pkg::card`
/// resolves to `{namespace_dir}/card.blade.php`, letting the test place that
/// directory inside or outside the project root at will.
fn config_with_view_namespace(root: &Path, namespace_dir: &Path) -> LaravelConfigData {
    let mut view_namespaces = HashMap::new();
    view_namespaces.insert("pkg".to_string(), namespace_dir.to_path_buf());

    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![(String::new(), root.join("resources/views/components"))],
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A `ViewReferenceData` for `name` at the document origin — positions are
/// irrelevant to path resolution.
fn view_ref(name: &str) -> ViewReferenceData {
    ViewReferenceData {
        name: name.to_string(),
        line: 0,
        column: 0,
        end_column: 0,
        is_route_view: false,
    }
}

/// Seed `config` as the cached config so `get_cached_config` returns it without
/// touching Salsa.
async fn seed(server: &LaravelLanguageServer, config: LaravelConfigData) {
    *server.cached_config.write().await = Some(config);
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn out_of_root_view_returns_none() {
    // The `pkg` namespace points OUTSIDE the project root (the shape a
    // `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')` produces). The resolved
    // `card` view exists on disk, so without the guard
    // `create_view_location_from_salsa` would hand back a LocationLink pointing
    // outside the root; the containment guard must refuse it on root grounds
    // alone and return None.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    write(&outside.path().join("card.blade.php"), CARD_VIEW);

    let server = test_server();
    seed(
        &server,
        config_with_view_namespace(root.path(), outside.path()),
    )
    .await;

    let result = server
        .create_view_location_from_salsa(&view_ref("pkg::card"))
        .await;

    assert!(
        result.is_none(),
        "an out-of-root view must not resolve, even though it exists on disk — \
         the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_root_view_still_resolves() {
    // Positive control: the `pkg` namespace points to a directory INSIDE the
    // project root, so the resolved `card` view passes containment and view
    // navigation resolves exactly as before — no regression.
    let root = tempfile::TempDir::new().unwrap();
    let namespace_dir = root.path().join("packages/pkg/resources/views");

    let card = namespace_dir.join("card.blade.php");
    write(&card, CARD_VIEW);

    let server = test_server();
    seed(
        &server,
        config_with_view_namespace(root.path(), &namespace_dir),
    )
    .await;

    let result = server
        .create_view_location_from_salsa(&view_ref("pkg::card"))
        .await;

    let Some(GotoDefinitionResponse::Link(links)) = result else {
        panic!("an in-root view must resolve to a Link definition response");
    };
    assert_eq!(links.len(), 1, "exactly one definition link is expected");
    assert_eq!(
        links[0].target_uri,
        Url::from_file_path(&card).unwrap(),
        "the definition must point at the in-root card view"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a lexical guard could not catch: the namespace
    // directory lives *under* the project root — a symlink at
    // `<root>/linked-views` — yet it resolves to a directory OUTSIDE the root.
    // The `card` view exists through the link, so a missing file can't explain a
    // None result. A purely lexical `starts_with` check would admit the path
    // (the link is under the root); only canonicalization — `path_within_root`
    // resolving the symlink to its out-of-tree target — can reject it.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // Real view file, outside the project root.
    let target_dir = outside.path().join("resources/views");
    write(&target_dir.join("card.blade.php"), CARD_VIEW);

    // Symlink under the root that points at the outside views directory.
    let link: PathBuf = root.path().join("linked-views");
    std::os::unix::fs::symlink(&target_dir, &link).unwrap();

    let server = test_server();
    // Register the namespace at the in-root symlink path so resolution is
    // lexically inside the root; only canonicalization reveals the escape.
    seed(&server, config_with_view_namespace(root.path(), &link)).await;

    let result = server
        .create_view_location_from_salsa(&view_ref("pkg::card"))
        .await;

    assert!(
        result.is_none(),
        "a view reached through an under-root symlink that resolves outside the \
         project root must not resolve — the canonicalize-based containment guard \
         refuses it even though the link path is lexically inside"
    );
}
