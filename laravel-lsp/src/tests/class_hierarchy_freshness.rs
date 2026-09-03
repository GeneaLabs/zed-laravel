//! The class-hierarchy index stays current WITHOUT a deferred-refresh queue
//! (issue #371).
//!
//! `ClassHierarchyIndex` carried a `mark_dirty` / `take_dirty` pair copied from
//! `symbol_index`. `mark_dirty` ran on every `update_file` and on the
//! `did_close` re-queue; `take_dirty` was never called anywhere in production —
//! only from its own unit test. So the set was write-only: it accumulated one
//! `PathBuf` per distinct path touched and was never drained for the life of
//! the session, while doing nothing.
//!
//! Nothing broke, because the index is refreshed EAGERLY somewhere else:
//! `handle_get_patterns` runs `remove_file` + `insert_file` on every PHP parse,
//! and its own comment calls that path "the ONLY populator" for files warming
//! skipped.
//!
//! Deleting a mechanism is only safe if the property it appeared to provide is
//! actually provided elsewhere, so that property is what these tests pin: after
//! an edit, the hierarchy reflects the file's new classes, with nothing
//! draining a dirty set. If the eager refresh in `handle_get_patterns` is ever
//! removed or made conditional, these go red — which is the failure the deleted
//! queue would otherwise have masked responsibility for.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::LspService;

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

const COMPOSER: &str = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;

fn model(class: &str, parent: &str) -> String {
    format!("<?php\nnamespace App\\Models;\nclass {class} extends {parent} {{}}\n")
}

/// Register `src` for `path` and force the parse that refreshes the hierarchy.
async fn update_and_parse(server: &LaravelLanguageServer, path: &Path, src: &str) {
    server
        .salsa
        .update_file(path.to_path_buf(), 1, src.to_string())
        .await
        .unwrap();
    let _ = server.salsa.get_patterns(path.to_path_buf()).await;
}

/// The FQCNs the hierarchy currently attributes to `path`.
async fn fqcns_for(server: &LaravelLanguageServer, path: &Path) -> Vec<String> {
    let mut names: Vec<String> = server
        .salsa
        .snapshot_hierarchy_nodes()
        .await
        .unwrap()
        .get(path)
        .map(|nodes| nodes.iter().map(|n| n.fqcn.clone()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn a_parsed_file_enters_the_hierarchy() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("app/Models")).unwrap();
    fs::write(root.join("composer.json"), COMPOSER).unwrap();
    let server = test_server();
    *server.root_path.write().await = Some(root.to_path_buf());

    let path = root.join("app/Models/Post.php");
    let src = model("Post", "Model");
    fs::write(&path, &src).unwrap();
    update_and_parse(&server, &path, &src).await;

    assert_eq!(
        fqcns_for(&server, &path).await,
        vec!["App\\Models\\Post".to_string()],
        "the eager refresh in handle_get_patterns is what populates the \
         hierarchy — no dirty-set drain is involved"
    );
}

#[tokio::test]
async fn a_renamed_class_replaces_its_predecessor_without_a_dirty_drain() {
    // The discriminating case. A stale hierarchy would keep `Post` alongside
    // `Article`, or keep `Post` alone. Only an eager remove+insert on the parse
    // yields exactly the new name.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("app/Models")).unwrap();
    fs::write(root.join("composer.json"), COMPOSER).unwrap();
    let server = test_server();
    *server.root_path.write().await = Some(root.to_path_buf());

    let path = root.join("app/Models/Post.php");
    let before = model("Post", "Model");
    fs::write(&path, &before).unwrap();
    update_and_parse(&server, &path, &before).await;
    assert_eq!(
        fqcns_for(&server, &path).await,
        vec!["App\\Models\\Post".to_string()],
        "fixture check — the pre-edit state must be indexed"
    );

    let after = model("Article", "Model");
    fs::write(&path, &after).unwrap();
    update_and_parse(&server, &path, &after).await;

    assert_eq!(
        fqcns_for(&server, &path).await,
        vec!["App\\Models\\Article".to_string()],
        "the edit must both add the new class AND evict the old one; a \
         write-only dirty set never did either"
    );
}

#[tokio::test]
async fn an_added_class_is_visible_after_the_edit() {
    // A second declaration appearing in an already-indexed file — the case a
    // deferred queue would have handled at drain time, and which the eager
    // path must handle at parse time instead.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("app/Models")).unwrap();
    fs::write(root.join("composer.json"), COMPOSER).unwrap();
    let server = test_server();
    *server.root_path.write().await = Some(root.to_path_buf());

    let path = root.join("app/Models/Post.php");
    let before = model("Post", "Model");
    fs::write(&path, &before).unwrap();
    update_and_parse(&server, &path, &before).await;

    let after =
        "<?php\nnamespace App\\Models;\nclass Post extends Model {}\nclass Draft extends Post {}\n";
    fs::write(&path, after).unwrap();
    update_and_parse(&server, &path, after).await;

    assert_eq!(
        fqcns_for(&server, &path).await,
        vec![
            "App\\Models\\Draft".to_string(),
            "App\\Models\\Post".to_string()
        ],
        "both declarations must be present after the edit"
    );
}
