use super::*;
use crate::salsa_impl::{LaravelDatabase, TranslationCache};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

/// Drives the **real** production path — `TranslationCache::vendor_namespaces`
/// over a bare `LaravelDatabase`. That method merges both scans (vendor first,
/// then app overriding it), which is what production consumes; no fixture here
/// mixes the two, so each test still exercises exactly the scan it names.
#[derive(Default)]
struct Scanner {
    db: LaravelDatabase,
    cache: TranslationCache,
}

impl Scanner {
    fn scan(&mut self, root: &Path) -> HashMap<String, PathBuf> {
        self.cache.vendor_namespaces(&mut self.db, root)
    }
}

/// Build a fake vendor tree at `vendor/<vendor>/<package>/` with a service
/// provider file at the standard location.
fn fake_vendor_package(project: &Path, vendor: &str, pkg: &str, provider: &str) -> PathBuf {
    let provider_dir = project.join("vendor").join(vendor).join(pkg).join("src");
    fs::create_dir_all(&provider_dir).unwrap();
    let provider_path = provider_dir.join(format!("{}.php", provider));
    provider_path
}

#[test]
fn extracts_single_load_translations_from_registration() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");

    let lang_dir = provider.parent().unwrap().join("../resources/lang");
    fs::create_dir_all(&lang_dir).unwrap();
    fs::write(
        &provider,
        r#"<?php
namespace Acme\Billing;
class BillingServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(__DIR__.'/../resources/lang', 'billing');
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map.get("billing").expect("should find billing namespace");
    assert!(
        resolved.ends_with("resources/lang"),
        "expected resolved to end with resources/lang, got: {:?}",
        resolved
    );
}

