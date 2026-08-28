//! Cross-provider `Livewire::addNamespace` merge precedence (#297 review).
//!
//! `load_livewire_config` folds every app and module provider's
//! registrations into one map. On a prefix conflict the LAST registration
//! wins — modules in ascending `modules.paths` precedence first, app
//! providers last — the same rule the translation-namespace merge follows
//! and the one `docs/configuration.md` documents. The completion fixtures
//! elsewhere build `LivewireConfig` directly and bypass this merge, so it
//! needs its own coverage.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::LspService;

async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.module_path_patterns.write().await = vec!["app/*/*".to_string()];
    backend
}

/// A module registering `prefix` for its own `app/Livewire` directory.
fn registering_module(root: &Path, parent: &str, name: &str, prefix: &str) -> PathBuf {
    let module = root.join(format!("app/{parent}/{name}"));
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("app/Livewire")).unwrap();
    fs::write(
        module.join("composer.json"),
        format!(
            r#"{{
    "autoload": {{ "psr-4": {{ "App\\{parent}\\{name}\\": "app/" }} }},
    "extra": {{ "laravel": {{ "providers": [
        "App\\{parent}\\{name}\\Providers\\Registrar"
    ] }} }}
}}"#
        ),
    )
    .unwrap();
    fs::write(
        module.join("app/Providers/Registrar.php"),
        format!(
            r#"<?php

namespace App\{parent}\{name}\Providers;

use Livewire\Livewire;

class Registrar
{{
    public function boot(): void
    {{
        Livewire::addNamespace('{prefix}', 'App\\{parent}\\{name}\\Livewire', __DIR__.'/../Livewire');
    }}
}}
"#
        ),
    )
    .unwrap();
    module
}

async fn resolved_class_namespace(
    backend: &LaravelLanguageServer,
    root: &Path,
    prefix: &str,
) -> Option<String> {
    let config = backend.load_livewire_config(root).await;
    config
        .class_namespaces
        .get(prefix)
        .map(|reg| reg.class_namespace.clone())
}

#[tokio::test]
async fn later_module_livewire_namespace_wins_over_an_earlier_one() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    registering_module(root, "Legal", "Alpha", "shared-ui");
    registering_module(root, "Legal", "Beta", "shared-ui");

    let backend = backend_for(root).await;
    assert_eq!(
        resolved_class_namespace(&backend, root, "shared-ui")
            .await
            .as_deref(),
        Some("App\\Legal\\Beta\\Livewire"),
        "the later module (higher modules.paths precedence) wins"
    );
}

#[tokio::test]
async fn app_provider_livewire_namespace_overrides_a_module() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    registering_module(root, "Legal", "ContractManagement", "shared-ui");
    fs::create_dir_all(root.join("app/Providers")).unwrap();
    fs::create_dir_all(root.join("app/Livewire")).unwrap();
    fs::write(
        root.join("app/Providers/AppServiceProvider.php"),
        r#"<?php

namespace App\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace('shared-ui', 'App\\Livewire', __DIR__.'/../Livewire');
    }
}
"#,
    )
    .unwrap();

    let backend = backend_for(root).await;
    assert_eq!(
        resolved_class_namespace(&backend, root, "shared-ui")
            .await
            .as_deref(),
        Some("App\\Livewire"),
        "the app boots last, so its registration wins over any module's"
    );
}

#[tokio::test]
async fn class_path_escaping_the_project_is_not_registered() {
    // A provider-source-derived path is discovered data: a registration
    // pointing outside the project must not enter the config at all, so
    // neither the completion walk nor `try_namespaced_class` can reach it.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let module = registering_module(&root, "Legal", "Escape", "escape-ui");
    fs::create_dir_all(tmp.path().join("outside")).unwrap();
    fs::write(
        module.join("app/Providers/Registrar.php"),
        r#"<?php

namespace App\Legal\Escape\Providers;

use Livewire\Livewire;

class Registrar
{
    public function boot(): void
    {
        Livewire::addNamespace('escape-ui', 'App\\Legal\\Escape\\Livewire', __DIR__.'/../../../../outside');
    }
}
"#,
    )
    .unwrap();

    let backend = backend_for(&root).await;
    assert!(
        resolved_class_namespace(&backend, &root, "escape-ui")
            .await
            .is_none(),
        "an out-of-root class path yields no registration"
    );
}
