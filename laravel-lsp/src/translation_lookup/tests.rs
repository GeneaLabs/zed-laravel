use super::*;
use std::fs;
use tempfile::TempDir;

/// Build a fake Laravel project root with a `lang/` directory ready for tests.
fn fake_project_with_lang() -> TempDir {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("lang");
    fs::create_dir_all(lang.join("en")).unwrap();
    dir
}

#[test]
fn resolves_dotted_key_from_php_file() {
    let project = fake_project_with_lang();
    let validation = project.path().join("lang/en/validation.php");
    fs::write(
        &validation,
        "<?php\nreturn [\n    'required' => 'The :attribute field is required.',\n];\n",
    )
    .unwrap();

    let got = resolve_translation(project.path(), "validation.required", "en");
    assert_eq!(got.as_deref(), Some("'The :attribute field is required.'"));
}

#[test]
fn resolves_nested_dotted_key_from_php_file() {
    let project = fake_project_with_lang();
    let auth = project.path().join("lang/en/auth.php");
    fs::write(
        &auth,
        "<?php\nreturn [\n    'failed' => 'These credentials do not match.',\n    'throttle' => [\n        'message' => 'Too many attempts.',\n    ],\n];\n",
    )
    .unwrap();

    assert_eq!(
        resolve_translation(project.path(), "auth.failed", "en").as_deref(),
        Some("'These credentials do not match.'")
    );
    assert_eq!(
        resolve_translation(project.path(), "auth.throttle.message", "en").as_deref(),
        Some("'Too many attempts.'")
    );
}

#[test]
fn resolves_text_key_from_json_file() {
    let project = fake_project_with_lang();
    let json = project.path().join("lang/en.json");
    fs::write(
        &json,
        r#"{
    "Welcome to our app": "Welcome to our app",
    "Sign in": "Sign in"
}
"#,
    )
    .unwrap();

    assert_eq!(
        resolve_translation(project.path(), "Welcome to our app", "en").as_deref(),
        Some("'Welcome to our app'")
    );
}

#[test]
fn returns_none_for_missing_dotted_key() {
    let project = fake_project_with_lang();
    let validation = project.path().join("lang/en/validation.php");
    fs::write(&validation, "<?php\nreturn ['present' => 'x'];\n").unwrap();

    assert_eq!(
        resolve_translation(project.path(), "validation.missing", "en"),
        None
    );
}

