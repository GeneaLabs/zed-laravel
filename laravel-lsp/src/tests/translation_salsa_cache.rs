//! Translation resolution through the Salsa cache (issue #293).
//!
//! Before this, `translation_lookup` read and parsed lang files with
//! `std::fs::read_to_string` on **every** call, uncached and uninvalidated.
//! Since #288 resolves a key against every locale a project defines, one hover
//! on a 25-locale project meant a `read_dir` plus up to 25 file reads —
//! repeated for go-to-definition and again for the diagnostic pass.
//!
//! Caching that is only half the job. A cache with no invalidation trades a
//! performance bug for a correctness one, so these tests come in two halves:
//!
//! 1. **Warmth** — a second resolution of the same key must not touch disk
//!    again, asserted against a real read counter rather than by inspection.
//! 2. **Invalidation** — an editor edit (`did_change`, no save), an external
//!    edit (`did_change_watched_files`, for both `.php` and `.json`
//!    catalogues), and a deletion must each be reflected by the next lookup.
//!
//! Everything drives the **real** `Backend` on the `LspService::new` harness
//! and the real LSP notification handlers, so what passes here is the path Zed
//! actually takes — not a direct poke at `salsa_impl` internals, which would
//! prove the cache works while saying nothing about whether it is wired up.

use crate::LaravelLanguageServer;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, FileChangeType, FileEvent,
    TextDocumentContentChangeEvent, Url, VersionedTextDocumentIdentifier,
};
use tower_lsp::{LanguageServer, LspService};

/// A backend with `root_path` primed and the Salsa debounce removed, so a
/// `did_change` can be awaited deterministically instead of slept on.
async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.auto_complete_debounce_ms.write().await = 0;
    backend
}

/// Write a file, creating parent directories. Returns its absolute path.
fn write(root: &Path, rel: &str, contents: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

/// Drive the real `did_change` handler and wait for the debounced Salsa update
/// it queues, so the assertion that follows cannot race the update.
async fn did_change(backend: &LaravelLanguageServer, path: &Path, text: &str, version: i32) {
    let uri = Url::from_file_path(path).unwrap();
    backend
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        })
        .await;
    if let Some(handle) = backend.pending_salsa_updates.write().await.remove(&uri) {
        let _ = handle.await;
    }
}

/// Drive the real watched-files handler with one synthesized event.
async fn watched_event(backend: &LaravelLanguageServer, path: &Path, typ: FileChangeType) {
    backend
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(path).unwrap(),
                typ,
            }],
        })
        .await;
}

/// How many times this backend's translation cache has touched disk. The
/// counter is per-cache, so concurrently-running tests cannot perturb it.
async fn disk_reads(backend: &LaravelLanguageServer) -> usize {
    backend.salsa.lang_disk_reads().await.unwrap()
}

/// Resolve a key in one locale, returning the bare value.
async fn value(
    backend: &LaravelLanguageServer,
    root: &Path,
    key: &str,
    locale: &str,
) -> Option<String> {
    backend
        .resolve_translation(root, key, locale, None)
        .await
        .map(|resolved| resolved.value)
}

// ---------------------------------------------------------------------------
// Warmth — the cache is warm ACROSS requests, not merely within one
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolving_the_same_key_twice_reads_disk_only_once() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "lang/de/validation.php",
        "<?php return ['required' => 'Pflichtfeld'];",
    );
    let backend = backend_for(&root).await;

    let before = disk_reads(&backend).await;
    let first = value(&backend, &root, "validation.required", "de").await;
    let after_first = disk_reads(&backend).await;
    let second = value(&backend, &root, "validation.required", "de").await;
    let after_second = disk_reads(&backend).await;

    assert_eq!(first.as_deref(), Some("'Pflichtfeld'"));
    assert_eq!(
        second, first,
        "a cached resolution must return the same value"
    );
    assert!(
        after_first > before,
        "the first resolution must actually touch disk, else this test proves nothing"
    );
    assert_eq!(
        after_second, after_first,
        "the second resolution must be served from Salsa, not re-read from disk"
    );
}

