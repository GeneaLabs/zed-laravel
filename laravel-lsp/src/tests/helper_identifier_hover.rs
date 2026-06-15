//! Behavior tests for curated helper-function identifier hover (#58).
//!
//! Exercises the full parse → position-index → card path the LSP runs: a PHP
//! snippet is parsed via [`parse_owned`], the cursor is placed on a helper's
//! NAME token, and the resolved pattern + rendered card are asserted. Mirrors
//! the example call sites in the issue's acceptance criteria.

use laravel_lsp::hover;
use laravel_lsp::pattern_indexer::parse_owned;
use laravel_lsp::salsa_impl::PatternAtPosition;
use std::path::Path;

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

    let card = hover::helper_identifier_card(&helper.name, None)
        .expect("config is curated, so a card renders");
    assert!(card.contains("**config**"), "card headers the helper name");
    assert!(
        card.contains("configuration variable"),
        "card carries the Laravel-aware synopsis"
    );
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

#[test]
fn every_curated_helper_renders_its_own_card() {
    // Lock in that each of the seven curated helpers renders ITS OWN card — the
    // right header AND the right Laravel-aware synopsis — rather than a bare
    // `is_some()` that would pass even if the wrong (or an empty) card rendered.
    // Each keyword is distinctive to that one synopsis (and absent from the
    // `**name**` header), so a swapped or blank card fails the assertion.
    let cases = [
        ("route", "named route"),
        ("view", "evaluated view"),
        ("config", "configuration variable"),
        ("auth", "auth guard"),
        ("app", "container"),
        ("session", "session store"),
        ("cache", "cache store"),
    ];

    for (name, synopsis_keyword) in cases {
        let card = hover::helper_identifier_card(name, None)
            .unwrap_or_else(|| panic!("`{name}` is curated, so a card renders"));
        assert!(
            card.contains(&format!("**{name}**")),
            "`{name}` card headers its own name, got: {card}"
        );
        assert!(
            card.contains(synopsis_keyword),
            "`{name}` card carries its distinctive synopsis keyword `{synopsis_keyword}`, got: {card}"
        );
    }
}
