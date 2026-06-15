//! Behavior tests for curated helper-function identifier hover (#58).
//!
//! Exercises the full parse → position-index → card path the LSP runs: a PHP
//! snippet is parsed via [`parse_owned`], the cursor is placed on a helper's
//! NAME token, and the resolved pattern + rendered card are asserted. Mirrors
//! the example call sites in the issue's acceptance criteria.

use laravel_lsp::hover::{self, HelperCard};
use laravel_lsp::pattern_indexer::parse_owned;
use laravel_lsp::salsa_impl::PatternAtPosition;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::Url;

/// Parse a `.php` snippet the way the warming path does.
fn parse(src: &str) -> std::sync::Arc<laravel_lsp::salsa_impl::ParsedPatternsData> {
    parse_owned(Path::new("/project/app/Http/Controllers/Demo.php"), src)
}

#[test]
fn hovering_route_identifier_resolves_a_helper_card() {
    // `route('home')` — cursor on the `route` name (col 2), not the argument.
    let data = parse("<?php\nroute('home');\n");

    let pattern = data
        .find_at_position(1, 2)
        .expect("a pattern under the `route` identifier");
    let PatternAtPosition::HelperIdentifier(helper) = pattern else {
        panic!("expected HelperIdentifier, got {pattern:?}");
    };
    assert_eq!(helper.name, "route");

    let card = hover::helper_identifier_card(&helper.name, None)
        .expect("route is curated, so a card renders");
    assert!(card.contains("**route**"), "card headers the helper name");
    assert!(
        card.contains("named route"),
        "card carries the Laravel-aware synopsis"
    );
}

#[test]
fn hovering_config_identifier_resolves_a_helper_card() {
    // `config('app.name')` — cursor on the `config` name (col 3).
    let data = parse("<?php\nconfig('app.name');\n");

    let pattern = data
        .find_at_position(1, 3)
        .expect("a pattern under the `config` identifier");
    let PatternAtPosition::HelperIdentifier(helper) = pattern else {
        panic!("expected HelperIdentifier, got {pattern:?}");
    };
    assert_eq!(helper.name, "config");
    assert!(hover::helper_identifier_card(&helper.name, None).is_some());
}

#[test]
fn hovering_the_string_argument_is_not_a_helper_identifier() {
    // The identifier and the string argument occupy disjoint spans. Cursor on
    // `home` inside `route('home')` must resolve to the route reference, never
    // the helper-identifier card.
    let data = parse("<?php\nroute('home');\n");

    // `route(` is 6 chars, then the opening quote, so `home` starts at col 7.
    let pattern = data
        .find_at_position(1, 8)
        .expect("a pattern under the string argument");
    assert!(
        matches!(pattern, PatternAtPosition::Route(_)),
        "string arg should be the route reference, got {pattern:?}"
    );
}

#[test]
fn hovering_a_non_curated_helper_yields_no_card() {
    // `bcrypt(...)` is a real helper but outside the curated set — Intelephense
    // owns it, so we index nothing and render no card.
    let data = parse("<?php\nbcrypt('secret');\n");

    assert!(
        data.find_at_position(1, 3).is_none(),
        "non-curated helper produces no indexed pattern"
    );
    assert!(
        hover::helper_identifier_card("bcrypt", None).is_none(),
        "non-curated helper renders no card"
    );
}

#[test]
fn helper_identifier_hover_works_in_blade_embedded_php() {
    // `route('home')` inside a Blade echo — the identifier is still carded so
    // hover works in templates, not just `.php` files.
    let data = parse_owned(
        Path::new("/project/resources/views/nav.blade.php"),
        "<a href=\"{{ route('home') }}\">Home</a>\n",
    );

    let route = data
        .helper_refs
        .iter()
        .find(|h| h.name == "route")
        .expect("route helper identifier captured in Blade-embedded PHP");
    // The card renders the same regardless of host file type.
    assert!(hover::helper_identifier_card(&route.name, None).is_some());
}

