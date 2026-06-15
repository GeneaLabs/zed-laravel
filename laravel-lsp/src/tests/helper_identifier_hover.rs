//! Behavior tests for curated helper-function identifier hover (#58).
//!
//! Exercises the full parse → position-index → card path the LSP runs: a PHP
//! snippet is parsed via [`parse_owned`], the cursor is placed on a helper's
//! NAME token, and the resolved pattern + rendered card are asserted. Mirrors
//! the example call sites in the issue's acceptance criteria.

use laravel_lsp::hover;
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
// `Backend::hover_for_helper` (`main.rs`) delegates its vendored-vs-docs
// source-link decision to `hover::resolve_helper_source_link`, which probes the
// workspace root for the vendored framework `helpers.php`: present → a `file://`
// link into that file, absent → the canonical `laravel.com/docs` anchor. The two
// arms below drive that *production* function directly with a `TempDir` — the
// same standalone-function + `TempDir` convention as `vendor_translations` and
// `vendor_member_prover` — and assert on the link it RETURNS, so a regression
// flipping the branch selection or breaking the vendor-path probe is caught.

#[tokio::test]
async fn vendored_helpers_file_present_yields_a_file_source_link() {
    let card = hover::helper_card("route").expect("route is curated");

    // A workspace root with the framework vendored: materialize the exact file
    // `resolve_helper_source_link` probes for.
    let dir = TempDir::new().unwrap();
    let vendored = dir.path().join(card.vendor_path);
    fs::create_dir_all(vendored.parent().unwrap()).unwrap();
    fs::write(&vendored, "<?php\n// Illuminate\\Foundation helpers\n").unwrap();

    // Drive the real production decision off the on-disk probe.
    let link = hover::resolve_helper_source_link(Some(dir.path()), card).await;

    // The probe hit → a `file://` link into the vendored helpers.php, labelled
    // with the root-relative vendored path. The `file://` URL is computed
    // independently here and must match what production returned (output, not
    // an echo of an input we built).
    let expected_url = Url::from_file_path(&vendored).expect("absolute path → file URL");
    assert!(
        link.contains(expected_url.as_str()),
        "present framework yields a file:// link into the vendored helpers.php:\n{link}",
    );
    assert!(
        link.contains(card.vendor_path),
        "the link is labelled with the root-relative vendored path:\n{link}",
    );
    // …and the docs-URL fallback is NOT used.
    assert!(
        !link.contains("laravel.com/docs"),
        "the docs fallback must not appear when the framework is vendored:\n{link}",
    );
}

#[tokio::test]
async fn vendored_helpers_file_absent_falls_back_to_the_docs_url() {
    let card = hover::helper_card("route").expect("route is curated");

    // An empty workspace root — the framework is not vendored, so the probe
    // misses and the docs anchor is the source link.
    let dir = TempDir::new().unwrap();
    assert!(
        !dir.path().join(card.vendor_path).exists(),
        "fixture root must not contain the vendored helpers.php",
    );

    // Drive the real production decision off the on-disk probe.
    let link = hover::resolve_helper_source_link(Some(dir.path()), card).await;

    // The probe missed → exactly the curated docs source link (anchor and all),
    // and no `file://` link. Asserting equality against the docs link the helper
    // would build distinguishes the branch's output from the vendored arm.
    assert_eq!(
        link,
        hover::source_link("Laravel documentation", card.docs_url, None),
        "absent framework falls back to the exact docs source link",
    );
    assert!(
        link.contains(card.docs_url),
        "the docs URL (with its #method-route anchor) is the link target:\n{link}",
    );
    assert!(
        !link.contains("file://"),
        "no file:// link when the framework isn't vendored:\n{link}",
    );
}
