//! Idempotency of `init_database_schema_provider`.
//!
//! The method is called from two startup sites (the config-discovery flow
//! and the `initialized` background task). Before the guard, each call spun
//! up its own provider + refresh loop + channel listener + breaker, and two
//! fresh breakers (each starting Closed) each fired a Closed→Open toast on
//! the first failure — the user saw DUPLICATE "can't reach the database"
//! warnings, and the first loop leaked (both kept probing every 30s).
//!
//! These tests assert the guard collapses that to exactly one live set of
//! tasks: a second init for the SAME root is a no-op, and a different root
//! aborts the old tasks and replaces them. No live DB is involved — a temp
//! dir has no `config/database.php`, so the refresh loop is never spawned
//! (only the always-on channel listener), and nothing connects.

use crate::LaravelLanguageServer;
use tempfile::TempDir;
use tower_lsp::LspService;

/// Build a backend for handler-level tests: `LspService::new` wires up a
/// real `Client`, and `inner().clone()` hands back the
/// `LaravelLanguageServer` so we can call its private methods. Mirrors the
/// `minimal_backend()` harness in `blade_var_rename_handler.rs`.
fn backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// The task `Id`s currently stored in the idempotency guard (one per live
/// background task). Fails if no tasks have been stored yet.
async fn stored_task_ids(b: &LaravelLanguageServer) -> Vec<tokio::task::Id> {
    let guard = b.db_provider_task.lock().await;
    let (_root, handles) = guard.as_ref().expect("init should have stored tasks");
    handles.iter().map(|h| h.id()).collect()
}

#[tokio::test]
async fn init_twice_same_root_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let backend = backend();

    backend.init_database_schema_provider(&root).await;
    let first_ids = stored_task_ids(&backend).await;
    // No config/database.php in a temp dir → no refresh loop, only the
    // always-on channel listener.
    assert_eq!(
        first_ids.len(),
        1,
        "without a DB config only the listener task should run"
    );
    assert!(
        backend.database_schema.read().await.is_some(),
        "the single provider is stored"
    );

    // Second init for the same root: must NOT spawn a second provider/loop/
    // listener/breaker — the stored task set is byte-for-byte the same.
    backend.init_database_schema_provider(&root).await;
    let second_ids = stored_task_ids(&backend).await;
    assert_eq!(
        first_ids, second_ids,
        "a same-root re-init must be a no-op (no new tasks) — this is the \
         duplicate-toast fix"
    );
}

#[tokio::test]
async fn init_different_root_aborts_old_tasks_and_replaces() {
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    let backend = backend();

    backend.init_database_schema_provider(dir1.path()).await;
    let first_ids = stored_task_ids(&backend).await;

    // Re-init for a DIFFERENT root: the guard aborts the old tasks and
    // starts a fresh set, so the stored root and task Ids both change and
    // the task set doesn't accumulate.
    backend.init_database_schema_provider(dir2.path()).await;
    let (second_root, second_ids) = {
        let guard = backend.db_provider_task.lock().await;
        let (root, handles) = guard.as_ref().expect("tasks stored");
        (
            root.clone(),
            handles.iter().map(|h| h.id()).collect::<Vec<_>>(),
        )
    };

    assert_eq!(second_root, dir2.path(), "the stored root is replaced");
    assert_ne!(
        first_ids, second_ids,
        "a different-root re-init spawns fresh tasks (the old ones are aborted)"
    );
    assert_eq!(
        second_ids.len(),
        1,
        "the replacement is clean — tasks don't accumulate across root changes"
    );
}