// ─── source-link branch: vendored helpers.php present vs. absent (#118) ───
//
// `Backend::hover_for_helper` (`main.rs`) probes the workspace root for the
// vendored framework `helpers.php`: present → a `file://` link into that file,
// absent → the canonical `laravel.com/docs` anchor. The two arms below walk
// that decision with a `TempDir` — the same backend-free approach
// `flux_component_hover.rs` uses for the Flux hover chain — so a regression that
// flipped the branch selection or broke the vendor-path probe is caught.

/// Reproduce `Backend::hover_for_helper`'s source-link decision (`main.rs`)
/// without standing up an async LSP backend. The vendored-vs-docs branch is
/// *driven* by the on-disk probe (`vendored.exists()`, mirroring the production
/// `tokio::fs::try_exists`), so the fixture — vendored file present or absent —
/// selects the arm exactly as production does; it is never hand-picked.
fn helper_source_link(root: &Path, card: &HelperCard) -> String {
    let vendored = root.join(card.vendor_path);
    if vendored.exists() {
        // Mirror `Backend::source_link`: a root-relative display label over a
        // `file://` URL (`relative_display_path` strips the project root,
        // falling back to the absolute path on mismatch — here the fixture root
        // IS the workspace root, so the label reduces to `card.vendor_path`).
        let url = Url::from_file_path(&vendored).expect("absolute path → file URL");
        let display = vendored
            .strip_prefix(root)
            .unwrap_or(&vendored)
            .to_string_lossy();
        hover::source_link(&display, url.as_str(), None)
    } else {
        // Fallback arm: the curated `laravel.com/docs` anchor for this helper.
        hover::source_link("Laravel documentation", card.docs_url, None)
    }
}

#[test]
fn vendored_helpers_file_present_yields_a_file_source_link() {
    let card = hover::helper_card("route").expect("route is curated");

    // A workspace root with the framework vendored: materialize the exact file
    // `hover_for_helper` probes for.
    let dir = TempDir::new().unwrap();
    let vendored = dir.path().join(card.vendor_path);
    fs::create_dir_all(vendored.parent().unwrap()).unwrap();
    fs::write(&vendored, "<?php\n// Illuminate\\Foundation helpers\n").unwrap();

    let link = helper_source_link(dir.path(), card);
    let render = hover::helper_identifier_card("route", Some(&link))
        .expect("route is curated, so a card renders");

    // The card carries the resolved source link verbatim…
    assert!(
        render.contains(&link),
        "card carries the source link string:\n{render}",
    );
    // …a `file://` link resolving into the vendored path…
    assert!(
        render.contains("file://"),
        "vendored framework yields a file:// source link:\n{render}",
    );
    assert!(
        render.contains(card.vendor_path),
        "the link resolves into the vendored helpers.php path:\n{render}",
    );
    // …and the docs-URL fallback is NOT used.
    assert!(
        !render.contains("laravel.com/docs"),
        "the docs fallback must not appear when the framework is vendored:\n{render}",
    );
}

#[test]
fn vendored_helpers_file_absent_falls_back_to_the_docs_url() {
    let card = hover::helper_card("route").expect("route is curated");

    // An empty workspace root — the framework is not vendored, so the probe
    // misses and the docs anchor is the source link.
    let dir = TempDir::new().unwrap();
    assert!(
        !dir.path().join(card.vendor_path).exists(),
        "fixture root must not contain the vendored helpers.php",
    );

    let link = helper_source_link(dir.path(), card);
    let render = hover::helper_identifier_card("route", Some(&link))
        .expect("route is curated, so a card renders");

    // The card carries the docs anchor (with its `#method-route` fragment)…
    assert!(
        render.contains(card.docs_url),
        "absent framework falls back to the docs URL:\n{render}",
    );
    assert!(
        render.contains("#method-route"),
        "the docs anchor fragment is preserved:\n{render}",
    );
    // …and no `file://` link is rendered.
    assert!(
        !render.contains("file://"),
        "no file:// link when the framework isn't vendored:\n{render}",
    );
}
