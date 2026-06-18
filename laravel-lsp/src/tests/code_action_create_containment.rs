//! Tests for the fail-closed root-containment backstop at the **write/create
//! seam** — `FileAction::build_code_action` (issue #199). This is the third
//! surface in the #130 → #143 → #148 → #194 containment-guard chain, alongside
//! the read/resolve paths: a file-create quick-fix (View, BladeComponent,
//! Livewire, Inertia, …) must never be *offered* for a `target_path` that
//! escapes the project root, so a forged or malformed `laravel` diagnostic can't
//! coax the editor into creating a file outside the workspace.
//!
//! The guard runs before any `ResourceOp::Create` is constructed: if the project
//! root is known and `target_path` resolves outside it, `build_code_action`
//! returns `None` (no action offered) instead of an out-of-root create.
//!
//! Why `path_within_root_lexical` and NOT the fail-closed `path_within_root` the
//! sibling *read* paths use: a create target does not exist yet, so
//! `path.canonicalize()` always fails for it and the fail-closed guard would
//! refuse **every** create — including legitimate in-root ones (the positive
//! control below would fail). The lexical guard refuses out-of-root and
//! interior-`..` escapes while admitting a not-yet-created in-root target, and
//! still canonicalizes to catch a symlink escape when the target *does* exist —
//! exactly the contract a speculative emitted path needs.
//!
//! These tests construct a `FileAction` directly (a private struct, reachable
//! from the in-crate test module) and call the private `build_code_action`,
//! asserting only on whether an action is offered. Each negative test would pass
//! `Some(_)` — i.e. fail — if the guard were removed, so none is vacuous.

use crate::{FileAction, FileActionType};
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// A minimal `laravel`-source diagnostic. `build_code_action` only clones it into
/// the emitted `CodeAction`; the containment backstop runs before it is read, so
/// its contents are irrelevant to what these tests assert.
fn diagnostic() -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
        severity: Some(DiagnosticSeverity::WARNING),
        code: None,
        source: Some("laravel".to_string()),
        message: "View file not found: 'welcome'".to_string(),
        related_information: None,
        tags: None,
        code_description: None,
        data: None,
    }
}

/// A `View` create action targeting `target` — the simplest action type, routed
/// through the standard file-creation branch. `file_exists: false` because the
/// whole point of a create action is that the target does not exist yet.
fn view_action(target: PathBuf) -> FileAction {
    FileAction {
        action_type: FileActionType::View,
        name: "welcome".to_string(),
        target_path: target,
        file_exists: false,
        copy_from: None,
    }
}

fn build(action: &FileAction, root: &Path) -> Option<()> {
    action
        .build_code_action("<div></div>\n".to_string(), &diagnostic(), Some(root))
        .map(|_| ())
}

#[test]
fn out_of_root_create_action_returns_none() {
    // The diagnostic's expected target lives OUTSIDE the project root (a forged or
    // malformed `Expected at:` path). The create action must not be offered — the
    // containment backstop refuses it before constructing the `ResourceOp::Create`.
    // Would return `Some(_)` (fail) without the guard.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    let target = outside.path().join("resources/views/welcome.blade.php");

    assert!(
        build(&view_action(target), root.path()).is_none(),
        "a create action whose target escapes the project root must not be \
         offered — the containment backstop refuses it before building the \
         ResourceOp::Create"
    );
}

#[test]
fn interior_dotdot_create_target_returns_none() {
    // A target that is textually prefixed by the root but escapes it through an
    // interior `..` (`<root>/../escape.blade.php`). `Path::starts_with` is
    // component-wise and would be fooled by the `<root>/` prefix; the lexical
    // guard's `normalize_path` collapses the `..` first, so the escape is refused.
    // Would return `Some(_)` (fail) without the guard.
    let root = tempfile::TempDir::new().unwrap();
    let target = root.path().join("..").join("escape.blade.php");

    assert!(
        build(&view_action(target), root.path()).is_none(),
        "a create target escaping via interior `..` must not be offered — the \
         lexical guard normalizes `..` before the containment check"
    );
}

#[test]
fn in_root_create_action_is_offered() {
    // Positive control: a genuine in-root target that does not exist yet. The
    // lexical guard admits a speculative (not-yet-created) in-root path, so the
    // standard view-creation action IS offered. This proves the guard doesn't
    // regress legitimate creates — and is exactly the case the fail-closed
    // `path_within_root` would have wrongly refused (the create target can't
    // canonicalize because it doesn't exist).
    let root = tempfile::TempDir::new().unwrap();
    let target = root.path().join("resources/views/welcome.blade.php");
    assert!(
        !target.exists(),
        "the create target must not exist yet — that's what 'create' means"
    );

    assert!(
        build(&view_action(target), root.path()).is_some(),
        "a valid in-root create target must still be offered as a code action"
    );
}

#[cfg(unix)]
#[test]
fn under_root_symlink_create_target_returns_none() {
    // The discriminating case a purely-textual check can't catch: the target is an
    // existing under-root symlink (`<root>/resources/views/welcome.blade.php`)
    // whose real target is OUTSIDE the root. The link path is lexically inside the
    // root, so it passes the lexical gate — only canonicalization, resolving the
    // symlink to its out-of-tree target, refuses it. Offering this create would
    // let a forged diagnostic point a "create" at a path that escapes the
    // workspace. Would return `Some(_)` (fail) without the canonicalize step.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // A real file outside the root, and an under-root symlink pointing at it.
    let outside_file = outside.path().join("welcome.blade.php");
    std::fs::write(&outside_file, "<div></div>\n").unwrap();
    let views = root.path().join("resources/views");
    std::fs::create_dir_all(&views).unwrap();
    let link = views.join("welcome.blade.php");
    std::os::unix::fs::symlink(&outside_file, &link).unwrap();

    assert!(
        build(&view_action(link), root.path()).is_none(),
        "a create target reached through an under-root symlink that resolves \
         outside the root must not be offered — canonicalization refuses it even \
         though the link path is lexically inside"
    );
}
