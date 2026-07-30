//! Tests for the Inertia diagnostic → code-action flow (issue #10).
//!
//! Two seams are covered here:
//!
//! 1. **Emission** — [`LaravelLanguageServer::inertia_not_found_diagnostic`] is
//!    the pure decision + message builder the server runs for every Inertia page
//!    reference. It emits an `Inertia page not found` ERROR (carrying the page
//!    name and the dominant-extension create path) only when the page is
//!    unresolved and its name is valid. Mirrors the `route_not_found_diagnostics`
//!    convention (the filesystem probe stays in the caller).
//! 2. **Parsing** — [`FileAction::from_diagnostic`] parses that message back into
//!    the "Create page" action the code-action handler turns into a workspace
//!    edit. The final test round-trips emission → parse so the two halves can't
//!    drift apart.

use crate::{FileAction, FileActionType, LaravelLanguageServer};
use laravel_lsp::salsa_impl::InertiaReferenceData;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::DiagnosticSeverity;

/// The diagnostic message the server builds for a missing Inertia page — see
/// [`LaravelLanguageServer::inertia_not_found_diagnostic`].
fn diagnostic(name: &str, expected_path: &str) -> String {
    format!("Inertia page not found: '{name}'\nExpected at: {expected_path}")
}

/// A page reference at a fixed position, for emission tests.
fn page_ref(name: &str) -> InertiaReferenceData {
    InertiaReferenceData {
        name: name.to_string(),
        line: 3,
        column: 10,
        end_column: 20,
    }
}

// --- Emission seam ---------------------------------------------------------

#[test]
fn missing_page_emits_error_diagnostic() {
    // The core AC assertion: when the server can't resolve a page, it emits an
    // ERROR diagnostic carrying the page name, the dominant-extension expected
    // path, and the reference's range.
    let root = Path::new("/project");
    let r = page_ref("Auth/Login");
    let diag = LaravelLanguageServer::inertia_not_found_diagnostic(&r, false, root, Some("vue"))
        .expect("a missing page must yield a diagnostic");

    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    // Pinned to the literal, not the constant: Zed renders the source to the
    // user, so it's an attribution tag they read. A constant-comparison would
    // stay green through any relabelling.
    assert_eq!(diag.source.as_deref(), Some("laravel-ce"));
    // No structured payload — that absence is what routes a path-based
    // diagnostic to the create-file quick-fixes rather than the query-chain
    // ones, now that both families share a single `source`.
    assert!(diag.data.is_none());
    assert!(diag.message.contains("Auth/Login"), "{}", diag.message);
    assert!(
        diag.message.contains("resources/js/Pages/Auth/Login.vue"),
        "{}",
        diag.message
    );
    // The range must track the reference position so the squiggle lands on the
    // page literal.
    assert_eq!(diag.range.start.line, 3);
    assert_eq!(diag.range.start.character, 10);
    assert_eq!(diag.range.end.character, 20);
}

#[test]
fn resolved_page_emits_no_diagnostic() {
    // The guard against false positives: a page that resolves on disk
    // (`resolved == true`) must never be flagged.
    let root = Path::new("/project");
    let r = page_ref("Dashboard");
    assert!(
        LaravelLanguageServer::inertia_not_found_diagnostic(&r, true, root, Some("vue")).is_none(),
        "an existing page must not be flagged"
    );
}

#[test]
fn traversing_page_name_yields_no_diagnostic() {
    // An invalid (traversing) page name has no actionable create path, so the
    // server emits nothing rather than an unactionable error.
    let root = Path::new("/project");
    let r = page_ref("../../etc/passwd");
    assert!(
        LaravelLanguageServer::inertia_not_found_diagnostic(&r, false, root, None).is_none(),
        "an invalid page name must not produce a diagnostic"
    );
}

// --- Parsing seam ----------------------------------------------------------

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

// --- Emission → parse round-trip (real dominant-extension selection) --------

#[test]
fn dominant_extension_flows_from_emission_through_to_create_action() {
    // Exercises the *real* selection seam, not a hardcoded string: the server
    // builds the "Expected at" path with the project's dominant extension
    // (via `page_create_path` inside the emission builder), and the parser turns
    // that same message into a create action of the matching type. A regression
    // in either the emission builder or the parser breaks this round-trip.
    let root = Path::new("/project");
    let r = page_ref("Dashboard");
    let diag = LaravelLanguageServer::inertia_not_found_diagnostic(&r, false, root, Some("tsx"))
        .expect("a missing page must yield a diagnostic");

    let action = &FileAction::from_diagnostic(&diag.message)[0];
    assert!(matches!(action.action_type, FileActionType::InertiaPage));
    assert_eq!(action.name, "Dashboard");
    assert_eq!(
        action.target_path.extension().and_then(|e| e.to_str()),
        Some("tsx"),
        "the create action must carry the dominant extension chosen upstream"
    );
    assert!(
        action
            .target_path
            .ends_with("resources/js/Pages/Dashboard.tsx"),
        "{:?}",
        action.target_path
    );
}
