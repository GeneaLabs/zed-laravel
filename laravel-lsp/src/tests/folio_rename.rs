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

/// Write a Folio page under `root/pages/<leaf>.blade.php`, where `<leaf>` is the
/// last dot-segment of `route_name` (the whole name when it has no dot), seed the
/// route index with `route_name` pointing at that file, set the project root, and
/// return the page path. The filename, the body's `name('...')` argument, and the
/// index entry's `uri` all track `route_name`, so nothing the helper produces is
/// hardcoded to `contact`:
///
///   * **filename** — the leaf: `"admin.contact"` → `pages/contact.blade.php`,
///     `"dashboard"` → `pages/dashboard.blade.php`, never a hardcoded
///     `contact.blade.php` that disagrees with its index key (issue #175).
///   * **body** — `<?php name('<leaf>'); ?>`, so the declaration the decl logic
///     re-reads agrees with the route-index key for every caller (issue #188).
///   * **uri** — dot separators become path segments: `"contact"` → `"/contact"`,
///     `"admin.contact"` → `"/admin/contact"` (issue #188).
///
/// Deriving all three keeps the fixture internally consistent rather than passing
/// for the wrong reason on a non-`contact` name. Mirrors how `inject_folio_routes`
/// seeds named pages (top-of-file anchor) so the decl logic must re-read the page
/// to find the `name(...)` span.
async fn seed_page(
    server: &LaravelLanguageServer,
    root: &Path,
    route_name: &str,
) -> std::path::PathBuf {
    // The page's own `name('...')` helper owns only the leaf segment, so both the
    // on-disk file and the body's `name('...')` argument are named for that leaf —
    // `admin.contact` → `contact.blade.php` carrying `name('contact')`. Generating
    // the body from the leaf (rather than a hardcoded `name('contact')`) keeps the
    // declaration the decl logic re-reads in sync with the route-index key for
    // every caller, not just `contact`-leaf ones (issue #188).
    let leaf = route_name.rsplit('.').next().unwrap_or(route_name);
    let page = root.join("pages").join(format!("{leaf}.blade.php"));
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, format!("<?php name('{leaf}'); ?>")).unwrap();

    // The seeded `uri` tracks `route_name` too — each dot separator becomes a path
    // segment, prefixed with `/` — so `"contact"` → `"/contact"` and
    // `"admin.contact"` → `"/admin/contact"`, never a hardcoded `/contact` that
    // disagrees with a non-`contact` key (issue #188).
    let uri = format!("/{}", route_name.replace('.', "/"));
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
            uri: Some(uri),
            action: None,
        },
    );
    *server.route_index.write().await = Some(index);
    *server.root_path.write().await = Some(root.to_path_buf());
    page
}

#[tokio::test]
async fn seed_page_filename_tracks_the_route_name_leaf() {
    // Guards the helper invariant (issue #175): `seed_page` derives the on-disk
    // filename from the leaf segment of `route_name` rather than hardcoding
    // `contact.blade.php`. With the old hardcode, a caller passing a name whose
    // leaf differs from `contact` would write a file whose leaf disagrees with the
    // route-index key — a logically-inconsistent fixture that could pass for the
    // wrong reason. This pins the derivation so the trap can't return.
    let root = TempDir::new().unwrap();
    let server = test_server();

    // No dot — the whole name is the leaf.
    let page = seed_page(&server, root.path(), "dashboard").await;
    assert_eq!(
        page.file_name().unwrap().to_str().unwrap(),
        "dashboard.blade.php",
        "a dotless route name uses the whole name as the file stem"
    );
    assert!(
        page.exists(),
        "the derived page file must be written to disk"
    );

    // Dotted — only the leaf segment names the file.
    let page = seed_page(&server, root.path(), "admin.dashboard").await;
    assert_eq!(
        page.file_name().unwrap().to_str().unwrap(),
        "dashboard.blade.php",
        "a dotted route name uses only its leaf segment as the file stem"
    );

    // AC2: the index entry's `file` points at the same derived path, so the index
    // key and the on-disk file stay consistent.
    let index = server.route_index.read().await;
    let def = index
        .as_ref()
        .unwrap()
        .get("admin.dashboard")
        .expect("the route index must carry the seeded name");
    assert_eq!(
        def.file, page,
        "RouteDefinition.file must point at the derived leaf-based path"
    );
}

