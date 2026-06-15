//! Tests for the explicit root-containment guard in
//! `LaravelLanguageServer::create_slot_location` (issue #130).
//!
//! Slot go-to-definition resolves a parent component to a Blade view and then
//! reads that view from disk (`locate_slot_in_view` →
//! `std::fs::read_to_string`). A `loadViewsFrom(__DIR__ . '/../../etc', 'ns')`
//! -style namespace can register an absolute view path that escapes the project
//! root, so a `<x-ns::component>` slot would otherwise read an out-of-root file.
//! The guard re-checks containment against `config.root` — via the same
//! `path_within_root` used by the Folio cursor and blade-var rename flows —
//! before the disk read. These tests drive the private async method directly by
//! building the server through `tower_lsp::LspService` and reaching its inner
//! value with `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{GotoDefinitionResponse, Position};
use tower_lsp::{lsp_types::Url, LspService};

/// A `<x-pkg::card>` parent component wrapping a named slot. The cursor sits on
/// the `s` of `<x-slot:title>` (line 1, character 7), so `find_slot_at_position`
/// matches and `find_enclosing_parent_component` resolves the parent to
/// `pkg::card`.
const SLOT_SRC: &str = "<x-pkg::card>\n    <x-slot:title>Hello</x-slot:title>\n</x-pkg::card>\n";
const CURSOR: Position = Position {
    line: 1,
    character: 7,
};