#[tokio::test]
async fn locale_discovery_enumerates_the_lang_directory_only_once() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for locale in ["de", "en", "fr"] {
        fs::create_dir_all(root.join("lang").join(locale)).unwrap();
    }
    let backend = backend_for(&root).await;

    let before = disk_reads(&backend).await;
    let first = backend
        .translation_locales(&root, "messages.welcome", None)
        .await;
    let after_first = disk_reads(&backend).await;
    let second = backend
        .translation_locales(&root, "messages.welcome", None)
        .await;
    let after_second = disk_reads(&backend).await;

    assert_eq!(first, vec!["de", "en", "fr"]);
    assert_eq!(second, first);
    assert!(
        after_first > before,
        "the first enumeration must actually run read_dir"
    );
    assert_eq!(
        after_second, after_first,
        "locale discovery must be cached across requests, not re-enumerated"
    );
}

#[tokio::test]
async fn a_key_missing_from_most_locales_does_not_re_probe_them() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Ten locales; only `de` defines the key. The other nine are the misses
    // that used to cost a failed read apiece on every single request.
    for locale in ["de", "en", "es", "fr", "it", "ja", "nl", "pl", "pt", "sv"] {
        fs::create_dir_all(root.join("lang").join(locale)).unwrap();
    }
    write(
        &root,
        "lang/de/contract.php",
        "<?php return ['title' => 'Vertrag'];",
    );
    let backend = backend_for(&root).await;
    let key = "contract.title";

    // Warm every locale the way a hover does.
    for locale in backend.translation_locales(&root, key, None).await {
        let _ = value(&backend, &root, key, &locale).await;
    }
    let before = disk_reads(&backend).await;
    for locale in backend.translation_locales(&root, key, None).await {
        let _ = value(&backend, &root, key, &locale).await;
    }

    assert_eq!(
        disk_reads(&backend).await,
        before,
        "a second full-locale sweep must touch disk zero times, misses included"
    );
}

// ---------------------------------------------------------------------------
// Invalidation — editor edits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn did_change_on_a_lang_buffer_is_reflected_without_a_save() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let file = write(
        &root,
        "lang/de/validation.php",
        "<?php return ['required' => 'Alt'];",
    );
    let backend = backend_for(&root).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de")
            .await
            .as_deref(),
        Some("'Alt'"),
        "precondition: the on-disk value resolves and is now cached"
    );

    did_change(&backend, &file, "<?php return ['required' => 'Neu'];", 2).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de")
            .await
            .as_deref(),
        Some("'Neu'"),
        "an unsaved buffer edit must be reflected by the next resolution"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "<?php return ['required' => 'Alt'];",
        "the file was never saved — the new value can only have come from the buffer"
    );
}

// ---------------------------------------------------------------------------
// Invalidation — external edits (a git pull, a branch switch, lang:publish)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_external_edit_to_a_php_catalogue_invalidates_the_cache() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let file = write(
        &root,
        "lang/de/validation.php",
        "<?php return ['required' => 'Alt'];",
    );
    let backend = backend_for(&root).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de")
            .await
            .as_deref(),
        Some("'Alt'"),
        "precondition: cached from disk"
    );

    // Edited outside the editor — never opened, never changed through the LSP.
    fs::write(&file, "<?php return ['required' => 'Neu'];").unwrap();
    watched_event(&backend, &file, FileChangeType::CHANGED).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de")
            .await
            .as_deref(),
        Some("'Neu'"),
        "an external edit must not keep serving the pre-change value"
    );
}

#[tokio::test]
async fn an_external_edit_to_a_json_catalogue_invalidates_the_cache() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let file = write(&root, "lang/de.json", r#"{"Welcome": "Willkommen"}"#);
    let backend = backend_for(&root).await;

    assert_eq!(
        value(&backend, &root, "Welcome", "de").await.as_deref(),
        Some("'Willkommen'"),
        "precondition: cached from disk"
    );

    fs::write(&file, r#"{"Welcome": "Servus"}"#).unwrap();
    watched_event(&backend, &file, FileChangeType::CHANGED).await;

    assert_eq!(
        value(&backend, &root, "Welcome", "de").await.as_deref(),
        Some("'Servus'"),
        "a JSON catalogue must invalidate on an external edit too"
    );
}

#[tokio::test]
async fn a_catalogue_created_externally_clears_the_negative_cache() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("lang/de")).unwrap();
    let backend = backend_for(&root).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de").await,
        None,
        "precondition: absent, and now negatively cached"
    );

    let file = write(
        &root,
        "lang/de/validation.php",
        "<?php return ['required' => 'Pflichtfeld'];",
    );
    watched_event(&backend, &file, FileChangeType::CREATED).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "de")
            .await
            .as_deref(),
        Some("'Pflichtfeld'"),
        "the negative cache entry must not outlive the file's creation"
    );
}

