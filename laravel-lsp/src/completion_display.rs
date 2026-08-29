//! Length-split rendering for config and translation completion items, plus
//! the two shared gates every `.env` value display consults: a name gate
//! ([`is_sensitive_env_name`]) and a value-shape gate
//! ([`mask_url_credentials`]).
//!
//! A completion item shows its value in two places that want two different
//! lengths:
//!
//! - `CompletionItem.detail` — a **single line** rendered inline beside the
//!   label in the completion list. Long values blow the line out, or the
//!   editor clips them at a width this server can't see.
//! - `CompletionItem.documentation` — the **markdown panel** beside the
//!   list. It has room for a full sentence, and clipping there throws away
//!   space that was already paid for.
//!
//! Both used to read one already-truncated string, so a single limit had to
//! serve both surfaces and served neither (issue #326). Laravel's own
//! validation and auth messages run 80–150 characters, which is exactly the
//! band where the two surfaces disagree: too long for the line, comfortably
//! inside the panel.
//!
//! The fix is to keep the full value on the completion struct and truncate
//! at each render site instead — [`COMPLETION_DETAIL_LIMIT`] for the line,
//! [`COMPLETION_DOC_LIMIT`] for the panel. Every truncation still routes
//! through [`crate::display_truncate::truncate_for_display`], so the
//! char-boundary safety won in #319 holds at both new cut points.

use crate::completion_format::{CodeBlock, CompletionDoc};
use crate::display_truncate::truncate_for_display;
use std::borrow::Cow;

/// What every *client-rendered* surface prints in place of a sensitive `.env`
/// value.
///
/// One constant rather than a per-site literal: the four surfaces that used to
/// echo dotenv values (env completion, `.env` hover, `config('…')` completion,
/// and the warm-start disk cache) are meant to be indistinguishable to a
/// reader, and a second spelling is how they drift apart (issue #344).
///
/// The server log is the one masked surface that does *not* use this string.
/// It renders `(set)` — see `database::mask_env_value_for_log` — because a log
/// line is read as a diagnostic, not as a popup, and that file already masked
/// the resolved DB password in exactly that spelling.
pub const REDACTED_ENV_VALUE: &str = "(redacted — matches sensitive-name pattern)";

/// Name segments that mark a `.env` variable as secret-bearing.
///
/// Matched as whole `_`-delimited segments, never as substrings — `AUTHOR_NAME`
/// and `TOKENIZE_INPUT` are ordinary settings that a `contains` test would
/// redact for no reason.
const SENSITIVE_ENV_SEGMENTS: [&str; 8] = [
    "KEY",
    "SECRET",
    "PASSWORD",
    "TOKEN",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
    "PWD",
];

/// Whether a `.env` variable's *value* must never be rendered.
///
/// A Laravel `.env` routinely holds `APP_KEY`, `DB_PASSWORD`, `MAIL_PASSWORD`
/// and third-party API tokens, and every surface that echoed them did so in a
/// popup most likely to be on screen during a screen-share or a recording
/// (issue #344). The heuristic is deliberately name-based and shared: a custom
/// name outside these segments still shows its value, but no surface may decide
/// that question for itself — including the server log, which is as visible in
/// a screen-share as any popup (`database::mask_env_value_for_log`).
///
/// A name gate cannot see a credential that lives *inside* the value, so it is
/// only half the policy: every caller pairs it with [`mask_url_credentials`],
/// which catches `DATABASE_URL=mysql://user:hunter2@host/db` — a name this
/// function correctly returns `false` for.
///
/// Case-insensitive, because `.env` keys are conventionally upper-case but
/// nothing enforces it.
pub fn is_sensitive_env_name(name: &str) -> bool {
    name.split('_').any(|segment| {
        SENSITIVE_ENV_SEGMENTS
            .iter()
            .any(|keyword| segment.eq_ignore_ascii_case(keyword))
    })
}

