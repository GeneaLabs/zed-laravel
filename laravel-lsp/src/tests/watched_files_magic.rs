//! Handler-level coverage for the external-edit magic-member convergence
//! introduced by M2: `did_change_watched_files` → the debounced incremental
//! batch → dependent re-resolution.
//!
//! Before M2, an external `.php` change (a `git pull`, a formatter run outside
//! Zed, a branch switch) only updated Salsa inputs and then scheduled a
//! project-wide reconverge that was SIZE-GATED OFF above 15,000 files — so on a
//! large project the magic-member index silently went stale after any disk-side
//! change. M2 routes the watched path through the same dependency-tracked
//! pre/post surface diff `did_save` uses, scaling with the *change*, not the
//! project, and drops the gate entirely.
//!
//! Lower layers are already covered elsewhere: the surface diff in
//! `class_hierarchy_index/tests.rs`, the dependency index in
//! `magic_dependency_index.rs`, receiver/member resolution in
//! `member_resolver/tests.rs`. What was NOT covered is the *handler* wiring:
//! that a synthesized `DidChangeWatchedFilesParams` actually converges a
//! previously-indexed dependent across create / change / delete, purges a
//! deleted file's dependency + view-render contributions, and runs regardless
//! of project size.
//!
//! These drive the **real** `Backend` on the `LspService::new` harness (same
//! seam as `inertia_handler.rs` / `macro_goto_def_handler.rs`), priming only
//! `root_path` — the one piece of state `refresh_file_magic` reads — and then
//! feeding synthesized watched-file events through the real notification
//! handler. The batch is awaited via its `JoinHandle` (which M2 keeps alive and
//! awaitable precisely so a test can observe convergence deterministically).

use crate::{LaravelLanguageServer, PendingWatchedChange};
use laravel_lsp::salsa_impl::ParsedPatternsData;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tower_lsp::lsp_types::{DidChangeWatchedFilesParams, FileChangeType, FileEvent, Url};
use tower_lsp::{LanguageServer, LspService};

const COMPOSER: &str = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;

/// A backend with `root_path` primed to `root` — the only state the watched
/// magic batch reads (`refresh_file_magic` resolves against it).
async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
}

/// Write a `.php` file (creating parent dirs) and return its absolute path.
fn write_file(root: &Path, rel: &str, src: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, src).unwrap();
    path
}

/// Register a file in Salsa and run the per-file magic refresh — exactly what
/// the save path does, so the file's entries + receiver deps + render sites
/// land in the live indexes.
async fn seed(backend: &LaravelLanguageServer, path: &Path, src: &str) {
    backend
        .salsa
        .update_file(path.to_path_buf(), 1, src.to_string())
        .await
        .unwrap();
    backend.refresh_file_magic(path, src).await;
}

/// The resolved magic-member *member names* the index currently holds for
/// `path` (empty if the file has no entries).
async fn member_names(backend: &LaravelLanguageServer, path: &Path) -> HashSet<String> {
    backend
        .salsa
        .export_magic_members()
        .await
        .unwrap()
        .get(path)
        .map(|entries| entries.iter().map(|e| e.member.clone()).collect())
        .unwrap_or_default()
}

/// Does the magic-dependency index still hold a `by_file` entry for `path`?
fn magic_deps_has(backend: &LaravelLanguageServer, path: &Path) -> bool {
    backend
        .magic_deps
        .read()
        .unwrap()
        .export()
        .iter()
        .any(|(p, _)| p == path)
}

/// One watched-file event for `path`.
fn watched(path: &Path, typ: FileChangeType) -> DidChangeWatchedFilesParams {
    DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: Url::from_file_path(path).unwrap(),
            typ,
        }],
    }
}

/// Await the in-flight watched-files batch to completion (M2 keeps the handle
/// alive and awaitable for exactly this).
async fn drain_batch(backend: &LaravelLanguageServer) {
    let handle = backend.magic_rebuild_handle.write().await.take();
    if let Some(h) = handle {
        let _ = h.await;
    }
}

