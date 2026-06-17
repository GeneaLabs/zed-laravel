//! Tests for the root-containment guard in
//! `LaravelLanguageServer::create_livewire_location_from_salsa` (issue #194,
//! extending the #130 → #143 → #148 containment-guard chain).
//!
//! The `<livewire:ns::component>` Blade-*tag* goto-definition flow — distinct
//! from the `@livewire(...)` *directive* flow #148 already guards — resolves
//! through `resolve_livewire_primary_path` → `livewire_resolver::resolve_component`,
//! which looks the component up under `component_namespaces[ns]`. That namespace
//! map (parsed by `livewire_config.rs`) explicitly accepts bare absolute paths,
//! so a `'component_namespaces' => ['x' => '/etc']`-style registration plus a
//! `<livewire:x::passwd>` tag could otherwise hand the LSP client a `LocationLink`
//! pointing outside the project root. The guard re-checks containment against
//! `config.root` — via the same `path_within_root` the view, component,
//! directive, and slot-navigation flows use — after the `file_exists_cached`
//! check and before building the link. These tests drive the private async
//! method directly by building the server through `tower_lsp::LspService` and
//! reaching its inner value with `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::livewire_config::LivewireConfig;
use laravel_lsp::livewire_version::LivewireVersion;
use laravel_lsp::naming::LIVEWIRE_EMOJI;
use laravel_lsp::salsa_impl::{LaravelConfigData, LivewireReferenceData};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::{lsp_types::Url, LspService};

/// The inline-class body that backs a V4 SFC Livewire component. Its contents
/// are irrelevant to resolution — `try_v4_sfc` only checks that the
/// `⚡{leaf}.blade.php` file exists — so a `None` result can only come from the
/// containment guard, never a missing file.
const SFC_BODY: &str = "<?php new class extends Component {}; ?>\n<div>component</div>\n";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call its
/// private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// The on-disk V4 SFC filename for a component leaf — `⚡{leaf}.blade.php`,
/// matching the `⚡` prefix the resolver (`try_v4_sfc`) discovers by.
fn sfc_file(dir: &Path, leaf: &str) -> PathBuf {
    dir.join(format!("{LIVEWIRE_EMOJI}{leaf}.blade.php"))
}

/// A Livewire config rooted at `root` that registers `ns` as a component
/// namespace pointing at `namespace_dir`. With this, `<livewire:ns::leaf>`
/// resolves to `{namespace_dir}/⚡{leaf}.blade.php`, letting the test place that
/// directory inside or outside the project root at will.
fn livewire_config_with_namespace(root: &Path, ns: &str, namespace_dir: &Path) -> LivewireConfig {
    let mut config = LivewireConfig::defaults(root);
    config
        .component_namespaces
        .insert(ns.to_string(), namespace_dir.to_path_buf());
    config
}

/// A minimal Laravel config whose only field the containment guard reads is
/// `root`. Everything else is empty — the guard never consults it.
fn laravel_config(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![],
        component_paths: vec![],
        livewire_path: None,
        has_livewire: true,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A `LivewireReferenceData` for `name` at the document origin — positions are
/// irrelevant to path resolution.
fn livewire_ref(name: &str) -> LivewireReferenceData {
    LivewireReferenceData {
        name: name.to_string(),
        line: 0,
        column: 0,
        end_column: 0,
    }
}

/// Seed the server so `get_cached_livewire` and `get_cached_config` both return
/// the supplied state without touching disk or Salsa. `get_cached_livewire`
/// keys its cache on `root_path`, so that must match the config's root.
async fn seed(
    server: &LaravelLanguageServer,
    root: &Path,
    livewire: LivewireConfig,
    config: LaravelConfigData,
) {
    *server.root_path.write().await = Some(root.to_path_buf());
    *server.cached_livewire.write().await =
        Some((root.to_path_buf(), livewire, LivewireVersion::V4));
    *server.cached_config.write().await = Some(config);
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn out_of_root_livewire_tag_returns_none() {
    // The `x` namespace points OUTSIDE the project root (the shape a
    // `'component_namespaces' => ['x' => '/etc']` registration produces). The
    // resolved `passwd` SFC exists on disk, so without the guard
    // `create_livewire_location_from_salsa` would hand back a LocationLink
    // pointing outside the root; the containment guard must refuse it on root
    // grounds alone and return None.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    write(&sfc_file(outside.path(), "passwd"), SFC_BODY);

    let server = test_server();
    seed(
        &server,
        root.path(),
        livewire_config_with_namespace(root.path(), "x", outside.path()),
        laravel_config(root.path()),
    )
    .await;

    let result = server
        .create_livewire_location_from_salsa(&livewire_ref("x::passwd"))
        .await;

    assert!(
        result.is_none(),
        "an out-of-root <livewire:x::passwd> target must not resolve, even though \
         it exists on disk — the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_root_livewire_tag_resolves() {
    // Positive control: the `ns` namespace points to a directory INSIDE the
    // project root, so the resolved `counter` SFC passes containment and tag
    // navigation resolves exactly as before — no regression.
    let root = tempfile::TempDir::new().unwrap();
    let namespace_dir = root.path().join("resources/views/components");

    let counter = sfc_file(&namespace_dir, "counter");
    write(&counter, SFC_BODY);

    let server = test_server();
    seed(
        &server,
        root.path(),
        livewire_config_with_namespace(root.path(), "ns", &namespace_dir),
        laravel_config(root.path()),
    )
    .await;

    let result = server
        .create_livewire_location_from_salsa(&livewire_ref("ns::counter"))
        .await;

    let Some(GotoDefinitionResponse::Link(links)) = result else {
        panic!("an in-root <livewire:ns::counter> must resolve to a Link response");
    };
    assert_eq!(links.len(), 1, "exactly one definition link is expected");
    assert_eq!(
        links[0].target_uri,
        Url::from_file_path(&counter).unwrap(),
        "the definition must point at the in-root counter component"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a lexical guard could not catch: the namespace
    // directory lives *under* the project root — a symlink at
    // `<root>/linked-components` — yet it resolves to a directory OUTSIDE the
    // root. The `counter` SFC exists through the link, so a missing file can't
    // explain a None result. A purely lexical `starts_with` check would admit
    // the path (the link is under the root); only canonicalization —
    // `path_within_root` resolving the symlink to its out-of-tree target — can
    // reject it.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // Real component file, outside the project root.
    let target_dir = outside.path().join("components");
    write(&sfc_file(&target_dir, "counter"), SFC_BODY);

    // Symlink under the root that points at the outside components directory.
    let link: PathBuf = root.path().join("linked-components");
    std::os::unix::fs::symlink(&target_dir, &link).unwrap();

    let server = test_server();
    // Register the namespace at the in-root symlink path so resolution is
    // lexically inside the root; only canonicalization reveals the escape.
    seed(
        &server,
        root.path(),
        livewire_config_with_namespace(root.path(), "ns", &link),
        laravel_config(root.path()),
    )
    .await;

    let result = server
        .create_livewire_location_from_salsa(&livewire_ref("ns::counter"))
        .await;

    assert!(
        result.is_none(),
        "a <livewire:...> tag reached through an under-root symlink that resolves \
         outside the project root must not resolve — the canonicalize-based \
         containment guard refuses it even though the link path is lexically inside"
    );
}
