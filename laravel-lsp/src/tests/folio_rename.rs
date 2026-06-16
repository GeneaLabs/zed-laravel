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
//! same harness `folio_cursor_containment.rs` uses. `prepare_rename` is now also
//! pinned end-to-end at the handler level by
//! `prepare_rename_handler_dispatches_a_folio_name_cursor_to_a_valid_range`
//! (added in PR #171), which drives the whole handler and proves a Folio
//! `name('...')` cursor falls through its `.blade.php` early-return to this decl
//! path. The full `rename` *apply* handler still isn't fixtured — it depends on
//! the Salsa actor for call-site `find_references`, covered by the symbol-index
//! tests — so the remaining tests here exercise the Folio decl logic that
//! handler composes.

use crate::{collect_route_declaration_targets, decl_range_at, LaravelLanguageServer};
use laravel_lsp::references::SymbolRef;
use laravel_lsp::rename::{build_rename_workspace_edit, EditTarget};
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex, PRIORITY_APP};
use laravel_lsp::salsa_impl::{ParsedPatternsData, RouteReferenceData, SymbolRefData};
use laravel_lsp::symbol_index::SymbolIndex;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    Position, PrepareRenameResponse, Range, TextDocumentIdentifier, TextDocumentPositionParams, Url,
};
use tower_lsp::{LanguageServer, LspService};

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
async fn prepare_rename_handler_dispatches_a_folio_name_cursor_to_a_valid_range() {
    // Handler-level dispatch guard (issue #142, follow-up to #100 / PR #138).
    //
    // The tests above drive `decl_range_at` directly — the unit that computes the
    // range. This one drives the whole `prepare_rename` HANDLER, pinning the
    // dispatch glue that wires that range to a Folio cursor. The seam: a Folio
    // page is a `.blade.php` file, so the handler's `.blade.php` early-return runs
    // `prepare_blade_var_rename` FIRST (main.rs). The Folio rename only works
    // because that helper returns `None` for a `name('...')` cursor — there is no
    // `$variable` under the cursor — letting execution fall through to
    // `classify_with_decl_fallback` → `decl_range_at`'s Folio fallback. A future
    // change to the Blade var/scope rename path that began intercepting a Folio
    // cursor would silently break rename with no red test; this is that test.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "contact").await;

    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(&page).unwrap(),
        },
        position: CURSOR,
    };

    let response = server
        .prepare_rename(params)
        .await
        .expect("prepare_rename must not error for a Folio name('...') cursor")
        .expect("the handler must offer a rename range, not fall through to None");

    assert_eq!(
        response,
        PrepareRenameResponse::Range(Range {
            start: Position {
                line: 0,
                character: NAME_START,
            },
            end: Position {
                line: 0,
                character: NAME_END,
            },
        }),
        "the handler must return the quote-excluded `contact` span (start 12, end 19), \
         proving the .blade.php early-return fell through to the Folio decl path"
    );
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

/// Build the call-site `EditTarget`s for a `route('<route_name>')` reference in
/// `call_site`, exactly the way the `rename` handler does. The handler gets its
/// call sites from `salsa.find_references` (a `SymbolIndex::find` under the
/// hood) and maps each hit to an `EditTarget` carrying the full `new_name`
/// (`main.rs`, the `targets` map before the per-kind decl extension). This
/// drives that same `SymbolIndex::find` path on a hand-built parse of the call
/// site — no Salsa actor needed — so the test exercises the real lookup rather
/// than a literal stand-in.
fn call_site_edit_targets(call_site: &Path, route_name: &str, new_name: &str) -> Vec<EditTarget> {
    let mut patterns = ParsedPatternsData::default();
    patterns.route_refs.push(Arc::new(RouteReferenceData {
        name: route_name.to_string(),
        line: 17,
        column: 23,
        end_column: 23 + route_name.len() as u32,
    }));
    let mut index = SymbolIndex::default();
    index.insert_file(call_site, &patterns);

    index
        .find(&SymbolRefData::Route(route_name.to_string()))
        .into_iter()
        .map(|r| EditTarget {
            file_path: r.file_path,
            line: r.line,
            start_column: r.column,
            end_column: r.end_column,
            // Call sites always get the full `new_name`; the decl side may
            // differ (leaf-only) — that's exactly what the next test asserts.
            new_text: new_name.to_string(),
        })
        .collect()
}

