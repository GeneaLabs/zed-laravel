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
//! returns `None` (no action offered) instead of an out-of-root create. The
//! multi-file types (`Livewire`, `BladeComponentWithClass`) emit a SECOND create
//! beyond `target_path` — a view/class path derived from the independent `name`
//! field — so each of those sibling paths is guarded the same way; a forged `name`
//! cannot escape the root past the in-root `target_path` check.
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

/// A `Livewire` create action. This is a MULTI-FILE type: it emits `target_path`
/// (the PHP class) AND a second create — the Blade view at
/// `get_livewire_view_path`, which is derived from `name`, NOT from `target_path`.
/// `name` and `target_path` are independent diagnostic fields, so they're set
/// separately here to exercise the second-file containment guard.
fn livewire_action(name: &str, target: PathBuf) -> FileAction {
    FileAction {
        action_type: FileActionType::Livewire,
        name: name.to_string(),
        target_path: target,
        file_exists: false,
        copy_from: None,
    }
}

/// A `BladeComponentWithClass` create action. Also MULTI-FILE: it emits
/// `target_path` (the Blade view) AND a second create — the PHP class at
/// `get_component_class_path`, derived from `name`. Same independence of `name`
/// and `target_path` as `livewire_action`.
fn blade_component_with_class_action(name: &str, target: PathBuf) -> FileAction {
    FileAction {
        action_type: FileActionType::BladeComponentWithClass,
        name: name.to_string(),
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

// --- Multi-file create actions (issue #199) -------------------------------
//
// `Livewire` and `BladeComponentWithClass` each emit a SECOND `ResourceOp::Create`
// beyond `target_path` — a view/class path derived from the *independent* `name`
// diagnostic field. The `target_path` guard alone does not cover it: because
// `PathBuf::join`/`push` of an absolute-looking segment *replaces* the base, a
// forged `name` escapes the root even when `target_path` is in-root. These tests
// pin an in-root `target_path` (so the first guard passes) and an escaping `name`,
// proving the second-file guard fires. Each negative would return `Some(_)` —
// i.e. fail — if its sibling guard were removed, so none is vacuous.

#[test]
fn livewire_out_of_root_view_path_returns_none() {
    // In-root `target_path` (passes the first guard), but `name = "/etc/passwd"`
    // makes `get_livewire_view_path` emit `/etc/passwd.blade.php` — the absolute
    // segment replaces the `resources/views/livewire` base, escaping the root. The
    // second-file guard must refuse to offer the action.
    let root = tempfile::TempDir::new().unwrap();
    let target = root.path().join("app/Livewire/Counter.php");

    assert!(
        build(&livewire_action("/etc/passwd", target), root.path()).is_none(),
        "a Livewire action whose name-derived view path escapes the root must not \
         be offered, even when target_path is in-root — the second create is guarded"
    );
}

#[test]
fn blade_component_with_class_out_of_root_class_path_returns_none() {
    // In-root `target_path` (the Blade view, passes the first guard), but
    // `name = "/etc/passwd"` makes `get_component_class_path` emit `/etc/passwd.php`
    // — the absolute segment replaces the `app/View/Components` base, escaping the
    // root. The second-file guard must refuse to offer the action.
    let root = tempfile::TempDir::new().unwrap();
    let target = root
        .path()
        .join("resources/views/components/card.blade.php");

    assert!(
        build(
            &blade_component_with_class_action("/etc/passwd", target),
            root.path()
        )
        .is_none(),
        "a BladeComponentWithClass action whose name-derived class path escapes the \
         root must not be offered, even when target_path is in-root — the second \
         create is guarded"
    );
}

#[test]
fn livewire_in_root_is_offered() {
    // Positive control: both the in-root `target_path` (PHP class) and the
    // name-derived view path (`resources/views/livewire/counter.blade.php`) stay
    // inside the root, so the multi-file action IS offered — the sibling guard does
    // not regress legitimate Livewire creates.
    let root = tempfile::TempDir::new().unwrap();
    let target = root.path().join("app/Livewire/Counter.php");

    assert!(
        build(&livewire_action("counter", target), root.path()).is_some(),
        "a valid in-root Livewire create (both files under the root) must still be \
         offered as a code action"
    );
}

#[test]
fn blade_component_with_class_in_root_is_offered() {
    // Positive control: both the in-root `target_path` (Blade view) and the
    // name-derived class path (`app/View/Components/Card.php`) stay inside the root,
    // so the multi-file action IS offered — the sibling guard does not regress
    // legitimate component-with-class creates.
    let root = tempfile::TempDir::new().unwrap();
    let target = root
        .path()
        .join("resources/views/components/card.blade.php");

    assert!(
        build(
            &blade_component_with_class_action("card", target),
            root.path()
        )
        .is_some(),
        "a valid in-root BladeComponentWithClass create (both files under the root) \
         must still be offered as a code action"
    );
}
