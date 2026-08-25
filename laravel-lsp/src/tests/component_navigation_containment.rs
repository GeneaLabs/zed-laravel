//! Tests for the root-containment guard in
//! `LaravelLanguageServer::resolve_component_existing_file` (issue #148,
//! extending #130).
//!
//! Component go-to-definition resolves a `<x-tag>` to a file via
//! `component_candidate_paths`. `resolve_component_path` already filters *its*
//! candidates against the project root, but `component_candidate_paths` appends
//! class-backed (`Blade::component('tag', Class::class)`) and PSR-4
//! `componentNamespace` candidates *after* that filter — those can resolve to a
//! class file outside the root. The guard re-checks containment against
//! `config.root` — via the same `path_within_root` the slot-navigation flow uses
//! — before returning the path. These tests register a class-backed component
//! whose file lives outside the root (the unfiltered escape vector) and drive the
//! private async methods directly through `tower_lsp::LspService` / `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{ComponentReferenceData, LaravelConfigData};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
// Used only by the `#[cfg(unix)]` symlink fixture below — without the
// same gate the import is dead on Windows, and `-D warnings` makes that a
// hard error (issue #292).
#[cfg(unix)]
use std::path::PathBuf;
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::{lsp_types::Url, LspService};

/// The PHP class file that backs the `card` component. Contents are irrelevant
/// to resolution — only that the file exists, so a `None` result can only come
/// from the containment guard, never a missing file.
const CARD_CLASS: &str = "<?php\nclass Card {}\n";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call its
/// private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A minimal config rooted at `root` that registers the `card` tag as a
/// class-backed component (`Blade::component('card', Card::class)`) whose
/// resolved file is `class_file`. `component_candidate_paths` appends this
/// candidate *after* the root-containment filter in `resolve_component_path`, so
/// `class_file` is exactly the unfiltered escape vector the guard must catch.
fn config_with_class_component(root: &Path, class_file: &Path) -> LaravelConfigData {
    let mut class_component_files = HashMap::new();
    class_component_files.insert("card".to_string(), class_file.to_path_buf());

    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![(String::new(), root.join("resources/views/components"))],
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files,
    }
}

/// A `ComponentReferenceData` for `name` at the document origin — positions are
/// irrelevant to path resolution.
fn component_ref(name: &str) -> ComponentReferenceData {
    ComponentReferenceData {
        name: name.to_string(),
        tag_name: format!("x-{name}"),
        line: 0,
        column: 0,
        end_column: 0,
    }
}

/// Seed `config` as the cached config and set the server's root path —
/// `resolve_component_existing_file` reads `root_path` to build the composer
/// autoload before walking the candidate paths.
async fn seed(server: &LaravelLanguageServer, root: &Path, config: LaravelConfigData) {
    *server.cached_config.write().await = Some(config);
    *server.root_path.write().await = Some(root.to_path_buf());
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn out_of_root_component_returns_none() {
    // The `card` tag is registered to a class file OUTSIDE the project root (the
    // shape a vendor `Blade::component('card', \Outside\Card::class)` produces).
    // The class file exists on disk, so without the guard
    // `resolve_component_existing_file` would return it; the containment guard
    // must refuse it on root grounds alone and return None.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let class_file = outside.path().join("Card.php");
    write(&class_file, CARD_CLASS);

    let server = test_server();
    seed(
        &server,
        root.path(),
        config_with_class_component(root.path(), &class_file),
    )
    .await;

    let result = server.resolve_component_existing_file("card").await;

    assert!(
        result.is_none(),
        "an out-of-root component class must not resolve, even though it exists \
         on disk — the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_root_component_still_resolves() {
    // Positive control: the `card` tag resolves to a class file INSIDE the
    // project root, so it passes containment and component navigation resolves
    // exactly as before. Assert both the low-level resolver and the public
    // `create_component_location_from_salsa` to prove the full flow is intact.
    let root = tempfile::TempDir::new().unwrap();
    let class_file = root.path().join("app/View/Components/Card.php");
    write(&class_file, CARD_CLASS);

    let server = test_server();
    seed(
        &server,
        root.path(),
        config_with_class_component(root.path(), &class_file),
    )
    .await;

    let resolved = server.resolve_component_existing_file("card").await;
    assert_eq!(
        resolved.as_deref(),
        Some(class_file.as_path()),
        "an in-root component class must resolve to its file"
    );

    let Some(GotoDefinitionResponse::Link(links)) = server
        .create_component_location_from_salsa(&component_ref("card"))
        .await
    else {
        panic!("an in-root component must resolve to a Link definition response");
    };
    assert_eq!(links.len(), 1, "exactly one definition link is expected");
    assert_eq!(
        links[0].target_uri,
        Url::from_file_path(&class_file).unwrap(),
        "the definition must point at the in-root component class"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a lexical guard could not catch: the registered
    // class file lives *under* the project root — reached through a symlink at
    // `<root>/linked` — yet canonicalizes to a file OUTSIDE the root. The file
    // exists through the link, so a missing file can't explain a None result.
    // Only canonicalization — `path_within_root` resolving the symlink to its
    // out-of-tree target — can reject it.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // Real class file, outside the project root.
    let target_dir = outside.path().join("app/View/Components");
    write(&target_dir.join("Card.php"), CARD_CLASS);

    // Symlink under the root that points at the outside components directory.
    let link: PathBuf = root.path().join("linked");
    std::os::unix::fs::symlink(&target_dir, &link).unwrap();

    let server = test_server();
    // Register the class file at the in-root symlink path so resolution is
    // lexically inside the root; only canonicalization reveals the escape.
    seed(
        &server,
        root.path(),
        config_with_class_component(root.path(), &link.join("Card.php")),
    )
    .await;

    let result = server.resolve_component_existing_file("card").await;

    assert!(
        result.is_none(),
        "a component class reached through an under-root symlink that resolves \
         outside the project root must not resolve — the canonicalize-based \
         containment guard refuses it even though the link path is lexically inside"
    );
}
