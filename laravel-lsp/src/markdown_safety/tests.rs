//! Unit coverage for the two escaping primitives. The end-to-end proof that
//! the *renderers* route through them lives in
//! `tests/env_key_navigation.rs`; these pin the primitives themselves.

use super::{escape_inline, fenced_block};
use std::borrow::Cow;

// ── escape_inline ─────────────────────────────────────────────────────────

#[test]
fn a_link_expression_is_defused() {
    // The attack shape from the module doc: a `.env` key that is a whole
    // markdown link. Both delimiters must be escaped, or the client renders a
    // live clickable target inside the card.
    let out = escape_inline("[click me](https://evil.example/harvest)");
    assert!(
        !out.contains("]("),
        "the link's `](` seam must not survive: {out}"
    );
    assert_eq!(
        out, "\\[click me\\]\\(https\\:\\/\\/evil\\.example\\/harvest\\)",
        "every punctuation character is escaped"
    );
}

#[test]
fn an_image_expression_is_defused() {
    // `![](url)` needs no click at all — the client fetches the URL to render
    // it. The leading `!` is what separates it from a plain link.
    let out = escape_inline("![](https://evil.example/pixel)");
    assert!(
        out.starts_with("\\!\\[\\]"),
        "the image marker must be escaped: {out}"
    );
}

#[test]
fn a_code_span_cannot_be_opened() {
    // A backtick in an inline position opens a code span that runs until the
    // next one — swallowing the `**` that closes the bold header.
    assert_eq!(escape_inline("a`b"), "a\\`b");
}

#[test]
fn every_ascii_punctuation_character_is_escaped() {
    // The completeness claim in the docs, asserted rather than described:
    // CommonMark's escapable set is exactly `char::is_ascii_punctuation`, so
    // walking the whole ASCII range proves the two agree with no hand-listed
    // set to drift.
    for byte in 0x21u8..0x7f {
        let c = byte as char;
        let one = c.to_string();
        let out = escape_inline(&one);
        if c.is_ascii_punctuation() {
            assert_eq!(out, format!("\\{c}"), "{c:?} must be escaped");
        } else {
            assert_eq!(out, c.to_string(), "{c:?} must pass through");
        }
    }
}

#[test]
fn punctuation_free_text_borrows_instead_of_allocating() {
    // The fast path is a behavioural claim, not a micro-optimisation note:
    // most headers are identifiers and must not pay for a copy.
    assert!(matches!(escape_inline("APPNAME"), Cow::Borrowed(_)));
    assert!(matches!(escape_inline("APP_NAME"), Cow::Owned(_)));
}

#[test]
fn multibyte_text_is_not_split() {
    // Iterating chars, not bytes — a value with a multibyte character next to
    // punctuation must survive intact.
    assert_eq!(escape_inline("café*"), "café\\*");
}

// ── fenced_block ──────────────────────────────────────────────────────────

#[test]
fn a_backtick_free_value_gets_the_ordinary_three_backtick_fence() {
    assert_eq!(fenced_block("", "hunter2"), "```\nhunter2\n```");
    assert_eq!(fenced_block("php", "echo 1;"), "```php\necho 1;\n```");
}

#[test]
fn a_value_holding_three_backticks_cannot_close_the_fence() {
    // The pre-existing twin of the header hole: with a fixed ``` fence, the
    // embedded run closes the block and `**pwned**` renders as markdown.
    let out = fenced_block("", "x ``` **pwned**");
    assert!(
        out.starts_with("````\n") && out.ends_with("\n````"),
        "fence must outgrow the run inside it: {out}"
    );
    // The whole payload is still inside one block: nothing after the opening
    // fence matches a closing fence until the final line.
    assert_eq!(out.matches("````").count(), 2, "exactly two fences: {out}");
}

#[test]
fn the_fence_outgrows_the_longest_run_not_the_first_one() {
    // A shorter run appearing first must not set the fence length — the
    // arithmetic has to scan the whole value.
    let out = fenced_block("", "`` a ````` b");
    assert!(
        out.starts_with("``````\n"),
        "fence must be one longer than the *longest* run (5 -> 6): {out}"
    );
}

#[test]
fn a_value_that_is_only_backticks_still_fences() {
    // Degenerate input: the split-on-non-backtick scan sees a single run and
    // no separators.
    let out = fenced_block("", "````");
    assert!(
        out.starts_with("`````\n") && out.ends_with("\n`````"),
        "{out}"
    );
}

#[test]
fn an_empty_value_still_fences() {
    assert_eq!(fenced_block("", ""), "```\n\n```");
}
