//! Issue #326: `detail` and `documentation` truncate independently.
//!
//! These drive the same functions the completion handler calls, so a
//! regression that re-introduced an ad-hoc `format!` + truncate inside the
//! handler closures would leave these tests asserting about dead code — the
//! handler branches are three-line delegations with nothing left to drift.

use super::*;

/// The panel's markdown, which is what the editor actually renders.
fn panel(doc: CompletionDoc) -> String {
    doc.render()
}

/// The value portion of a `detail` line — everything before the trailing
/// ` (source)`. `detail` is `"<value> (<source>)"`, so assertions about the
/// truncated value have to look past the source suffix.
fn detail_value<'a>(detail: &'a str, source: &str) -> &'a str {
    detail
        .strip_suffix(&format!(" ({})", source))
        .unwrap_or_else(|| panic!("detail {detail:?} must end with the source in parens"))
}

// ---- the reported bug: one limit cannot serve both surfaces --------------

/// 80 chars is the band Laravel's own validation and auth messages sit in:
/// past the 50-char line budget, comfortably inside the 200-char panel. Under
/// the old shared limit both surfaces showed the same string; now the line
/// clips and the panel does not.
#[test]
fn config_eighty_char_value_clips_in_detail_and_survives_in_documentation() {
    let value = "v".repeat(80);
    let detail = completion_detail(&value, "config/app.php");
    let doc = panel(config_documentation("app.name", &value, "config/app.php"));

    let clipped = detail_value(&detail, "config/app.php");
    assert!(clipped.ends_with('…'), "the line must show it was cut");
    assert_eq!(clipped.chars().count(), COMPLETION_DETAIL_LIMIT + 1);

    assert!(
        doc.contains(&value),
        "the panel had room for all 80 chars and must show them"
    );
    assert!(!doc.contains('…'), "the panel must not have been cut");
}

#[test]
fn translation_eighty_char_value_clips_in_detail_and_survives_in_documentation() {
    let value = "v".repeat(80);
    let detail = completion_detail(&value, "lang/en/auth.php");
    let doc = panel(translation_documentation(
        "auth.failed",
        &value,
        "lang/en/auth.php",
    ));

    let clipped = detail_value(&detail, "lang/en/auth.php");
    assert!(clipped.ends_with('…'));
    assert_eq!(clipped.chars().count(), COMPLETION_DETAIL_LIMIT + 1);

    assert!(doc.contains(&value));
    assert!(!doc.contains('…'));
}

/// A value inside the line budget is untouched on both surfaces — the split
/// must not have introduced a cut where there was none.
#[test]
fn short_values_are_untouched_on_both_surfaces() {
    let value = "Welcome";
    assert_eq!(
        completion_detail(value, "lang/en/messages.php"),
        "Welcome (lang/en/messages.php)"
    );
    assert!(panel(config_documentation("app.name", value, "config/app.php")).contains("Welcome"));
    assert!(panel(translation_documentation(
        "messages.welcome",
        value,
        "lang/en/messages.php"
    ))
    .contains("Welcome"));
}

// ---- char-boundary safety at both new cut points -------------------------
//
// The old single cut point was 200. Splitting it added a *second* place a
// byte slice could land mid-character, so both cutoffs need proving, on both
// branches. `€` is three bytes, so byte offset 50 falls at char 16.67 and
// byte offset 200 at char 66.67 — squarely inside a character, exactly the
// index a `&s[..n]` slice would panic on.

#[test]
fn config_detail_cut_lands_on_a_multibyte_char_without_panicking() {
    let value = "€".repeat(60);
    let source = "config/app.php";
    let detail = completion_detail(&value, source);
    let clipped = detail_value(&detail, source);

    assert_eq!(clipped, format!("{}…", "€".repeat(COMPLETION_DETAIL_LIMIT)));
}

