//! Tests for the explicit root-containment guards in
//! `LaravelLanguageServer::folio_route_name_for_cursor` (issue #104) and its
//! prepare-rename sibling `folio_name_decl_range` (issue #141).
//!
//! The guard is defense in depth: the route index already holds only mounts
//! that stayed under the project root, but the methods re-check containment
//! against `self.root_path` before reading the cursor file from disk. These
//! tests drive the private methods directly by building the server through
//! `tower_lsp::LspService` and reaching its inner value with `inner()`.

use crate::LaravelLanguageServer;
// Only the `#[cfg(unix)]` symlink tests below call the guard directly.
#[cfg(unix)]
use crate::path_within_root;
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex, PRIORITY_APP};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::{Position, Range};
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

// --- folio_name_decl_range: prepare-rename highlight range (issue #141) ---
//
// `folio_name_decl_range` resolves the quote-excluded span of a Folio page's
// own `name('...')` call straight from the page source (it does not consult the
// route index), so these tests only need the page on disk plus `root_path` set.
// The guard mirrors `folio_route_name_for_cursor`: refuse to read a page whose
// canonical path falls outside the project root.

/// The expected highlight range for `PAGE_SRC` — the `contact` argument content
/// (columns 12..19 on line 0), quotes excluded.
const DECL_RANGE: Range = Range {
    start: Position {
        line: 0,
        character: 12,
    },
    end: Position {
        line: 0,
        character: 19,
    },
};

#[tokio::test]
async fn decl_range_out_of_root_page_returns_none() {
    // The page lives outside the project root, yet exists on disk and has a
    // valid `name(...)` call with the cursor on it. Without the guard the method
    // would read it and return a range; the guard must refuse it on containment
    // grounds alone.
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let page = outside.path().join("contact.blade.php");
    fs::write(&page, PAGE_SRC).unwrap();

    let server = test_server();
    *server.root_path.write().await = Some(root.path().to_path_buf());

    let result = server.folio_name_decl_range(&page, CURSOR).await;

    assert_eq!(
        result, None,
        "an out-of-root page must not yield a decl range, even though it exists \
         and the cursor is on its name() call — the containment guard refuses it"
    );
}

#[tokio::test]
async fn decl_range_in_tree_page_returns_range() {
    // Positive control: a page inside the project root resolves to the correct
    // highlight range, confirming the guard doesn't regress in-tree behavior.
    let root = TempDir::new().unwrap();
    let page = root.path().join("pages").join("contact.blade.php");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, PAGE_SRC).unwrap();

    let server = test_server();
    *server.root_path.write().await = Some(root.path().to_path_buf());

    let result = server.folio_name_decl_range(&page, CURSOR).await;

    assert_eq!(
        result,
        Some(DECL_RANGE),
        "an in-tree page on its name() call must still yield the quote-excluded \
         highlight range of its name argument"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn decl_range_under_root_symlink_to_outside_target_returns_none() {
    // The discriminating case a lexical guard could not catch: the page path
    // lives *under* the project root — a symlink at
    // `<root>/pages/contact.blade.php` — yet it resolves to a file OUTSIDE the
    // root. The target exists on disk (through the link) and the cursor sits on
    // its `name(...)` call, so a missing file can't explain a `None` result. A
    // purely lexical `starts_with` check would *admit* this path; only
    // canonicalization — `path_within_root` resolving the symlink to its
    // out-of-tree target — can reject it.
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

    let result = server.folio_name_decl_range(&link, CURSOR).await;

    assert_eq!(
        result, None,
        "a page reached through an under-root symlink that resolves outside the \
         project root must not yield a decl range — the canonicalize-based \
         containment guard refuses it even though the link path is lexically \
         inside the root"
    );
}

// --- path_within_root: fail-closed on canonicalize failure (issue #134) ---
//
// The guard above proved the *live*-symlink escape leg (a link whose target
// resolves outside the root). This exercises the helper directly on the
// *dangling*-symlink leg: a link under the root whose target does not exist, so
// `canonicalize` returns `Err(ENOENT)`. The fail-closed contract must refuse it
// rather than fall back to a lexical prefix check that would admit it.

#[cfg(unix)]
#[test]
fn path_within_root_refuses_dangling_under_root_symlink() {
    // A symlink at `<root>/dangling` pointing at a target that was never
    // created. `dangling.canonicalize()` fails (the target is missing), so the
    // (Err, _) arm of `path_within_root` decides the outcome.
    let root = TempDir::new().unwrap();
    let missing_target = root.path().join("never-created.blade.php");
    let dangling = root.path().join("dangling");
    std::os::unix::fs::symlink(&missing_target, &dangling).unwrap();

    // Sanity: the link itself exists on disk, but it cannot be canonicalized
    // because its target is missing — this is exactly the case the old
    // `_ => path.starts_with(root)` fallback admitted.
    assert!(
        std::fs::symlink_metadata(&dangling).is_ok(),
        "the dangling symlink must exist on disk for this to be a real test"
    );
    assert!(
        dangling.canonicalize().is_err(),
        "a dangling symlink must fail to canonicalize"
    );

    // Discriminating companion assertion: the dangling path PASSES the old
    // lexical `path.starts_with(root)` check (its link path is textually inside
    // the root). So this test would FAIL against the previous
    // `_ => path.starts_with(root)` fallback, which admitted the path, and only
    // passes against the fail-closed `_ => false`.
    assert!(
        dangling.starts_with(root.path()),
        "precondition: the dangling link path is lexically inside the root, so \
         the old lexical fallback would have admitted it"
    );

    assert!(
        !path_within_root(&dangling, root.path()),
        "a dangling under-root symlink (canonicalize fails) must be refused — \
         path_within_root is fail-closed, not fail-open via a lexical prefix check"
    );
}
