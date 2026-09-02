//! Handler-level tests for the rename fail-closed guard (#369).
//!
//! `rename::require_declaration_edits` is unit-tested in isolation, but the
//! bug it prevents lives in the WIRING: call-site edits are collected before
//! the per-kind declaration walkers run, so a missing guard means the handler
//! applies the call sites alone and silently repoints every one of them at a
//! key that does not exist. Bypassing the guard at its two call sites in
//! `rename()` left the whole suite green, which is why these exist.
//!
//! `folio_rename.rs:22` records that the rename APPLY handler was previously
//! unfixtured; these are the first tests to drive it end to end.
//!
//! What they prove: the guard is wired into both arms and refuses the key with
//! a named reason, where a build without it returns `Ok(None)` and tells the
//! user nothing. What they do NOT prove: that a half-applied edit would
//! otherwise reach disk. This fixture never populates the references index, so
//! no call-site edits exist here to be applied — even the passing control
//! edits only the declaration file. Demonstrating the half-apply needs a warm
//! reference index, which no test in this crate currently builds.

use crate::LaravelLanguageServer;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    Position, RenameParams, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A project whose `config/codes.php` declares one quoted key, one bare
/// integer key, and one list — plus a PHP file using all three.
fn fixture(root: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/codes.php"),
        "<?php\n\nreturn [\n    'named' => 'ok',\n    404 => 'Not found',\n    'list' => ['a'],\n];\n",
    )
    .unwrap();
    let caller = root.join("app/Uses.php");
    std::fs::create_dir_all(caller.parent().unwrap()).unwrap();
    std::fs::write(
        &caller,
        "<?php\n\n$a = config('codes.named');\n$b = config('codes.404');\n$c = config('codes.list.0');\n",
    )
    .unwrap();
    caller
}

async fn rename_at(
    server: &LaravelLanguageServer,
    file: &Path,
    line: u32,
    character: u32,
    new_name: &str,
) -> tower_lsp::jsonrpc::Result<Option<tower_lsp::lsp_types::WorkspaceEdit>> {
    let uri = Url::from_file_path(file).unwrap();
    server
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            new_name: new_name.to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
}

/// The (line, column) of `needle` inside `file`, so a cursor is never
/// hand-counted — an earlier version of this file put the 404 cursor on the
/// closing quote, where the handler classifies nothing, and the tests passed
/// against a deliberately broken build as a result.
fn cursor_at(file: &Path, needle: &str) -> (u32, u32) {
    let text = std::fs::read_to_string(file).unwrap();
    for (row, line) in text.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return (row as u32, col as u32);
        }
    }
    panic!("{needle:?} not found in {}", file.display());
}

#[tokio::test]
async fn a_bare_integer_key_is_refused_rather_than_half_renamed() {
    let dir = TempDir::new().unwrap();
    let caller = fixture(dir.path());
    let server = test_server();
    *server.root_path.write().await = Some(dir.path().to_path_buf());

    let (line, col) = cursor_at(&caller, "404');");
    let result = rename_at(&server, &caller, line, col, "missing").await;
    let err = result.expect_err("a bare integer key must be refused, not silently ignored");
    assert!(
        err.message.contains("codes.404") && err.message.contains("no quoted declaration"),
        "the toast must name the key and the reason, got {:?}",
        err.message
    );
}

#[tokio::test]
async fn a_list_index_is_refused_rather_than_half_renamed() {
    let dir = TempDir::new().unwrap();
    let caller = fixture(dir.path());
    let server = test_server();
    *server.root_path.write().await = Some(dir.path().to_path_buf());

    let (line, col) = cursor_at(&caller, "0');");
    let result = rename_at(&server, &caller, line, col, "first").await;
    let err = result.expect_err("a list index must be refused, not silently ignored");
    assert!(
        err.message.contains("codes.list.0") && err.message.contains("no quoted declaration"),
        "the toast must name the key and the reason, got {:?}",
        err.message
    );
}

#[tokio::test]
async fn a_quoted_key_still_renames_and_rewrites_its_declaration() {
    // The control. Without it the two refusals above could pass simply
    // because rename never reaches config keys in this fixture at all.
    let dir = TempDir::new().unwrap();
    let caller = fixture(dir.path());
    let server = test_server();
    *server.root_path.write().await = Some(dir.path().to_path_buf());

    let (line, col) = cursor_at(&caller, "named');");
    let edit = rename_at(&server, &caller, line, col, "renamed")
        .await
        .expect("a quoted config key is renameable")
        .expect("a quoted config key produces a WorkspaceEdit");
    let changes = edit.changes.expect("edits are keyed by file");
    let touches_declaration = changes
        .keys()
        .any(|uri| uri.path().ends_with("config/codes.php"));
    assert!(
        touches_declaration,
        "the declaration file must be edited, got {:?}",
        changes
            .keys()
            .map(|u| u.path().to_string())
            .collect::<Vec<_>>()
    );
}