#[test]
fn config_documentation_cut_lands_on_a_multibyte_char_without_panicking() {
    let value = "€".repeat(220);
    let doc = panel(config_documentation("app.name", &value, "config/app.php"));

    assert!(doc.contains(&format!("return {}…;", "€".repeat(COMPLETION_DOC_LIMIT))));
}

#[test]
fn translation_detail_cut_lands_on_a_multibyte_char_without_panicking() {
    let value = "€".repeat(60);
    let source = "lang/en/auth.php";
    let detail = completion_detail(&value, source);
    let clipped = detail_value(&detail, source);

    assert_eq!(clipped, format!("{}…", "€".repeat(COMPLETION_DETAIL_LIMIT)));
}

#[test]
fn translation_documentation_cut_lands_on_a_multibyte_char_without_panicking() {
    let value = "€".repeat(220);
    let doc = panel(translation_documentation(
        "auth.failed",
        &value,
        "lang/en/auth.php",
    ));

    assert!(doc.contains(&format!("{}…", "€".repeat(COMPLETION_DOC_LIMIT))));
}

// ---- the panel is bounded, not merely longer ----------------------------
//
// The failure mode this split invites: now that the struct carries the full
// value, forgetting to truncate in the panel ships an *unbounded* popup —
// worse than the bug being fixed.

#[test]
fn config_documentation_is_bounded_at_the_doc_limit() {
    let value = "v".repeat(5_000);
    let doc = panel(config_documentation("app.name", &value, "config/app.php"));

    assert!(doc.contains('…'), "a 5,000-char value must be cut");
    assert!(
        doc.chars().count() < COMPLETION_DOC_LIMIT + 200,
        "the panel must be bounded by the doc limit, not by the value length"
    );
}

#[test]
fn translation_documentation_is_bounded_at_the_doc_limit() {
    let value = "v".repeat(5_000);
    let doc = panel(translation_documentation(
        "auth.failed",
        &value,
        "lang/en/auth.php",
    ));

    assert!(doc.contains('…'));
    assert!(doc.chars().count() < COMPLETION_DOC_LIMIT + 200);
}

#[test]
fn detail_is_bounded_at_the_detail_limit() {
    let value = "v".repeat(5_000);
    let source = "config/app.php";
    let detail = completion_detail(&value, source);

    assert_eq!(
        detail_value(&detail, source).chars().count(),
        COMPLETION_DETAIL_LIMIT + 1
    );
}

// ---- the empty-value fallbacks survive the refactor ----------------------

#[test]
fn an_empty_value_renders_the_source_alone_in_detail() {
    assert_eq!(completion_detail("", "config/app.php"), "(config/app.php)");
    assert_eq!(
        completion_detail("", "lang/en/auth.php"),
        "(lang/en/auth.php)"
    );
}

#[test]
fn an_empty_config_value_omits_the_code_block() {
    let doc = panel(config_documentation("app.name", "", "config/app.php"));

    assert!(!doc.contains("```"), "no value means no PHP block: {doc:?}");
    assert!(doc.contains("**app.name**"));
    assert!(doc.contains("Source: config/app.php"));
}

#[test]
fn an_empty_translation_value_falls_back_to_a_generic_summary() {
    let doc = panel(translation_documentation(
        "auth.failed",
        "",
        "lang/en/auth.php",
    ));

    assert!(doc.contains("Translation key."));
    assert!(doc.contains("**auth.failed**"));
    assert!(doc.contains("Source: lang/en/auth.php"));
}

// ---- the two limits are actually different ------------------------------

/// Every test above is written against the constants rather than literals, so
/// that retuning a limit doesn't require editing eleven assertions. That makes
/// this the one place the actual numbers are pinned — without it, a limit
/// could be changed to anything and the suite would stay green.
///
/// Their *ordering* is enforced at compile time in the module itself.
#[test]
fn the_two_budgets_are_the_tuned_values() {
    assert_eq!(COMPLETION_DETAIL_LIMIT, 50);
    assert_eq!(COMPLETION_DOC_LIMIT, 200);
}
