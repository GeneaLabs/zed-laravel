//! `try_discover_from_file` must not hijack the active Laravel project root
//! when the discovered root is just another git worktree of the same repo
//! (issue: opening a file inside a Claude Code agent worktree under
//! `.claude/worktrees/<name>/` — or any manually-created sibling worktree —
//! forced a full re-index and aborted an in-flight DB connection, because
//! every worktree is a full checkout and therefore carries its own
//! `composer.json` + `artisan`, tripping the "more specific root" heuristic
//! as a false positive).
//!
//! `config::is_same_git_repo` (unit-tested directly in `config/tests.rs`) is
//! the guard; this test proves it's actually wired into the call site.

use crate::LaravelLanguageServer;
use tempfile::TempDir;
use tower_lsp::LspService;

/// Mirrors the `backend()` harness in `db_provider_init_idempotency.rs`.
fn backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Turns `worktree_root` into a linked worktree of `main_root`, reproducing
/// the on-disk layout `git worktree add` produces: a `.git` *file* in the
/// worktree pointing at an admin dir under the main repo's
/// `.git/worktrees/<name>/`, which names the shared `.git` dir via a
/// `commondir` file.
fn link_worktree(main_root: &std::path::Path, worktree_root: &std::path::Path, name: &str) {
    let admin_dir = main_root.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&admin_dir).unwrap();
    std::fs::write(admin_dir.join("commondir"), "../..\n").unwrap();

    std::fs::create_dir_all(worktree_root).unwrap();
    std::fs::write(
        worktree_root.join(".git"),
        format!("gitdir: {}\n", admin_dir.display()),
    )
    .unwrap();
}

#[tokio::test]
async fn keeps_root_when_opened_file_is_in_a_worktree_of_the_active_repo() {
    let tmp = TempDir::new().unwrap();

    let main_root = tmp.path().join("project");
    std::fs::create_dir_all(main_root.join(".git")).unwrap();
    std::fs::write(main_root.join("composer.json"), "{}").unwrap();
    std::fs::write(main_root.join("artisan"), "").unwrap();

    // A nested worktree, the shape Claude Code creates for agent branches.
    let worktree_root = main_root
        .join(".claude")
        .join("worktrees")
        .join("gifted-heisenberg-3c0899");
    link_worktree(&main_root, &worktree_root, "gifted-heisenberg-3c0899");
    std::fs::write(worktree_root.join("composer.json"), "{}").unwrap();
    std::fs::write(worktree_root.join("artisan"), "").unwrap();

    let file = worktree_root
        .join("app")
        .join("Notifications")
        .join("ReleaseNotification.php");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "<?php\n").unwrap();

    let backend = backend();
    *backend.root_path.write().await = Some(main_root.clone());

    backend.try_discover_from_file(&file).await;

    assert_eq!(
        *backend.root_path.read().await,
        Some(main_root),
        "a file opened inside a worktree of the already-active repo must not \
         switch the project root — same project, not a nested one"
    );
}

#[tokio::test]
async fn self_corrects_from_linked_worktree_back_to_main() {
    // Confirmed live in a real session: once `root_path` drifted onto a
    // linked worktree (however that happened — e.g. an agent touching a
    // worktree file before the user opened anything in main), the original
    // "keep current root" guard made it sticky FOREVER, because every file
    // in either tree looks like "same repo, don't switch". A linked
    // worktree is a point-in-time snapshot of its branch — a model class
    // added on `main` after the worktree branched off is invisible from
    // inside it, so hover/goto for that class silently failed for the rest
    // of the session with no way to recover short of restarting the LSP.
    // Opening a file that lives in `main` must pull the root back.
    let tmp = TempDir::new().unwrap();

    let main_root = tmp.path().join("project");
    std::fs::create_dir_all(main_root.join(".git")).unwrap();
    std::fs::write(main_root.join("composer.json"), "{}").unwrap();
    std::fs::write(main_root.join("artisan"), "").unwrap();

    let worktree_root = main_root
        .join(".claude")
        .join("worktrees")
        .join("gifted-heisenberg-3c0899");
    link_worktree(&main_root, &worktree_root, "gifted-heisenberg-3c0899");
    std::fs::write(worktree_root.join("composer.json"), "{}").unwrap();
    std::fs::write(worktree_root.join("artisan"), "").unwrap();

    // A file that exists only in `main` (the worktree's branch never got
    // it) — the exact shape of the real failure.
    let file = main_root
        .join("app")
        .join("Models")
        .join("ReleaseNoteGroup.php");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "<?php\n").unwrap();

    let backend = backend();
    // Root starts stuck on the linked worktree, as it would after the
    // original hijack (or, as observed live, after startup for reasons
    // upstream of this guard).
    *backend.root_path.write().await = Some(worktree_root);

    backend.try_discover_from_file(&file).await;

    assert_eq!(
        *backend.root_path.read().await,
        Some(main_root),
        "opening a file that resolves to the main worktree must self-correct \
         back to it — a linked worktree must never be permanently sticky"
    );
}

#[tokio::test]
async fn switches_root_for_a_file_outside_any_known_repo() {
    // Negative control: two unrelated directories (neither is a worktree of
    // the other, and neither carries git plumbing at all) must still trigger
    // the pre-existing "file outside root" switch — the new guard only
    // suppresses the same-repo-worktree case, nothing else.
    let tmp = TempDir::new().unwrap();

    let root_a = tmp.path().join("project-a");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::write(root_a.join("composer.json"), "{}").unwrap();
    std::fs::write(root_a.join("artisan"), "").unwrap();

    let root_b = tmp.path().join("project-b");
    std::fs::create_dir_all(&root_b).unwrap();
    std::fs::write(root_b.join("composer.json"), "{}").unwrap();
    std::fs::write(root_b.join("artisan"), "").unwrap();
    let file_b = root_b.join("routes").join("web.php");
    std::fs::create_dir_all(file_b.parent().unwrap()).unwrap();
    std::fs::write(&file_b, "<?php\n").unwrap();

    let backend = backend();
    *backend.root_path.write().await = Some(root_a);

    backend.try_discover_from_file(&file_b).await;

    assert_eq!(
        *backend.root_path.read().await,
        Some(root_b),
        "an unrelated project outside the current root must still switch, \
         same as before this fix"
    );
}