#[tokio::test]
async fn a_deleted_catalogue_stops_resolving() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let file = write(
        &root,
        "lang/es/validation.php",
        "<?php return ['required' => 'Obligatorio'];",
    );
    let backend = backend_for(&root).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "es")
            .await
            .as_deref(),
        Some("'Obligatorio'"),
        "precondition: cached from disk"
    );

    fs::remove_file(&file).unwrap();
    watched_event(&backend, &file, FileChangeType::DELETED).await;

    assert_eq!(
        value(&backend, &root, "validation.required", "es").await,
        None,
        "a deleted catalogue must not keep serving its pre-deletion value"
    );
}

// ---------------------------------------------------------------------------
// Locale discovery edge cases the fallback depends on
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_project_with_no_lang_directory_still_falls_back_to_en() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let backend = backend_for(&root).await;

    assert_eq!(
        backend
            .translation_locales(&root, "messages.welcome", None)
            .await,
        vec!["en"],
        "the backend must never hand callers an empty locale set — they index \
         `[0]` for the expected-path hint. The cache's own fallback is asserted \
         directly in `translation_lookup::tests`; this pins the backend contract \
         that sits over it."
    );
}

#[tokio::test]
async fn a_project_with_an_empty_lang_directory_still_falls_back_to_en() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("lang")).unwrap();
    let backend = backend_for(&root).await;

    assert_eq!(
        backend
            .translation_locales(&root, "messages.welcome", None)
            .await,
        vec!["en"],
        "an empty lang directory is not a locale set"
    );
}

#[tokio::test]
async fn app_locale_from_dotenv_leads_the_locale_list() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for locale in ["de", "en", "es", "fr"] {
        fs::create_dir_all(root.join("lang").join(locale)).unwrap();
    }
    fs::write(root.join(".env"), "APP_NAME=Test\nAPP_LOCALE=fr\n").unwrap();

    let backend = backend_for(&root).await;
    backend.register_env_files_with_salsa(&root).await;

    assert_eq!(
        backend
            .translation_locales(&root, "messages.welcome", None)
            .await,
        vec!["fr", "de", "en", "es"],
        "APP_LOCALE must still lead once locale discovery is served from Salsa"
    );
}

#[tokio::test]
async fn a_commented_app_locale_does_not_lead() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for locale in ["de", "en", "fr"] {
        fs::create_dir_all(root.join("lang").join(locale)).unwrap();
    }
    // The pre-Salsa reader matched `^APP_LOCALE=` line-anchored, so a commented
    // line never counted. Reading APP_LOCALE out of the env cache must not
    // quietly start honouring it.
    fs::write(root.join(".env"), "#APP_LOCALE=fr\n").unwrap();

    let backend = backend_for(&root).await;
    backend.register_env_files_with_salsa(&root).await;

    assert_eq!(
        backend
            .translation_locales(&root, "messages.welcome", None)
            .await,
        vec!["de", "en", "fr"],
        "a commented-out APP_LOCALE is not an APP_LOCALE"
    );
}

// ---------------------------------------------------------------------------
// The vendor map must never be memoized into a stale answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_changed_vendor_map_never_serves_a_stale_namespaced_resolution() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Two candidate package lang dirs, each defining the same key differently.
    write(
        &root,
        "vendor/acme/one/lang/de/messages.php",
        "<?php return ['title' => 'Eins'];",
    );
    write(
        &root,
        "vendor/acme/two/lang/de/messages.php",
        "<?php return ['title' => 'Zwei'];",
    );
    let backend = backend_for(&root).await;

    let map_for = |rel: &str| -> Option<Arc<HashMap<String, PathBuf>>> {
        let mut map = HashMap::new();
        map.insert("shop".to_string(), root.join(rel));
        Some(Arc::new(map))
    };

    let first = backend
        .resolve_translation(
            &root,
            "shop::messages.title",
            "de",
            map_for("vendor/acme/one/lang"),
        )
        .await;
    assert_eq!(first.map(|r| r.value).as_deref(), Some("'Eins'"));

    // The namespace now points somewhere else — as it would after a
    // `composer update` re-registered it.
    let second = backend
        .resolve_translation(
            &root,
            "shop::messages.title",
            "de",
            map_for("vendor/acme/two/lang"),
        )
        .await;
    assert_eq!(
        second.map(|r| r.value).as_deref(),
        Some("'Zwei'"),
        "the vendor map must not be baked into a memo key; a changed map must \
         resolve against the new directory"
    );
}

