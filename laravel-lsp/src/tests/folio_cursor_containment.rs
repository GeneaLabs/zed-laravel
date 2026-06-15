//! Tests for the explicit root-containment guard in
//! `LaravelLanguageServer::folio_route_name_for_cursor` (issue #104).
//!
//! The guard is defense in depth: the route index already holds only mounts
//! that stayed under the project root, but the method re-checks containment
//! against `self.root_path` before reading the cursor file from disk. These
//! tests drive the private method directly by building the server through
//! `tower_lsp::LspService` and reaching its inner value with `inner()`.

use crate::LaravelLanguageServer;
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex, PRIORITY_APP};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::Position;
use tower_lsp::LspService;

/// A bare Folio page that declares `name('contact')`. Cursor character 13 sits
/// inside the `contact` argument string, so `cursor_on_page_name` matches.
const PAGE_SRC: &str = "<?php name('contact'); ?>";
const CURSOR: Position = Position {
    line: 0,
    character: 13,
};

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call
/// its private methods directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Seed the route index with a single Folio page keyed by `name`, pointing at
/// `file`. With this entry present, `folio_name_for_file` succeeds, so the
/// containment guard — not a missing index match — decides the outcome.
async fn seed_folio_page(server: &LaravelLanguageServer, name: &str, file: &Path) {
    let mut index = RouteIndex::new();
    index.insert(
        name.to_string(),
        RouteDefinition {
            file: file.to_path_buf(),
            line: 0,
            column: 0,
            end_column: 0,
            priority: PRIORITY_APP,
            method: None,
            uri: Some("/contact".to_string()),
            action: None,
        },
    );
    *server.route_index.write().await = Some(index);
}

#[tokio::test]
async fn out_of_root_page_returns_none_without_disk_read() {
    // The page lives in a directory that is NOT under the project root, yet it
    // exists on disk, is indexed, and the cursor sits on its `name(...)` call.
    // Without the guard the method would read it and resolve `contact`; the
    // guard must refuse it on containment grounds alone.
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let page = outside.path().join("contact.blade.php");
    fs::write(&page, PAGE_SRC).unwrap();

    let server = test_server();
    *server.root_path.write().await = Some(root.path().to_path_buf());
    seed_folio_page(&server, "contact", &page).await;

    let result = server.folio_route_name_for_cursor(&page, CURSOR).await;

    assert_eq!(
        result, None,
        "an out-of-root page must not resolve, even though it exists, is indexed, \
         and the cursor is on its name() call — the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_tree_page_still_resolves() {
    // Positive control: a page inside the project root resolves exactly as
    // before, confirming the guard doesn't regress in-tree behavior.
    let root = TempDir::new().unwrap();
    let page = root.path().join("pages").join("contact.blade.php");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, PAGE_SRC).unwrap();

    let server = test_server();
    *server.root_path.write().await = Some(root.path().to_path_buf());
    seed_folio_page(&server, "contact", &page).await;

    let result = server.folio_route_name_for_cursor(&page, CURSOR).await;

    assert_eq!(
        result,
        Some("contact".to_string()),
        "an in-tree page on its name() call must still resolve to its route name"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case the lexical guard could not catch (issue #116): the
    // page path lives *under* the project root — a symlink at
    // `<root>/pages/contact.blade.php` — yet it resolves to a file OUTSIDE the
    // root. The target exists on disk (through the link), the page is indexed
    // under its in-root link path, and the cursor sits on its `name(...)` call,
    // so a missing index entry can't explain a `None` result. A purely lexical
    // `starts_with` check would *admit* this path (the link itself is under the
    // root); only canonicalization — `path_within_root` resolving the symlink to
    // its out-of-tree target — can reject it. That makes the test fail against
    // the old lexical guard and pass against the canonicalize guard.
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();

    // Real target file, outside the project root.
    let target = outside.path().join("contact.blade.php");
    fs::write(&target, PAGE_SRC).unwrap();

    // Symlink under the root that points at the outside target.
    let pages = root.path().join("pages");
    fs::create_dir_all(&pages).unwrap();
    let link = pages.join("contact.blade.php");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let server = test_server();
    *server.root_path.write().await = Some(root.path().to_path_buf());
    // Index under the in-root symlink path so `folio_name_for_file` matches and
    // the containment guard — not a missing entry — decides the outcome.
    seed_folio_page(&server, "contact", &link).await;

    let result = server.folio_route_name_for_cursor(&link, CURSOR).await;

    assert_eq!(
        result, None,
        "a page reached through an under-root symlink that resolves outside the \
         project root must not resolve — the canonicalize-based containment guard \
         refuses it even though the link path is lexically inside the root"
    );
}