fn post_with_accessors(accessors: &[&str]) -> String {
    let methods: String = accessors
        .iter()
        .map(|a| format!("    public function get{a}Attribute(): string {{ return ''; }}\n"))
        .collect();
    format!(
        "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Post extends Model {{\n{methods}}}\n"
    )
}

/// A controller reading `$post->headline` and `$post->summary` off a typed
/// `Post` param — `headline`/`summary` resolve only when `Post` declares the
/// matching accessor, which is what makes it a dependent of `App\Models\Post`.
const CONSUMER: &str = r#"<?php
namespace App\Http\Controllers;
use App\Models\Post;
class PostController {
    public function show(Post $post) {
        return [$post->headline, $post->summary];
    }
}
"#;

#[tokio::test]
async fn changed_dependency_converges_dependent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post_v1 = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_v1);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);

    seed(&backend, &post, &post_v1).await;
    seed(&backend, &consumer, CONSUMER).await;

    let before = member_names(&backend, &consumer).await;
    assert!(
        before.contains("headline"),
        "seed: headline must resolve; got {before:?}"
    );
    assert!(
        !before.contains("summary"),
        "seed: summary must NOT resolve before Post declares it; got {before:?}"
    );

    // External edit adds a `summary` accessor — a surface change.
    let post_v2 = post_with_accessors(&["Headline", "Summary"]);
    fs::write(&post, &post_v2).unwrap();
    backend
        .did_change_watched_files(watched(&post, FileChangeType::CHANGED))
        .await;
    drain_batch(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        after.contains("headline") && after.contains("summary"),
        "dependent must converge to BOTH accessors after the external change; got {after:?}"
    );
}

#[tokio::test]
async fn created_dependency_converges_pre_existing_dependent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // The consumer references App\Models\Post BEFORE the model exists. A typed
    // param records the receiver dep syntactically (before classification), so
    // the dependent is discoverable the moment Post lands on disk.
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(&backend, &consumer, CONSUMER).await;
    assert!(
        member_names(&backend, &consumer).await.is_empty(),
        "seed: nothing resolves while Post is absent"
    );

    // Post is created on disk (git pull, scaffolding tool).
    let post_src = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_src);
    backend
        .did_change_watched_files(watched(&post, FileChangeType::CREATED))
        .await;
    drain_batch(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        after.contains("headline"),
        "creating the dependency must ripple to the pre-existing dependent; got {after:?}"
    );
}

#[tokio::test]
async fn deleted_dependency_converges_dependent_and_purges_deps() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // Post both declares an accessor AND reads it via `$this->headline`, so it
    // has its own magic-dependency contribution (a self-dep on App\Models\Post)
    // that the delete must purge — the leak M2 closes.
    let post_src = "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Post extends Model {\n    public function getHeadlineAttribute(): string { return ''; }\n    public function describe(): string { return $this->headline; }\n}\n";
    let post = write_file(root, "app/Models/Post.php", post_src);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);

    seed(&backend, &post, post_src).await;
    seed(&backend, &consumer, CONSUMER).await;

    assert!(
        member_names(&backend, &consumer).await.contains("headline"),
        "seed: consumer resolves headline"
    );
    assert!(
        magic_deps_has(&backend, &post),
        "seed: Post has its own magic-dependency contribution"
    );

    // Post is deleted on disk.
    fs::remove_file(&post).unwrap();
    backend
        .did_change_watched_files(watched(&post, FileChangeType::DELETED))
        .await;
    drain_batch(&backend).await;

    assert!(
        member_names(&backend, &consumer).await.is_empty(),
        "dependent must converge — Post gone, its accessors no longer resolve"
    );
    assert!(
        member_names(&backend, &post).await.is_empty(),
        "the deleted file's own entries are evicted"
    );
    assert!(
        !magic_deps_has(&backend, &post),
        "the deleted file's magic-dependency contribution must be purged"
    );
}

