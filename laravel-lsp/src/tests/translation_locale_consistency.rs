//! Hover, go-to-definition and diagnostics must agree about a translation key.
//!
//! All three used to hardcode `"en"`. #287 made hover multi-locale and left the
//! other two behind, so a key defined only in `de` rendered in the hover,
//! refused to navigate, and was still reported missing by diagnostics — the
//! comment claiming "navigation, diagnostics, and hover all agree on where a
//! key lives" was simply false (issue #288).
//!
//! These tests drive all three paths against one fixture and assert they land
//! on the same file, for both key shapes the issue names: a plain dotted key
//! and a namespaced one.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::TranslationReferenceData;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::{lsp_types::GotoDefinitionResponse, LspService};

fn backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

fn reference(key: &str) -> TranslationReferenceData {
    TranslationReferenceData {
        key: key.to_string(),
        line: 0,
        column: 0,
        end_column: key.len() as u32,
    }
}

/// The single file goto navigated to, or `None`.
fn goto_target(response: Option<GotoDefinitionResponse>) -> Option<PathBuf> {
    match response? {
        GotoDefinitionResponse::Link(links) => links.first()?.target_uri.to_file_path().ok(),
        _ => None,
    }
}

/// A project whose ONLY locale is `de` — no `en` anywhere, which is what broke
/// the hardcoded paths.
fn de_only_project() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let de = root.join("lang/de");
    std::fs::create_dir_all(&de).unwrap();
    std::fs::write(
        de.join("contract.php"),
        "<?php return ['prefill' => ['failed_title' => 'Analyse fehlgeschlagen']];",
    )
    .unwrap();
    (tmp, root)
}

async fn assert_all_three_agree(
    backend: &LaravelLanguageServer,
    root: &Path,
    key: &str,
    expected_file: &Path,
    expected_value: &str,
) {
    let hover = backend.hover_for_translation(key, Some(root)).await;
    assert!(
        hover.contains(expected_value),
        "hover must show the value; got:\n{hover}"
    );
    assert!(
        !hover.contains("not found"),
        "hover must not report the key missing; got:\n{hover}"
    );

    let goto = goto_target(
        backend
            .create_translation_location_from_salsa(&reference(key))
            .await,
    );
    assert_eq!(
        goto.as_deref(),
        Some(expected_file),
        "goto must navigate to the same file hover sourced the value from"
    );

    let check = LaravelLanguageServer::check_translation_file(root, key, None);
    assert!(
        check.exists,
        "diagnostics must not report a key the hover displays as missing"
    );
}

#[tokio::test]
async fn dotted_key_defined_only_in_de_agrees_across_hover_goto_and_diagnostics() {
    let (_tmp, root) = de_only_project();
    let backend = backend();
    *backend.root_path.write().await = Some(root.clone());

    assert_all_three_agree(
        &backend,
        &root,
        "contract.prefill.failed_title",
        &root.join("lang/de/contract.php"),
        "Analyse fehlgeschlagen",
    )
    .await;
}

#[tokio::test]
async fn namespaced_key_defined_only_in_de_agrees_across_hover_goto_and_diagnostics() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let de = root.join("lang/vendor/shop/de");
    std::fs::create_dir_all(&de).unwrap();
    std::fs::write(
        de.join("messages.php"),
        "<?php return ['title' => 'Warenkorb'];",
    )
    .unwrap();

    let backend = backend();
    *backend.root_path.write().await = Some(root.clone());

    assert_all_three_agree(
        &backend,
        &root,
        "shop::messages.title",
        &de.join("messages.php"),
        "Warenkorb",
    )
    .await;
}

/// A lang file that exists but does not define the hovered key must not be
/// reported present by diagnostics, nor navigated to by goto — the divergence
/// that "check the file exists" produced.
#[tokio::test]
async fn a_locale_file_without_the_key_is_not_treated_as_a_definition() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let en = root.join("lang/en");
    std::fs::create_dir_all(&en).unwrap();
    // The file exists; the key does not.
    std::fs::write(en.join("contract.php"), "<?php return ['other' => 'x'];").unwrap();

    let backend = backend();
    *backend.root_path.write().await = Some(root.clone());

    let key = "contract.prefill.failed_title";
    let check = LaravelLanguageServer::check_translation_file(&root, key, None);
    assert!(
        !check.exists,
        "an existing lang file without the key is not a definition"
    );

    let goto = goto_target(
        backend
            .create_translation_location_from_salsa(&reference(key))
            .await,
    );
    assert_eq!(
        goto, None,
        "goto must not navigate to a file lacking the key"
    );
}
