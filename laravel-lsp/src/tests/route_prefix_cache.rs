//! Regression tests for issue #80 — request handlers must read inherited
//! external-load route prefixes (issue #43) from the **warm route index**, not
//! by re-running the project load graph.
//!
//! `route_discovery::external_prefixes_for_file` / `external_prefixes_map` both
//! begin with `discover_route_files`, which `read_to_string`s every `.php` file
//! under `vendor/` — 16,051 of them in this repo's own `test-project/`. Four
//! per-request handlers called one of those two on every single request:
//! `document_symbol`, `references`' declaration walk, `classify_with_decl_
//! fallback` (references + prepare_rename + rename) and `decl_range_at`
//! (prepare_rename). Measured cost: `textDocument/documentSymbol` on
//! `routes/web.php` took 2,214 ms cold and ~510 ms on *every* repeat, against
//! 0.2–1.0 ms for the same request on `app/Models/User.php`. The whole result
//! was then discarded for any file with no external prefix, which is nearly
//! every file.
//!
//! Two independent properties are pinned here, because either alone can pass
//! for the wrong reason:
//!
//!   1. **No walk.** `route_discovery::discovery_walk_count()` — a per-thread
//!      counter bumped at the top of `discover_route_files` — must not move
//!      across a handler call. This is the direct call-count seam; restoring
//!      any of the four old call sites makes it move.
//!   2. **The cache is the source of truth.** The fixtures below deliberately
//!      put a prefix in the *index* that no on-disk `->group(<path>)` load
//!      would ever produce. A handler still consulting the filesystem answers
//!      `""` and drops the prefix; one reading the index answers `admin.`. A
//!      no-walk assertion alone would also pass if the handler silently stopped
//!      resolving prefixes at all, so the behaviour has to be pinned too.

use crate::{classify_with_decl_fallback, decl_range_at, LaravelLanguageServer};
use laravel_lsp::references::SymbolRef;
use laravel_lsp::route_discovery::{discovery_walk_count, normalize_path, RouteIndex};
use laravel_lsp::salsa_impl::ParsedPatternsData;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DocumentSymbolParams, DocumentSymbolResponse, PartialResultParams, Position,
    TextDocumentIdentifier, Url, WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

/// `routes/admin.php` as Laravel would have it after being loaded by
/// `Route::as('admin.')->group(base_path('routes/admin.php'))` from elsewhere.
/// The file itself carries no hint of that prefix — which is the point: the
/// prefix can only come from the cross-file load graph, and therefore (post-fix)
/// only from the warm index.
///
/// The `->name(` call sits on line 1, where the opening quote is column 48 —
/// so the `users.index` content spans columns 49..60 (end exclusive) and a
/// cursor at column 50 is inside the declaration's argument.
const ROUTE_SRC: &str = "<?php\nRoute::get('/users', [C::class, 'index'])->name('users.index');\n";
const NAME_START: u32 = 49;
const NAME_END: u32 = 60;
const NAME_CURSOR: Position = Position {
    line: 1,
    character: 50,
};

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Lay down `root/routes/admin.php` containing [`ROUTE_SRC`], plus a `vendor/`
/// tree holding one route-registering package file.
///
/// The vendor file is what the retired walk would have opened and read. It is
/// not needed to make the counter move (the counter bumps on the walk, not per
/// file), but it keeps the fixture honest: this is a project where a walk would
/// have real work to do and a real prefix source to find.
fn seed_project(root: &Path) -> PathBuf {
    let routes = root.join("routes");
    fs::create_dir_all(&routes).unwrap();
    let file = routes.join("admin.php");
    fs::write(&file, ROUTE_SRC).unwrap();

    let vendor_routes = root.join("vendor").join("acme").join("pkg").join("routes");
    fs::create_dir_all(&vendor_routes).unwrap();
    fs::write(
        vendor_routes.join("web.php"),
        "<?php\nRoute::get('/pkg', fn () => null)->name('pkg.home');\n",
    )
    .unwrap();

    file
}

/// A route index whose `external_prefixes` claims `file` inherits `admin.`.
///
/// Nothing on disk says so. `external_prefixes_for_file` walking this fixture
/// would return `[""]`, so any assertion below that observes `admin.` proves
/// the answer came from here and not from the filesystem.
fn index_with_prefix(file: &Path) -> RouteIndex {
    let mut index = RouteIndex::new();
    index.source_files.insert(normalize_path(file));
    index.external_prefixes.insert(
        normalize_path(file),
        vec![String::new(), "admin.".to_string()],
    );
    index
}

