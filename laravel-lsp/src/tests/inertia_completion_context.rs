//! Tests for the Inertia completion context (issue #10).
//!
//! `get_inertia_call_context` detects when the cursor sits inside the page-name
//! string of an Inertia call and reports the partial page name plus the text
//! range to replace. These cover the three call sites that drive page-name
//! completion: `inertia('…')`, `Inertia::render('…')`, and the second-argument
//! `Route::inertia('/path', '…')`.

use crate::LaravelLanguageServer;

/// The partial page name detected at the end of `line`.
fn prefix(line: &str) -> Option<String> {
    LaravelLanguageServer::get_inertia_call_context(line, line.len() as u32).map(|c| c.prefix)
}

#[test]
fn helper_call_detected() {
    assert_eq!(prefix("return inertia('Dash").as_deref(), Some("Dash"));
    assert_eq!(prefix("return inertia(\"Dash").as_deref(), Some("Dash"));
}

#[test]
fn facade_render_detected() {
    assert_eq!(
        prefix("Inertia::render('Auth/Log").as_deref(),
        Some("Auth/Log")
    );
    assert_eq!(
        prefix("\\Inertia\\Inertia::render('Auth/Log").as_deref(),
        Some("Auth/Log"),
    );
}

#[test]
fn route_inertia_second_argument_detected() {
    // The page name is the SECOND argument — the URI in the first argument must
    // not be treated as the completion target.
    assert_eq!(
        prefix("Route::inertia('/welcome', 'Wel").as_deref(),
        Some("Wel"),
    );
    assert_eq!(
        prefix("Route::inertia('/welcome', \"Wel").as_deref(),
        Some("Wel"),
    );
}

#[test]
fn route_inertia_first_argument_is_not_a_page_context() {
    // Cursor inside the URI (first argument) of Route::inertia must not offer
    // page completions.
    assert!(prefix("Route::inertia('/wel").is_none());
}

#[test]
fn reports_replacement_range_after_opening_quote() {
    // `inertia('Dash` — the page string content starts right after the quote at
    // index 9, and with no closing quote the range ends at the cursor.
    let line = "inertia('Dash";
    let c = LaravelLanguageServer::get_inertia_call_context(line, line.len() as u32).unwrap();
    assert_eq!(c.prefix, "Dash");
    assert_eq!(c.start_col, 9);
    assert_eq!(c.end_col, line.len() as u32);
}

#[test]
fn not_inside_a_string_returns_none() {
    // Closed string, and plain code with no Inertia call.
    assert!(prefix("inertia('Dashboard')").is_none());
    assert!(prefix("$x = 1 + 2").is_none());
}
