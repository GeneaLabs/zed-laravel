//! Tests for the cached `<x-` / `<flux:` / `<livewire:` completion
//! enumerations (issue #371).
//!
//! Before the cache, every keystroke in a tag context re-walked the component
//! directories and paid a `canonicalize()` per emitted entry — ~23 ms for
//! `<x-` and ~16 ms for `<flux:` on this repo's `test-project/`, with no repeat
//! ever cheaper than the first.
//!
//! Freshness has two mechanisms, and the whole point of these tests is to keep
//! them apart. The watcher hook is the primary; the TTL is a backstop for
//! directories the watcher's globs cannot see. The dangerous failure is the
//! backstop MASKING a broken primary: if the watcher hook stops firing, a live
//! TTL turns that into "completions feel intermittently stale" — self-
//! correcting, never reported, and able to survive indefinitely.
//!
//! So every test of the watcher or config hook first raises
//! `component_completion_ttl` beyond reach (`UNREACHABLE_TTL`). If the hook
//! under test is broken, the assertion fails outright instead of degrading into
//! tolerable staleness. Only `ttl_backstop_*` exercises the TTL, and it does so
//! by setting the TTL to zero rather than by sleeping — a test that waits out a
//! real 2 s TTL is both slow and unable to tell the two mechanisms apart.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tower_lsp::lsp_types::{DidChangeWatchedFilesParams, FileChangeType, FileEvent, Url};
use tower_lsp::{LanguageServer, LspService};

/// Far longer than any test runs, so the TTL backstop can never fire and the
/// mechanism under test is the only thing that could clear the cache.
const UNREACHABLE_TTL: Duration = Duration::from_secs(3600);

fn test_server(ttl: Duration) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let mut server = service.inner().clone();
    server.component_completion_ttl = ttl;
    server
}

/// Lay down a project with one anonymous Blade component and one Flux
/// component, and point the server at it.
async fn seed(server: &LaravelLanguageServer, root: &Path) {
    write_component(root, "alpha");
    write_flux(root, "alpha");
    *server.root_path.write().await = Some(root.to_path_buf());
}

fn write_component(root: &Path, name: &str) {
    let p = root
        .join("resources/views/components")
        .join(format!("{name}.blade.php"));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "<div>{{ $slot }}</div>").unwrap();
}

fn write_flux(root: &Path, name: &str) {
    let p = root
        .join("resources/views/flux")
        .join(format!("{name}.blade.php"));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "<div>{{ $slot }}</div>").unwrap();
}

fn names(items: &[laravel_lsp::component_completion::ComponentCandidate]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|c| c.name.clone()).collect();
    v.sort();
    v
}

/// Fire a watched-file event for `path`, the way the client would.
async fn watched_change(server: &LaravelLanguageServer, path: &Path, typ: FileChangeType) {
    server
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(path).unwrap(),
                typ,
            }],
        })
        .await;
}

// ---------------------------------------------------------------------------
// The cache exists at all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn repeat_requests_are_served_without_rescanning() {
    // Proves the result is actually cached, by changing the filesystem behind
    // the server's back with no event: a scan would see `beta`, the cache
    // cannot. Without this, every "the cache was cleared" test below could pass
    // on a server that never cached anything.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;

    let first = server.cached_blade_components().await;
    assert_eq!(names(&first), vec!["alpha".to_string()]);

    write_component(tmp.path(), "beta");
    let second = server.cached_blade_components().await;

    assert_eq!(
        names(&second),
        vec!["alpha".to_string()],
        "a second request must be served from the cache, not re-walked"
    );
}

#[tokio::test]
async fn flux_repeat_requests_are_served_without_rescanning() {
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;

    assert_eq!(
        names(&server.cached_flux_components().await),
        vec!["alpha".to_string()]
    );
    write_flux(tmp.path(), "beta");

    assert_eq!(
        names(&server.cached_flux_components().await),
        vec!["alpha".to_string()],
        "the Flux enumeration is cached too"
    );
}

// ---------------------------------------------------------------------------
// The watcher hook, with the TTL held beyond reach
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_watched_blade_event_alone_clears_the_cache() {
    // THE test this design exists for. The TTL cannot fire, so if the watcher
    // hook is ever dropped by a refactor this goes red rather than quietly
    // degrading to "stale for two seconds".
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()],
        "cache warmed"
    );

    write_component(tmp.path(), "beta");
    let created = tmp.path().join("resources/views/components/beta.blade.php");
    watched_change(&server, &created, FileChangeType::CREATED).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string(), "beta".to_string()],
        "the watcher event alone must clear the cache — with the TTL unable to \
         fire, nothing else could have"
    );
}

