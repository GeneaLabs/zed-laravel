//! Module providers registering view namespaces (issue #297).
//!
//! A `loadViewsFrom($path, $namespace)` call in a module service provider —
//! discovered through the module composer.json's `extra.laravel.providers`,
//! never by filename convention — must register a `ns::` view namespace.
//! The fixture namespace is reachable through NO other mechanism: the view
//! directory sits outside every configured view path and outside `vendor/`,
//! so resolution succeeding proves the module-provider path did it.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
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

/// Three modules, all claiming one key, listed in `modules.paths` as
/// Alpha, Gamma, Beta. The winner is therefore **Beta** — and Beta is
/// neither the lexicographically first nor the last of the three, so
/// neither the old lexicographic provider sort nor a "just reverse the
/// sort" shortcut can produce it. Only a real `modules.paths` rank lookup
/// picks Beta.
const DISCRIMINATING_ORDER: [&str; 3] = ["Alpha", "Gamma", "Beta"];

fn discriminating_patterns() -> Vec<String> {
    DISCRIMINATING_ORDER
        .iter()
        .map(|name| format!("app/Legal/{name}"))
        .collect()
}

#[tokio::test]
async fn later_module_view_namespace_wins_over_an_earlier_one() {
    // Three modules claiming one namespace: the one listed LAST in
    // `modules.paths` wins, matching the documented rule and the
    // translation-namespace merge. The previous two-module fixture used a
    // single `app/*/*` glob, which made lexicographic provider order and
    // configured order coincide — it passed under either rule and so
    // certified the wrong one (#354 item 3).
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    for name in DISCRIMINATING_ORDER {
        registering_module(root, "Legal", name, "shared-ns");
    }
    let winner = root.join("app/Legal/Beta");

    let backend = backend_with_patterns(root, discriminating_patterns()).await;
    let dir = view_namespace_dir_named(&backend, root, "shared-ns")
        .await
        .expect("shared-ns registered");
    assert_eq!(
        dir.canonicalize().unwrap(),
        winner.join("resources/views").canonicalize().unwrap(),
        "the last module in modules.paths order wins — not the last by path sort"
    );
}

// ---- one tie-break rule across all five registries (#354 items 2 and 4) ----

/// A module registering the SAME key in all five provider-registration
/// registries the config merge builds, so one fixture can prove they resolve
/// a collision identically. The class-backed component's target file is
/// written where `App\` → `app/` resolution looks for it.
fn module_registering_everything(root: &Path, name: &str) -> PathBuf {
    let module = root.join(format!("app/Legal/{name}"));
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("resources/views/components")).unwrap();
    fs::create_dir_all(module.join("View/Components")).unwrap();
    fs::write(module.join("View/Components/Card.php"), "<?php class Card {}").unwrap();
    fs::write(
        module.join("composer.json"),
        format!(
            r#"{{
    "autoload": {{ "psr-4": {{ "App\\Legal\\{name}\\": "app/" }} }},
    "extra": {{ "laravel": {{ "providers": [
        "App\\Legal\\{name}\\Providers\\Registrar"
    ] }} }}
}}"#
        ),
    )
    .unwrap();
    fs::write(
        module.join("app/Providers/Registrar.php"),
        format!(
            r#"<?php

namespace App\Legal\{name}\Providers;

use Illuminate\Support\Facades\Blade;

class Registrar
{{
    public function boot(): void
    {{
        $this->loadViewsFrom(__DIR__.'/../../resources/views', 'shared-ns');
        Blade::componentNamespace('App\\Legal\\{name}\\View\\Components', 'shared-ns');
        Blade::anonymousComponentPath(__DIR__.'/../../resources/views/components', 'shared-ns');
        Blade::anonymousComponentNamespace('components.{name}', 'shared-ns');
        Blade::component('shared-tag', \App\Legal\{name}\View\Components\Card::class);
    }}
}}
"#
        ),
    )
    .unwrap();
    module
}

async fn config_for(backend: &LaravelLanguageServer, root: &Path) -> LaravelConfigData {
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("salsa actor accepts config root");
    backend
        .register_service_provider_files_with_salsa(root)
        .await;
    backend
        .salsa
        .get_laravel_config()
        .await
        .ok()
        .flatten()
        .expect("salsa config")
}

