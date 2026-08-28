//! Which files the completion handler treats as env files (issue #345).
//!
//! `completion` picks the context helper for the cursor from the file's
//! classification: an env file gets `${...}` interpolation, a PHPUnit XML file
//! gets its `<env name="…">` attributes, and everything else — PHP and Blade
//! included — gets `env('...')` call completion. That choice used to gate on
//! `path.contains(".env")`, a substring test over the whole path, so every
//! file under a directory such as `.envs/` classified as an env file. A `.php`
//! file there was handed the interpolation helper, so its `env('…')` calls
//! never reached the call-context helper that completes them.
//!
//! `env_key_locator::path_is_env_file` now owns the decision, and its own unit
//! tests pin the predicate. They say nothing about whether this dispatch
//! consults it: a predicate can be perfect while the handler that classifies
//! with it reads something else entirely. `env_source_registration_gate`
//! makes the same argument for the Salsa-registration call site; this module
//! is its counterpart for the completion call site.
//!
//! So everything here drives the **real** `textDocument/completion` request
//! and asserts on the items it returns. The two directions are both pinned,
//! because a one-directional test cannot tell a narrowed gate from a closed
//! one:
//!
//! - a rejected file must not get interpolation completion, and
//! - a real `.env` variant must still get it.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    CompletionParams, CompletionResponse, DidChangeTextDocumentParams, Position,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    VersionedTextDocumentIdentifier,
};
use tower_lsp::{LanguageServer, LspService};

/// The variable the project's own `.env` declares. Deliberately unlike any
/// real environment variable: the handler appends `std::env::vars()` matching
/// the same prefix, so a name that could collide with the machine's
/// environment would make "no items" mean "no match" rather than "no context".
const SENTINEL: &str = "ZL345_SENTINEL";

/// The prefix typed at the cursor. Must remain a prefix of [`SENTINEL`], so
/// every fixture below builds its template from this constant rather than
/// spelling the prefix out: a rejection test asserts that *nothing* is
/// offered, and a prefix that had drifted out of sync with the sentinel would
/// satisfy that assertion by matching nothing at all. The positive tests share
/// the constant and so keep it honest — they fail if it stops matching.
const PREFIX: &str = "ZL345_";

/// A backend with `root_path` primed and the Salsa debounce removed, so a
/// `did_change` can be awaited deterministically instead of slept on.
/// Mirrors `env_source_registration_gate::backend_for`.
async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.auto_complete_debounce_ms.write().await = 0;
    backend
}

/// Register the project's `.env` as a real Salsa env source by driving the
/// `did_change` handler, so the completion under test has something to offer.
/// Without this every assertion below would read "no items" and pass for the
/// wrong reason.
async fn seed_env_source(backend: &LaravelLanguageServer, root: &Path) {
    let text = format!("{SENTINEL}=from_dot_env\n");
    let path = root.join(".env");
    fs::write(&path, &text).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    backend
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 1,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.clone(),
            }],
        })
        .await;
    if let Some(handle) = backend.pending_salsa_updates.write().await.remove(&uri) {
        let _ = handle.await;
    }
}

/// Build `(content, cursor position)` from a template carrying a single `◊`
/// marker at the desired cursor, matching the convention in
/// `query_chain_completion_handler`. The marker is stripped from the returned
/// content; the position is its 0-based line + code-point column.
fn cursor_at(template: &str) -> (String, Position) {
    const MARK: char = '◊';
    let byte_idx = template
        .find(MARK)
        .expect("template must contain the ◊ cursor marker");
    let before = &template[..byte_idx];
    let line = before.matches('\n').count() as u32;
    let character = before.rsplit('\n').next().unwrap_or(before).chars().count() as u32;
    (template.replace(MARK, ""), Position { line, character })
}

/// Write `rel` under `root`, open it in the server's document map, and drive
/// the real `completion` request at the template's `◊` marker. Returns the
/// offered labels — empty when the handler declines, which is what "not a
/// completion context here" looks like from the client's side.
async fn completion_labels(
    backend: &LaravelLanguageServer,
    root: &Path,
    rel: &str,
    template: &str,
) -> Vec<String> {
    let (content, position) = cursor_at(template);
    let path: PathBuf = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &content).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    backend
        .documents
        .write()
        .await
        .insert(uri.clone(), (content, 1));

    let response = backend
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .expect("completion must not error");

    match response {
        None => Vec::new(),
        Some(CompletionResponse::Array(items)) => items.into_iter().map(|i| i.label).collect(),
        Some(CompletionResponse::List(list)) => list.items.into_iter().map(|i| i.label).collect(),
    }
}

