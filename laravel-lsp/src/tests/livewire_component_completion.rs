//! `<livewire:` completion (`get_all_livewire_components`) for projects
//! whose components live ONLY under a registered class namespace
//! (`Livewire::addNamespace` / `modules.livewireRegistrars`) — no
//! conventional `app/Livewire` or `app/Http/Livewire` directory at all.
//!
//! The function used to resolve the conventional path first and
//! `return Vec::new()` immediately when none existed, before ever reaching
//! the registered-namespace walk further down — so a project like the
//! fixture (`common-google::guest-test-page`, registered via
//! `loadLivewireComponentsFrom(__DIR__.'/../Livewire', 'prefix')`, no root
//! `app/Livewire`) got zero `<livewire:` completions. The conventional-path
//! section is now skipped rather than short-circuiting the whole function;
//! the namespace walk always runs.
//!
//! These tests drive the private async method directly on a server built
//! through the `tower_lsp::LspService` harness (same pattern as
//! `livewire_tag_navigation_containment.rs`), priming `root_path`,
//! `cached_config`, and `cached_livewire` so the walk runs purely against a
//! tempdir.

use crate::LaravelLanguageServer;
use laravel_lsp::livewire_config::LivewireConfig;
use laravel_lsp::livewire_namespaces::LivewireClassNamespace;
use laravel_lsp::livewire_version::LivewireVersion;
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::LspService;

/// A minimal Laravel config with no conventional Livewire path configured —
/// the shape `get_all_livewire_components` sees when `livewire:publish
/// --config` was never run and there's no root `app/Livewire` either.
fn laravel_config_without_conventional_path(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![],
        component_paths: vec![],
        livewire_path: None,
        has_livewire: true,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A Laravel config with an explicit conventional Livewire path.
fn laravel_config_with_livewire_path(root: &Path, livewire_path: &str) -> LaravelConfigData {
    LaravelConfigData {
        livewire_path: Some(PathBuf::from(livewire_path)),
        ..laravel_config_without_conventional_path(root)
    }
}

/// A Livewire config registering `ns` as a class namespace pointing at
/// `class_path` (the `loadLivewireComponentsFrom(...)` / `addNamespace(...)`
/// shape).
fn livewire_config_with_class_namespace(
    root: &Path,
    ns: &str,
    class_path: &Path,
) -> LivewireConfig {
    let mut config = LivewireConfig::defaults(root);
    config.class_namespaces.insert(
        ns.to_string(),
        LivewireClassNamespace {
            class_namespace: format!("App\\{ns}"),
            class_path: class_path.to_path_buf(),
        },
    );
    config
}

async fn seed(
    root: &Path,
    livewire: LivewireConfig,
    config: LaravelConfigData,
) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.cached_livewire.write().await =
        Some((root.to_path_buf(), livewire, LivewireVersion::V4));
    *backend.cached_config.write().await = Some(std::sync::Arc::new(config));
    backend
}

#[tokio::test]
async fn namespace_only_project_still_completes() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // No root app/Livewire on disk — matches the real fixture project.
    let class_path = root.join("app/Common/Google/Livewire");
    fs::create_dir_all(&class_path).unwrap();
    fs::write(
        class_path.join("GuestTestPage.php"),
        "<?php\nnamespace App\\Common\\Google\\Livewire;\nclass GuestTestPage {}\n",
    )
    .unwrap();

    let livewire = livewire_config_with_class_namespace(&root, "common-google", &class_path);
    let config = laravel_config_without_conventional_path(&root);
    let backend = seed(&root, livewire, config).await;

    let completions = backend.get_all_livewire_components().await;
    let names: Vec<&str> = completions.iter().map(|c| c.name.as_str()).collect();

    assert!(
        names.contains(&"common-google::guest-test-page"),
        "namespace-only project should still yield the registered component, got {names:?}"
    );
}

#[tokio::test]
async fn conventional_path_and_namespace_both_contribute() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let conventional = root.join("app/Livewire");
    fs::create_dir_all(&conventional).unwrap();
    fs::write(
        conventional.join("Counter.php"),
        "<?php\nclass Counter {}\n",
    )
    .unwrap();

    let class_path = root.join("app/Common/Google/Livewire");
    fs::create_dir_all(&class_path).unwrap();
    fs::write(
        class_path.join("GuestTestPage.php"),
        "<?php\nclass GuestTestPage {}\n",
    )
    .unwrap();

    let livewire = livewire_config_with_class_namespace(&root, "common-google", &class_path);
    let config = laravel_config_with_livewire_path(&root, "app/Livewire");
    let backend = seed(&root, livewire, config).await;

    let completions = backend.get_all_livewire_components().await;
    let names: Vec<&str> = completions.iter().map(|c| c.name.as_str()).collect();

    assert!(names.contains(&"counter"), "got {names:?}");
    assert!(
        names.contains(&"common-google::guest-test-page"),
        "got {names:?}"
    );
}

#[tokio::test]
async fn neither_conventional_nor_namespace_yields_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let livewire = LivewireConfig::defaults(&root);
    let config = laravel_config_without_conventional_path(&root);
    let backend = seed(&root, livewire, config).await;

    let completions = backend.get_all_livewire_components().await;
    let names: Vec<&str> = completions.iter().map(|c| c.name.as_str()).collect();
    assert!(completions.is_empty(), "got {names:?}");
}
