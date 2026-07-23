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
//!
//! Also here (same harness, same fixtures): the M4 **on-open vendor** lazy
//! indexing tests at the bottom — `did_open` of a `vendor/` file resolves THAT
//! file's own magic-member usages into the reverse index (the eager build
//! skips vendor files as usage sites, so their occurrence lines were missing
//! from find-references), bounded by `vendor_open_magic_lru` so a session's
//! vendor browsing can't grow the index monotonically.

use crate::{LaravelLanguageServer, PendingWatchedChange};
use laravel_lsp::salsa_impl::ParsedPatternsData;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DidChangeWatchedFilesParams, DidOpenTextDocumentParams, FileChangeType, FileEvent,
    TextDocumentItem, Url,
};
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

/// A singletons registry map binding `key` → `concrete`, in the shape the
/// actor's LIVE binding registry holds. The container-resolution resolver
/// (`app('key')`, facade accessors) reads this map — and
/// `register_service_provider_source` alone does NOT populate it (that's the
/// async App-rescan's job), so tests that resolve through a binding must drive
/// it explicitly.
fn singleton_registry(
    key: &str,
    concrete: &str,
) -> HashMap<String, laravel_lsp::salsa_impl::BindingRegistrationData> {
    let mut m = HashMap::new();
    m.insert(
        key.to_string(),
        laravel_lsp::salsa_impl::BindingRegistrationData {
            abstract_name: key.to_string(),
            concrete_class: concrete.to_string(),
            file_path: None,
            binding_type: laravel_lsp::salsa_impl::BindingTypeData::Singleton,
            source_file: None,
            source_line: None,
            priority: 2,
        },
    );
    m
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

/// Await the in-flight save-path dependent ripple (#255 keeps the handle
/// alive and awaitable for exactly this — mirrors `drain_batch`).
async fn drain_ripple(backend: &LaravelLanguageServer) {
    let handle = backend.magic_ripple_handle.write().await.take();
    if let Some(h) = handle {
        let _ = h.await;
    }
}

/// #255 Bug A end-to-end regression, through the REAL save path: a macro
/// rename inside a provider's `boot()` body — an edit whose class-surface
/// diff is empty — must re-resolve dependent call sites in other files.
/// Crucially, the edit is delivered through `execute_salsa_update` (the
/// did_change debounce body) BEFORE the save, exactly like a real
/// "type → pause → save" flow: the debounce eagerly overwrites the live
/// provider input, so a pre-save snapshot of the live inputs would diff
/// empty and never ripple — the actor-kept baseline is what keeps the
/// `before` side pre-edit.
#[tokio::test]
async fn provider_body_macro_rename_converges_dependent_on_save() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let provider_v1 = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('slugify', fn () => 'x');
    }
}
"#;
    let provider = write_file(root, "app/Providers/AppServiceProvider.php", provider_v1);
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .unwrap();
    backend
        .salsa
        .register_service_provider_source(
            provider.clone(),
            provider_v1.to_string(),
            2,
            root.to_path_buf(),
        )
        .await
        .unwrap();

    // The dependent call site, in ANOTHER file.
    let consumer_src = r#"<?php
namespace App\Support;
use Illuminate\Support\Str;
class Ids {
    public function make(): string { return Str::slugify(); }
}
"#;
    let consumer = write_file(root, "app/Support/Ids.php", consumer_src);
    seed(&backend, &consumer, consumer_src).await;
    assert!(
        member_names(&backend, &consumer).await.contains("slugify"),
        "seed: the consumer's macro call must classify while the macro exists",
    );

    // Save #1 establishes the provider's registration baseline.
    let uri = Url::from_file_path(&provider).unwrap();
    backend.refresh_magic_on_save(&uri, provider_v1).await;
    drain_ripple(&backend).await;

    // The user types the rename; the did_change debounce fires and eagerly
    // re-registers the provider input with the EDITED text — pre-save.
    let provider_v2 = provider_v1.replace("'slugify'", "'sluggish'");
    backend.execute_salsa_update(&uri, &provider_v2, 2).await;

    // Then saves. The registration diff must come from the baseline, not the
    // (already-edited) live input, for the ripple to fire at all.
    backend.refresh_magic_on_save(&uri, &provider_v2).await;
    drain_ripple(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        !after.contains("slugify"),
        "the dependent's stale macro classification must clear on the provider save; got {after:?}"
    );
}

