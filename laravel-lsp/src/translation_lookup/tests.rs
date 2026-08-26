use super::*;
use crate::salsa_impl::{LaravelDatabase, ResolvedTranslationData, TranslationCache};
use std::fs;
use tempfile::TempDir;

/// Drives the **real** production resolution path — [`TranslationCache`] over a
/// bare [`LaravelDatabase`] — rather than a test-only reimplementation. This is
/// the same code hover, go-to-definition and diagnostics run; the LSP actor
/// adds only channel plumbing on top of it, so a guarantee proven here (a
/// containment refusal, a precedence order) is a guarantee about production.
///
/// Each `Resolver::default()` is a cold cache, which is what most of these
/// tests want. Warmth across calls is covered separately, in
/// `translation_salsa_cache.rs`.
#[derive(Default)]
struct Resolver {
    db: LaravelDatabase,
    cache: TranslationCache,
}

impl Resolver {
    /// Resolve a key, returning the value and the catalogue it came from.
    fn resolve(
        &mut self,
        root: &Path,
        key: &str,
        locale: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
    ) -> Option<ResolvedTranslationData> {
        self.cache
            .resolve(&mut self.db, root, key, locale, vendor_map)
    }

    /// Resolve a key to its value alone.
    fn value(&mut self, root: &Path, key: &str, locale: &str) -> Option<String> {
        self.resolve(root, key, locale, None).map(|r| r.value)
    }

    /// Every locale that could define `key`, with no APP_LOCALE — the
    /// discovery cases below are about *which* locales are found, not their
    /// ordering.
    fn locales(
        &mut self,
        root: &Path,
        key: &str,
        vendor_map: Option<&HashMap<String, PathBuf>>,
    ) -> Vec<String> {
        self.cache
            .locales(&mut self.db, root, key, vendor_map, None)
    }

    /// Every locale that could define `key`, ordered against an explicit
    /// APP_LOCALE.
    ///
    /// Passed directly rather than written into a `.env`: the cache takes the
    /// locale as an argument, and the actor is what reads it out of the Salsa
    /// env cache. Writing a `.env` here would prove nothing about ordering —
    /// these assertions would pass on a cache that ignored APP_LOCALE
    /// entirely. The `.env` -> ordering wiring is proven end to end in
    /// `tests/translation_salsa_cache.rs` instead.
    fn locales_led_by(&mut self, root: &Path, key: &str, app_locale: &str) -> Vec<String> {
        self.cache
            .locales(&mut self.db, root, key, None, Some(app_locale))
    }
}

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

    let got = Resolver::default().value(project.path(), "validation.required", "en");
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
        Resolver::default()
            .value(project.path(), "auth.failed", "en")
            .as_deref(),
        Some("'These credentials do not match.'")
    );
    assert_eq!(
        Resolver::default()
            .value(project.path(), "auth.throttle.message", "en")
            .as_deref(),
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
        Resolver::default()
            .value(project.path(), "Welcome to our app", "en")
            .as_deref(),
        Some("'Welcome to our app'")
    );
}

#[test]
fn returns_none_for_missing_dotted_key() {
    let project = fake_project_with_lang();
    let validation = project.path().join("lang/en/validation.php");
    fs::write(&validation, "<?php\nreturn ['present' => 'x'];\n").unwrap();

    assert_eq!(
        Resolver::default().value(project.path(), "validation.missing", "en"),
        None
    );
}