/// The parent view that backs `<x-pkg::card>`; references `{{ $title }}` so
/// `locate_slot_in_view` has a slot variable to pin navigation to.
const CARD_VIEW: &str = "<div>\n    {{ $title }}\n</div>\n";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call its
/// private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A minimal config rooted at `root` that registers `pkg` as a `loadViewsFrom`
/// -style namespace pointing at `namespace_dir`. With this, `<x-pkg::card>`
/// resolves to `{namespace_dir}/components/card.blade.php`, letting the test
/// place that directory inside or outside the project root at will.
fn config_with_namespace(root: &Path, namespace_dir: &Path) -> LaravelConfigData {
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

/// Seed `config` as the cached config and open `SLOT_SRC` under `source_uri`, so
/// `create_slot_location` reads the slot source from `documents` and resolves
/// the parent through the seeded namespace.
async fn seed(server: &LaravelLanguageServer, config: LaravelConfigData, source_uri: &Url) {
    *server.cached_config.write().await = Some(config);
    server
        .documents
        .write()
        .await
        .insert(source_uri.clone(), (SLOT_SRC.to_string(), 1));
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn out_of_root_component_view_returns_none_without_disk_read() {
    // The `pkg` namespace points OUTSIDE the project root (the shape a
    // `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')` produces). The resolved
    // `card` view exists on disk and references `{{ $title }}`, so without the
    // guard `create_slot_location` would read it and resolve the slot; the
    // containment guard must refuse it on root grounds alone and return None.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let card = outside.path().join("components/card.blade.php");
    write(&card, CARD_VIEW);

    let server = test_server();
    let source_uri =
        Url::from_file_path(root.path().join("resources/views/page.blade.php")).unwrap();
    let config = config_with_namespace(root.path(), outside.path());
    seed(&server, config, &source_uri).await;

    let result = server.create_slot_location(&source_uri, CURSOR).await;

    assert!(
        result.is_none(),
        "an out-of-root component view must not resolve, even though it exists \
         on disk and references the slot variable — the containment guard refuses it"
    );
}

#[tokio::test]
async fn out_of_root_candidate_is_never_stated() {
    // Ordering proof for issue #145: the containment guard must run *before*
    // `file_exists_cached`, so an out-of-root candidate is rejected without any
    // disk `stat`, closing the out-of-root existence oracle. `file_exists_cached`
    // writes a path into `file_exists_cache` only *after* it `stat`s the path, so
    // that cache is a faithful record of which candidates were probed on disk.
    //
    // We resolve an out-of-root `card` view that exists on disk, run the lookup,
    // and assert its path never entered the existence cache — i.e. the syscall
    // was never made. This is what guards the ordering: the result-level
    // `out_of_root_component_view_returns_none_without_disk_read` test passes
    // under *either* loop order (the guard rejects the path eventually), so it
    // can't catch a regression that reverts the guard back behind
    // `file_exists_cached`. Asserting the absence of the `stat` can.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let card = outside.path().join("components/card.blade.php");
    write(&card, CARD_VIEW);

    let server = test_server();
    let source_uri =
        Url::from_file_path(root.path().join("resources/views/page.blade.php")).unwrap();
    let config = config_with_namespace(root.path(), outside.path());
    seed(&server, config, &source_uri).await;

    let result = server.create_slot_location(&source_uri, CURSOR).await;

    assert!(
        result.is_none(),
        "an out-of-root component view must not resolve"
    );
    assert!(
        !server.file_exists_cache.read().await.contains_key(&card),
        "the out-of-root candidate must never be stat'ed: its path must not enter \
         file_exists_cache. Its presence would mean file_exists_cached ran before \
         the containment guard — the exact ordering bug #145 closes"
    );
}

#[tokio::test]
async fn in_root_component_view_still_resolves() {
    // Positive control: the `pkg` namespace points to a directory INSIDE the
    // project root, so the resolved `card` view passes containment and slot
    // navigation resolves exactly as before — no regression.
    //
    // Assert the exact file, line, AND column (AC #4), matching the folio
    // precedent (`folio_cursor_containment.rs` asserts the exact route name).
    // "Something resolved" (`is_some()`) is too weak: it also passes when
    // `locate_slot_in_view` finds nothing and `create_slot_location` falls back
    // to `(0, 0)`. Pinning file + line + column proves the jump lands on the
    // real `{{ $title }}` usage, closing that degenerate-`Some` gap.
    let root = tempfile::TempDir::new().unwrap();
    let namespace_dir = root.path().join("packages/pkg/resources/views");

    let card = namespace_dir.join("components/card.blade.php");
    write(&card, CARD_VIEW);

    let server = test_server();
    let source_uri =
        Url::from_file_path(root.path().join("resources/views/page.blade.php")).unwrap();
    let config = config_with_namespace(root.path(), &namespace_dir);
    seed(&server, config, &source_uri).await;

    let result = server.create_slot_location(&source_uri, CURSOR).await;

    let Some(GotoDefinitionResponse::Link(links)) = result else {
        panic!("an in-root component view on a slot must resolve to a Link definition response");
    };
    assert_eq!(links.len(), 1, "exactly one definition link is expected");
    let link = &links[0];

    // Correct file: the link must target the seeded card view itself.
    assert_eq!(
        link.target_uri,
        Url::from_file_path(&card).unwrap(),
        "the definition must point at the in-root card view that backs the component"
    );
    // Correct line + column: `CARD_VIEW` references the slot variable on its
    // second line (0-based line 1), at character 7 — four spaces plus `{{ `
    // precede `$title`. The `(0, 0)` fallback would put it on the file top.
    assert_eq!(
        link.target_range.start.line, 1,
        "the jump must land on the slot-variable line, not the file top"
    );
    assert_eq!(
        link.target_range.start.character, 7,
        "the jump must land on the slot variable's column"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a lexical guard could not catch: the namespace
    // directory lives *under* the project root — a symlink at
    // `<root>/linked-views` — yet it resolves to a directory OUTSIDE the root.
    // The `card` view exists through the link and references the slot variable,
    // so a missing file can't explain a None result. A purely lexical
    // `starts_with` check would admit the path (the link is under the root);
    // only canonicalization — `path_within_root` resolving the symlink to its
    // out-of-tree target — can reject it.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // Real view file, outside the project root.
    let target_dir = outside.path().join("resources/views");
    write(&target_dir.join("components/card.blade.php"), CARD_VIEW);

    // Symlink under the root that points at the outside views directory.
    let link: PathBuf = root.path().join("linked-views");
    std::os::unix::fs::symlink(&target_dir, &link).unwrap();

    let server = test_server();
    let source_uri =
        Url::from_file_path(root.path().join("resources/views/page.blade.php")).unwrap();
    // Register the namespace at the in-root symlink path so resolution is
    // lexically inside the root; only canonicalization reveals the escape.
    let config = config_with_namespace(root.path(), &link);
    seed(&server, config, &source_uri).await;

    let result = server.create_slot_location(&source_uri, CURSOR).await;

    assert!(
        result.is_none(),
        "a component view reached through an under-root symlink that resolves \
         outside the project root must not resolve — the canonicalize-based \
         containment guard refuses it even though the link path is lexically inside"
    );
}
