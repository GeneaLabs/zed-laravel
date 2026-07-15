//! Coverage for `LaravelLanguageServer::hover_for_route` guard parity with
//! `route_not_found_diagnostics` (issue #247).
//!
//! The diagnostic path learned three guards — absent index, empty (cold-start)
//! index, wildcard name (issue #209) — but the hover path kept rendering a
//! "route not found in index" trailer in all three situations, so hover
//! disagreed with the (silent) squiggly. These tests drive the real private
//! `hover_for_route` on a live server instance (repo `test_server()` pattern)
//! with the route index primed directly, and pin down:
//!   (a) absent index      → empty string, no card
//!   (b) empty index       → empty string, no card
//!   (c) wildcard name     → empty string, no card (even against a non-empty index)
//!   (d) genuine miss      → "route not found in index" trailer, unchanged
//!   (e) found route       → full detail card, unchanged

use crate::LaravelLanguageServer;
use laravel_lsp::route_discovery::{RouteDefinition, RouteIndex};
use std::path::PathBuf;
use tower_lsp::LspService;

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can prime
/// its private `route_index` and call its private `hover_for_route`.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// An index holding `home` with full detail metadata, so the found-route card
/// renders the verb + URI + action line. Mirrors `index_with` in
/// `route_diagnostics.rs`, plus the optional detail fields.
fn index_with_home() -> RouteIndex {
    let mut idx = RouteIndex::new();
    idx.insert(
        "home".to_string(),
        RouteDefinition {
            file: PathBuf::from("routes/web.php"),
            line: 0,
            column: 0,
            end_column: 0,
            priority: 0,
            method: Some("get".to_string()),
            uri: Some("/".to_string()),
            action: Some("HomeController@index".to_string()),
        },
    );
    idx
}

/// Prime the server's route index and render the hover for `name`.
async fn hover_with_index(index: Option<RouteIndex>, name: &str) -> String {
    let server = test_server();
    *server.route_index.write().await = index;
    server.hover_for_route(name).await
}

#[tokio::test]
async fn absent_index_renders_nothing() {
    // Before the index is built, every route would look missing — same guard
    // as `route_not_found_diagnostics`.
    let rendered = hover_with_index(None, "home").await;
    assert!(
        rendered.is_empty(),
        "absent index must suppress the hover card, got: {rendered:?}"
    );
}

#[tokio::test]
async fn empty_index_renders_nothing() {
    // Cold-start guard: an index that exists but holds nothing yet must not
    // make every route look missing.
    let rendered = hover_with_index(Some(RouteIndex::new()), "home").await;
    assert!(
        rendered.is_empty(),
        "empty index must suppress the hover card, got: {rendered:?}"
    );
}

#[tokio::test]
async fn wildcard_name_renders_nothing() {
    // Wildcard patterns (`players.*`) reference a family of routes and never
    // appear verbatim in the index (issue #209) — no not-found trailer, even
    // against a non-empty index.
    let rendered = hover_with_index(Some(index_with_home()), "players.*").await;
    assert!(
        rendered.is_empty(),
        "wildcard route names must suppress the hover card, got: {rendered:?}"
    );
}

#[tokio::test]
async fn genuine_miss_renders_not_found_trailer() {
    let rendered = hover_with_index(Some(index_with_home()), "does.not.exist").await;
    assert!(
        rendered.contains("route not found in index"),
        "a genuine miss must keep the not-found trailer, got: {rendered:?}"
    );
}

#[tokio::test]
async fn found_route_renders_detail_card() {
    let rendered = hover_with_index(Some(index_with_home()), "home").await;
    assert!(
        !rendered.is_empty(),
        "a found route must render the detail card"
    );
    assert!(
        !rendered.contains("not found"),
        "a found route must not carry a not-found trailer, got: {rendered:?}"
    );
    assert!(
        rendered.contains("GET"),
        "the detail card must carry the HTTP verb, got: {rendered:?}"
    );
    // `hover_for_route` formats the detail line as `` `GET /` → `action` `` —
    // assert the backticked verb+URI pair rather than a bare "/" (which any
    // source link would satisfy vacuously).
    assert!(
        rendered.contains("`GET /`"),
        "the detail card must carry the URI, got: {rendered:?}"
    );
    assert!(
        rendered.contains("HomeController@index"),
        "the detail card must carry the action, got: {rendered:?}"
    );
    // The source link built via `source_link(&def.file, Some(def.line + 1))`
    // always displays the originating file path, whatever the link shape.
    assert!(
        rendered.contains("routes/web.php"),
        "the detail card must carry a source link to the route's file, got: {rendered:?}"
    );
}
