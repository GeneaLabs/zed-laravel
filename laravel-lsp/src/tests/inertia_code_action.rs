//! Tests for the Inertia diagnostic → code-action flow (issue #10).
//!
//! When a page can't be resolved the server emits a "Inertia page not found"
//! ERROR diagnostic carrying the page name and the expected create path (built
//! with the project's dominant extension). `FileAction::from_diagnostic` parses
//! that message back into the "Create page" action that the code-action handler
//! turns into a workspace edit. These tests exercise that parsing seam — the
//! same `from_diagnostic` the handler calls in `code_action`.

use crate::{FileAction, FileActionType};
use std::path::PathBuf;

/// The diagnostic message the server builds for a missing Inertia page — see
/// the `inertia_refs` loop in `publish_diagnostics`.
fn diagnostic(name: &str, expected_path: &str) -> String {
    format!("Inertia page not found: '{name}'\nExpected at: {expected_path}")
}

#[test]
fn missing_page_yields_create_page_action() {
    let msg = diagnostic("Auth/Login", "/project/resources/js/Pages/Auth/Login.vue");
    let actions = FileAction::from_diagnostic(&msg);

    assert_eq!(actions.len(), 1, "exactly one create-page action");
    let action = &actions[0];
    assert!(matches!(action.action_type, FileActionType::InertiaPage));
    assert_eq!(action.name, "Auth/Login");
    assert_eq!(
        action.target_path,
        PathBuf::from("/project/resources/js/Pages/Auth/Login.vue"),
    );
    assert!(
        !action.file_exists,
        "a missing page is, by definition, not on disk"
    );
}

#[test]
fn target_path_carries_the_dominant_extension() {
    // The "Expected at" path is built upstream with the dominant project
    // extension; the action must create a file of that exact type.
    let msg = diagnostic("Dashboard", "/project/resources/js/Pages/Dashboard.tsx");
    let action = &FileAction::from_diagnostic(&msg)[0];
    assert_eq!(
        action.target_path.extension().and_then(|e| e.to_str()),
        Some("tsx"),
    );
}

#[test]
fn nested_page_name_round_trips() {
    let msg = diagnostic(
        "Settings/Profile/Edit",
        "/project/resources/js/Pages/Settings/Profile/Edit.vue",
    );
    let action = &FileAction::from_diagnostic(&msg)[0];
    assert_eq!(action.name, "Settings/Profile/Edit");
}

#[test]
fn message_without_expected_path_yields_no_action() {
    // No "Expected at:" line → nothing actionable, so no code action is offered.
    let actions = FileAction::from_diagnostic("Inertia page not found: 'Orphan'");
    assert!(actions.is_empty());
}