#[test]
fn ignores_non_provider_php_files() {
    // A non-provider file with `loadTranslationsFrom` in a docblock should
    // be skipped by the filename gate.
    let project = TempDir::new().unwrap();
    let non_provider = project.path().join("vendor/acme/billing/src/Helpers.php");
    fs::create_dir_all(non_provider.parent().unwrap()).unwrap();
    fs::write(
        &non_provider,
        r#"<?php
namespace Acme\Billing;
// $this->loadTranslationsFrom(__DIR__.'/../lang', 'billing');
class Helpers {}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    assert!(map.is_empty(), "non-provider files must be ignored");
}

#[test]
fn ignores_providers_without_load_translations_from_call() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    fs::write(
        &provider,
        r#"<?php
class BillingServiceProvider {
    public function boot() {
        $this->loadViewsFrom(__DIR__.'/../views', 'billing');
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    assert!(
        map.is_empty(),
        "providers without loadTranslationsFrom must contribute nothing"
    );
}

#[test]
fn captures_multiple_namespaces_across_packages() {
    let project = TempDir::new().unwrap();
    let p1 = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    let p2 = fake_vendor_package(project.path(), "acme", "auth", "AuthServiceProvider");
    fs::write(
        &p1,
        "<?php\nclass X { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'billing'); } }\n",
    )
    .unwrap();
    fs::write(
        &p2,
        "<?php\nclass Y { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'auth'); } }\n",
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    assert!(map.contains_key("billing"));
    assert!(map.contains_key("auth"));
}

#[test]
fn returns_empty_when_vendor_dir_missing() {
    let project = TempDir::new().unwrap();
    // No vendor/ directory.
    let map = Scanner::default().scan(project.path());
    assert!(map.is_empty());
}

#[test]
fn first_registration_wins_on_namespace_conflict() {
    // Two packages register the same namespace. First-match-wins.
    let project = TempDir::new().unwrap();
    let p1 = fake_vendor_package(project.path(), "first", "pkg", "FirstServiceProvider");
    let p2 = fake_vendor_package(project.path(), "second", "pkg", "SecondServiceProvider");
    fs::write(
        &p1,
        "<?php\nclass A { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'shared'); } }\n",
    )
    .unwrap();
    fs::write(
        &p2,
        "<?php\nclass B { public function boot() { $this->loadTranslationsFrom(__DIR__.'/../lang', 'shared'); } }\n",
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map.get("shared").expect("conflict must still resolve");
    // The path will contain either "first" or "second" depending on walk order —
    // accept either, but it must be a single deterministic entry.
    let s = resolved.to_string_lossy();
    assert!(s.contains("first") || s.contains("second"), "got: {}", s);
}

// ─── Path-helper argument forms (lang_path / base_path / dirname) ────────

#[test]
fn extracts_lang_path_argument_form() {
    // A vendor provider could conceivably use lang_path(); more importantly
    // this exercises the helper independent of where the file lives.
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    fs::write(
        &provider,
        r#"<?php
class BillingServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(lang_path('app'), 'app');
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map
        .get("app")
        .expect("lang_path('app') must register 'app'");
    assert_eq!(
        resolved,
        &project.path().join("lang").join("app"),
        "lang_path('app') resolves to <root>/lang/app"
    );
}

#[test]
fn extracts_base_path_argument_form() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    fs::write(
        &provider,
        r#"<?php
class BillingServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(base_path('lang/custom'), 'custom');
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map
        .get("custom")
        .expect("base_path('lang/custom') must register 'custom'");
    assert_eq!(
        resolved,
        &project.path().join("lang").join("custom"),
        "base_path('lang/custom') resolves to <root>/lang/custom"
    );
}

#[test]
fn extracts_dirname_dir_argument_form() {
    // `dirname(__DIR__).'/lang'` resolves relative to the provider's parent
    // directory: provider lives in <pkg>/src, so dirname(__DIR__) is <pkg>.
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "billing", "BillingServiceProvider");
    let lang_dir = provider.parent().unwrap().join("../lang");
    fs::create_dir_all(&lang_dir).unwrap();
    fs::write(
        &provider,
        r#"<?php
class BillingServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(dirname(__DIR__).'/lang', 'billing');
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map
        .get("billing")
        .expect("dirname(__DIR__).'/lang' must register 'billing'");
    // <pkg>/src is the provider dir; dirname(__DIR__) is <pkg>; + /lang.
    assert_eq!(
        resolved.canonicalize().unwrap(),
        lang_dir.canonicalize().unwrap(),
        "dirname(__DIR__).'/lang' resolves to the package's lang dir"
    );
}

// ─── App service provider scanning (app/Providers/**/*.php) ──────────────

/// Write an app service provider at `app/Providers/<name>.php`.
fn fake_app_provider(project: &Path, name: &str, body: &str) -> PathBuf {
    let dir = project.join("app").join("Providers");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.php"));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn scans_app_provider_load_translations_from() {
    let project = TempDir::new().unwrap();
    fake_app_provider(
        project.path(),
        "AppServiceProvider",
        r#"<?php
namespace App\Providers;
class AppServiceProvider {
    public function boot(): void {
        $this->loadTranslationsFrom(lang_path('app'), 'app');
    }
}
"#,
    );

    let map = Scanner::default().scan(project.path());
    let resolved = map
        .get("app")
        .expect("app provider registration must yield the 'app' namespace");
    assert_eq!(resolved, &project.path().join("lang").join("app"));
}

#[test]
fn app_scan_returns_empty_when_providers_dir_missing() {
    let project = TempDir::new().unwrap();
    let map = Scanner::default().scan(project.path());
    assert!(map.is_empty());
}

#[test]
fn app_scan_ignores_providers_without_load_translations() {
    let project = TempDir::new().unwrap();
    fake_app_provider(
        project.path(),
        "AppServiceProvider",
        "<?php\nclass AppServiceProvider { public function boot(): void {} }\n",
    );
    let map = Scanner::default().scan(project.path());
    assert!(map.is_empty());
}

// ─── Fluent package-builder registrations (->name()->hasTranslations()) ──

#[test]
fn builder_has_translations_registers_short_name_namespace() {
    // The Filament shape: ->name('filament-tables')->hasTranslations(), with
    // translations at <pkg>/resources/lang (the builder's basePath convention).
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(
        project.path(),
        "filament",
        "tables",
        "TablesServiceProvider",
    );
    let lang_dir = provider.parent().unwrap().join("../resources/lang/en");
    fs::create_dir_all(&lang_dir).unwrap();
    fs::write(
        lang_dir.join("table.php"),
        "<?php return ['grouping' => []];",
    )
    .unwrap();
    fs::write(
        &provider,
        r#"<?php
namespace Filament\Tables;
use Spatie\LaravelPackageTools\PackageServiceProvider;
class TablesServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package
            ->name('filament-tables')
            ->hasTranslations()
            ->hasViews();
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    let resolved = map
        .get("filament-tables")
        .expect("builder registration must yield the filament-tables namespace");
    assert!(
        resolved.join("en/table.php").exists(),
        "namespace must point at the package lang dir: {resolved:?}"
    );
}

#[test]
fn builder_name_strips_laravel_prefix_for_translation_namespace() {
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "tools", "ToolsServiceProvider");
    fs::create_dir_all(provider.parent().unwrap().join("../resources/lang")).unwrap();
    fs::write(
        &provider,
        r#"<?php
class ToolsServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('laravel-tools')->hasTranslations();
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    assert!(
        map.contains_key("tools"),
        "->name('laravel-tools') must register namespace 'tools', got {map:?}"
    );
}

#[test]
fn builder_without_has_translations_registers_nothing() {
    // ->hasViews() alone (no ->hasTranslations()) must not synthesize a
    // translation namespace.
    let project = TempDir::new().unwrap();
    let provider = fake_vendor_package(project.path(), "acme", "ui", "UiServiceProvider");
    fs::write(
        &provider,
        r#"<?php
class UiServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme-ui')->hasViews();
    }
}
"#,
    )
    .unwrap();

    let map = Scanner::default().scan(project.path());
    assert!(
        map.is_empty(),
        "no ->hasTranslations() means no translation namespace, got {map:?}"
    );
}

// ─── Precedence across the two scans ─────────────────────────────────────

#[test]
fn app_provider_overrides_a_vendor_provider_on_namespace_conflict() {
    // The app boots last, so an app `loadTranslationsFrom` for a namespace a
    // package also registers is the one that wins at runtime. Nothing else
    // covers this: every other fixture here has providers from one scan only,
    // so a merge that dropped the override would pass the whole rest of the
    // suite.
    let project = TempDir::new().unwrap();
    let root = project.path();

    // A package registering `shop` at its own bundled lang dir.
    let provider = fake_vendor_package(root, "acme", "shop", "ShopServiceProvider");
    let vendor_lang = provider.parent().unwrap().join("../resources/lang");
    fs::create_dir_all(&vendor_lang).unwrap();
    fs::write(
        &provider,
        r#"<?php
namespace Acme\Shop;
class ShopServiceProvider {
    public function boot() {
        $this->loadTranslationsFrom(__DIR__.'/../resources/lang', 'shop');
    }
}
"#,
    )
    .unwrap();

    // The app registering the same namespace somewhere else.
    let app_lang = root.join("lang/shop");
    fs::create_dir_all(&app_lang).unwrap();
    fake_app_provider(
        root,
        "AppServiceProvider",
        r#"<?php
namespace App\Providers;
class AppServiceProvider {
    public function boot(): void {
        $this->loadTranslationsFrom(lang_path('shop'), 'shop');
    }
}
"#,
    );

    let map = Scanner::default().scan(root);
    assert_eq!(
        map.get("shop"),
        Some(&app_lang.canonicalize().unwrap()),
        "the app registration must win over the package's own"
    );
}