#[tokio::test]
async fn deleted_controller_purges_view_renders() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // A model to give the render var a resolvable type, and a controller that
    // renders a view — so the controller has a view-render contribution.
    let post_src = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_src);
    let controller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\Post;
class ReportController {
    public function show(Post $post) {
        return view('reports.show', ['post' => $post]);
    }
}
"#;
    let controller = write_file(
        root,
        "app/Http/Controllers/ReportController.php",
        controller_src,
    );

    seed(&backend, &post, &post_src).await;
    seed(&backend, &controller, controller_src).await;
    assert!(
        backend
            .view_vars
            .read()
            .unwrap()
            .renders_for(&controller)
            .is_some(),
        "seed: controller has a recorded view-render contribution"
    );

    fs::remove_file(&controller).unwrap();
    backend
        .did_change_watched_files(watched(&controller, FileChangeType::DELETED))
        .await;
    drain_batch(&backend).await;

    assert!(
        backend
            .view_vars
            .read()
            .unwrap()
            .renders_for(&controller)
            .is_none(),
        "the deleted controller's view-render contribution must be purged"
    );
}

#[tokio::test]
async fn incremental_batch_runs_regardless_of_project_size() {
    // Prove the 15k size gate is gone: inflate the pattern cache past the old
    // MAX_FULL_REBUILD_FILES ceiling with cheap dummy entries, then run the
    // change flow. Convergence still happens because the batch scales with the
    // change set, not the project — the old gated full rebuild would have
    // skipped entirely at this size.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post_v1 = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_v1);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(&backend, &post, &post_v1).await;
    seed(&backend, &consumer, CONSUMER).await;

    // Inflate the pattern cache past the retired 15,000-file gate with cheap
    // empty entries (no parsing — direct DashMap inserts).
    let cache = backend.salsa.pattern_cache();
    for i in 0..15_001 {
        cache.insert(
            root.join(format!("app/Filler/F{i}.php")),
            (0, Arc::new(ParsedPatternsData::default())),
        );
    }
    assert!(cache.len() > 15_000, "cache must exceed the retired gate");

    let post_v2 = post_with_accessors(&["Headline", "Summary"]);
    fs::write(&post, &post_v2).unwrap();
    backend
        .did_change_watched_files(watched(&post, FileChangeType::CHANGED))
        .await;
    drain_batch(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        after.contains("summary"),
        "incremental convergence must run even above the old 15k gate; got {after:?}"
    );
}

#[tokio::test]
async fn record_watched_change_is_first_event_wins() {
    // Two events for one path in a burst collapse to ONE pending entry: the
    // pre-change surface is captured on the FIRST event and never overwritten
    // (an interleaved re-parse would otherwise poison it), while the `deleted`
    // flag tracks the file's final existence (last-event-wins).
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post_src = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_src);
    seed(&backend, &post, &post_src).await;

    // First event captures Post's live surface.
    backend.record_watched_change(&post, false).await;

    // Now tear Post's hierarchy node down — a *fresh* snapshot here would come
    // back empty. First-event-wins means the second event must NOT re-snapshot.
    backend.salsa.remove_file(post.clone()).await.unwrap();
    backend.record_watched_change(&post, true).await;

    let state = backend.magic_batch_state.lock().await;
    assert_eq!(
        state.pending.len(),
        1,
        "two events for one path collapse to one entry"
    );
    let entry: &PendingWatchedChange = state.pending.get(&post).unwrap();
    assert!(
        entry.deleted,
        "the `deleted` flag tracks the final event (last-event-wins)"
    );
    assert!(
        entry
            .old_surfaces
            .keys()
            .any(|fqcn| fqcn == "App\\Models\\Post"),
        "the ORIGINAL pre-change surface is kept, not the now-empty re-snapshot; got {:?}",
        entry.old_surfaces
    );
}