#[test]
fn returns_none_for_missing_json_key() {
    let project = fake_project_with_lang();
    let json = project.path().join("lang/en.json");
    fs::write(&json, r#"{"Present": "Present"}"#).unwrap();

    assert_eq!(
        resolve_translation(project.path(), "Missing entry", "en"),
        None
    );
}

#[test]
fn returns_none_when_file_does_not_exist() {
    let project = fake_project_with_lang();
    // No files written.
    assert_eq!(
        resolve_translation(project.path(), "validation.required", "en"),
        None
    );
    assert_eq!(
        resolve_translation(project.path(), "Free-form text", "en"),
        None
    );
}

#[test]
fn dotted_key_classifier_distinguishes_shapes() {
    assert!(is_dotted_key("validation.required"));
    assert!(is_dotted_key("auth.throttle.message"));
    // A user-facing sentence with a period — has spaces, so treated as a text key.
    assert!(!is_dotted_key("Welcome to our app."));
    // No dot, no spaces — treat as text key (degenerate case).
    assert!(!is_dotted_key("single"));
}

#[test]
fn namespace_splitter_separates_vendor_from_rest() {
    assert_eq!(
        split_namespace("filament-tables::table.actions.label"),
        Some(("filament-tables", "table.actions.label"))
    );
    assert_eq!(split_namespace("validation.required"), None);
    assert_eq!(split_namespace("plain text"), None);
}

#[test]
fn resolves_namespaced_translation_from_published_path() {
    let project = fake_project_with_lang();
    let vendor_dir = project.path().join("lang/vendor/filament-tables/en");
    fs::create_dir_all(&vendor_dir).unwrap();
    let table = vendor_dir.join("table.php");
    fs::write(
        &table,
        "<?php\nreturn [\n    'actions' => [\n        'filter' => [\n            'label' => 'Filter',\n        ],\n    ],\n];\n",
    )
    .unwrap();

    let got = resolve_translation(
        project.path(),
        "filament-tables::table.actions.filter.label",
        "en",
    );
    assert_eq!(got.as_deref(), Some("'Filter'"));
}

#[test]
fn resolves_namespaced_translation_with_source_path() {
    let project = fake_project_with_lang();
    let vendor_dir = project.path().join("lang/vendor/livewire/en");
    fs::create_dir_all(&vendor_dir).unwrap();
    fs::write(
        vendor_dir.join("validation.php"),
        "<?php\nreturn ['required' => 'This field is required.'];\n",
    )
    .unwrap();

    let resolved =
        resolve_translation_detailed(project.path(), "livewire::validation.required", "en", None)
            .expect("namespaced lookup should hit");

    assert_eq!(resolved.value, "'This field is required.'");
    assert!(
        resolved
            .source_file
            .ends_with("lang/vendor/livewire/en/validation.php"),
        "got: {:?}",
        resolved.source_file
    );
}

#[test]
fn returns_none_for_missing_namespaced_file() {
    let project = fake_project_with_lang();
    assert_eq!(
        resolve_translation(project.path(), "filament-tables::table.actions.label", "en"),
        None
    );
}

#[test]
fn falls_back_to_unpublished_vendor_dir_when_published_path_missing() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let project = fake_project_with_lang();
    // No published translations at lang/vendor/<ns>. Instead, simulate an
    // unpublished package at vendor/<ns>/lang/en/<file>.php.
    let vendor_lang = project.path().join("vendor/acme/billing/resources/lang");
    let en_dir = vendor_lang.join("en");
    fs::create_dir_all(&en_dir).unwrap();
    fs::write(
        en_dir.join("invoice.php"),
        "<?php\nreturn ['total' => 'Total'];\n",
    )
    .unwrap();

    let mut vendor_map: HashMap<String, PathBuf> = HashMap::new();
    vendor_map.insert("billing".to_string(), vendor_lang.clone());

    let resolved = resolve_translation_detailed(
        project.path(),
        "billing::invoice.total",
        "en",
        Some(&vendor_map),
    )
    .expect("unpublished vendor fallback should resolve");
    assert_eq!(resolved.value, "'Total'");
    assert!(
        resolved
            .source_file
            .ends_with("vendor/acme/billing/resources/lang/en/invoice.php"),
        "got: {:?}",
        resolved.source_file
    );
}

#[test]
fn published_path_still_wins_over_vendor_map_when_both_exist() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let project = fake_project_with_lang();
    // Published value
    let published = project.path().join("lang/vendor/billing/en");
    fs::create_dir_all(&published).unwrap();
    fs::write(
        published.join("invoice.php"),
        "<?php\nreturn ['total' => 'Published total'];\n",
    )
    .unwrap();
    // Unpublished value with the same key but different string
    let vendor_lang = project.path().join("vendor/acme/billing/lang");
    let en_dir = vendor_lang.join("en");
    fs::create_dir_all(&en_dir).unwrap();
    fs::write(
        en_dir.join("invoice.php"),
        "<?php\nreturn ['total' => 'Vendor total'];\n",
    )
    .unwrap();

    let mut vendor_map: HashMap<String, PathBuf> = HashMap::new();
    vendor_map.insert("billing".to_string(), vendor_lang);

    let resolved = resolve_translation_detailed(
        project.path(),
        "billing::invoice.total",
        "en",
        Some(&vendor_map),
    )
    .expect("should resolve");
    // Published overrides — the user's choice when they ran
    // `php artisan vendor:publish` should take precedence.
    assert_eq!(resolved.value, "'Published total'");
}

#[test]
fn dotted_key_without_path_returns_none() {
    let project = fake_project_with_lang();
    // A bare file name like "validation" — no key segment after the dot.
    assert_eq!(
        resolve_translation(project.path(), "validation", "en"),
        None
    );
}

#[test]
fn respects_locale_argument() {
    let project = fake_project_with_lang();
    fs::create_dir_all(project.path().join("lang/fr")).unwrap();
    let en = project.path().join("lang/en/validation.php");
    let fr = project.path().join("lang/fr/validation.php");
    fs::write(&en, "<?php\nreturn ['required' => 'English'];\n").unwrap();
    fs::write(&fr, "<?php\nreturn ['required' => 'Français'];\n").unwrap();

    assert_eq!(
        resolve_translation(project.path(), "validation.required", "en").as_deref(),
        Some("'English'")
    );
    assert_eq!(
        resolve_translation(project.path(), "validation.required", "fr").as_deref(),
        Some("'Français'")
    );
}

