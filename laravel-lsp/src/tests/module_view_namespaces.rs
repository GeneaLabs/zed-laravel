//! Module providers registering view namespaces (issue #297).
//!
//! A `loadViewsFrom($path, $namespace)` call in a module service provider —
//! discovered through the module composer.json's `extra.laravel.providers`,
//! never by filename convention — must register a `ns::` view namespace.
//! The fixture namespace is reachable through NO other mechanism: the view
//! directory sits outside every configured view path and outside `vendor/`,
//! so resolution succeeding proves the module-provider path did it.

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

/// A module whose composer.json lists `provider_class`, with that provider's
/// file containing `provider_source`, plus a views directory holding one
/// template the namespace should expose.
fn module_fixture(root: &Path, provider_source: &str) -> PathBuf {
    let module = root.join("app/Legal/ContractManagement");
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("resources/views/pages")).unwrap();
    fs::write(
        module.join("resources/views/pages/edit.blade.php"),
        "<div>edit</div>",
    )
    .unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{
    "autoload": { "psr-4": { "App\\Legal\\ContractManagement\\": "app/" } },
    "extra": { "laravel": { "providers": [
        "App\\Legal\\ContractManagement\\Providers\\ContractRegistrar"
    ] } }
}"#,
    )
    .unwrap();
    fs::write(
        module.join("app/Providers/ContractRegistrar.php"),
        provider_source,
    )
    .unwrap();
    module
}

const REGISTERING_PROVIDER: &str = r#"<?php

namespace App\Legal\ContractManagement\Providers;

class ContractRegistrar
{
    public function boot(): void
    {
        $this->loadViewsFrom(__DIR__.'/../../resources/views', 'legal-contractmanagement');
    }
}
"#;

const SILENT_PROVIDER: &str = r#"<?php

namespace App\Legal\ContractManagement\Providers;

class ContractRegistrar
{
    public function boot(): void
    {
    }
}
"#;

async fn view_namespace_dir_named(
    backend: &LaravelLanguageServer,
    root: &Path,
    namespace: &str,
) -> Option<PathBuf> {
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("salsa actor accepts config root");
    backend
        .register_service_provider_files_with_salsa(root)
        .await;
    let config = backend
        .salsa
        .get_laravel_config()
        .await
        .ok()
        .flatten()
        .expect("salsa config");
    config.view_namespaces.get(namespace).cloned()
}

async fn view_namespace_dir(backend: &LaravelLanguageServer, root: &Path) -> Option<PathBuf> {
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("salsa actor accepts config root");
    backend
        .register_service_provider_files_with_salsa(root)
        .await;
    let config = backend
        .salsa
        .get_laravel_config()
        .await
        .ok()
        .flatten()
        .expect("salsa config");
    config
        .view_namespaces
        .get("legal-contractmanagement")
        .cloned()
}

#[tokio::test]
async fn module_provider_load_views_from_registers_the_namespace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = module_fixture(tmp.path(), REGISTERING_PROVIDER);
    let backend = backend_for(tmp.path()).await;

    let dir = view_namespace_dir(&backend, tmp.path())
        .await
        .expect("legal-contractmanagement:: registered via the module provider");
    assert_eq!(
        dir.canonicalize().unwrap(),
        module.join("resources/views").canonicalize().unwrap()
    );
}

#[tokio::test]
async fn namespace_disappears_when_the_load_views_from_call_is_removed() {
    // Negative control: the identical fixture minus the registration call
    // resolves nothing — proving the positive test isn't a coincidence of
    // some broader scan.
    let tmp = tempfile::TempDir::new().unwrap();
    module_fixture(tmp.path(), SILENT_PROVIDER);
    let backend = backend_for(tmp.path()).await;

    assert!(
        view_namespace_dir(&backend, tmp.path()).await.is_none(),
        "no loadViewsFrom call, no namespace"
    );
}

#[tokio::test]
async fn provider_absent_from_composer_manifest_is_not_indexed() {
    // The provider file exists with a conventional name and a registration
    // call — but the manifest doesn't list it, so it is not a booted
    // provider and must not be indexed.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = module_fixture(tmp.path(), SILENT_PROVIDER);
    fs::write(
        module.join("app/Providers/RogueServiceProvider.php"),
        REGISTERING_PROVIDER.replace("class ContractRegistrar", "class RogueServiceProvider"),
    )
    .unwrap();
    let backend = backend_for(tmp.path()).await;

    assert!(
        view_namespace_dir(&backend, tmp.path()).await.is_none(),
        "unlisted providers are not scanned"
    );
}