#[tokio::test]
async fn scheduler_respawns_after_batch_exit_for_a_lone_event() {
    // FIX 1 (lost-wakeup race): once a batch drains and the task exits, it must
    // relinquish liveness under the state lock so a SINGLE subsequent event —
    // with no second unrelated event to un-stick it — spawns a fresh task and
    // converges. If the exit and the spawn-decision weren't serialized on one
    // lock (the old `is_finished` split), the second lone event could be
    // stranded with no task to drain it.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post_v1 = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_v1);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(&backend, &post, &post_v1).await;
    seed(&backend, &consumer, CONSUMER).await;

    // Batch 1 drains fully and the task exits.
    let post_v2 = post_with_accessors(&["Headline", "Summary"]);
    fs::write(&post, &post_v2).unwrap();
    backend
        .did_change_watched_files(watched(&post, FileChangeType::CHANGED))
        .await;
    drain_batch(&backend).await;
    assert!(member_names(&backend, &consumer).await.contains("summary"));

    // Liveness must have been relinquished under the state lock, map emptied.
    {
        let state = backend.magic_batch_state.lock().await;
        assert!(
            !state.running,
            "task must clear `running` on its empty-exit"
        );
        assert!(state.pending.is_empty(), "map drained");
    }

    // A LONE follow-up event (no second nudge) must spawn a fresh task and
    // converge — proving liveness was correctly relinquished. Drop the Summary
    // accessor this time: a surface change the consumer observes as `summary`
    // ceasing to resolve.
    let post_v3 = post_with_accessors(&["Headline"]);
    fs::write(&post, &post_v3).unwrap();
    backend
        .did_change_watched_files(watched(&post, FileChangeType::CHANGED))
        .await;
    drain_batch(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        after.contains("headline") && !after.contains("summary"),
        "a lone event after batch exit must converge on its own; got {after:?}"
    );
}

#[tokio::test]
async fn drain_treats_present_flagged_but_missing_file_as_delete() {
    // FIX 2 (flag-race): existence is authoritative AT DRAIN, from re-reading
    // the file — not the recorded `deleted` flag. Simulate the outcome of two
    // overlapping notifications leaving `deleted == false` on a file that is
    // actually gone (its Salsa/hierarchy state already removed), and assert the
    // batch converges it as a DELETE: the dependent drops, and the gone file's
    // dep/render contributions are purged rather than leaked.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // Post reads its own accessor (`$this->headline`) so it holds a magic-dep
    // contribution that the purge must drop.
    let post_src = "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Post extends Model {\n    public function getHeadlineAttribute(): string { return ''; }\n    public function describe(): string { return $this->headline; }\n}\n";
    let post = write_file(root, "app/Models/Post.php", post_src);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(&backend, &post, post_src).await;
    seed(&backend, &consumer, CONSUMER).await;
    assert!(member_names(&backend, &consumer).await.contains("headline"));
    assert!(magic_deps_has(&backend, &post));

    // Snapshot Post's live surface (what the FIRST, present-flagged notification
    // would have captured), then make Post actually gone — file removed AND its
    // Salsa/hierarchy state removed (the DELETE notification's index effect that
    // the flag race lost track of).
    let old_surfaces = backend
        .salsa
        .file_class_surfaces(post.clone())
        .await
        .unwrap();
    fs::remove_file(&post).unwrap();
    backend.salsa.remove_file(post.clone()).await.unwrap();

    // Hand the batch a stale present-flagged entry for the now-gone file.
    let mut batch = HashMap::new();
    batch.insert(
        post.clone(),
        PendingWatchedChange {
            old_surfaces,
            old_render_views: Vec::new(),
            deleted: false, // WRONG on purpose — the flag race
        },
    );
    backend.run_magic_batch_once(batch).await;

    assert!(
        member_names(&backend, &consumer).await.is_empty(),
        "a present-flagged-but-missing file must converge the dependent as a delete"
    );
    assert!(
        !magic_deps_has(&backend, &post),
        "the gone file's dep contribution must be purged, not leaked"
    );
}
