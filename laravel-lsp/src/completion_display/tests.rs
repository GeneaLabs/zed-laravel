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

// ---- the sensitive-name gate (issue #344) --------------------------------

/// Run a fixture table through the predicate and report **every** row that
/// disagrees, not merely the first.
///
/// A bare `assert!` per row inside a loop stops at the first failure, so
/// deleting one keyword from `SENSITIVE_ENV_SEGMENTS` would credit only one
/// fixture and hide the rest of the damage. Collecting first makes each row
/// discriminate on its own.
fn mismatches(names: &[&str], expected: bool) -> Vec<String> {
    names
        .iter()
        .filter(|name| is_sensitive_env_name(name) != expected)
        .map(|name| format!("{name} (expected {expected})"))
        .collect()
}

/// One positive fixture per keyword, so removing any single entry from
/// `SENSITIVE_ENV_SEGMENTS` reddens this test. `AWS_SECRET_ACCESS_KEY` carries
/// two keywords deliberately — it is the real-world shape — but every keyword
/// also has a fixture that isolates it.
#[test]
fn every_sensitive_keyword_redacts_on_its_own() {
    let names = [
        "APP_KEY",
        "APP_SECRET",
        "DB_PASSWORD",
        "MAIL_PASSWORD",
        "API_TOKEN",
        "GOOGLE_CREDENTIAL_FILE",
        "JWT_PRIVATE",
        "BASIC_AUTH",
        "DB_PWD",
        "AWS_SECRET_ACCESS_KEY",
    ];
    assert_eq!(mismatches(&names, true), Vec::<String>::new());
}

/// Ordinary settings keep their values. The second group is the point of
/// splitting on `_`: every one of these *contains* a keyword as a substring,
/// and a `contains`-based predicate would redact all seven.
#[test]
fn ordinary_names_are_not_redacted() {
    let plain = ["APP_NAME", "DB_CONNECTION", "APP_DEBUG"];
    assert_eq!(mismatches(&plain, false), Vec::<String>::new());

    let substring_but_not_segment = [
        "PASSKEYBOARD_LAYOUT",
        "AUTHOR_NAME",
        "SECRETARY_ID",
        "TOKENIZE_INPUT",
        "PRIVATELY_OWNED",
        "CREDENTIALED_USER",
        "PWDLESS_LOGIN",
    ];
    assert_eq!(
        mismatches(&substring_but_not_segment, false),
        Vec::<String>::new()
    );
}

/// `.env` keys are upper-case by convention and by nothing else, so a
/// lower-case or mixed-case declaration must redact identically.
#[test]
fn the_gate_ignores_case() {
    let names = ["db_password", "Api_Token", "app_key"];
    assert_eq!(mismatches(&names, true), Vec::<String>::new());
}

/// A single-segment name has no `_` to split on, and an empty name must not
/// match anything.
#[test]
fn single_segment_and_empty_names_are_handled() {
    assert!(is_sensitive_env_name("PASSWORD"));
    assert!(!is_sensitive_env_name("PASSWORDS"));
    assert!(!is_sensitive_env_name(""));
}

// ---- the value-shape gate (issue #344, round 2) --------------------------

/// The shape the name gate cannot see. `DATABASE_URL` splits to `DATABASE` /
/// `URL` — no segment matches — and stock Laravel's `config/database.php` reads
/// `'url' => env('DATABASE_URL')`, so this is the default configuration, not a
/// contrived one.
#[test]
fn a_credential_inside_the_value_is_masked_whatever_the_name_says() {
    assert!(
        !is_sensitive_env_name("DATABASE_URL"),
        "the premise: the name gate lets this one through, so the shape gate is \
         the only thing standing between it and the screen"
    );
    assert_eq!(
        mask_url_credentials("mysql://sail:secret@127.0.0.1:3306/db"),
        "mysql://sail:***@127.0.0.1:3306/db"
    );
    assert_eq!(
        mask_url_credentials("redis://default:redis-secret@redis:6379"),
        "redis://default:***@redis:6379"
    );
}

// ---- the authority parse (issue #355) ------------------------------------

/// RFC 3986 wants an `@` in a password percent-encoded as `%40`, but nothing
/// stops a developer typing one — and `database::userinfo` interpolates the
/// `.env` password into a connection URL verbatim, so the server builds this
/// shape itself before logging it through this function.
///
/// Reading the *first* `@` as the host separator left everything after it on
/// screen; the password's tail is exactly the part long enough to be worth
/// stealing. The credentials end at the last `@` in the authority instead.
#[test]
fn an_unencoded_at_in_the_password_does_not_end_the_credentials_early() {
    for (value, expected) in [
        (
            "postgres://user:p@ssw0rd@host/db",
            "postgres://user:***@host/db",
        ),
        // More than one, and a port after the host: the *last* `@` wins, not
        // the second one either.
        (
            "mysql://sail:p@ss@w0rd@127.0.0.1:3306/db",
            "mysql://sail:***@127.0.0.1:3306/db",
        ),
        // The no-username Redis form, which starts its credentials with the
        // `:` the mask keys on.
        ("redis://:p@ss@redis:6379", "redis://:***@redis:6379"),
        // The libpq socket URL `build_postgres_candidates` generates: the
        // authority ends immediately after the `@`, and the socket path lives
        // in the query string.
        (
            "postgres://user:p@ss@/laravel?host=/var/run/postgresql",
            "postgres://user:***@/laravel?host=/var/run/postgresql",
        ),
        // An `@` in the query must not be mistaken for the separator and drag
        // the real credentials into the "host" half.
        (
            "mysql://sail:secret@127.0.0.1/db?user=a@b",
            "mysql://sail:***@127.0.0.1/db?user=a@b",
        ),
    ] {
        assert_eq!(
            mask_url_credentials(value),
            expected,
            "{value:?} must mask the whole credential"
        );
    }
}

/// Fail-open, and borrowed while it is at it: a value the parser does not
/// recognise is returned untouched rather than blanked. `Cow::Borrowed` is
/// asserted rather than just string equality because it is the observable proof
/// that the untouched path never rebuilt the string — the property that lets
/// every surface call this unconditionally.
#[test]
fn a_value_carrying_no_credential_is_returned_untouched() {
    for value in [
        "Example",                        // an ordinary setting
        "not a url",                      // no scheme
        "mysql://sail@127.0.0.1/db",      // credentials, but no password
        "https://example.com/webhook",    // no credentials at all
        "sqlite:///absolute/path.sqlite", // scheme, no `@`
        "",
        // An `@` in the *path*, and a `:` that is a port rather than a
        // credential separator. Reading the first `@` in the whole value made
        // this `mysql://host:***@x` — the port masked as a password, host,
        // port and database all gone (issue #355).
        "mysql://host:3306/db@x",
        // The same host and port with nothing after them to mislead the parse.
        "mysql://host:3306/db",
        // An `@` in the query string of a URL that carries no credentials.
        "https://example.com/webhook?notify=ops@example.com",
        // A documented limitation, pinned so it stays deliberate: an unencoded
        // `/` in the password ends the authority before the `@`, so the value
        // fails open. `postgres://user:p/ss@host/db` cannot be told apart from
        // `mysql://host:3306/db@x` above, and honouring the later `@` there
        // masks a port and throws the host away.
        "postgres://user:p/ss@host/db",
    ] {
        assert!(
            matches!(mask_url_credentials(value), std::borrow::Cow::Borrowed(v) if v == value),
            "{value:?} must come back borrowed and unchanged"
        );
    }
}
