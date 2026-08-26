//! The untruncated value survives the whole completion pipeline (issue #326).
//!
//! `completion_display::tests` proves the two render sites clip to their own
//! budgets, and `byte_offset_panic_hardening` proves the two extractors no
//! longer clip at all. Neither says anything about what happens *between*
//! them — and the value crosses a lot of ground in between: a regex parse, a
//! Salsa query and its memoized `Vec<(String, String)>`, an actor round trip
//! over a oneshot channel, and a struct conversion.
//!
//! Truncation reinstated anywhere along that path would leave every other
//! test in this change green while the panel silently went back to showing a
//! clipped string. So these drive the real `get_all_config_keys` /
//! `get_all_translation_keys` against a real project on disk and assert the
//! completion struct's `value` is exactly as long as what was written.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::LspService;

/// A backend with `root_path` primed at `root`.
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

/// Well past the 200-char panel budget, and multibyte throughout so a
/// byte-based reinstatement panics rather than merely shortening.
fn long_value() -> String {
    "ä".repeat(5_000)
}

#[tokio::test]
async fn a_config_value_reaches_the_completion_struct_untruncated() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let value = long_value();
    write(
        &root,
        "config/app.php",
        &format!("<?php\nreturn [\n    'name' => '{}',\n];\n", value),
    );
    let backend = backend_for(&root).await;

    let keys = backend.get_all_config_keys().await;
    let entry = keys
        .iter()
        .find(|c| c.key == "app.name")
        .expect("the config key must be discovered at all");

    assert_eq!(
        entry.value.chars().count(),
        value.chars().count(),
        "the completion struct must carry the full value, not a clipped copy"
    );
    assert_eq!(entry.value, value);
}

#[tokio::test]
async fn a_translation_value_reaches_the_completion_struct_untruncated() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let value = long_value();
    write(
        &root,
        "lang/en/messages.php",
        &format!("<?php\nreturn [\n    'welcome' => '{}',\n];\n", value),
    );
    let backend = backend_for(&root).await;

    let keys = backend.get_all_translation_keys().await;
    let entry = keys
        .iter()
        .find(|t| t.key == "messages.welcome")
        .expect("the translation key must be discovered at all");

    assert_eq!(
        entry.value.chars().count(),
        value.chars().count(),
        "the completion struct must carry the full value, not a clipped copy"
    );
    assert_eq!(entry.value, value);
}