/// #267 end-to-end, binding kind: a provider that retargets a container binding
/// (`singleton('svc', Aa)` → `singleton('svc', Bb)`) in its `boot()` body must
/// re-resolve dependent `app('svc')->…()` call sites on save, mirroring the
/// macro test but for the binding registration kind — proving the alias/binding
/// ripple end-to-end, not just at the pure-helper level.
#[tokio::test]
async fn provider_body_binding_retarget_converges_dependent_on_save() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // v1 binds `svc` to `Aa`, which carries a `boom` macro.
    let provider_v1 = r#"<?php
namespace App\Providers;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        \App\Services\Aa::macro('boom', fn () => 'x');
        $this->app->singleton('svc', \App\Services\Aa::class);
    }
}
"#;
    let provider = write_file(root, "app/Providers/AppServiceProvider.php", provider_v1);
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .unwrap();
    backend
        .salsa
        .register_service_provider_source(
            provider.clone(),
            provider_v1.to_string(),
            2,
            root.to_path_buf(),
        )
        .await
        .unwrap();
    // Drive the live binding registry `svc` → Aa (source registration alone
    // doesn't populate it).
    backend
        .salsa
        .register_service_provider_registry(
            HashMap::new(),
            HashMap::new(),
            singleton_registry("svc", "App\\Services\\Aa"),
        )
        .await
        .unwrap();

    // The dependent resolves its receiver through the `svc` binding.
    let consumer_src = r#"<?php
namespace App\Support;
class Uses {
    public function go(): string { return app('svc')->boom(); }
}
"#;
    let consumer = write_file(root, "app/Support/Uses.php", consumer_src);
    seed(&backend, &consumer, consumer_src).await;
    assert!(
        member_names(&backend, &consumer).await.contains("boom"),
        "seed: the consumer's macro call must classify while `svc` binds to Aa",
    );

    // Save #1 establishes the provider's registration baseline.
    let uri = Url::from_file_path(&provider).unwrap();
    backend.refresh_magic_on_save(&uri, provider_v1).await;
    drain_ripple(&backend).await;

    // Retarget the binding to `Bb`, which has no `boom` macro. Deliver the edit
    // through the debounce first (like a real type→pause→save), then save.
    let provider_v2 = r#"<?php
namespace App\Providers;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        $this->app->singleton('svc', \App\Services\Bb::class);
    }
}
"#;
    backend.execute_salsa_update(&uri, provider_v2, 2).await;
    // The App rescan a provider save queues rebuilds the binding registry; drive
    // that here so `svc` now resolves to Bb.
    backend
        .salsa
        .register_service_provider_registry(
            HashMap::new(),
            HashMap::new(),
            singleton_registry("svc", "App\\Services\\Bb"),
        )
        .await
        .unwrap();
    backend.refresh_magic_on_save(&uri, provider_v2).await;
    drain_ripple(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        !after.contains("boom"),
        "the dependent's stale binding-resolved classification must clear on the retarget save; got {after:?}"
    );
}

/// #267 end-to-end, facade-alias kind, first-save edge: a `config/app.php` alias
/// retarget on the FIRST save of a session — when the registration baseline is
/// still empty, so the diff sees only the NEW target — must still re-resolve the
/// OLD target's dependent sites. This works ONLY because global-alias facade
/// sites record the `alias:<token>` attempt key resolved-or-not (#267): the old
/// target FQCN is never in the diff, so `alias:cache` is the sole key that can
/// reach the stale site.
#[tokio::test]
async fn provider_body_alias_first_save_retarget_converges_dependent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // A provider binds the `cache` accessor to a concrete carrying a `boom`
    // macro. The default `Cache` facade alias → `…\Facades\Cache` → accessor
    // `cache` → this concrete, so the consumer resolves through the alias.
    let provider_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        \App\Services\CacheImpl::macro('boom', fn () => 'x');
        $this->app->singleton('cache', \App\Services\CacheImpl::class);
    }
}
"#;
    let provider = write_file(root, "app/Providers/AppServiceProvider.php", provider_src);
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .unwrap();
    backend
        .salsa
        .register_service_provider_source(
            provider.clone(),
            provider_src.to_string(),
            2,
            root.to_path_buf(),
        )
        .await
        .unwrap();
    // Drive the live binding registry: `cache` → CacheImpl, so the default
    // `Cache` facade's accessor resolves to it.
    backend
        .salsa
        .register_service_provider_registry(
            HashMap::new(),
            HashMap::new(),
            singleton_registry("cache", "App\\Services\\CacheImpl"),
        )
        .await
        .unwrap();

    // Global-namespace consumer using the BARE `Cache` token — the global-alias
    // path, so it records `alias:cache` (a `use`-import would not).
    let consumer_src = r#"<?php