#[tokio::test]
async fn a_watched_delete_alone_clears_the_cache() {
    // Deletes matter as much as creates: a removed component that lingers in
    // completion inserts a tag that no longer resolves.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;
    write_component(tmp.path(), "beta");

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string(), "beta".to_string()]
    );

    let removed = tmp.path().join("resources/views/components/beta.blade.php");
    fs::remove_file(&removed).unwrap();
    watched_change(&server, &removed, FileChangeType::DELETED).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()],
        "a delete event must clear the cache too"
    );
}

#[tokio::test]
async fn a_watched_event_clears_the_flux_and_livewire_entries_too() {
    // The three enumerations are dropped as a group. A hook that only cleared
    // the Blade entry would leave the other two stale forever under an
    // unreachable TTL.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;

    let _ = server.cached_blade_components().await;
    assert_eq!(
        names(&server.cached_flux_components().await),
        vec!["alpha".to_string()],
        "flux cache warmed"
    );

    write_flux(tmp.path(), "beta");
    let created = tmp.path().join("resources/views/flux/beta.blade.php");
    watched_change(&server, &created, FileChangeType::CREATED).await;

    assert_eq!(
        names(&server.cached_flux_components().await),
        vec!["alpha".to_string(), "beta".to_string()],
        "the Flux entry must be cleared by the same event"
    );
}

#[tokio::test]
async fn a_non_php_watched_event_does_not_clear_the_cache() {
    // Discriminates the hook from "clear on absolutely everything", which would
    // make the cache useless. An asset change cannot add a component tag.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;
    let _ = server.cached_blade_components().await;

    let asset = tmp.path().join("resources/css/app.css");
    fs::create_dir_all(asset.parent().unwrap()).unwrap();
    fs::write(&asset, "body{}").unwrap();
    write_component(tmp.path(), "beta");
    watched_change(&server, &asset, FileChangeType::CHANGED).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()],
        "a non-PHP event must leave the cache intact"
    );
}

// ---------------------------------------------------------------------------
// The config hook, with the TTL held beyond reach
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_invalidation_alone_clears_the_cache() {
    // The scanned directory SET is config-derived, so a config change can move
    // the directories rather than just their contents. Same isolation: the TTL
    // cannot fire, so only this hook can explain a cleared cache.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);
    seed(&server, tmp.path()).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()]
    );

    write_component(tmp.path(), "beta");
    server.invalidate_config_cache().await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string(), "beta".to_string()],
        "invalidate_config_cache alone must clear the component enumerations"
    );
}

// ---------------------------------------------------------------------------
// The TTL backstop, exercised without sleeping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ttl_backstop_expires_the_cache_without_any_event() {
    // The backstop's whole job: bound staleness for a directory no watcher glob
    // covers, where no event will ever arrive. Driven by a zero TTL rather than
    // by sleeping out the real one — a 2 s sleep would be slow AND unable to
    // distinguish the backstop from the hooks.
    let tmp = TempDir::new().unwrap();
    let server = test_server(Duration::ZERO);
    seed(&server, tmp.path()).await;

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()]
    );

    write_component(tmp.path(), "beta");

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string(), "beta".to_string()],
        "an expired entry must be rebuilt with no watcher or config event at all"
    );
}

#[tokio::test]
async fn the_shipped_ttl_is_short_enough_to_be_a_backstop() {
    // Guards the value from drifting into something that would make staleness
    // user-visible. Deliberately loose: the watcher carries the common case, so
    // the exact number is not load-bearing and this is not a tuning knob.
    let server = test_server(crate::COMPONENT_COMPLETION_TTL);
    assert!(
        server.component_completion_ttl <= Duration::from_secs(5),
        "the backstop must bound staleness to something a user would not \
         notice, got {:?}",
        server.component_completion_ttl
    );
    assert!(
        server.component_completion_ttl > Duration::ZERO,
        "a zero TTL would disable caching entirely, which is the bug this \
         whole change removes"
    );
}

// ---------------------------------------------------------------------------
// Root identity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_different_project_root_discards_the_cache() {
    // Cached entries are keyed by root. Answering one project's completion with
    // another's components would be worse than a slow scan.
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);

    seed(&server, first.path()).await;
    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()]
    );

    write_component(second.path(), "gamma");
    *server.root_path.write().await = Some(second.path().to_path_buf());

    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["gamma".to_string()],
        "a new root must rescan, never serve the previous project's components"
    );
}

#[tokio::test]
async fn no_root_yields_no_components_and_caches_nothing() {
    // Fail-closed: before a root is discovered there is nothing to scan, and
    // that emptiness must not be cached as if it were an answer.
    let tmp = TempDir::new().unwrap();
    let server = test_server(UNREACHABLE_TTL);

    assert!(
        server.cached_blade_components().await.is_empty(),
        "no root means no components"
    );

    seed(&server, tmp.path()).await;
    assert_eq!(
        names(&server.cached_blade_components().await),
        vec!["alpha".to_string()],
        "once a root arrives the scan must run — the rootless answer must not \
         have been cached"
    );
}
