//! Per-site discovered-path containment for the `controllers_dir`
//! `follow_links(true)` walk in `check_controller_view_variable` (`main.rs`),
//! extending the `path_within_root` containment lineage
//! (#130 → #143 → #148 → #194 → #199 → #201 → #214 → #218 → #222 → #226 → #228)
//! to the controllers-walk leg (issue #230, follow-up from Holmes's review of
//! PR #229).
//!
//! PR #229 (issue #228) made the controllers walk a *confirmed gated site*: it
//! now runs every discovered entry through `path_within_root_walk_entry` before
//! the out-of-root read primitive (`std::fs::read_to_string(entry.path())`).
//! That walk starts from the in-root `app/Http/Controllers`, but
//! `WalkDir::new(dir).follow_links(true)` would otherwise follow a symlink
//! *inside* it whose target escapes the project root and read the controllers it
//! surfaces — leaking out-of-root file contents into view-variable type
//! inference. The unit tests in `path_containment.rs` cover the gate function
//! and `tests/scan_dir_containment.rs` covers the `scan_dir` leg, but this
//! specific call site (right root threaded, gate actually drops an escaping
//! entry in *this* walk) had no end-to-end test. These close that gap, mirroring
//! `scan_dir_containment.rs`.
//!
//! Each negative case is *discriminating*: the escaping controller is written to
//! disk OUTSIDE the root and reached through an under-root symlink, so without
//! the gate `follow_links(true)` would surface it and infer its view variable's
//! type. The result being `None` can therefore only come from the containment
//! gate, never from the file not existing — the precondition assertions make
//! that explicit, and an in-root sentinel controller (queried separately) proves
//! the walk still runs and reads in-root files. The positive controls prove the
//! gate does not over-refuse: an in-root symlink (target inside the root) and an
//! ordinary nested subdirectory still resolve their variables.
//!
//! `check_controller_view_variable(root, view_name, var_name)` returns the
//! inferred type of `var_name` for the controller that renders `view_name`. The
//! controllers are crafted so a typed method parameter (`show(Account $account)`)
//! passed via `view('accounts.show', ['account' => $account])` resolves to the
//! class name — so a non-`None` return is unambiguous proof the file was read.

use crate::LaravelLanguageServer;
use tower_lsp::LspService;

/// Build a bare server. `LspService::new` wires a real `Client`;
/// `inner().clone()` hands back the `LaravelLanguageServer` (all-`Arc` state, so
/// the clone shares it). `check_controller_view_variable` reads none of the
/// primed state — `root`, `view_name`, and `var_name` are all passed as
/// arguments — so nothing needs priming; the walk runs purely against the
/// tempdir on disk.
async fn bare_backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A controller that renders `view` passing `var` (typed `ty` via a method
/// parameter). `check_controller_view_variable(root, view, var)` resolves this
/// to `Some(ty)` once the file is read — the class name is irrelevant to the
/// regex-based inference, so a fixed name keeps the fixtures terse.
fn controller_php(view: &str, var: &str, ty: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Http\\Controllers;\n\n\
         class SomeController\n{{\n    \
         public function show({ty} ${var})\n    {{\n        \
         return view('{view}', ['{var}' => ${var}]);\n    }}\n}}\n"
    )
}