class CacheUser {
    public function go(): string { return Cache::boom(); }
}
"#;
    let consumer = write_file(root, "app/CacheUser.php", consumer_src);
    seed(&backend, &consumer, consumer_src).await;
    assert!(
        member_names(&backend, &consumer).await.contains("boom"),
        "seed: the consumer's macro call must classify while `Cache` aliases the default facade",
    );

    // FIRST save of config/app.php retargets `Cache` to a facade with no
    // resolvable accessor — baseline is empty, so the diff carries only the new
    // target. `alias:cache` (recorded resolved-or-not at the consumer) is the
    // only key that can clear the stale classification.
    let config_v1 = "<?php\nreturn [\n    'aliases' => [\n        'Cache' => App\\Facades\\Nothing::class,\n    ],\n];\n";
    let config = write_file(root, "config/app.php", config_v1);
    let uri = Url::from_file_path(&config).unwrap();
    backend.refresh_magic_on_save(&uri, config_v1).await;
    drain_ripple(&backend).await;

    let after = member_names(&backend, &consumer).await;
    assert!(
        !after.contains("boom"),
        "the old alias target's stale classification must clear on the first-save retarget; got {after:?}"
    );
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

// === M4: on-open vendor lazy indexing =======================================

/// A vendor consumer reading `$post->headline` off a typed `Post` param —
/// the same resolution shape as [`CONSUMER`], but under `vendor/`, so the
/// eager build never resolves it as a usage site.
const VENDOR_CONSUMER: &str = r#"<?php
namespace Acme\Blog;
use App\Models\Post;
class PostPresenter {
    public function present(Post $post) {
        return $post->headline;
    }
}
"#;

/// 0-based (line, column) of a cursor inside `headline` in [`VENDOR_CONSUMER`].
const VENDOR_USAGE: (u32, u32) = (5, 23);
/// 0-based (line, column) of a cursor inside `headline` in [`CONSUMER`].
const APP_USAGE: (u32, u32) = (5, 24);

/// Drive the **real** `did_open` handler for `path` with `src` as the buffer.
async fn open(backend: &LaravelLanguageServer, path: &Path, src: &str) {
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "php".to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;
}

/// Seed the shared app-side fixture — a `Post` model declaring the `headline`
/// accessor plus an app consumer reading it — both eagerly indexed the way
/// the build/save paths would. Returns `(post, consumer)` paths.
async fn seed_app_fixture(backend: &LaravelLanguageServer, root: &Path) -> (PathBuf, PathBuf) {
    write_file(root, "composer.json", COMPOSER);
    let post_src = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_src);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(backend, &post, &post_src).await;
    seed(backend, &consumer, CONSUMER).await;
    (post, consumer)
}

#[tokio::test]
async fn opening_a_vendor_file_indexes_its_own_usages() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    let (_post, consumer) = seed_app_fixture(&backend, root).await;

    let vendor = write_file(
        root,
        "vendor/acme/blog/src/PostPresenter.php",
        VENDOR_CONSUMER,
    );
    // Mirror the eager build's end state: the vendor file is registered in
    // Salsa (patterns/literals exist) but its usages were never resolved into
    // the reverse index — vendor is skipped as a usage site.
    backend
        .salsa
        .update_file(vendor.clone(), 1, VENDOR_CONSUMER.to_string())
        .await
        .unwrap();

    // THE GAP: before the open, the reference set omits the vendor file's own
    // occurrence — from a cursor inside the vendor file and from the app
    // usage site alike.
    let from_vendor = backend
        .salsa
        .find_member_references(vendor.clone(), VENDOR_USAGE.0, VENDOR_USAGE.1)
        .await
        .unwrap();
    assert!(
        from_vendor.iter().all(|r| r.file_path != vendor),
        "pre-open: no vendor self-reference; got {from_vendor:?}"
    );
    let from_app = backend
        .salsa
        .find_member_references(consumer.clone(), APP_USAGE.0, APP_USAGE.1)
        .await
        .unwrap();
    assert!(
        from_app.iter().any(|r| r.file_path == consumer),
        "sanity: the app usage itself is indexed; got {from_app:?}"
    );
    assert!(
        from_app.iter().all(|r| r.file_path != vendor),
        "pre-open: app-site references omit the vendor occurrence"
    );
    assert!(
        member_names(&backend, &vendor).await.is_empty(),
        "pre-open: the vendor file holds no magic entries"
    );

    open(&backend, &vendor, VENDOR_CONSUMER).await;

    assert!(
        member_names(&backend, &vendor).await.contains("headline"),
        "post-open: the vendor file's own usage is resolved into the index"
    );
    let from_app = backend
        .salsa
        .find_member_references(consumer.clone(), APP_USAGE.0, APP_USAGE.1)
        .await
        .unwrap();
    assert!(
        from_app
            .iter()
            .any(|r| r.file_path == vendor && r.line == VENDOR_USAGE.0),
        "post-open: the vendor file's own occurrence line joins the reference set; got {from_app:?}"
    );
    assert_eq!(
        backend.vendor_open_magic_lru.lock().unwrap().len(),
        1,
        "the on-open path records the vendor file in the session LRU"
    );
}