#[tokio::test]
async fn seed_page_body_and_uri_track_the_route_name() {
    // Guards the helper invariant (issue #188) — the same latent-trap class #175
    // fixed for the filename, one level deeper. `seed_page` must derive the body's
    // `name('...')` argument (leaf-only) and the index entry's `uri` from
    // `route_name`, not hardcode `contact`/`/contact`. With the old hardcodes a
    // caller seeding a non-`contact` name got a body and uri that silently
    // disagreed with the route-index key — a fixture that could pass for the wrong
    // reason once a test re-read the page's `name('…')` span or its `uri`.
    let root = TempDir::new().unwrap();
    let server = test_server();

    // AC4: a dotless non-`contact` name — the whole name is the leaf in the body,
    // and the uri is a single segment.
    let page = seed_page(&server, root.path(), "dashboard").await;
    assert_eq!(
        fs::read_to_string(&page).unwrap(),
        "<?php name('dashboard'); ?>",
        "the body's name('...') argument tracks the route name leaf, not `contact`"
    );
    {
        let index = server.route_index.read().await;
        let def = index
            .as_ref()
            .unwrap()
            .get("dashboard")
            .expect("the route index must carry the seeded name");
        assert_eq!(
            def.uri.as_deref(),
            Some("/dashboard"),
            "a dotless route name yields a single-segment uri, not hardcoded `/contact`"
        );
    } // drop the read guard before seed_page below re-acquires the write lock

    // AC5: a dotted non-`contact` name — the body carries only the leaf, while the
    // uri expands every dot segment into a path segment.
    let page = seed_page(&server, root.path(), "admin.dashboard").await;
    assert_eq!(
        fs::read_to_string(&page).unwrap(),
        "<?php name('dashboard'); ?>",
        "a dotted route name puts only the leaf in the page's name('...') call"
    );
    let index = server.route_index.read().await;
    let def = index
        .as_ref()
        .unwrap()
        .get("admin.dashboard")
        .expect("the route index must carry the seeded name");
    assert_eq!(
        def.uri.as_deref(),
        Some("/admin/dashboard"),
        "a dotted route name expands each segment into the uri path"
    );
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
async fn decl_range_at_returns_the_page_name_span_for_a_non_contact_length_leaf() {
    // Span-length regression guard (issue #191). Every other decl/rename test
    // seeds a `contact` leaf (7 chars), so they all re-read a span that happens
    // to coincide with the `NAME_START`/`NAME_END` constants — a span-computation
    // bug for a different name length would still pass green. This drives
    // `decl_range_at` against a 9-char leaf (`dashboard`) and asserts the
    // *literal* columns the page body dictates, NOT the `contact`-sized constants.
    //
    // `<?php name('dashboard'); ?>` — the `dashboard` content occupies columns
    // 12..=20, so its quote-excluded span is start 12, end 21. The `CURSOR` at
    // column 13 sits inside it, just as it does for the shorter `contact`.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "dashboard").await;

    let range = decl_range_at(
        &server,
        &page,
        CURSOR,
        &SymbolRef::Route("dashboard".to_string()),
    )
    .await;

    assert_eq!(
        range,
        Some(Range {
            start: Position {
                line: 0,
                character: 12,
            },
            end: Position {
                line: 0,
                character: 21,
            },
        }),
        "the prepare_rename range must cover the 9-char `dashboard` content \
         (columns 12..21), proving the span is re-read from the page body rather \
         than fixed to the 7-char `contact` constants (NAME_START 12, NAME_END 19)"
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

/// Seed the route index with `route_name` → `file` and set the project root,
/// without writing `file` itself — the caller controls where it lives (inside
/// or outside `root`) so the containment guard, not a missing index entry,
/// decides the outcome. Mirrors `seed_folio_page` in `folio_cursor_containment`.
async fn seed_index_for(
    server: &LaravelLanguageServer,
    root: &Path,
    route_name: &str,
    file: &Path,
) {
    let mut index = RouteIndex::new();
    index.insert(
        route_name.to_string(),
        RouteDefinition {
            file: file.to_path_buf(),
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
}

#[tokio::test]
async fn collect_targets_skips_an_out_of_root_folio_page() {
    // Containment guard (issue #139): a Folio page that exists on disk and is
    // indexed, but lives OUTSIDE the project root, must produce no decl
    // `EditTarget`. Without the guard `collect_route_declaration_targets` would
    // read it and emit a rename edit against an out-of-project file.
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let page = outside.path().join("contact.blade.php");
    fs::write(&page, PAGE_SRC).unwrap();

    let server = test_server();
    seed_index_for(&server, root.path(), "contact", &page).await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "contact", "support").await;

    assert!(
        targets.is_empty(),
        "an out-of-root Folio page must not produce a decl edit, even though it \
         exists and is indexed — the containment guard refuses it"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn collect_targets_skips_an_under_root_symlink_resolving_outside() {
    // The discriminating case a lexical guard could not catch: the page path
    // lives *under* the project root — a symlink at
    // `<root>/pages/contact.blade.php` — yet resolves to a file OUTSIDE the
    // root. The target exists on disk (through the link) and is indexed under
    // its in-root link path, so a missing entry can't explain an empty result. A
    // purely lexical `starts_with` check would *admit* this path; only
    // canonicalization — `path_within_root` resolving the symlink to its
    // out-of-tree target — rejects it, so no decl edit is emitted.
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
    seed_index_for(&server, root.path(), "contact", &link).await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "contact", "support").await;

    assert!(
        targets.is_empty(),
        "a page reached through an under-root symlink that resolves outside the \
         project root must produce no decl edit — the canonicalize-based guard \
         refuses it even though the link path is lexically inside the root"
    );
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
async fn collect_targets_rewrites_a_non_contact_length_leaf_declaration() {
    // Span-length regression guard (issue #191), the rename-apply counterpart to
    // `decl_range_at_returns_the_page_name_span_for_a_non_contact_length_leaf`.
    // The sibling rewrite tests all seed a `contact` leaf (7 chars), so their
    // `start_column`/`end_column` assertions reuse the `NAME_START`/`NAME_END`
    // constants — a span miscomputation for another name length would pass green.
    // This drives `collect_route_declaration_targets` against a 9-char leaf
    // (`dashboard`) and asserts the *literal* edit columns, NOT the constants.
    //
    // `<?php name('dashboard'); ?>` — the `dashboard` content occupies columns
    // 12..=20, so the edit span is start 12, end 21.
    let root = TempDir::new().unwrap();
    let server = test_server();
    let page = seed_page(&server, root.path(), "dashboard").await;

    let targets =
        collect_route_declaration_targets(&server, root.path(), "dashboard", "home").await;

    assert_eq!(
        targets.len(),
        1,
        "exactly one declaration target — the page itself"
    );
    let t = &targets[0];
    assert_eq!(t.file_path, page);
    assert_eq!(t.line, 0);
    assert_eq!(
        t.start_column, 12,
        "the edit starts at the `dashboard` content's first column, re-read from \
         the body — matching the 7-char case's NAME_START only by coincidence"
    );
    assert_eq!(
        t.end_column, 21,
        "the edit ends at the 9-char `dashboard` content's end column (21), not \
         the 7-char `contact` NAME_END (19) — proving the span tracks name length"
    );
    assert_eq!(
        t.new_text, "home",
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