#[tokio::test]
async fn a_dotted_key_resolution_is_unaffected_by_the_vendor_map() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "lang/de/validation.php",
        "<?php return ['required' => 'Pflichtfeld'];",
    );
    let backend = backend_for(&root).await;

    let mut map = HashMap::new();
    map.insert("shop".to_string(), root.join("vendor/acme/one/lang"));

    let without = backend
        .resolve_translation(&root, "validation.required", "de", None)
        .await;
    let with = backend
        .resolve_translation(&root, "validation.required", "de", Some(Arc::new(map)))
        .await;

    assert_eq!(without.map(|r| r.value).as_deref(), Some("'Pflichtfeld'"));
    assert_eq!(
        with.map(|r| r.value).as_deref(),
        Some("'Pflichtfeld'"),
        "the common dotted path never consults the vendor map, so supplying one \
         must change neither the answer nor the cache it is served from"
    );
}

// ---------------------------------------------------------------------------
// Go-to-definition's key locator, and autocomplete's key list
//
// Both resolve a catalogue and then read it *again* for a second purpose —
// locating the key's line, or listing every key in it. Those reads were the
// siblings the first pass of #293 missed: routing resolution through Salsa
// while leaving them on direct `fs` would have left go-to-definition
// re-parsing a PHP file on every jump, and autocomplete re-reading every
// catalogue in a locale on every keystroke-triggered request.
// ---------------------------------------------------------------------------

/// The `target_range` go-to-definition would land on for `key`.
async fn goto_range(
    backend: &LaravelLanguageServer,
    key: &str,
) -> Option<tower_lsp::lsp_types::Range> {
    let reference = laravel_lsp::salsa_impl::TranslationReferenceData {
        key: key.to_string(),
        line: 0,
        column: 0,
        end_column: key.len() as u32,
    };
    match backend
        .create_translation_location_from_salsa(&reference)
        .await?
    {
        tower_lsp::lsp_types::GotoDefinitionResponse::Link(links) => {
            Some(links.first()?.target_range)
        }
        _ => None,
    }
}

#[tokio::test]
async fn goto_lands_on_the_nested_php_key_line() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // `title` is on line 4 (0-based).
    write(
        &root,
        "lang/de/notification.php",
        "<?php\nreturn [\n    'status' => [\n        'other' => 'x',\n        'title' => 'Statuswechsel',\n    ],\n];\n",
    );
    let backend = backend_for(&root).await;

    let range = goto_range(&backend, "notification.status.title")
        .await
        .expect("goto must resolve the key");
    assert_eq!(
        range.start.line, 4,
        "goto must land on the key's own line, not the top of the file"
    );
}

#[tokio::test]
async fn goto_lands_on_the_json_key_line() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // "Sign in" is on line 3 (0-based).
    write(
        &root,
        "lang/de.json",
        "{\n    \"Welcome\": \"Willkommen\",\n\n    \"Sign in\": \"Anmelden\"\n}\n",
    );
    let backend = backend_for(&root).await;

    let range = goto_range(&backend, "Sign in")
        .await
        .expect("goto must resolve the text key");
    assert_eq!(range.start.line, 3, "goto must land on the key's own line");
    assert_eq!(
        range.start.character, 5,
        "the span must start inside the opening quote"
    );
    assert_eq!(
        range.end.character, 12,
        "and cover exactly the key's characters"
    );
}

#[tokio::test]
async fn locating_the_same_key_twice_reads_disk_only_once() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "lang/de/notification.php",
        "<?php\nreturn [\n    'title' => 'Statuswechsel',\n];\n",
    );
    let backend = backend_for(&root).await;

    let first = goto_range(&backend, "notification.title").await;
    let after_first = disk_reads(&backend).await;
    let second = goto_range(&backend, "notification.title").await;
    let after_second = disk_reads(&backend).await;

    assert_eq!(first.map(|r| r.start.line), Some(2));
    assert_eq!(second, first);
    assert_eq!(
        after_second, after_first,
        "a repeat jump must re-read nothing — resolution already loaded the file"
    );
}

