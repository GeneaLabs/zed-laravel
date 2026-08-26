//! Handler-level coverage for go-to-definition on a namespaced translation
//! key (`app::file.key`) — issue #248 follow-up.
//!
//! `create_translation_location_from_salsa` (`main.rs`) used to split every key
//! on `.` and build `lang/en/<first-segment>.php`. For a namespaced key that
//! produced a bogus `lang/en/app::notification.php`, the file never existed, and
//! the handler returned `None` — so cmd-hover showed no underline and the click
//! did nothing. The fix resolves the namespace through the merged vendor/app
//! provider map (the same map hover and diagnostics use) to the real lang file.
//!
//! These tests drive the real async handler on a server built through the
//! `tower_lsp::LspService` harness the other handler tests use, priming the one
//! piece of state the chain reads — the project root — and writing a real
//! `AppServiceProvider` for the namespace scan to discover, so it runs purely
//! against a tempdir.
//!
//! The namespace map used to be injected into an `RwLock` field directly.
//! Since #293 it is derived through Salsa from the providers actually on disk,
//! so these tests now exercise the provider scan end to end rather than
//! assuming its output.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::TranslationReferenceData;
use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::{GotoDefinitionResponse, Url};
use tower_lsp::LspService;

/// Build a backend rooted at `root`, with an `AppServiceProvider` registering
/// `lang_path('app')` under the `app` namespace — the real registration the
/// namespace scan reads, rather than a hand-injected map.
///
/// `lang/app/` must exist for the registration to resolve: `loadTranslationsFrom`
/// only contributes a namespace whose path argument names a real directory.
async fn backend_with_app_namespace(root: &Path) -> LaravelLanguageServer {
    fs::create_dir_all(root.join("lang/app")).unwrap();
    let providers = root.join("app/Providers");
    fs::create_dir_all(&providers).unwrap();
    fs::write(
        providers.join("AppServiceProvider.php"),
        "<?php\nnamespace App\\Providers;\nclass AppServiceProvider {\n    public function boot(): void {\n        $this->loadTranslationsFrom(lang_path('app'), 'app');\n    }\n}\n",
    )
    .unwrap();

    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
}

/// A `TranslationReferenceData` for `key`. `column`/`end_column` are the string
/// content span — exactly what becomes the link's `origin_selection_range`
/// (the underline).
fn trans_ref(key: &str, column: u32, end_column: u32) -> TranslationReferenceData {
    TranslationReferenceData {
        key: key.to_string(),
        line: 3,
        column,
        end_column,
    }
}

fn single_link(resp: GotoDefinitionResponse) -> tower_lsp::lsp_types::LocationLink {
    match resp {
        GotoDefinitionResponse::Link(mut links) => {
            assert_eq!(links.len(), 1, "expected exactly one LocationLink");
            links.remove(0)
        }
        other => panic!("expected GotoDefinitionResponse::Link, got {other:?}"),
    }
}

#[tokio::test]
async fn namespaced_key_navigates_to_app_provider_lang_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();

    // The app-registered lang file the namespace points at — a nested array so
    // the leaf key sits on its own line (line 3, 0-based).
    let lang_file = root.join("lang/app/en/notification.php");
    fs::create_dir_all(lang_file.parent().unwrap()).unwrap();
    fs::write(
        &lang_file,
        "<?php\nreturn [\n    'task_group_status_change' => [\n        'title' => 'Status changed',\n    ],\n];\n",
    )
    .unwrap();

    let backend = backend_with_app_namespace(root).await;

    let resp = backend
        .create_translation_location_from_salsa(&trans_ref(
            "app::notification.task_group_status_change.title",
            9,
            57,
        ))
        .await
        .expect("a namespaced key with an existing lang file must resolve to a link");

    let link = single_link(resp);
    // Compared canonicalized: `loadTranslationsFrom` resolves its path argument
    // through `canonicalize`, so on macOS the registered directory carries the
    // real `/private/var` prefix where the tempdir handle reports `/var`. Same
    // file, different spelling.
    assert_eq!(
        link.target_uri,
        Url::from_file_path(lang_file.canonicalize().unwrap()).unwrap(),
        "target must be the app-registered lang file, not lang/en/app::… or lang/vendor/app/…"
    );
    // The underline covers exactly the key's string content.
    let origin = link
        .origin_selection_range
        .expect("origin selection range drives the cmd-hover underline");
    assert_eq!(origin.start.character, 9);
    assert_eq!(origin.end.character, 57);
    assert_eq!(origin.start.line, 3);
    // The jump lands on the leaf key's line (`'title'` on line 3), not line 0.
    assert_eq!(
        link.target_range.start.line, 3,
        "navigation must land on the key's line, not the top of the file"
    );
}

#[tokio::test]
async fn namespaced_key_without_file_yields_no_link() {
    // No lang file on disk → no navigation target (and so no phantom underline).
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();

    let backend = backend_with_app_namespace(root).await;

    let resp = backend
        .create_translation_location_from_salsa(&trans_ref("app::missing.title", 9, 27))
        .await;
    assert!(
        resp.is_none(),
        "a namespaced key whose file doesn't exist must not produce a link"
    );
}