/// The five registries, each reduced to the name of the module that won
/// `shared-ns` / `shared-tag`. `None` means the key never registered.
fn winners(config: &LaravelConfigData) -> Vec<(&'static str, Option<String>)> {
    fn module_of(path: &Path) -> Option<String> {
        path.components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .windows(2)
            .find(|w| w[0] == "Legal")
            .map(|w| w[1].clone())
    }
    vec![
        (
            "view_namespaces",
            config.view_namespaces.get("shared-ns").and_then(|p| module_of(p)),
        ),
        (
            "component_namespaces",
            config
                .component_namespaces
                .get("shared-ns")
                // The extractor stores the literal PHP text, so the
                // separators are still escaped (`App\\Legal\\Beta\\…`).
                .and_then(|ns| {
                    ns.split('\\')
                        .filter(|seg| !seg.is_empty())
                        .nth(2)
                        .map(str::to_string)
                }),
        ),
        (
            "anonymous_component_paths",
            config
                .anonymous_component_paths
                .get("shared-ns")
                .and_then(|p| module_of(p)),
        ),
        (
            "anonymous_component_namespaces",
            config
                .anonymous_component_namespaces
                .get("shared-ns")
                .and_then(|d| d.rsplit('/').next().map(str::to_string)),
        ),
        (
            "class_component_files",
            config
                .class_component_files
                .get("shared-tag")
                .and_then(|p| module_of(p)),
        ),
    ]
}

#[tokio::test]
async fn every_registry_breaks_an_equal_priority_tie_the_same_way() {
    // Item 4: four registries used to run three different tie-break rules —
    // last-wins, first-wins, and priority ignored entirely. One collision,
    // constructed once, must now resolve to the same module in all five.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    for name in DISCRIMINATING_ORDER {
        module_registering_everything(root, name);
    }

    let backend = backend_with_patterns(root, discriminating_patterns()).await;
    let config = config_for(&backend, root).await;

    for (registry, winner) in winners(&config) {
        assert_eq!(
            winner.as_deref(),
            Some("Beta"),
            "{registry}: the last module in modules.paths order wins, like every other registry"
        );
    }
}

#[tokio::test]
async fn an_app_registration_beats_a_module_in_every_registry() {
    // Item 2: the anonymous-component maps ignored priority entirely, so a
    // module's registration beat the app's whenever the module provider
    // sorted first — which it always does under `app/Legal/…` vs
    // `app/Providers/…`. docs/configuration.md promises the opposite.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    module_registering_everything(root, "Alpha");

    fs::create_dir_all(root.join("app/Providers")).unwrap();
    fs::create_dir_all(root.join("resources/views/components")).unwrap();
    fs::create_dir_all(root.join("app/Legal/AppOwned/View/Components")).unwrap();
    fs::write(
        root.join("app/Legal/AppOwned/View/Components/Card.php"),
        "<?php class Card {}",
    )
    .unwrap();
    fs::write(
        root.join("app/Providers/AppServiceProvider.php"),
        r#"<?php

namespace App\Providers;

use Illuminate\Support\Facades\Blade;

class AppServiceProvider
{
    public function boot(): void
    {
        $this->loadViewsFrom(__DIR__.'/../../app/Legal/AppOwned/resources/views', 'shared-ns');
        Blade::componentNamespace('App\\Legal\\AppOwned\\View\\Components', 'shared-ns');
        Blade::anonymousComponentPath(__DIR__.'/../../app/Legal/AppOwned/components', 'shared-ns');
        Blade::anonymousComponentNamespace('components.AppOwned', 'shared-ns');
        Blade::component('shared-tag', \App\Legal\AppOwned\View\Components\Card::class);
    }
}
"#,
    )
    .unwrap();

    // Only Alpha is a module; `app/Legal/AppOwned` is plain app code the
    // app provider points at, so the app tier is the one under test.
    let backend = backend_with_patterns(root, vec!["app/Legal/Alpha".to_string()]).await;
    let config = config_for(&backend, root).await;

    for (registry, winner) in winners(&config) {
        assert_eq!(
            winner.as_deref(),
            Some("AppOwned"),
            "{registry}: the app boots last, so its registration wins over the module's"
        );
    }
}