#[tokio::test]
async fn autocomplete_keys_are_served_from_cache_on_the_second_request() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "lang/en/messages.php",
        "<?php\nreturn [\n    'welcome' => 'Welcome',\n];\n",
    );
    write(
        &root,
        "lang/en/auth.php",
        "<?php\nreturn [\n    'failed' => 'These credentials do not match.',\n];\n",
    );
    let backend = backend_for(&root).await;

    let before = disk_reads(&backend).await;
    let first = backend.get_all_translation_keys().await;
    let after_first = disk_reads(&backend).await;
    let second = backend.get_all_translation_keys().await;
    let after_second = disk_reads(&backend).await;

    let keys: Vec<&str> = first.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["auth.failed", "messages.welcome"],
        "every catalogue in the locale contributes, sorted by key"
    );
    assert_eq!(
        second.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
        keys
    );
    assert!(
        after_first > before,
        "the first request must actually read the catalogues"
    );
    assert_eq!(
        after_second, after_first,
        "autocomplete must not re-read and re-parse every catalogue per request"
    );
}

#[tokio::test]
async fn autocomplete_keys_reflect_an_external_edit() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let file = write(
        &root,
        "lang/en/messages.php",
        "<?php\nreturn [\n    'welcome' => 'Welcome',\n];\n",
    );
    let backend = backend_for(&root).await;

    assert_eq!(
        backend
            .get_all_translation_keys()
            .await
            .iter()
            .map(|c| c.key.as_str())
            .collect::<Vec<_>>(),
        vec!["messages.welcome"],
        "precondition: cached"
    );

    fs::write(
        &file,
        "<?php\nreturn [\n    'welcome' => 'Welcome',\n    'goodbye' => 'Goodbye',\n];\n",
    )
    .unwrap();
    watched_event(&backend, &file, FileChangeType::CHANGED).await;

    assert_eq!(
        backend
            .get_all_translation_keys()
            .await
            .iter()
            .map(|c| c.key.as_str())
            .collect::<Vec<_>>(),
        vec!["messages.goodbye", "messages.welcome"],
        "a newly-added key must be offered without restarting the LSP"
    );
}

#[tokio::test]
async fn autocomplete_answers_from_a_single_locale_directory() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Two locales; each declares the same key plus one of its own.
    write(
        &root,
        "lang/de/messages.php",
        "<?php\nreturn [\n    'shared' => 'Geteilt',\n    'only_de' => 'Nur DE',\n];\n",
    );
    write(
        &root,
        "lang/en/messages.php",
        "<?php\nreturn [\n    'shared' => 'Shared',\n    'only_en' => 'Only EN',\n];\n",
    );
    let backend = backend_for(&root).await;

    let keys: Vec<String> = backend
        .get_all_translation_keys()
        .await
        .into_iter()
        .map(|c| c.key)
        .collect();

    // `de` sorts first, so it is the locale that answers.
    assert_eq!(
        keys,
        vec!["messages.only_de", "messages.shared"],
        "one locale answers for the whole project — unioning every locale would \
         read every catalogue to produce the same list, and would surface a key \
         from a partially-translated locale as though it were project-wide"
    );
}

#[tokio::test]
async fn autocomplete_is_empty_for_a_project_with_no_lang_directory() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let backend = backend_for(&root).await;

    assert!(
        backend.get_all_translation_keys().await.is_empty(),
        "no lang directory means nothing to offer"
    );
}

// ---------------------------------------------------------------------------
// The vendor translation-namespace map must not outlive the providers it was
// scanned from
//
// The map is built by scanning service providers for `loadTranslationsFrom`
// and was cached for the entire session with no invalidation path at all. Once
// #293 made that map decide where a namespaced key resolves, a stale map stopped
// being merely old and started being wrong: a `composer update`, a
// newly-installed package, or an edited registration was invisible until the
// LSP restarted.
// ---------------------------------------------------------------------------

/// An app provider registering `namespace` at `lang/<dir>`.
fn provider_registering(namespace: &str, dir: &str) -> String {
    format!(
        "<?php\nnamespace App\\Providers;\nclass AppServiceProvider {{\n    public function boot(): void {{\n        $this->loadTranslationsFrom(lang_path('{dir}'), '{namespace}');\n    }}\n}}\n"
    )
}

