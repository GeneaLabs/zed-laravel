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
    backend_with_patterns(root, vec!["app/*/*".to_string()]).await
}

async fn backend_with_patterns(root: &Path, patterns: Vec<String>) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.module_path_patterns.write().await = patterns;
    backend
}

/// A module registering `prefix` for its own `app/Livewire` directory.
fn registering_module(root: &Path, parent: &str, name: &str, prefix: &str) -> PathBuf {
    let module = root.join(format!("app/{parent}/{name}"));
    write_module(&module, parent, name, prefix, "__DIR__.'/../Livewire'");
    module
}

/// Write one module — composer manifest, provider, `app/Livewire` — into
/// `module`, wherever that directory lives. `class_path_expr` is the PHP
/// expression the provider passes as `Livewire::addNamespace`'s third
/// argument, so a test can aim a registration outside its own module.
fn write_module(
    module: &Path,
    parent: &str,
    name: &str,
    prefix: &str,
    class_path_expr: &str,
) -> PathBuf {
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
        Livewire::addNamespace('{prefix}', 'App\\{parent}\\{name}\\Livewire', {class_path_expr});
    }}
}}
"#
        ),
    )
    .unwrap();
    module.to_path_buf()
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

// ---- containment: the owning module, not the project root (#354 item 1) ----

#[cfg(unix)]
#[tokio::test]
async fn symlinked_path_repo_module_keeps_its_livewire_registration() {
    // A composer path repository symlinked into the module tree canonicalizes
    // OUTSIDE the project root, so gating the canonical class path against
    // the root dropped the registration silently. `expand_module_dirs`
    // deliberately admits this layout; the Livewire gate has to agree.
    //
    // The in-root module is the control: same builder, same provider shape,
    // the ONLY difference is that one module dir is a symlink. It resolving
    // while the symlinked one does not is what isolates the gate as the cause.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("proj");

    let real = tmp.path().join("packages/ui-kit");
    write_module(&real, "Common", "UiKit", "ui-kit", "__DIR__.'/../Livewire'");
    fs::create_dir_all(root.join("app/Common")).unwrap();
    std::os::unix::fs::symlink(&real, root.join("app/Common/UiKit")).unwrap();

    write_module(
        &root.join("app/Common/InRoot"),
        "Common",
        "InRoot",
        "in-root",
        "__DIR__.'/../Livewire'",
    );

    let backend = backend_with_patterns(&root, vec!["app/Common/*".to_string()]).await;
    assert_eq!(
        resolved_class_namespace(&backend, &root, "in-root")
            .await
            .as_deref(),
        Some("App\\Common\\InRoot\\Livewire"),
        "control: an in-root module registers"
    );
    assert_eq!(
        resolved_class_namespace(&backend, &root, "ui-kit")
            .await
            .as_deref(),
        Some("App\\Common\\UiKit\\Livewire"),
        "a symlinked composer path-repo module keeps its registration too"
    );
}

#[tokio::test]
async fn module_class_path_reaching_outside_its_own_module_is_dropped() {
    // The other half of gating against the owning module: a module provider
    // whose class path lands INSIDE the project root but outside its own
    // module is not registering its own components. Dropping it is what
    // proves ownership is genuinely resolved and consulted — a gate stubbed
    // to always fall back to the root would admit this.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    write_module(
        &root.join("app/Legal/Owner"),
        "Legal",
        "Owner",
        "own-ui",
        "__DIR__.'/../Livewire'",
    );
    write_module(
        &root.join("app/Legal/Reacher"),
        "Legal",
        "Reacher",
        "reach-ui",
        "__DIR__.'/../../../Owner/app/Livewire'",
    );

    let backend = backend_for(root).await;
    assert_eq!(
        resolved_class_namespace(&backend, root, "own-ui")
            .await
            .as_deref(),
        Some("App\\Legal\\Owner\\Livewire"),
        "control: a module registering its OWN directory still resolves"
    );
    assert!(
        resolved_class_namespace(&backend, root, "reach-ui")
            .await
            .is_none(),
        "a sibling module's directory is in-root but outside the owning module"
    );
}

#[tokio::test]
async fn app_provider_class_path_is_still_gated_by_the_project_root() {
    // An app provider has no owning module, so the gate falls back to the
    // root exactly as before — the escape case must not have been widened.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    fs::create_dir_all(root.join("app/Providers")).unwrap();
    fs::create_dir_all(root.join("app/Livewire")).unwrap();
    fs::create_dir_all(tmp.path().join("outside")).unwrap();
    fs::write(
        root.join("app/Providers/AppServiceProvider.php"),
        r#"<?php

namespace App\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace('app-ui', 'App\\Livewire', __DIR__.'/../Livewire');
        Livewire::addNamespace('escape-ui', 'App\\Escape', __DIR__.'/../../../outside');
    }
}
"#,
    )
    .unwrap();

    let backend = backend_for(&root).await;
    assert_eq!(
        resolved_class_namespace(&backend, &root, "app-ui")
            .await
            .as_deref(),
        Some("App\\Livewire"),
        "control: the app's own in-root registration resolves"
    );
    assert!(
        resolved_class_namespace(&backend, &root, "escape-ui")
            .await
            .is_none(),
        "an app provider reaching outside the root yields no registration"
    );
}
