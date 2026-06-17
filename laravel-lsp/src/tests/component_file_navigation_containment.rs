//! Tests for the fail-closed root-containment guard in
//! `LaravelLanguageServer::resolve_component_file` (issue #199) — the next
//! sibling in the #130 → #143 → #148 → #194 containment-guard chain.
//!
//! `resolve_component_file` backs anonymous Blade component hover (the
//! `@props([...])` snippet and file link) and component-prop completion. It
//! resolves a `<x-tag>` to a `.blade.php` file via
//! `LaravelConfigData::resolve_component_path`, which already drops out-of-root
//! candidates with the *lexical* `path_within_root_lexical` filter. The guard
//! added here re-checks the surviving candidate with the **fail-closed**
//! `path_within_root` — the same check the sibling
//! `resolve_component_existing_file` (#148) applies — so the containment
//! invariant holds *uniformly* across every FS-touching component resolver.
//!
//! Because the lexical filter (and `file_exists_cached`, which follows symlinks)
//! already close every escape vector that reaches `resolve_component_file`
//! today, the new guard is defense-in-depth: not currently exploitable, but the
//! backstop that holds the line if a future call site or a weakened upstream
//! filter ever routes an unfiltered candidate through here. These tests pin the
//! invariant at the `resolve_component_file` boundary — an out-of-root component
//! file never resolves (whether it escapes lexically or through an under-root
//! symlink that canonicalizes outside), while a genuine in-root file still
//! resolves through the guard's allow branch. They drive the private async
//! method directly via `tower_lsp::LspService` / `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tower_lsp::LspService;

/// Contents are irrelevant to resolution — only that the file exists, so a
/// `None` result can come only from the containment guard, never a missing file.
const CARD_BLADE: &str = "<div>{{ $message }}</div>\n";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call its
/// private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A minimal config rooted at `root` whose single conventional component
/// directory is `components_dir`, so `<x-card>` resolves to
/// `<components_dir>/card.blade.php`. Pointing `components_dir` outside `root`
/// (directly or through a symlink) is the escape vector the guard must refuse.
fn config_with_components_dir(root: &Path, components_dir: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![(String::new(), components_dir.to_path_buf())],
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

/// Seed `config` as the cached config and set the server root. `resolve_component_file`
/// reads the cached config via `get_cached_config`; `root_path` is set for parity
/// with the sibling component flow.
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
async fn out_of_root_component_file_returns_none() {
    // The conventional components directory lives OUTSIDE the project root, and
    // `card.blade.php` genuinely exists there — so a `None` result can only come
    // from the containment refusal, never a missing file. `resolve_component_path`
    // drops this lexically-out-of-root candidate; the fail-closed guard in
    // `resolve_component_file` is the boundary backstop for the same invariant.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let components_dir = outside.path().join("components");
    write(&components_dir.join("card.blade.php"), CARD_BLADE);

    let server = test_server();
    seed(
        &server,
        root.path(),
        config_with_components_dir(root.path(), &components_dir),
    )
    .await;

    let result = server.resolve_component_file("card").await;

    assert!(
        result.is_none(),
        "an out-of-root component file must not resolve, even though it exists \
         on disk — the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_root_component_file_still_resolves() {
    // Positive control: a genuine in-root `card.blade.php` passes the lexical
    // filter, exists on disk, and passes the fail-closed guard — so anonymous
    // component resolution returns its path exactly as before. This drives the
    // guard's allow branch, proving the new check doesn't regress in-root
    // resolution.
    let root = tempfile::TempDir::new().unwrap();
    let components_dir = root.path().join("resources/views/components");
    let card = components_dir.join("card.blade.php");
    write(&card, CARD_BLADE);

    let server = test_server();
    seed(
        &server,
        root.path(),
        config_with_components_dir(root.path(), &components_dir),
    )
    .await;

    let result = server.resolve_component_file("card").await;

    assert_eq!(
        result.as_deref(),
        Some(card.as_path()),
        "an in-root component file must resolve to its path"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a purely-textual check could not catch: the
    // conventional candidate `<root>/resources/views/components/card.blade.php`
    // is an under-root symlink whose target is a real file OUTSIDE the root. The
    // link path is lexically inside the root, and the target exists (so
    // `file_exists_cached`, which follows symlinks, would pass) — only
    // canonicalization, resolving the symlink to its out-of-tree target, can
    // reject it. The lexical filter's canonicalize step catches it here, and the
    // fail-closed `path_within_root` guard holds the same line at the resolver
    // boundary.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // Real component file, outside the project root.
    let outside_card = outside.path().join("card.blade.php");
    write(&outside_card, CARD_BLADE);

    // Under-root conventional path, materialized as a symlink to the outside file.
    let components_dir = root.path().join("resources/views/components");
    fs::create_dir_all(&components_dir).unwrap();
    let link = components_dir.join("card.blade.php");
    std::os::unix::fs::symlink(&outside_card, &link).unwrap();

    let server = test_server();
    seed(
        &server,
        root.path(),
        config_with_components_dir(root.path(), &components_dir),
    )
    .await;

    let result = server.resolve_component_file("card").await;

    assert!(
        result.is_none(),
        "a component file reached through an under-root symlink that resolves \
         outside the project root must not resolve — canonicalization refuses it \
         even though the link path is lexically inside"
    );
}
