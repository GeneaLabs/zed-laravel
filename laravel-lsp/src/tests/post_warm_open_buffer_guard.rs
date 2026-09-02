//! Regression: the post-warm pattern-cache import must not clobber an open
//! buffer's live-parsed entry with one parsed from disk.
//!
//! `bulk_import_patterns` bulk-inserts DISK-parsed `ParsedPatternsData` into
//! the shared `pattern_cache` after a warm. Before the fix it did this
//! unconditionally, so a file the user has open — with unsaved edits that
//! shift positions relative to disk — would have its cache entry silently
//! overwritten by the disk-parsed (differently-positioned) version. Because
//! `pattern_cache` is checked FIRST on every `get_patterns` call (see
//! `handle_get_patterns`), that stale entry then wins over the live buffer
//! until the user's next edit evicts it — goto/hover dead in exactly the
//! file being worked on.
//!
//! This test reproduces the sequence directly against the real
//! `SalsaHandle` (the same layer `register_project_files_with_salsa`'s warm
//! task drives), mirroring how `factory_goto_def_handler.rs` primes the
//! actor without going through the full LSP open/index dance. Deterministic:
//! no sleeps, no real warm timing — `bulk_import_patterns` is the exact unit
//! under test.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{ConfigReferenceData, ParsedPatternsData};
use std::path::PathBuf;
use tempfile::TempDir;
use tower_lsp::lsp_types::Url;
use tower_lsp::LspService;

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

const DISK_SRC: &str = "<?php\nconfig('app.name');\n";

/// Same content with an extra line prepended — every existing position
/// shifts down by one line, exactly what an unsaved edit at the top of a
/// file does.
const BUFFER_SRC: &str = "<?php\n// unsaved edit\nconfig('app.name');\n";

fn config_ref_at_line(data: &ParsedPatternsData, line: u32) -> Option<&ConfigReferenceData> {
    data.config_refs
        .iter()
        .map(|r| r.as_ref())
        .find(|r| r.line == line)
}

#[tokio::test]
async fn bulk_import_skips_open_buffer_and_position_still_resolves() {
    let dir = TempDir::new().unwrap();
    let path: PathBuf = dir.path().join("config_probe.php");
    std::fs::write(&path, DISK_SRC).unwrap();

    let server = test_server();

    // Simulate `did_open` with unsaved edits: register the buffer text with
    // the salsa layer AND the server's `documents` map — the latter is what
    // `bulk_import_patterns` consults via `SalsaHandle::set_documents`
    // (wired in `LaravelLanguageServer::new`).
    server
        .salsa
        .update_file(path.clone(), 1, BUFFER_SRC.to_string())
        .await
        .unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    server
        .documents
        .write()
        .await
        .insert(uri, (BUFFER_SRC.to_string(), 1));

    // Sanity: before any warm import, the live buffer parses with the
    // shifted position (line 2, 0-based) — proves the fixture is set up as
    // intended.
    let live = server
        .salsa
        .get_patterns(path.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        config_ref_at_line(&live, 2).is_some(),
        "buffer text parses with the config() call on the shifted line"
    );

    // Register the project so the actor publishes the shared pattern cache
    // (bulk_import_patterns errors before publication — the real warm only
    // runs after project registration, and this test mirrors that order).
    server
        .salsa
        .register_project_files(
            dir.path().to_path_buf(),
            vec![std::path::PathBuf::from("app/Http/Controllers")],
            vec![dir.path().join("resources/views")],
            None,
            std::path::PathBuf::from("routes"),
            // The shared vendor walk the production caller passes in
            // (issue #371) — built from the same root, so the actor
            // registers exactly what it would in production.
            laravel_lsp::vendor_index::VendorIndex::build(dir.path())
                .files()
                .iter()
                .map(|f| f.path.clone())
                .collect(),
        )
        .await
        .unwrap();

    // Simulate the warm's disk-parse step for the SAME file: parse the
    // UNEDITED disk content, exactly as `register_project_files_with_salsa`'s
    // warm task does via `parse_owned_with_hierarchy`, then hand it to the
    // exact function under test.
    let (disk_data, _hierarchy) =
        laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(&path, DISK_SRC);
    assert!(
        config_ref_at_line(&disk_data, 1).is_some(),
        "disk text parses with the config() call one line up from the buffer"
    );
    server
        .salsa
        .bulk_import_patterns(vec![(path.clone(), disk_data)])
        .await
        .unwrap();

    // The guard must have skipped the open path: a lookup at the buffer's
    // (shifted) position still resolves. Without the fix,
    // `bulk_import_patterns` would have overwritten the cache with the
    // disk-parsed entry, and this lookup at line 2 would come back `None`
    // (the stale entry only has the reference at line 1).
    let after_warm = server
        .salsa
        .get_patterns(path.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        config_ref_at_line(&after_warm, 2).is_some(),
        "position lookup at the open buffer's line must still resolve after the warm import \
         (would be None without the open-buffer guard)"
    );
}