#[tokio::test]
async fn dynamic_arguments_skip_registration_without_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    module_fixture(
        tmp.path(),
        r#"<?php

namespace App\Legal\ContractManagement\Providers;

class ContractRegistrar
{
    public function boot(): void
    {
        $this->loadViewsFrom($this->viewPath(), $this->namespaceName());
    }
}
"#,
    );
    let backend = backend_for(tmp.path()).await;

    assert!(
        view_namespace_dir(&backend, tmp.path()).await.is_none(),
        "a non-literal path/namespace argument degrades to no registration"
    );
}

// ---- Blade::directive in a module provider ----------------------------------

#[test]
fn blade_directive_in_a_module_provider_is_discovered() {
    // Regression pin for the module-provider directive scan: a
    // `Blade::directive(...)` registered by a composer-listed module
    // provider shows up like an app-provider one — and only for LISTED
    // providers.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = tmp.path().join("app/Legal/ContractManagement");
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{
    "autoload": { "psr-4": { "App\\Legal\\ContractManagement\\": "app/" } },
    "extra": { "laravel": { "providers": [
        "App\\Legal\\ContractManagement\\Providers\\DirectiveProvider"
    ] } }
}"#,
    )
    .unwrap();
    fs::write(
        module.join("app/Providers/DirectiveProvider.php"),
        r#"<?php
class DirectiveProvider {
    public function boot(): void {
        Blade::directive('contractdate', fn ($expression) => $expression);
    }
}
"#,
    )
    .unwrap();

    let directives = crate::scan_custom_blade_directives(tmp.path(), std::slice::from_ref(&module));
    assert!(
        directives.iter().any(|d| d.name == "contractdate"),
        "module-provider directive discovered: {:?}",
        directives.iter().map(|d| &d.name).collect::<Vec<_>>()
    );

    let without_modules = crate::scan_custom_blade_directives(tmp.path(), &[]);
    assert!(
        !without_modules.iter().any(|d| d.name == "contractdate"),
        "negative control: without modules.paths nothing is scanned"
    );
}

// ---- view-namespace precedence (app > module, later module wins) ----------

/// A module at `app/{parent}/{name}` registering `namespace` for its own
/// views directory, holding one template at `pages/edit.blade.php`.
fn registering_module(root: &Path, parent: &str, name: &str, namespace: &str) -> PathBuf {
    let module = root.join(format!("app/{parent}/{name}"));
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("resources/views/pages")).unwrap();
    fs::write(
        module.join("resources/views/pages/edit.blade.php"),
        "<div/>",
    )
    .unwrap();
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

class Registrar
{{
    public function boot(): void
    {{
        $this->loadViewsFrom(__DIR__.'/../../resources/views', '{namespace}');
    }}
}}
"#
        ),
    )
    .unwrap();
    module
}

#[tokio::test]
async fn app_provider_view_namespace_overrides_a_module_registration() {
    // docs/configuration.md promises "an app/Providers registration
    // overrides modules" — the app boots last. The module's provider path
    // sorts BEFORE `app/Providers/…` lexicographically, so a first-wins
    // merge would have resolved this backwards.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    registering_module(root, "Legal", "ContractManagement", "shared-ns");

    let app_views = root.join("resources/views/app-owned");
    fs::create_dir_all(&app_views).unwrap();
    fs::create_dir_all(root.join("app/Providers")).unwrap();
    fs::write(
        root.join("app/Providers/AppServiceProvider.php"),
        r#"<?php

namespace App\Providers;

class AppServiceProvider
{
    public function boot(): void
    {
        $this->loadViewsFrom(__DIR__.'/../../resources/views/app-owned', 'shared-ns');
    }
}
"#,
    )
    .unwrap();

    let backend = backend_for(root).await;
    let dir = view_namespace_dir_named(&backend, root, "shared-ns")
        .await
        .expect("shared-ns registered");
    assert_eq!(
        dir.canonicalize().unwrap(),
        app_views.canonicalize().unwrap(),
        "the app registration wins over the module's"
    );
}

#[tokio::test]
async fn later_module_view_namespace_wins_over_an_earlier_one() {
    // Two modules claiming one namespace: the LAST-registered (higher
    // `modules.paths` precedence) wins, matching the documented rule and
    // the translation-namespace merge.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    registering_module(root, "Legal", "Alpha", "shared-ns");
    let beta = registering_module(root, "Legal", "Beta", "shared-ns");

    let backend = backend_for(root).await;
    let dir = view_namespace_dir_named(&backend, root, "shared-ns")
        .await
        .expect("shared-ns registered");
    assert_eq!(
        dir.canonicalize().unwrap(),
        beta.join("resources/views").canonicalize().unwrap(),
        "the later module wins"
    );
}