#[test]
fn namespaced_dir_outside_root_is_refused() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    // A malicious `loadTranslationsFrom` argument could seed the vendor map
    // with a directory *outside* the project root (e.g. via
    // `base_path('../secrets')`). A real lang file lives there, so resolution
    // would succeed and leak its contents — except the containment guard
    // fail-closes the read.
    let outside = TempDir::new().unwrap();
    let secret_lang = outside.path().join("en");
    fs::create_dir_all(&secret_lang).unwrap();
    fs::write(
        secret_lang.join("invoice.php"),
        "<?php\nreturn ['total' => 'LEAKED'];\n",
    )
    .unwrap();

    let project = fake_project_with_lang();
    let mut vendor_map: HashMap<String, PathBuf> = HashMap::new();
    // Namespace points at a directory entirely outside the project root.
    vendor_map.insert("billing".to_string(), outside.path().to_path_buf());

    let resolved = resolve_translation_detailed(
        project.path(),
        "billing::invoice.total",
        "en",
        Some(&vendor_map),
    );
    assert!(
        resolved.is_none(),
        "an out-of-root namespace directory must never be read"
    );
}

// ---------------------------------------------------------------------------
// available_locales — the shared locale set (issue #288)
// ---------------------------------------------------------------------------

/// A root with the given locale subdirectories under `lang/`.
fn root_with_locales(locales: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for locale in locales {
        fs::create_dir_all(dir.path().join("lang").join(locale)).unwrap();
    }
    dir
}

#[test]
fn available_locales_enumerates_locale_directories() {
    let dir = root_with_locales(&["en", "de", "fr"]);
    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["de", "en", "fr"]
    );
}

#[test]
fn available_locales_extracts_json_catalogue_stems() {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("lang");
    fs::create_dir_all(&lang).unwrap();
    fs::write(lang.join("de.json"), "{}").unwrap();
    fs::write(lang.join("en.json"), "{}").unwrap();
    // A non-JSON file is not a locale.
    fs::write(lang.join("README.md"), "").unwrap();

    assert_eq!(
        available_locales(dir.path(), "Welcome to our app", None),
        vec!["de", "en"]
    );
}

#[test]
fn available_locales_excludes_the_vendor_directory() {
    let dir = root_with_locales(&["en", "de"]);
    fs::create_dir_all(dir.path().join("lang/vendor/somepkg/en")).unwrap();

    let locales = available_locales(dir.path(), "messages.welcome", None);
    assert!(
        !locales.contains(&"vendor".to_string()),
        "vendor is a namespace container, not a locale: {locales:?}"
    );
    assert_eq!(locales, vec!["de", "en"]);
}

#[test]
fn available_locales_falls_back_when_the_lang_directory_is_missing() {
    let dir = TempDir::new().unwrap();
    // No lang/ at all — read_dir errors on every candidate.
    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["en"]
    );
}

#[test]
fn available_locales_falls_back_when_the_lang_directory_is_empty() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("lang")).unwrap();
    // Exists, but holds no locale subdirectories and no .json catalogues.
    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["en"]
    );
}

#[test]
fn available_locales_unions_published_and_unpublished_vendor_dirs() {
    let dir = TempDir::new().unwrap();
    // Published override defines only `de`.
    fs::create_dir_all(dir.path().join("lang/vendor/shop/de")).unwrap();
    // The package's own (unpublished) lang dir defines only `fr`.
    let pkg_lang = dir.path().join("vendor/acme/shop/resources/lang");
    fs::create_dir_all(pkg_lang.join("fr")).unwrap();

    let mut map = HashMap::new();
    map.insert("shop".to_string(), pkg_lang);

    assert_eq!(
        available_locales(dir.path(), "shop::messages.title", Some(&map)),
        vec!["de", "fr"],
        "both directories contribute; neither is consulted in isolation"
    );
}

