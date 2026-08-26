//! Namespaced translation keys (`package::file.key`) in the diagnostic's
//! existence check. The check delegates to `translation_lookup` with the
//! vendor-package map — the same machinery hover uses — so unpublished
//! package translations (e.g. `filament-tables::table.…`) stop false-flagging
//! while genuinely missing keys still do.

use crate::LaravelLanguageServer;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tower_lsp::LspService;

/// A backend whose Salsa actor backs the translation cache. `root` is passed
/// to `check_translation_file` explicitly, so no other state needs priming.
fn backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A project whose vendor package ships `lang/en/table.php` with one key,
/// plus the vendor map the translation scan would have produced for it.
fn project_with_vendor_translations() -> (TempDir, PathBuf, HashMap<String, PathBuf>) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let lang_dir = root.join("vendor/filament/tables/resources/lang");
    fs::create_dir_all(lang_dir.join("en")).unwrap();
    fs::write(
        lang_dir.join("en/table.php"),
        "<?php return ['grouping' => ['label' => 'Group']];",
    )
    .unwrap();

    let mut vendor_map = HashMap::new();
    vendor_map.insert("filament-tables".to_string(), lang_dir);
    (dir, root, vendor_map)
}

#[tokio::test]
async fn namespaced_key_resolves_through_vendor_map() {
    let (_dir, root, vendor_map) = project_with_vendor_translations();

    let check = backend()
        .check_translation_file(
            &root,
            "filament-tables::table.grouping.label",
            Some(Arc::new(vendor_map)),
        )
        .await;
    assert!(
        check.exists,
        "an unpublished package translation must resolve via the vendor map"
    );
}

#[tokio::test]
async fn missing_namespaced_key_still_flags_with_vendor_path() {
    let (_dir, root, vendor_map) = project_with_vendor_translations();

    let check = backend()
        .check_translation_file(
            &root,
            "filament-tables::table.does.not.exist",
            Some(Arc::new(vendor_map)),
        )
        .await;
    assert!(!check.exists, "a genuinely missing key must still flag");
    // The diagnostic should point at the package's real lang file, not the
    // bogus `lang/en/filament-tables::table.php` guess it used to emit.
    let expected = check.expected_path.expect("expected path set");
    assert!(
        expected.ends_with("vendor/filament/tables/resources/lang/en/table.php"),
        "expected path must target the package lang dir: {expected:?}"
    );
    assert!(
        check.file_exists,
        "the file itself exists — only the key is missing"
    );
}

#[tokio::test]
async fn app_provider_load_translations_from_resolves_namespaced_key() {
    // Issue #248: an `AppServiceProvider` registering
    // `loadTranslationsFrom(lang_path('app'), 'app')` must make
    // `app::notification.title` resolve to `lang/app/en/notification.php` —
    // no false "translation not found" diagnostic.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // The registered lang files.
    let lang_en = root.join("lang/app/en");
    fs::create_dir_all(&lang_en).unwrap();
    fs::write(
        lang_en.join("notification.php"),
        "<?php return ['task_group_status_change' => ['title' => 'Status changed']];",
    )
    .unwrap();

    // The provider that registers them.
    let providers = root.join("app/Providers");
    fs::create_dir_all(&providers).unwrap();
    fs::write(
        providers.join("AppServiceProvider.php"),
        r#"<?php
namespace App\Providers;
class AppServiceProvider {
    public function boot(): void {
        $this->loadTranslationsFrom(lang_path('app'), 'app');
    }
}
"#,
    )
    .unwrap();

    // The merged map the LSP builds — here just the app-provider scan.
    let map = laravel_lsp::vendor_translations::scan_app_translation_namespaces(&root);
    // The dir exists, so the scan canonicalizes it (resolving macOS's
    // /var → /private/var symlink) — compare against the canonical form.
    assert_eq!(
        map.get("app"),
        Some(&root.join("lang/app").canonicalize().unwrap()),
        "app scan must register the 'app' namespace at lang/app"
    );

    let check = backend()
        .check_translation_file(
            &root,
            "app::notification.task_group_status_change.title",
            Some(Arc::new(map)),
        )
        .await;
    assert!(
        check.exists,
        "the app-registered translation must resolve via the merged map"
    );
    let expected = check.expected_path.expect("expected path set");
    assert!(
        expected.ends_with("lang/app/en/notification.php"),
        "expected path must target the app-registered lang file: {expected:?}"
    );
}

#[tokio::test]
async fn namespaced_key_without_vendor_map_expects_published_path() {
    let (_dir, root, _vendor_map) = project_with_vendor_translations();

    let check = backend()
        .check_translation_file(&root, "unknown-pkg::messages.hi", None)
        .await;
    assert!(!check.exists);
    let expected = check.expected_path.expect("expected path set");
    assert!(
        expected.ends_with("lang/vendor/unknown-pkg/en/messages.php"),
        "without a vendor-map hit the published location is the expectation: {expected:?}"
    );
}