// ---------------------------------------------------------------------------
// Negative (unix): a controller reached through an under-root symlink whose
// target escapes the root is NOT read
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn controllers_walk_drops_entry_through_under_root_symlink_escaping_root() {
    // The walk root `<root>/app/Http/Controllers` is in-root, but it contains a
    // symlink `Escape` -> `<tmp>/outside`. `follow_links(true)` descends it and
    // would surface `<root>/app/Http/Controllers/Escape/SecretController.php`,
    // whose real path is `<tmp>/outside/SecretController.php` — outside the
    // project root. The gate must drop it before `read_to_string`. A legitimate
    // in-root `AccountController.php` is included so the walk is proven to run
    // and read: only the escaping controller's inference must come back `None`.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let controllers = root.join("app").join("Http").join("Controllers");
    std::fs::create_dir_all(&controllers).unwrap();
    std::fs::write(
        controllers.join("AccountController.php"),
        controller_php("accounts.show", "account", "Account"),
    )
    .unwrap();

    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("SecretController.php");
    std::fs::write(&secret, controller_php("secrets.leak", "ssn", "Ssn")).unwrap();

    let escape_link = controllers.join("Escape");
    std::os::unix::fs::symlink(&outside, &escape_link).unwrap();

    // Precondition: the escaping controller exists through the symlink and
    // resolves OUTSIDE the root, so a `None` inference can only be the gate
    // refusing it.
    let escaping_entry = controllers.join("Escape").join("SecretController.php");
    assert_eq!(
        escaping_entry.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the discovered controller resolves through the symlink to \
         the out-of-root file"
    );
    assert!(
        !escaping_entry
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the resolved controller escapes the project root"
    );

    let backend = bare_backend().await;

    // The in-root sentinel proves the walk ran and read an in-root controller.
    assert_eq!(
        backend.check_controller_view_variable(&root, "accounts.show", "account"),
        Some("Account".to_string()),
        "the in-root controller must still be read — the walk ran"
    );

    // The escaping controller must NOT be read: its target exists outside the
    // root (precondition above), so a `None` here proves the gate fired rather
    // than the file being missing.
    assert_eq!(
        backend.check_controller_view_variable(&root, "secrets.leak", "ssn"),
        None,
        "a controller reached through an under-root symlink resolving outside the \
         root must NOT be read"
    );
}

// ---------------------------------------------------------------------------
// Positive control (unix): a controller reached through an IN-root symlink
// (target inside the root) is still read — the gate does not over-refuse
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn controllers_walk_still_reads_entry_through_in_root_symlink() {
    // A symlink `<root>/app/Http/Controllers/Linked` -> `<root>/extra-controllers`
    // (inside the root). `follow_links(true)` surfaces `.../Linked/ProfileController.php`,
    // which canonicalizes to `<root>/extra-controllers/ProfileController.php` —
    // inside the root — so it must still be read. This pins that the
    // canonicalize-based gate refuses *escapes*, not every symlink.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let controllers = root.join("app").join("Http").join("Controllers");
    std::fs::create_dir_all(&controllers).unwrap();

    let real = root.join("extra-controllers");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(
        real.join("ProfileController.php"),
        controller_php("profiles.show", "profile", "Profile"),
    )
    .unwrap();

    let link = controllers.join("Linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Precondition: the controller resolves through the symlink to a path INSIDE
    // the root, so reading it proves the gate admitted an in-root symlink.
    let entry = controllers.join("Linked").join("ProfileController.php");
    assert!(
        entry
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the controller resolves through the in-root symlink, \
         staying inside the root"
    );

    let backend = bare_backend().await;
    assert_eq!(
        backend.check_controller_view_variable(&root, "profiles.show", "profile"),
        Some("Profile".to_string()),
        "a controller reached through an in-root symlink (target inside the root) \
         must still be read — the gate must not over-refuse"
    );
}

// ---------------------------------------------------------------------------
// Positive control: an ordinary in-root nested subdirectory is still walked
// ---------------------------------------------------------------------------

#[tokio::test]
async fn controllers_walk_still_reads_ordinary_in_root_subdir() {
    // No symlinks at all: a plain nested subdirectory controller must still be
    // read after the gate is wired in (the common case must not regress).
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let controllers = root.join("app").join("Http").join("Controllers");
    std::fs::create_dir_all(controllers.join("Admin")).unwrap();
    std::fs::write(
        controllers.join("Admin").join("StatsController.php"),
        controller_php("admin.dashboard", "stats", "Stats"),
    )
    .unwrap();

    let backend = bare_backend().await;
    assert_eq!(
        backend.check_controller_view_variable(&root, "admin.dashboard", "stats"),
        Some("Stats".to_string()),
        "an ordinary in-root controller in a nested subdir must still be read \
         after the containment gate is applied"
    );
}
