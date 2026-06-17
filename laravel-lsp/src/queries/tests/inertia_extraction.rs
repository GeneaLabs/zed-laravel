//! Tests for Inertia page-reference extraction (issue #10).
//!
//! Verifies the tree-sitter queries capture the page-name string from all
//! three Inertia call sites and route them into
//! `ExtractedPhpPatterns.inertia_pages`, so goto / completion / diagnostics
//! dispatch without any per-call-site special-casing downstream.

use super::super::*;
use crate::parser::{language_php, parse_php};

/// The page names extracted from `php`, in source order.
fn pages(php: &str) -> Vec<String> {
    let tree = parse_php(php).expect("Should parse PHP");
    let lang = language_php();
    let patterns = extract_all_php_patterns(&tree, php, &lang).expect("Should extract patterns");
    patterns
        .inertia_pages
        .iter()
        .map(|m| m.page_name.to_string())
        .collect()
}

#[test]
fn helper_call_site() {
    // inertia('Page') — the helper-function form, single- and double-quoted.
    assert_eq!(
        pages(r#"<?php return inertia('Dashboard');"#),
        vec!["Dashboard"]
    );
    assert_eq!(
        pages(r#"<?php return inertia("Auth/Login");"#),
        vec!["Auth/Login"]
    );
}

#[test]
fn facade_render_call_site() {
    // Inertia::render('Page', $props) — the facade form, bare and FQ.
    assert_eq!(
        pages(r#"<?php return Inertia::render('Users/Index', ['users' => $users]);"#),
        vec!["Users/Index"]
    );
    assert_eq!(
        pages(r#"<?php return \Inertia\Inertia::render('Settings/Profile');"#),
        vec!["Settings/Profile"]
    );
}

#[test]
fn route_inertia_call_site() {
    // Route::inertia('/path', 'Page') — the page name is the SECOND argument,
    // not the first (which is the URI). The URI must not be captured.
    assert_eq!(
        pages(r#"<?php Route::inertia('/welcome', 'Welcome');"#),
        vec!["Welcome"]
    );
    assert_eq!(
        pages(r#"<?php Route::inertia('/profile', "User/Profile");"#),
        vec!["User/Profile"]
    );
}

#[test]
fn all_three_call_sites_in_one_file() {
    // Each call site contributes exactly one page — no double-capture between
    // the bare and fully-qualified facade queries.
    let php = r#"<?php
    inertia('A');
    Inertia::render('B', $x);
    Route::inertia('/c', 'C');
    "#;
    let mut names = pages(php);
    names.sort();
    assert_eq!(names, vec!["A", "B", "C"]);
}
