//! Livewire must be detected when it arrives as a TRANSITIVE dependency.
//!
//! `has_livewire` was decided by `composer.json.contains("\"livewire/livewire\"")`.
//! `composer.json` lists only DIRECT requirements, but Livewire usually arrives
//! through something else: `livewire/flux`, `livewire/volt`,
//! `filament/filament` and `robsontenorio/mary` all depend on it, and Laravel's
//! own `livewire-starter-kit` — which this repo vendors as `test-project/` —
//! ships exactly that shape.
//!
//! Consequence: `has_livewire` false → `livewire_path` `None` → the
//! conventional `app/Livewire` scan skipped entirely. `<livewire:` completion
//! returned ZERO items on a project whose `app/Livewire` is full of components,
//! and the Livewire directory was never handed to the file watcher.
//!
//! Measured before the fix, driving the real binary against `test-project/`:
//! 0 completion items as shipped, 1 item with `"livewire/livewire"` added to
//! its `require` block, everything else identical.
//!
//! The codebase already knew: `salsa_impl.rs`'s v4 component-namespace loop
//! carries a comment saying it is "Not gated on `has_livewire`: that flag only
//! sees direct composer.json requires, while Livewire commonly arrives
//! transitively (Flux, Filament, MaryUI)". That was a local workaround for one
//! consumer; this fixes the flag itself.
//!
//! Every fixture here uses a `composer.json` that does NOT name Livewire.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::LspService;

/// A starter-kit-shaped `composer.json`: Livewire is nowhere in it, and
/// arrives through Flux and Volt.
const COMPOSER_JSON_TRANSITIVE: &str = r#"{
    "require": {
        "php": "^8.2",
        "laravel/framework": "^12.0",
        "livewire/flux": "^2.9.0",
        "livewire/volt": "^1.7.0"
    },
    "autoload": { "psr-4": { "App\\": "app/" } }
}"#;

/// The same, with Livewire required directly — the shape that already worked.
const COMPOSER_JSON_DIRECT: &str = r#"{
    "require": {
        "php": "^8.2",
        "laravel/framework": "^12.0",
        "livewire/livewire": "^3.0"
    },
    "autoload": { "psr-4": { "App\\": "app/" } }
}"#;

/// A `composer.lock` in which Livewire appears only because Flux pulled it in.
const COMPOSER_LOCK_WITH_LIVEWIRE: &str = r#"{
    "packages": [
        { "name": "livewire/flux", "version": "v2.9.1" },
        { "name": "livewire/livewire", "version": "v3.6.4" }
    ],
    "packages-dev": []
}"#;

/// A lock with no Livewire at all.
const COMPOSER_LOCK_WITHOUT_LIVEWIRE: &str = r#"{
    "packages": [
        { "name": "laravel/framework", "version": "v12.0.1" }
    ],
    "packages-dev": []
}"#;

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Lay down a project with one Livewire component, and whichever composer
/// files the caller wants.
fn seed(root: &Path, composer_json: &str, composer_lock: Option<&str>) {
    fs::create_dir_all(root.join("app/Livewire")).unwrap();
    fs::write(root.join("composer.json"), composer_json).unwrap();
    if let Some(lock) = composer_lock {
        fs::write(root.join("composer.lock"), lock).unwrap();
    }
    fs::write(
        root.join("app/Livewire/Counter.php"),
        "<?php\nnamespace App\\Livewire;\nuse Livewire\\Component;\nclass Counter extends Component {}\n",
    )
    .unwrap();
}

/// Register the project's config with the actor and read back the resolved
/// Laravel config.
async fn config_for(
    server: &LaravelLanguageServer,
    root: &Path,
) -> laravel_lsp::salsa_impl::LaravelConfigData {
    *server.root_path.write().await = Some(root.to_path_buf());
    server
        .salsa
        .register_config_files(
            root.to_path_buf(),
            fs::read_to_string(root.join("composer.json")).ok(),
            None,
            None,
            fs::read_to_string(root.join("composer.lock")).ok(),
        )
        .await
        .unwrap();
    server
        .salsa
        .get_laravel_config()
        .await
        .unwrap()
        .expect("the actor must return a config for a registered project")
}

// ---------------------------------------------------------------------------
// The defect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_transitive_livewire_is_detected() {
    // THE regression test. `composer.json` never mentions Livewire; only the
    // lock does, and only because Flux pulled it in.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITH_LIVEWIRE),
    );
    let server = test_server();

    let config = config_for(&server, root).await;

    assert!(
        config.has_livewire,
        "Livewire is installed via livewire/flux — reading composer.json alone \
         reported it absent, which emptied <livewire: completion"
    );
    assert_eq!(
        config.livewire_path,
        Some(root.join("app/Livewire")),
        "and the conventional component path must resolve, or the scan is \
         skipped and completion returns nothing"
    );
}

#[tokio::test]
async fn a_transitive_livewire_yields_component_completions() {
    // The user-visible half, through the same accessor the completion handler
    // uses. Asserting on `has_livewire` alone would not prove the components
    // actually surface.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITH_LIVEWIRE),
    );
    let server = test_server();
    let config = config_for(&server, root).await;
    *server.cached_config.write().await = Some(std::sync::Arc::new(config));

    let names: Vec<String> = server
        .cached_livewire_components()
        .await
        .iter()
        .map(|c| c.name.clone())
        .collect();

    assert_eq!(
        names,
        vec!["counter".to_string()],
        "the component under app/Livewire must be offered; before the fix this \
         list was empty on every starter-kit project"
    );
}