/// Mask a credential carried *inside* a `.env` value, whatever the variable is
/// called.
///
/// [`is_sensitive_env_name`] reads the variable's **name**, and stock Laravel
/// ships `'url' => env('DATABASE_URL')` (and `env('REDIS_URL')`) whose value is
/// `mysql://user:hunter2@host/db`: the password lives in the value, and
/// `DATABASE_URL` splits to `DATABASE` / `URL`, matching none of
/// [`SENSITIVE_ENV_SEGMENTS`]. The two gates are complementary and every
/// surface applies both — the name gate drops a value whose *name* says it is
/// secret, this one masks a credential the value's own *shape* reveals
/// (issue #344).
///
/// Matches the standard `scheme://user:password@host…` shape and replaces the
/// password with `***`. Best-effort and fail-open: a value with no `://`, no
/// `@`, or no `:` in its credentials comes back borrowed and untouched. A
/// display surface must not blank a value it merely failed to parse, and a log
/// line must not panic the server.
///
/// The credentials end at the **last** `@` inside the authority component —
/// the run between `://` and the first `/`, `?` or `#` — which is the standard
/// RFC 3986 authority parse. Taking the *first* `@` anywhere in the value left
/// the tail of an unencoded-`@` password on screen
/// (`postgres://user:***@ssw0rd@host/db`, issue #355) and read an `@` in the
/// *path* as a credential separator, masking a host's port
/// (`mysql://host:3306/db@x`).
///
/// One malformed shape still fails open: an unencoded `/`, `?` or `#` *inside*
/// the password ends the authority early, so no `@` is found and the value
/// comes back untouched. RFC 3986 requires all four characters percent-encoded
/// in userinfo, and `postgres://user:p/ss@host/db` is indistinguishable from
/// `mysql://host:3306/db@x` — honouring the later `@` for one masks the other's
/// port and destroys the host, which is the more common URL and the more
/// misleading diagnostic.
///
/// Lives here rather than in `database`, where it started life as
/// `mask_url_password`: it is now the second half of the redaction policy the
/// four display surfaces and the server log share, and a second copy is how the
/// two spellings drift apart.
pub fn mask_url_credentials(value: &str) -> Cow<'_, str> {
    // Find the `://` separator, then the authority component it opens.
    let Some(scheme_end) = value.find("://") else {
        return Cow::Borrowed(value);
    };
    let creds_start = scheme_end + 3;
    // The authority ends at the first `/`, `?` or `#`. Past that is path, query
    // or fragment, where an `@` is an ordinary character and not a separator.
    let authority_end = value[creds_start..]
        .find(['/', '?', '#'])
        .map_or(value.len(), |offset| creds_start + offset);
    // The credentials end at the *last* `@` in the authority, so an unencoded
    // `@` in the password does not end them early (issue #355).
    let Some(at_offset) = value[creds_start..authority_end].rfind('@') else {
        return Cow::Borrowed(value);
    };
    let creds_end = creds_start + at_offset;
    // Credentials are `user[:password]`. Only mask if there is a `:`.
    let Some(colon_offset) = value[creds_start..creds_end].find(':') else {
        return Cow::Borrowed(value);
    };
    let user_end = creds_start + colon_offset;
    let mut masked = String::with_capacity(value.len());
    masked.push_str(&value[..user_end + 1]); // up to and including the `:`
    masked.push_str("***");
    masked.push_str(&value[creds_end..]); // from the `@` onwards
    Cow::Owned(masked)
}

/// Char budget for the inline `detail` line beside a completion label.
///
/// Deliberately short: `detail` competes for horizontal space with the label
/// and the editor's own chrome, and a value that overflows is either wrapped
/// or silently clipped by the client.
pub const COMPLETION_DETAIL_LIMIT: usize = 50;

/// Char budget for the markdown `documentation` panel.
///
/// Four times [`COMPLETION_DETAIL_LIMIT`], which clears the 80–150 char band
/// Laravel's stock validation and auth messages occupy, so those render in
/// full. Still bounded — the value is attacker-adjacent only in the sense
/// that it comes off disk, but an unbounded panel is a hostile popup for
/// anyone with a long string in a catalogue.
pub const COMPLETION_DOC_LIMIT: usize = 200;

/// The whole point of issue #326 is that these two are *different*. A future
/// tidy-up that collapses them back to one value reproduces the bug, so make
/// it a build failure rather than something a reviewer has to notice.
const _: () = assert!(COMPLETION_DETAIL_LIMIT < COMPLETION_DOC_LIMIT);

/// The inline `detail` line: the value, then its source file in parens.
///
/// Shared by the config and translation branches — the two rendered
/// identically before this split and still do, so one function keeps them
/// from drifting apart when the limit is tuned.
///
/// An empty value degrades to the source alone rather than rendering a
/// leading space and an orphan paren.
pub fn completion_detail(value: &str, source: &str) -> String {
    if value.is_empty() {
        format!("({})", source)
    } else {
        format!(
            "{} ({})",
            truncate_for_display(value, COMPLETION_DETAIL_LIMIT),
            source
        )
    }
}

/// The `documentation` panel for a config key: the key, the value as a PHP
/// `return` statement, and the file it came from.
///
/// Truncates `value` independently of [`completion_detail`] — it is handed
/// the same raw value, never that function's already-clipped output.
pub fn config_documentation(key: &str, value: &str, source: &str) -> CompletionDoc {
    let mut doc = CompletionDoc::new().header(key);
    if !value.is_empty() {
        doc = doc.code(CodeBlock::new(
            "php",
            format!(
                "return {};",
                truncate_for_display(value, COMPLETION_DOC_LIMIT)
            ),
        ));
    }
    doc.section(format!("Source: {}", source))
}

/// The `documentation` panel for a translation key: the key, the translated
/// string as the summary, and the catalogue it came from.
///
/// Truncates `value` independently of [`completion_detail`], for the same
/// reason. A key with no extractable value falls back to a generic summary
/// rather than an empty panel.
pub fn translation_documentation(key: &str, value: &str, source: &str) -> CompletionDoc {
    let summary = if value.is_empty() {
        "Translation key.".to_string()
    } else {
        truncate_for_display(value, COMPLETION_DOC_LIMIT)
    };
    CompletionDoc::new()
        .header(key)
        .summary(summary)
        .section(format!("Source: {}", source))
}

#[cfg(test)]
mod tests;