/// Point the server at `root` and install `index` (or leave the index absent).
async fn prime(server: &LaravelLanguageServer, root: &Path, index: Option<RouteIndex>) {
    *server.root_path.write().await = Some(root.to_path_buf());
    *server.route_index.write().await = index;
}

async fn document_symbol_names(server: &LaravelLanguageServer, file: &Path) -> Vec<String> {
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(file).unwrap(),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    match server.document_symbol(params).await.unwrap() {
        Some(DocumentSymbolResponse::Nested(symbols)) => {
            symbols.into_iter().map(|s| s.name).collect()
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// document_symbol — the handler issue #80 was reported against
// ---------------------------------------------------------------------------

#[tokio::test]
async fn document_symbol_on_a_routes_file_never_walks_the_project() {
    // The unprefixed common case: a routes file with no external load at all.
    // Pre-fix this still paid a full `discover_route_files` — the walk ran
    // first and its `[""]` result was thrown away one line later.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    let before = discovery_walk_count();
    let _ = document_symbol_names(&server, &file).await;
    let after = discovery_walk_count();

    assert_eq!(
        before, after,
        "document_symbol on a routes file must not run discover_route_files \
         (it reads every .php file under vendor/); walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn document_symbol_on_a_routes_file_never_walks_even_when_prefixed() {
    // The prefixed case must be cache-fed too. If the fix had only short-
    // circuited the empty case, this fixture would still walk.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let before = discovery_walk_count();
    let _ = document_symbol_names(&server, &file).await;
    let after = discovery_walk_count();

    assert_eq!(
        before, after,
        "an externally-prefixed routes file must resolve its prefix from the \
         warm index, not a fresh walk; walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn document_symbol_applies_the_prefix_held_only_by_the_warm_index() {
    // Behavioural half of the pair: the prefix exists nowhere on disk, so
    // seeing it in the outline proves the index is the source of truth.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let names = document_symbol_names(&server, &file).await;

    assert!(
        names.iter().any(|n| n.contains("[name=admin.users.index]")),
        "the outline must carry the index-held `admin.` prefix, got: {names:?}"
    );
}

#[tokio::test]
async fn document_symbol_leaves_symbols_unprefixed_when_the_index_holds_none() {
    // Discriminates the assertion above from one that would pass on any
    // non-empty outline: same file, same handler, no prefix in the index.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    let names = document_symbol_names(&server, &file).await;

    assert!(
        !names.iter().any(|n| n.contains("admin.")),
        "with no prefix in the index nothing may be prefixed, got: {names:?}"
    );
}

#[tokio::test]
async fn document_symbol_never_walks_before_the_route_index_exists() {
    // Cold start: `route_index` is still `None` because warm-up hasn't
    // finished. The handler must degrade to "no prefix", never to a walk —
    // this is the window in which the walk was most expensive (2,214 ms).
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), None).await;

    let before = discovery_walk_count();
    let _ = document_symbol_names(&server, &file).await;
    let after = discovery_walk_count();

    assert_eq!(
        before, after,
        "a cold-start document_symbol must not walk the project; \
         walk count moved {before} → {after}"
    );
}

// ---------------------------------------------------------------------------
// The cached accessors themselves — all three branches each
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_external_prefixes_defaults_to_the_empty_prefix_without_an_index() {
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), None).await;

    assert_eq!(
        server.cached_external_prefixes(&file).await,
        vec![String::new()],
        "an absent index must answer `[\"\"]`, the same default an unreachable \
         file gets — never a guessed prefix"
    );
}

#[tokio::test]
async fn cached_external_prefixes_defaults_for_a_file_the_index_does_not_hold() {
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    assert_eq!(
        server.cached_external_prefixes(&file).await,
        vec![String::new()],
        "a file the load graph never reached is still scanned directly, so the \
         empty prefix applies"
    );
}

#[tokio::test]
async fn cached_external_prefixes_returns_the_indexed_prefixes() {
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    assert_eq!(
        server.cached_external_prefixes(&file).await,
        vec![String::new(), "admin.".to_string()],
        "the indexed prefixes must come back verbatim, `\"\"` included"
    );
}

#[tokio::test]
async fn cached_external_prefix_map_is_empty_without_an_index() {
    let root = TempDir::new().unwrap();
    let server = test_server();
    prime(&server, root.path(), None).await;

    assert!(
        server.cached_external_prefix_map().await.is_empty(),
        "an absent index yields an empty map, which callers already read as \
         `[\"\"]` per file"
    );
}

#[tokio::test]
async fn cached_external_prefix_map_mirrors_the_index() {
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let map = server.cached_external_prefix_map().await;

    assert_eq!(
        map.get(&normalize_path(&file)),
        Some(&vec![String::new(), "admin.".to_string()]),
        "the map handed to the declaration walks must be the index's own, \
         keyed by normalized path; got: {map:?}"
    );
}

// ---------------------------------------------------------------------------
// references / prepare_rename / rename — the other three retired call sites
// ---------------------------------------------------------------------------

#[tokio::test]
async fn classify_with_decl_fallback_resolves_the_prefix_without_walking() {
    // Drives the shared classifier behind references, prepare_rename and
    // rename. Empty patterns force the declaration-fallback branch, which is
    // the one that used to call `external_prefixes_for_file`.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;
    let patterns = ParsedPatternsData::default();

    let before = discovery_walk_count();
    let symbol =
        classify_with_decl_fallback(&server, Some(root.path()), &file, &patterns, NAME_CURSOR)
            .await;
    let after = discovery_walk_count();

    assert_eq!(
        symbol,
        Some(SymbolRef::Route("admin.users.index".to_string())),
        "the declaration must anchor to the index-held prefixed name"
    );
    assert_eq!(
        before, after,
        "classifying a route declaration must not walk the project; \
         walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn classify_with_decl_fallback_takes_the_first_of_several_prefixes() {
    // The consumer half of the ordering contract. `compute_effective_prefixes`
    // guarantees `""` first then lexicographic order; this pins that the
    // classifier reads position 0 of the non-empty tail, so the two halves
    // together give one stable project-level name for a file loaded by more
    // than one prefixed group.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    let mut index = RouteIndex::new();
    index.source_files.insert(normalize_path(&file));
    index.external_prefixes.insert(
        normalize_path(&file),
        vec![String::new(), "admin.".to_string(), "blog.".to_string()],
    );
    prime(&server, root.path(), Some(index)).await;
    let patterns = ParsedPatternsData::default();

    let symbol =
        classify_with_decl_fallback(&server, Some(root.path()), &file, &patterns, NAME_CURSOR)
            .await;

    assert_eq!(
        symbol,
        Some(SymbolRef::Route("admin.users.index".to_string())),
        "the first non-empty prefix wins, and the index guarantees which one \
         that is"
    );
}

#[tokio::test]
async fn classify_with_decl_fallback_keeps_the_raw_name_without_a_prefix() {
    // Same cursor, no prefix in the index: the raw in-file name stands. Pins
    // that the test above is reading the prefix rather than always prepending
    // something.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;
    let patterns = ParsedPatternsData::default();

    let symbol =
        classify_with_decl_fallback(&server, Some(root.path()), &file, &patterns, NAME_CURSOR)
            .await;

    assert_eq!(
        symbol,
        Some(SymbolRef::Route("users.index".to_string())),
        "with no external prefix the declaration keeps its raw in-file name"
    );
}

#[tokio::test]
async fn decl_range_at_matches_a_prefixed_name_without_walking() {
    // prepare_rename's highlight range. The symbol carries the project-level
    // prefixed name; the file on disk only declares the raw leaf, so the range
    // is only found if the prefix is prepended from the index before matching.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let before = discovery_walk_count();
    let range = decl_range_at(
        &server,
        &file,
        NAME_CURSOR,
        &SymbolRef::Route("admin.users.index".to_string()),
    )
    .await;
    let after = discovery_walk_count();

    let range = range.expect("a cursor on the `->name(` argument must yield a rename range");
    assert_eq!(
        (range.start.line, range.start.character, range.end.character),
        (1, NAME_START, NAME_END),
        "the range must cover the `users.index` argument content, quotes excluded"
    );
    assert_eq!(
        before, after,
        "prepare_rename must not walk the project; walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn decl_range_at_finds_nothing_for_a_prefix_the_index_does_not_hold() {
    // Fail-closed companion: without the `admin.` entry, `admin.users.index`
    // matches no declaration in this file and no range is offered.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    let range = decl_range_at(
        &server,
        &file,
        NAME_CURSOR,
        &SymbolRef::Route("admin.users.index".to_string()),
    )
    .await;

    assert_eq!(
        range, None,
        "an unindexed prefix must not be invented to force a match"
    );
}

#[tokio::test]
async fn collect_declaration_locations_resolves_prefixes_without_walking() {
    // references' declaration half. It used to build its per-file prefix map
    // with `external_prefixes_map`, the map-shaped sibling of the same walk.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let before = discovery_walk_count();
    let locations = crate::collect_declaration_locations(
        &server,
        root.path(),
        &SymbolRef::Route("admin.users.index".to_string()),
    )
    .await;
    let after = discovery_walk_count();

    assert_eq!(
        locations.len(),
        1,
        "the prefixed name must match the raw in-file declaration once the \
         index-held `admin.` prefix is applied; got: {locations:?}"
    );
    assert_eq!(
        (
            locations[0].range.start.line,
            locations[0].range.start.character
        ),
        (1, NAME_START),
        "the location must point at the `->name(` argument content"
    );
    assert_eq!(
        before, after,
        "find-references must not walk the project; walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn collect_declaration_locations_finds_nothing_for_an_unindexed_prefix() {
    // Fail-closed companion: no `admin.` in the index, no match invented.
    let root = TempDir::new().unwrap();
    seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    let locations = crate::collect_declaration_locations(
        &server,
        root.path(),
        &SymbolRef::Route("admin.users.index".to_string()),
    )
    .await;

    assert!(
        locations.is_empty(),
        "an unindexed prefix must not resolve to a declaration; got: {locations:?}"
    );
}

#[tokio::test]
async fn collect_route_declaration_targets_resolves_prefixes_without_walking() {
    // rename's declaration half — the fourth and last retired call site. The
    // rewritten segment is the LEAF only, so the `admin.` prefix is never
    // written back into the source.
    let root = TempDir::new().unwrap();
    let file = seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(index_with_prefix(&file))).await;

    let before = discovery_walk_count();
    let targets = crate::collect_route_declaration_targets(
        &server,
        root.path(),
        "admin.users.index",
        "admin.users.list",
    )
    .await;
    let after = discovery_walk_count();

    assert_eq!(
        targets.len(),
        1,
        "the prefixed target must match the raw declaration; got: {targets:?}"
    );
    assert_eq!(
        (
            targets[0].line,
            targets[0].start_column,
            targets[0].end_column
        ),
        (1, NAME_START, NAME_END),
        "the edit must span the `users.index` argument content"
    );
    assert_eq!(
        targets[0].new_text, "users.list",
        "only the leaf is rewritten — the inherited `admin.` prefix has no \
         source text in this file to double"
    );
    assert_eq!(
        before, after,
        "rename must not walk the project; walk count moved {before} → {after}"
    );
}

#[tokio::test]
async fn collect_route_declaration_targets_skips_an_unindexed_prefix() {
    // Fail-closed companion.
    let root = TempDir::new().unwrap();
    seed_project(root.path());
    let server = test_server();
    prime(&server, root.path(), Some(RouteIndex::new())).await;

    let targets = crate::collect_route_declaration_targets(
        &server,
        root.path(),
        "admin.users.index",
        "admin.users.list",
    )
    .await;

    assert!(
        targets.is_empty(),
        "an unindexed prefix must not produce an edit; got: {targets:?}"
    );
}

// ---------------------------------------------------------------------------
// The seam itself
// ---------------------------------------------------------------------------

#[test]
fn discovery_walk_count_moves_when_a_walk_actually_happens() {
    // Canary for the instrumentation every assertion above rests on: a green
    // suite never exercises the counter's increment, so pin it directly. If
    // this ever stops moving, every "must not walk" test above has quietly
    // become unfalsifiable.
    let root = TempDir::new().unwrap();
    seed_project(root.path());

    let before = discovery_walk_count();
    let _ = laravel_lsp::route_discovery::discover_route_files(root.path());
    let after = discovery_walk_count();

    assert_eq!(
        after,
        before + 1,
        "discover_route_files must bump the per-thread walk counter exactly once"
    );
}
