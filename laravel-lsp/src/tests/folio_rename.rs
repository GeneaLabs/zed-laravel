//! Integration tests for Folio page-route **rename** support (issue #100).
//!
//! A named Folio page declares its route via its own `name('...')` helper, which
//! lives outside `routes/` and is no `->name()` chain. Find-references already
//! surfaced these pages; rename half-worked — it rewrote `route('...')` call
//! sites but silently dropped the page's own declaration, leaving a latent
//! decl/call-site desync. These tests cover the two pieces that close the gap:
//!
//!   * `decl_range_at` — the prepare_rename highlight range for a cursor on a
//!     page's `name('...')` call (previously `None`, so Zed offered no rename).
//!   * `collect_route_declaration_targets` — the page-declaration `EditTarget`
//!     that now travels alongside the call-site edits, so a rename rewrites both
//!     atomically (including leaf-only rewrites under a mount `->name()` prefix).
//!
//! Both are driven directly on a server built through `tower_lsp::LspService`,
//! with the route index seeded the way the route-index build seeds it — the
//! same harness `folio_cursor_containment.rs` uses. The async `rename` handler
//! itself isn't fixtured (it depends on the Salsa actor for call-site
//! `find_references`, covered by the symbol-index tests); these exercise the
//! Folio decl logic the handler composes.

use crate::{collect_route_declaration_targets, decl_range_at, LaravelLanguageServer};
use laravel_lsp::references::SymbolRef;
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex, PRIORITY_APP};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::{Position, Range};
use tower_lsp::LspService;

/// `<?php name('contact'); ?>` — the `contact` argument content occupies columns
/// 12..=18, so its quote-excluded span is start 12, end 19. A cursor at column
/// 13 sits inside it.
const PAGE_SRC: &str = "<?php name('contact'); ?>";
const NAME_START: u32 = 12;
const NAME_END: u32 = 19;
const CURSOR: Position = Position {
    line: 0,
    character: 13,
};

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Write a Folio page under `root/pages/contact.blade.php`, seed the route index
/// with `route_name` pointing at it, set the project root, and return the page
/// path. Mirrors how `inject_folio_routes` seeds named pages (top-of-file
/// anchor) so the decl logic must re-read the page to find the `name(...)` span.
async fn seed_page(
    server: &LaravelLanguageServer,
    root: &Path,
    route_name: &str,
) -> std::path::PathBuf {
    let page = root.join("pages").join("contact.blade.php");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, PAGE_SRC).unwrap();

    let mut index = RouteIndex::new();
    index.insert(
        route_name.to_string(),
        RouteDefinition {
            file: page.clone(),
            line: 0,
            column: 0,
            end_column: 0,
            priority: PRIORITY_APP,
            method: Some("get".to_string()),
            uri: Some("/contact".to_string()),
            action: None,
        },
    );
    *server.route_index.write().await = Some(index);
    *server.root_path.write().await = Some(root.to_path_buf());
    page
}

#[tokio::test]
async fn decl_range_at_returns_the_page_name_span_for_a_folio_cursor() {
    // prepare_rename AC: a cursor on a Folio page's `name('...')` call yields a
    // valid highlight range (previously `None` → silent no-op).
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "contact").await;

    let range = decl_range_at(
        &server,
        Some(root.path()),
        &page,
        CURSOR,
        &SymbolRef::Route("contact".to_string()),
    )
    .await;

    assert_eq!(
        range,
        Some(Range {
            start: Position {
                line: 0,
                character: NAME_START,
            },
            end: Position {
                line: 0,
                character: NAME_END,
            },
        }),
        "the prepare_rename range must cover the `contact` content (quotes excluded)"
    );
}

#[tokio::test]
async fn decl_range_at_returns_none_when_cursor_off_the_name_call() {
    // Off the `name(...)` call (column 0), there is nothing to rename here.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "contact").await;

    let range = decl_range_at(
        &server,
        Some(root.path()),
        &page,
        Position {
            line: 0,
            character: 0,
        },
        &SymbolRef::Route("contact".to_string()),
    )
    .await;

    assert_eq!(range, None);
}

#[tokio::test]
async fn collect_targets_rewrites_the_page_name_declaration() {
    // rename AC: the page's own `name('...')` declaration is rewritten so it
    // stays in sync with the `route('...')` call sites the handler rewrites
    // separately. No `routes/` directory exists here — the Folio branch must
    // still fire.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "contact").await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "contact", "support").await;

    assert_eq!(
        targets.len(),
        1,
        "exactly one declaration target — the page itself"
    );
    let t = &targets[0];
    assert_eq!(t.file_path, page);
    assert_eq!(t.line, 0);
    assert_eq!(t.start_column, NAME_START);
    assert_eq!(t.end_column, NAME_END);
    assert_eq!(
        t.new_text, "support",
        "the full new name is written at the decl"
    );
}

#[tokio::test]
async fn collect_targets_writes_leaf_only_under_a_mount_name_prefix() {
    // When the page sits under a Folio mount `->name('admin.')`, the index key
    // is the fully-qualified `admin.contact`, but the page's own `name('contact')`
    // helper only owns the leaf. Renaming `admin.contact` → `admin.support` must
    // write just `support` at the decl — never doubling the mount prefix.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "admin.contact").await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "admin.contact", "admin.support")
            .await;

    assert_eq!(targets.len(), 1);
    let t = &targets[0];
    assert_eq!(t.file_path, page);
    assert_eq!(
        t.new_text, "support",
        "only the leaf is rewritten; the mount `admin.` prefix lives on the mount, not the page"
    );
}

#[tokio::test]
async fn collect_targets_ignores_a_conventional_route_name() {
    // A name with no Folio page in the index (e.g. a conventional `routes/`
    // route, or an unknown name) contributes no Folio decl target — the branch
    // only fires for `.blade.php`-backed index entries.
    let root = TempDir::new().unwrap();
    let server = test_server();
    seed_page(&server, root.path(), "contact").await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "dashboard", "home").await;

    assert!(
        targets.is_empty(),
        "an unindexed / non-Folio name must not produce a page decl edit"
    );
}