#[tokio::test]
async fn a_direct_livewire_requirement_still_works() {
    // No regression for the projects the old test DID serve.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_DIRECT,
        Some(COMPOSER_LOCK_WITH_LIVEWIRE),
    );
    let server = test_server();

    let config = config_for(&server, root).await;

    assert!(config.has_livewire);
    assert_eq!(config.livewire_path, Some(root.join("app/Livewire")));
}

#[tokio::test]
async fn a_project_without_livewire_is_still_reported_as_such() {
    // The discriminator. Without this, "always true" would pass every test
    // above — and a project with no Livewire would get a phantom component
    // path and phantom completions.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITHOUT_LIVEWIRE),
    );
    let server = test_server();

    let config = config_for(&server, root).await;

    assert!(
        !config.has_livewire,
        "a lock with no livewire/livewire means it is not installed"
    );
    assert_eq!(config.livewire_path, None);
}

// ---------------------------------------------------------------------------
// The absent-lock fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn without_a_lock_a_direct_requirement_is_still_detected() {
    // Fresh clone, before `composer install`. Falling back to the old
    // composer.json test is strictly better than assuming "not installed", and
    // preserves exactly the behaviour these projects had before.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root, COMPOSER_JSON_DIRECT, None);
    let server = test_server();

    let config = config_for(&server, root).await;

    assert!(
        config.has_livewire,
        "with no lock to read, a direct composer.json requirement must still count"
    );
}

#[tokio::test]
async fn without_a_lock_a_transitive_livewire_is_undetectable() {
    // Honest limit, pinned so it is a known behaviour rather than a surprise.
    // Before `composer install` there is no lock and nothing in `vendor/`, so
    // no evidence of a transitive dependency exists anywhere on disk. The
    // answer corrects itself the moment the lock appears.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root, COMPOSER_JSON_TRANSITIVE, None);
    let server = test_server();

    let config = config_for(&server, root).await;

    assert!(
        !config.has_livewire,
        "with no lock and no direct requirement there is no evidence to read"
    );
}

// ---------------------------------------------------------------------------
// The file watcher — the second consumer of `livewire_path`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_livewire_directory_is_watched_on_a_transitive_project() {
    // `build_registration` takes `config.livewire_path`, so a `None` path meant
    // the Livewire directory was never watched: external edits to components
    // (a `git pull`, a branch switch) never invalidated anything. Verified
    // rather than assumed.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITH_LIVEWIRE),
    );
    let server = test_server();
    let config = config_for(&server, root).await;

    let registration = laravel_lsp::file_watcher::build_registration(
        root,
        &config.view_paths,
        config.livewire_path.as_deref(),
        &[],
        &[],
    );
    let rendered = serde_json::to_string(&registration.register_options).unwrap();

    assert!(
        rendered.contains("app/Livewire"),
        "the Livewire directory must be registered with the watcher; got {rendered}"
    );
}

#[tokio::test]
async fn no_livewire_directory_is_watched_without_livewire() {
    // Discriminates the assertion above from one that passes on any
    // registration.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITHOUT_LIVEWIRE),
    );
    let server = test_server();
    let config = config_for(&server, root).await;

    let registration = laravel_lsp::file_watcher::build_registration(
        root,
        &config.view_paths,
        config.livewire_path.as_deref(),
        &[],
        &[],
    );
    let rendered = serde_json::to_string(&registration.register_options).unwrap();

    assert!(
        !rendered.contains("app/Livewire"),
        "a project without Livewire must not get a Livewire watcher; got {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Go-to-definition — checked, not assumed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn goto_definition_never_depended_on_the_livewire_flag() {
    // I reported goto as "possibly affected" and would not claim either way
    // without testing. It is NOT affected, and this pins why: goto resolves
    // through `resolve_livewire_component` → `get_cached_livewire`, which reads
    // `config/livewire.php` and the installed version directly and never
    // consults `has_livewire` or `livewire_path`.
    //
    // The stale `LaravelConfigData::resolve_livewire_path` — which DID gate on
    // `livewire_path` — has no production callers left; two comments in
    // `main.rs` refer to it as "the old" resolver.
    //
    // Driven on the worst case: a lock that says Livewire is NOT installed, so
    // `has_livewire` is false and `livewire_path` is `None`. Resolution must
    // still find the component on disk.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(
        root,
        COMPOSER_JSON_TRANSITIVE,
        Some(COMPOSER_LOCK_WITHOUT_LIVEWIRE),
    );
    let server = test_server();
    let config = config_for(&server, root).await;
    assert!(
        !config.has_livewire && config.livewire_path.is_none(),
        "fixture check — the flag must be false, or this proves nothing"
    );
    *server.cached_config.write().await = Some(std::sync::Arc::new(config));

    let resolved = server.resolve_livewire_primary_path("counter").await;

    assert_eq!(
        resolved,
        Some(root.join("app/Livewire/Counter.php")),
        "goto resolves independently of the Livewire flag, so the defect never \
         reached it"
    );
}