/// A `.php` file under a `.env`-named directory, with the cursor inside a
/// `${...}` interpolation. Interpolation is env-file syntax, so the handler
/// must not offer it here — under the old `path.contains(".env")` gate this
/// file classified as an env file and the interpolation completed.
#[tokio::test]
async fn php_under_an_env_named_directory_is_not_offered_interpolation() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    seed_env_source(&backend, dir.path()).await;

    let labels = completion_labels(
        &backend,
        dir.path(),
        ".envs/deploy.php",
        &format!("<?php\n$value = \"${{{PREFIX}◊}}\";\n"),
    )
    .await;

    assert!(
        labels.is_empty(),
        "`.envs/deploy.php` is a PHP file whose *directory* merely mentions \
         `.env`; `${{...}}` is env-file syntax and must not complete there. \
         Got {labels:?}"
    );
}

/// The same file reaching the PHP arm it belongs to. This is the positive half
/// of the pair: a gate that classified nothing as an env file would satisfy
/// the assertion above while breaking env completion everywhere, and a gate
/// that classified this file as an env file would send `env('…')` to the
/// interpolation helper, which finds no `${` and declines.
#[tokio::test]
async fn php_under_an_env_named_directory_gets_env_call_completion() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    seed_env_source(&backend, dir.path()).await;

    let labels = completion_labels(
        &backend,
        dir.path(),
        ".envs/deploy.php",
        &format!("<?php\n$value = env('{PREFIX}◊');\n"),
    )
    .await;

    assert!(
        labels.iter().any(|l| l == SENTINEL),
        "a PHP file must reach `env('…')` call completion regardless of a \
         `.env` substring in its directory; got {labels:?}"
    );
}

/// direnv's file. Starts with `.env`, is not a Laravel env file, and is
/// common in Laravel repositories that pin per-project tooling.
#[tokio::test]
async fn envrc_is_not_offered_interpolation() {
    assert_rejects(".envrc").await;
}

/// A dot-less suffix — the variant arm requires the separating dot.
#[tokio::test]
async fn environment_is_not_offered_interpolation() {
    assert_rejects(".environment").await;
}

/// A non-dot separator. Named in the issue alongside `.envrc`.
#[tokio::test]
async fn env_backup_is_not_offered_interpolation() {
    assert_rejects(".env-backup").await;
}

/// Drive interpolation completion in `name` and require the handler to
/// decline. Each caller is its own test so that reverting the gate reddens
/// every rejected name independently — an assertion loop would stop at the
/// first and credit only one row.
async fn assert_rejects(name: &str) {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    seed_env_source(&backend, dir.path()).await;

    let labels =
        completion_labels(&backend, dir.path(), name, &format!("FOO=${{{PREFIX}◊}}\n")).await;

    assert!(
        labels.is_empty(),
        "`{name}` is not a Laravel env file, so the project's env variables \
         must not complete inside its `${{...}}`. Got {labels:?}"
    );
}

/// The control. Every accepted variant the predicate documents must still
/// reach interpolation completion through this same dispatch — the gate
/// narrows the class, it does not close it.
///
/// The misses are collected and asserted once rather than asserted per
/// iteration: an `assert!` inside the loop stops at the first failing variant,
/// so a gate that dropped the whole `.env.<suffix>` arm would be credited by
/// `.env.local` alone and leave `.env.example` and `.env.testing` unproven.
#[tokio::test]
async fn real_env_variants_are_still_offered_interpolation() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    seed_env_source(&backend, dir.path()).await;

    let mut missing = Vec::new();
    for variant in [".env", ".env.local", ".env.example", ".env.testing"] {
        let labels = completion_labels(
            &backend,
            dir.path(),
            variant,
            &format!("{SENTINEL}=from_dot_env\nOTHER=${{{PREFIX}◊}}\n"),
        )
        .await;
        if !labels.iter().any(|l| l == SENTINEL) {
            missing.push(variant);
        }
    }

    assert!(
        missing.is_empty(),
        "every Laravel env variant must still complete `${{...}}` \
         interpolation through this dispatch; these did not: {missing:?}"
    );
}