#[tokio::test]
async fn rename_workspace_edit_carries_both_page_decl_and_call_site() {
    // AC6: a Folio rename must emit ONE `WorkspaceEdit` that rewrites BOTH the
    // page's own `name('...')` declaration AND every `route('...')` call site.
    // This is the exact seam issue #100 exists to guard: the original bug
    // rewrote the call sites while leaving the page declaration stale. Drive
    // the handler's assembly order — call-site targets from the symbol-index
    // lookup, then `targets.extend(collect_route_declaration_targets(...))`,
    // then `build_rename_workspace_edit` — and assert both edits land together.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "contact").await;

    let call_site = root
        .path()
        .join("app/Http/Controllers/ContactController.php");
    let mut targets = call_site_edit_targets(&call_site, "contact", "support");
    targets.extend(
        collect_route_declaration_targets(&server, root.path(), "contact", "support").await,
    );

    let edit = build_rename_workspace_edit(&targets, &[])
        .expect("a Folio rename with a decl + a call site must produce a WorkspaceEdit");
    let changes = edit
        .changes
        .expect("a text-only rename populates the legacy `changes` map");

    // The page's `name('contact')` declaration — leaf-only `support` at the
    // quote-excluded content span — is in the same edit.
    let page_uri = Url::from_file_path(&page).unwrap();
    let page_edits = changes
        .get(&page_uri)
        .expect("the page declaration edit must travel in the WorkspaceEdit");
    assert_eq!(page_edits.len(), 1, "one decl edit for the page");
    assert_eq!(page_edits[0].new_text, "support");
    assert_eq!(page_edits[0].range.start.character, NAME_START);
    assert_eq!(page_edits[0].range.end.character, NAME_END);

    // The `route('contact')` call site — rewritten with the full new name — is
    // in the SAME edit, so the two never drift apart.
    let call_uri = Url::from_file_path(&call_site).unwrap();
    let call_edits = changes
        .get(&call_uri)
        .expect("the route('...') call site must travel in the same WorkspaceEdit");
    assert_eq!(call_edits.len(), 1, "one call-site edit");
    assert_eq!(call_edits[0].new_text, "support");
}

#[tokio::test]
async fn rename_workspace_edit_pairs_leaf_only_decl_with_full_call_site_under_a_mount_prefix() {
    // AC6 under a Folio mount `->name('admin.')`: the page's own
    // `name('contact')` helper owns only the leaf, while the call site uses the
    // fully-qualified `admin.contact`. Renaming `admin.contact` →
    // `admin.support` must put the leaf-only `support` at the page declaration
    // and the full `admin.support` at the call site — both in ONE
    // `WorkspaceEdit`, never doubling the mount prefix at the decl.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "admin.contact").await;

    let call_site = root.path().join("app/Http/Controllers/AdminController.php");
    let mut targets = call_site_edit_targets(&call_site, "admin.contact", "admin.support");
    targets.extend(
        collect_route_declaration_targets(&server, root.path(), "admin.contact", "admin.support")
            .await,
    );

    let edit = build_rename_workspace_edit(&targets, &[])
        .expect("the mount-prefixed Folio rename must produce a WorkspaceEdit");
    let changes = edit.changes.expect("text-only rename populates `changes`");

    // Decl: leaf-only — the mount owns the `admin.` prefix, not the page.
    let page_uri = Url::from_file_path(&page).unwrap();
    let page_edits = changes
        .get(&page_uri)
        .expect("the page declaration edit must be present");
    assert_eq!(
        page_edits[0].new_text, "support",
        "the decl rewrites only the leaf — the mount prefix lives on the mount"
    );

    // Call site: the full dotted name, in the same edit alongside the
    // leaf-only decl.
    let call_uri = Url::from_file_path(&call_site).unwrap();
    let call_edits = changes
        .get(&call_uri)
        .expect("the call site must travel in the same WorkspaceEdit");
    assert_eq!(
        call_edits[0].new_text, "admin.support",
        "call sites get the full dotted name"
    );
}