#[test]
fn available_locales_deduplicates_overlapping_vendor_dirs() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("lang/vendor/shop/de")).unwrap();
    fs::create_dir_all(dir.path().join("lang/vendor/shop/en")).unwrap();
    let pkg_lang = dir.path().join("vendor/acme/shop/resources/lang");
    fs::create_dir_all(pkg_lang.join("de")).unwrap();
    fs::create_dir_all(pkg_lang.join("en")).unwrap();

    let mut map = HashMap::new();
    map.insert("shop".to_string(), pkg_lang);

    assert_eq!(
        available_locales(dir.path(), "shop::messages.title", Some(&map)),
        vec!["de", "en"],
        "a locale in both directories is listed once, not doubled"
    );
}

/// The unpublished vendor dir is built from a `loadTranslationsFrom` argument
/// in untrusted source and can point anywhere. Enumerating it is a read, so it
/// takes the same fail-closed containment guard the namespaced *resolver*
/// already applies (issue #248) — an out-of-root directory must contribute no
/// locales, rather than having its subdirectory names rendered as this key's
/// locale list.
#[test]
fn available_locales_refuses_an_out_of_root_vendor_dir() {
    let dir = TempDir::new().unwrap();
    // The published override contributes `de`, so a non-empty result can't be
    // mistaken for the "no locales anywhere" fallback.
    fs::create_dir_all(dir.path().join("lang/vendor/shop/de")).unwrap();

    // A sibling tree entirely outside the project root, holding locales that
    // must never surface.
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(outside.path().join("ja")).unwrap();
    fs::create_dir_all(outside.path().join("ko")).unwrap();

    let mut map = HashMap::new();
    map.insert("shop".to_string(), outside.path().to_path_buf());

    assert_eq!(
        available_locales(dir.path(), "shop::messages.title", Some(&map)),
        vec!["de"],
        "an out-of-root vendor dir must contribute nothing"
    );
}

// --- APP_LOCALE ordering ----------------------------------------------------

/// `fr` is deliberately NOT alphabetically first among the discovered
/// locales — an implementation that ignored APP_LOCALE entirely, or that only
/// sorted, would still pass a fixture whose APP_LOCALE happened to sort first.
#[test]
fn available_locales_leads_with_app_locale_then_alphabetical() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);
    fs::write(dir.path().join(".env"), "APP_NAME=Test\nAPP_LOCALE=fr\n").unwrap();

    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["fr", "de", "en", "es"],
        "APP_LOCALE leads; the remainder stays alphabetical"
    );
}

#[test]
fn available_locales_is_alphabetical_when_app_locale_is_unset() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);
    fs::write(dir.path().join(".env"), "APP_NAME=Test\n").unwrap();

    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["de", "en", "es", "fr"]
    );
}

#[test]
fn available_locales_ignores_an_app_locale_no_directory_defines() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);
    fs::write(dir.path().join(".env"), "APP_LOCALE=ja\n").unwrap();

    assert_eq!(
        available_locales(dir.path(), "messages.welcome", None),
        vec!["de", "en", "es", "fr"],
        "an APP_LOCALE outside the discovered set must not panic or reorder"
    );
}

// --- resources/lang parity --------------------------------------------------

/// A Laravel-8-style project keeps translations under `resources/lang/`.
/// Discovery and resolution must agree there, or hover finds nothing while
/// diagnostics resolves happily — the divergence issue #288 closes.
#[test]
fn resources_lang_only_project_discovers_and_resolves() {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("resources/lang");
    fs::create_dir_all(lang.join("de")).unwrap();
    fs::write(
        lang.join("de/contract.php"),
        "<?php return ['title' => 'Vertrag'];",
    )
    .unwrap();

    assert_eq!(
        available_locales(dir.path(), "contract.title", None),
        vec!["de"],
        "discovery must see resources/lang"
    );
    let resolved = resolve_translation_detailed(dir.path(), "contract.title", "de", None)
        .expect("resolution must see resources/lang too");
    assert_eq!(resolved.value, "'Vertrag'");
    assert_eq!(resolved.source_file, lang.join("de/contract.php"));
}

#[test]
fn resources_lang_json_text_key_resolves() {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("resources/lang");
    fs::create_dir_all(&lang).unwrap();
    fs::write(lang.join("de.json"), r#"{"Welcome":"Willkommen"}"#).unwrap();

    assert_eq!(available_locales(dir.path(), "Welcome", None), vec!["de"]);
    let resolved = resolve_translation_detailed(dir.path(), "Welcome", "de", None)
        .expect("JSON catalogue under resources/lang must resolve");
    assert_eq!(resolved.value, "'Willkommen'");
}