#[tokio::test]
async fn an_edited_service_provider_refreshes_the_vendor_namespace_map() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("lang/app")).unwrap();
    fs::create_dir_all(root.join("lang/shop")).unwrap();
    let provider = write(
        &root,
        "app/Providers/AppServiceProvider.php",
        &provider_registering("app", "app"),
    );
    let backend = backend_for(&root).await;

    let before = backend.vendor_translation_namespaces_for(&root).await;
    assert!(
        before.as_ref().is_some_and(|m| m.contains_key("app")),
        "precondition: the original registration is scanned and cached"
    );

    // The registration changes — a `composer update`, or a hand edit.
    fs::write(&provider, provider_registering("shop", "shop")).unwrap();
    watched_event(&backend, &provider, FileChangeType::CHANGED).await;

    let after = backend.vendor_translation_namespaces_for(&root).await;
    assert!(
        after.as_ref().is_some_and(|m| m.contains_key("shop")),
        "the new namespace must be picked up without restarting the LSP"
    );
    assert!(
        after.as_ref().is_some_and(|m| !m.contains_key("app")),
        "and the removed one must not linger"
    );
}

#[tokio::test]
async fn a_namespaced_key_resolves_against_the_refreshed_provider_map() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(
        &root,
        "lang/shop/de/messages.php",
        "<?php return ['title' => 'Warenkorb'];",
    );
    fs::create_dir_all(root.join("lang/app")).unwrap();
    let provider = write(
        &root,
        "app/Providers/AppServiceProvider.php",
        &provider_registering("app", "app"),
    );
    let backend = backend_for(&root).await;

    let map = backend.vendor_translation_namespaces_for(&root).await;
    assert_eq!(
        backend
            .resolve_translation(&root, "shop::messages.title", "de", map)
            .await,
        None,
        "precondition: nothing registers the `shop` namespace yet"
    );

    fs::write(&provider, provider_registering("shop", "shop")).unwrap();
    watched_event(&backend, &provider, FileChangeType::CHANGED).await;

    let map = backend.vendor_translation_namespaces_for(&root).await;
    assert_eq!(
        backend
            .resolve_translation(&root, "shop::messages.title", "de", map)
            .await
            .map(|r| r.value)
            .as_deref(),
        Some("'Warenkorb'"),
        "a namespaced key must resolve against the provider registration as it \
         is now, not as it was when the session started"
    );
}

#[tokio::test]
async fn the_provider_scan_is_warm_across_requests() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("lang/app")).unwrap();
    write(
        &root,
        "app/Providers/AppServiceProvider.php",
        &provider_registering("app", "app"),
    );
    // A vendor provider too, so the walk has both halves to do.
    write(
        &root,
        "vendor/acme/billing/src/BillingServiceProvider.php",
        "<?php\nclass BillingServiceProvider {\n    public function boot() {}\n}\n",
    );
    let backend = backend_for(&root).await;

    let before = disk_reads(&backend).await;
    let first = backend.vendor_translation_namespaces_for(&root).await;
    let after_first = disk_reads(&backend).await;
    let second = backend.vendor_translation_namespaces_for(&root).await;
    let after_second = disk_reads(&backend).await;

    assert!(first.as_ref().is_some_and(|m| m.contains_key("app")));
    assert_eq!(second, first);
    assert!(
        after_first > before,
        "the first scan must actually walk and read the providers"
    );
    assert_eq!(
        after_second, after_first,
        "the provider scan must be served from Salsa, not re-walked per request"
    );
}

#[tokio::test]
async fn a_provider_created_externally_is_discovered() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("lang/shop")).unwrap();
    fs::create_dir_all(root.join("app/Providers")).unwrap();
    let backend = backend_for(&root).await;

    assert!(
        backend
            .vendor_translation_namespaces_for(&root)
            .await
            .is_some_and(|m| m.is_empty()),
        "precondition: no providers, and that empty result is now cached"
    );

    // A package is installed, or a provider is written, outside the editor.
    let provider = write(
        &root,
        "app/Providers/ShopServiceProvider.php",
        &provider_registering("shop", "shop"),
    );
    watched_event(&backend, &provider, FileChangeType::CREATED).await;

    assert!(
        backend
            .vendor_translation_namespaces_for(&root)
            .await
            .is_some_and(|m| m.contains_key("shop")),
        "a provider that did not exist at scan time must still be discovered — \
         invalidation has to drop the discovered set, not just the file texts"
    );
}
