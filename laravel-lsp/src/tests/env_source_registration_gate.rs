//! Which files register as Laravel env sources (issue #337).
//!
//! `execute_salsa_update` classifies every changed document before handing it
//! to Salsa. Its env arm used to read `filename.starts_with(".env")`, which is
//! a prefix test, not the project's env-file predicate: `.envrc` (direnv's
//! file, common in Laravel repositories that pin per-project tooling),
//! `.environment`, and `.env-backup` all satisfy it. Each one registered as an
//! env source, and Salsa merges parsed env sources by variable name — so a
//! `DATABASE_URL` in `.envrc` surfaced in completion and hover as though the
//! project's own `.env` had declared it.
//!
//! Four sibling call sites already routed through
//! `env_key_locator::is_env_file_name`; this one did not. These tests pin the
//! Salsa dispatch specifically, because the predicate's own unit tests pass
//! either way — they never touch this branch.
//!
//! Everything drives the **real** `did_change` handler on the `LspService`
//! harness and then reads back through the public Salsa accessor, matching the
//! convention in `translation_salsa_cache`: a direct poke at the classifier
//! would prove the predicate works while saying nothing about whether the
//! dispatch calls it.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, TextDocumentContentChangeEvent, Url,
    VersionedTextDocumentIdentifier,
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

/// Write `rel` under `root` and drive the real `did_change` handler over it,
/// waiting for the Salsa update it queues so the assertion cannot race.
async fn change(backend: &LaravelLanguageServer, root: &Path, rel: &str, text: &str) -> PathBuf {
    let path = root.join(rel);
    fs::write(&path, text).unwrap();
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
                text: text.to_string(),
            }],
        })
        .await;
    if let Some(handle) = backend.pending_salsa_updates.write().await.remove(&uri) {
        let _ = handle.await;
    }
    path
}

/// Every variable name Salsa currently holds from a registered env source.
async fn registered_names(backend: &LaravelLanguageServer) -> Vec<String> {
    backend
        .salsa
        .get_all_parsed_env_vars()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect()
}

/// The direnv case named in review: `.envrc` starts with `.env` but is not a
/// Laravel env file, and its variables must not enter the project's env space.
#[tokio::test]
async fn envrc_does_not_register_as_an_env_source() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;

    change(&backend, dir.path(), ".envrc", "DIRENV_ONLY=leaked\n").await;

    assert!(
        !registered_names(&backend)
            .await
            .contains(&"DIRENV_ONLY".to_string()),
        "`.envrc` is direnv's file, not Laravel's — its keys must not register as env sources"
    );
}

/// `.envrc` is the reported trigger, not the whole class. The old prefix test
/// admitted every name beginning `.env`, so the sibling shapes are pinned too:
/// a dot-less suffix and a non-dot separator.
#[tokio::test]
async fn env_prefixed_non_env_files_do_not_register() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;

    change(
        &backend,
        dir.path(),
        ".environment",
        "ENVIRONMENT_ONLY=leaked\n",
    )
    .await;
    change(&backend, dir.path(), ".env-backup", "BACKUP_ONLY=leaked\n").await;

    let names = registered_names(&backend).await;
    assert!(
        !names.contains(&"ENVIRONMENT_ONLY".to_string()),
        "`.environment` is not a `.env.<suffix>` variant"
    );
    assert!(
        !names.contains(&"BACKUP_ONLY".to_string()),
        "`.env-backup` separates with a hyphen, not the variant dot"
    );
}

/// The control. A gate that rejected everything would satisfy the tests above
/// while silently removing env support altogether, so the real variants must
/// still register — including through the same dispatch, at their documented
/// priorities.
#[tokio::test]
async fn real_env_variants_still_register() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;

    change(&backend, dir.path(), ".env", "APP_KEY=real\n").await;
    change(&backend, dir.path(), ".env.local", "LOCAL_KEY=real\n").await;
    change(&backend, dir.path(), ".env.example", "EXAMPLE_KEY=real\n").await;

    let names = registered_names(&backend).await;
    for expected in ["APP_KEY", "LOCAL_KEY", "EXAMPLE_KEY"] {
        assert!(
            names.contains(&expected.to_string()),
            "`{expected}` must still register — the gate narrows the class, it does not close it"
        );
    }
}

/// The priority ladder below the gate is unchanged: `.env` (2) outranks
/// `.env.local` (1), which outranks `.env.example` (0). Pinned here because
/// the gate edit sits directly above that `match`, and a merge that dropped it
/// would leave every assertion above still passing.
#[tokio::test]
async fn env_priority_ladder_survives_the_gate() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;

    change(
        &backend,
        dir.path(),
        ".env.example",
        "SHARED=from_example\n",
    )
    .await;
    change(&backend, dir.path(), ".env.local", "SHARED=from_local\n").await;
    change(&backend, dir.path(), ".env", "SHARED=from_env\n").await;

    let winner = backend
        .salsa
        .get_parsed_env_var("SHARED".to_string())
        .await
        .unwrap()
        .expect("`SHARED` is declared in all three variants");
    assert_eq!(winner.value, "from_env", "`.env` has the highest priority");
    assert_eq!(winner.priority, 2);
}