#[tokio::test]
async fn reopening_a_vendor_file_replaces_not_duplicates() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    seed_app_fixture(&backend, root).await;

    let vendor = write_file(
        root,
        "vendor/acme/blog/src/PostPresenter.php",
        VENDOR_CONSUMER,
    );
    open(&backend, &vendor, VENDOR_CONSUMER).await;
    let entry_count =
        |members: &HashMap<PathBuf, Vec<_>>| members.get(&vendor).map(|e| e.len()).unwrap_or(0);
    let first = entry_count(&backend.salsa.export_magic_members().await.unwrap());
    assert!(first > 0, "first open indexes the vendor usage");

    // Re-open (references multibuffer, tab churn): the actor's remove-then-
    // insert must REPLACE the file's entries, never append duplicates — and
    // the LRU `push` of the same key is a recency touch, not an eviction.
    open(&backend, &vendor, VENDOR_CONSUMER).await;
    let second = entry_count(&backend.salsa.export_magic_members().await.unwrap());
    assert_eq!(first, second, "re-open must replace, not duplicate");
    assert!(
        member_names(&backend, &vendor).await.contains("headline"),
        "re-open keeps the entries alive (same-key push must not evict)"
    );
}

#[tokio::test]
async fn vendor_open_lru_evicts_oldest_contributions_only() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    let (_post, consumer) = seed_app_fixture(&backend, root).await;

    let oldest = write_file(
        root,
        "vendor/acme/blog/src/PostPresenter.php",
        VENDOR_CONSUMER,
    );
    open(&backend, &oldest, VENDOR_CONSUMER).await;
    assert!(
        member_names(&backend, &oldest).await.contains("headline"),
        "seed: the oldest vendor open is indexed"
    );
    assert!(magic_deps_has(&backend, &oldest), "seed: and records deps");

    // Fill the LRU to capacity with trivial vendor opens (they occupy slots
    // regardless of whether resolution produced entries)…
    for i in 0..crate::VENDOR_OPEN_MAGIC_LRU_CAP - 1 {
        let filler = write_file(root, &format!("vendor/acme/blog/src/F{i}.php"), "<?php\n");
        open(&backend, &filler, "<?php\n").await;
    }
    // …then one more real vendor open pushes past the cap: the oldest evicts.
    let newest_src = VENDOR_CONSUMER.replace("PostPresenter", "PostSummary");
    let newest = write_file(root, "vendor/acme/blog/src/PostSummary.php", &newest_src);
    open(&backend, &newest, &newest_src).await;

    assert!(
        member_names(&backend, &oldest).await.is_empty(),
        "overflow evicts the OLDEST opened vendor file's entries"
    );
    assert!(
        !magic_deps_has(&backend, &oldest),
        "…and purges its recorded receiver deps"
    );
    assert!(
        member_names(&backend, &newest).await.contains("headline"),
        "a recently opened vendor file's entries survive"
    );
    assert!(
        member_names(&backend, &consumer).await.contains("headline"),
        "eager app-file entries are never LRU-tracked, so never evicted"
    );
}

#[tokio::test]
async fn app_file_open_does_not_trigger_vendor_refresh() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    // The model exists and is seeded, so resolution WOULD succeed if the
    // on-open refresh (wrongly) ran for app files.
    let post_src = post_with_accessors(&["Headline"]);
    let post = write_file(root, "app/Models/Post.php", &post_src);
    seed(&backend, &post, &post_src).await;

    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    open(&backend, &consumer, CONSUMER).await;

    assert!(
        member_names(&backend, &consumer).await.is_empty(),
        "did_open must not magic-index app files — the eager build owns those"
    );
    assert_eq!(
        backend.vendor_open_magic_lru.lock().unwrap().len(),
        0,
        "an app open must not occupy a vendor-LRU slot"
    );
}