#[test]
fn returns_none_for_missing_json_key() {
    let project = fake_project_with_lang();
    let json = project.path().join("lang/en.json");
    fs::write(&json, r#"{"Present": "Present"}"#).unwrap();

    assert_eq!(
        Resolver::default().value(project.path(), "Missing entry", "en"),
        None
    );
}

#[test]
fn returns_none_when_file_does_not_exist() {
    let project = fake_project_with_lang();
    // No files written.
    assert_eq!(
        Resolver::default().value(project.path(), "validation.required", "en"),
        None
    );
    assert_eq!(
        Resolver::default().value(project.path(), "Free-form text", "en"),
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

    let got = Resolver::default().value(
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

    let resolved = Resolver::default()
        .resolve(project.path(), "livewire::validation.required", "en", None)
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
        Resolver::default().value(project.path(), "filament-tables::table.actions.label", "en"),
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

    let resolved = Resolver::default()
        .resolve(
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

    let resolved = Resolver::default()
        .resolve(
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
        Resolver::default().value(project.path(), "validation", "en"),
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
        Resolver::default()
            .value(project.path(), "validation.required", "en")
            .as_deref(),
        Some("'English'")
    );
    assert_eq!(
        Resolver::default()
            .value(project.path(), "validation.required", "fr")
            .as_deref(),
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

    let resolved = Resolver::default().resolve(
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

/// A sibling tree outside any project root, holding a `{locale}/{file}.php`
/// whose value must never surface. Returns the temp dir that owns it.
fn secret_tree_outside_any_root() -> TempDir {
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(outside.path().join("en")).unwrap();
    fs::write(
        outside.path().join("en").join("invoice.php"),
        "<?php\nreturn ['total' => 'LEAKED'];\n",
    )
    .unwrap();
    outside
}

#[test]
fn absolute_namespace_in_the_published_path_is_refused() {
    // `namespace` is the `vendor::` prefix lifted verbatim out of parsed source
    // — including from an indexed `vendor/**.php` file, so a compromised
    // dependency can choose it. An *absolute* namespace wins outright, because
    // `Path::join` discards everything to its left: the published path
    // `lang/vendor/{namespace}/en/invoice.php` collapses to
    // `{namespace}/en/invoice.php`, straight out of the tree.
    let outside = secret_tree_outside_any_root();
    let project = fake_project_with_lang();
    let key = format!("{}::invoice.total", outside.path().display());

    // The fixture is only worth anything if the unguarded path really would
    // land on the secret file — pin that, so this can never rot into a test
    // that passes because the join went nowhere.
    assert!(
        project
            .path()
            .join("lang")
            .join("vendor")
            .join(outside.path())
            .join("en")
            .join("invoice.php")
            .exists(),
        "fixture wiring: the published path must actually reach the secret file"
    );

    assert!(
        Resolver::default()
            .resolve(project.path(), &key, "en", None)
            .is_none(),
        "an absolute namespace must never escape the project root"
    );
}

#[test]
fn traversing_namespace_in_the_published_path_is_refused() {
    // The relative shape of the same hole: `../../../{sibling}` walks out of
    // `lang/vendor/` into a tree beside the project root.
    let outside = secret_tree_outside_any_root();
    let project = fake_project_with_lang();
    // `read_to_string` resolves `..` through real directories only, so the
    // published prefix has to exist for the traversal to be live at all.
    fs::create_dir_all(project.path().join("lang").join("vendor")).unwrap();

    let sibling = outside.path().file_name().unwrap().to_str().unwrap();
    let namespace = format!("../../../{sibling}");
    let escaped = project
        .path()
        .join("lang")
        .join("vendor")
        .join(&namespace)
        .join("en")
        .join("invoice.php");
    assert!(
        escaped.exists(),
        "fixture wiring: the traversal must actually reach the secret file \
         (both temp dirs must share a parent)"
    );

    let key = format!("{namespace}::invoice.total");
    assert!(
        Resolver::default()
            .resolve(project.path(), &key, "en", None)
            .is_none(),
        "a traversing namespace must never escape the project root"
    );
}

/// Symlink-based containment fixtures. Creating a symlink on Windows needs
/// either Developer Mode or elevation, so `std::os::unix::fs::symlink` has no
/// portable counterpart worth reaching for here — the guard these pin is
/// platform-independent, only the fixture is not. Gated rather than dropped so
/// the Unix and macOS legs keep covering it (issue #292).
#[cfg(unix)]
#[test]
fn dotted_key_read_through_an_escaping_symlink_is_refused() {
    // Containment is canonical, not textual: `lang/en` is spelled inside the
    // root but resolves outside it. The dotted-key read site takes the same
    // guard as the namespaced ones.
    let outside = secret_tree_outside_any_root();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("lang")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("en"),
        project.path().join("lang").join("en"),
    )
    .unwrap();
    assert!(
        project.path().join("lang/en/invoice.php").exists(),
        "fixture wiring: the symlink must actually reach the secret file"
    );

    assert!(
        Resolver::default()
            .value(project.path(), "invoice.total", "en")
            .is_none(),
        "a lang directory symlinked out of the root must never be read"
    );
}

/// Symlink-based containment fixtures. Creating a symlink on Windows needs
/// either Developer Mode or elevation, so `std::os::unix::fs::symlink` has no
/// portable counterpart worth reaching for here — the guard these pin is
/// platform-independent, only the fixture is not. Gated rather than dropped so
/// the Unix and macOS legs keep covering it (issue #292).
#[cfg(unix)]
#[test]
fn text_key_read_through_an_escaping_symlink_is_refused() {
    // Same guard on the JSON catalogue read.
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("en.json"), r#"{"Welcome":"LEAKED"}"#).unwrap();

    let project = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path().join("lang")).unwrap();
    assert!(
        project.path().join("lang/en.json").exists(),
        "fixture wiring: the symlink must actually reach the secret catalogue"
    );

    assert!(
        Resolver::default()
            .value(project.path(), "Welcome", "en")
            .is_none(),
        "a lang directory symlinked out of the root must never be read"
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
        Resolver::default().locales(dir.path(), "messages.welcome", None),
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
        Resolver::default().locales(dir.path(), "Welcome to our app", None),
        vec!["de", "en"]
    );
}

#[test]
fn available_locales_excludes_the_vendor_directory() {
    let dir = root_with_locales(&["en", "de"]);
    fs::create_dir_all(dir.path().join("lang/vendor/somepkg/en")).unwrap();

    let locales = Resolver::default().locales(dir.path(), "messages.welcome", None);
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
        Resolver::default().locales(dir.path(), "messages.welcome", None),
        vec!["en"]
    );
}

#[test]
fn available_locales_falls_back_when_the_lang_directory_is_empty() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("lang")).unwrap();
    // Exists, but holds no locale subdirectories and no .json catalogues.
    assert_eq!(
        Resolver::default().locales(dir.path(), "messages.welcome", None),
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
        Resolver::default().locales(dir.path(), "shop::messages.title", Some(&map)),
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
        Resolver::default().locales(dir.path(), "shop::messages.title", Some(&map)),
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
        Resolver::default().locales(dir.path(), "shop::messages.title", Some(&map)),
        vec!["de"],
        "an out-of-root vendor dir must contribute nothing"
    );
}

#[test]
fn available_locales_refuses_an_escaping_published_namespace() {
    // The enumeration twin of `absolute_namespace_in_the_published_path_is_refused`:
    // the published dir is `lang/vendor/{namespace}`, and an absolute namespace
    // collapses that join to the namespace itself. Unguarded, `read_dir` would
    // list a directory outside the project and render its subdirectory names as
    // this key's locales.
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(outside.path().join("ja")).unwrap();
    fs::create_dir_all(outside.path().join("ko")).unwrap();

    let project = fake_project_with_lang();
    let key = format!("{}::messages.title", outside.path().display());

    let locales = Resolver::default().locales(project.path(), &key, None);
    assert_eq!(
        locales,
        vec!["en"],
        "an escaping published namespace must contribute no locales, leaving \
         only the default-locale fallback"
    );
}

// --- APP_LOCALE ordering ----------------------------------------------------

/// `fr` is deliberately NOT alphabetically first among the discovered
/// locales — an implementation that ignored APP_LOCALE entirely, or that only
/// sorted, would still pass a fixture whose APP_LOCALE happened to sort first.
#[test]
fn available_locales_leads_with_app_locale_then_alphabetical() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);

    assert_eq!(
        Resolver::default().locales_led_by(dir.path(), "messages.welcome", "fr"),
        vec!["fr", "de", "en", "es"],
        "APP_LOCALE leads; the remainder stays alphabetical"
    );
}

#[test]
fn available_locales_is_alphabetical_when_app_locale_is_unset() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);

    assert_eq!(
        Resolver::default().locales(dir.path(), "messages.welcome", None),
        vec!["de", "en", "es", "fr"]
    );
}

#[test]
fn available_locales_ignores_an_app_locale_no_directory_defines() {
    let dir = root_with_locales(&["en", "de", "fr", "es"]);

    assert_eq!(
        Resolver::default().locales_led_by(dir.path(), "messages.welcome", "ja"),
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
        Resolver::default().locales(dir.path(), "contract.title", None),
        vec!["de"],
        "discovery must see resources/lang"
    );
    let resolved = Resolver::default()
        .resolve(dir.path(), "contract.title", "de", None)
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

    assert_eq!(
        Resolver::default().locales(dir.path(), "Welcome", None),
        vec!["de"]
    );
    let resolved = Resolver::default()
        .resolve(dir.path(), "Welcome", "de", None)
        .expect("JSON catalogue under resources/lang must resolve");
    assert_eq!(resolved.value, "'Willkommen'");
}
