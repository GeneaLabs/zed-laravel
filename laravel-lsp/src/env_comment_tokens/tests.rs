//! Tests for inline-comment detection in `.env` files.
//!
//! The expectations are not invented: each asserts the boundary
//! `vlucas/phpdotenv`'s `EntryParser::processToken()` produces, which is what
//! Laravel actually reads at runtime.

use super::{extract_env_comment_tokens, inline_comment_start};

/// Offset of `#` in a line, for readable expectations.
fn hash_at(line: &str) -> usize {
    line.find('#').expect("test line must contain a #")
}

// ── The three shapes bash gets wrong ────────────────────────────────────────

#[test]
fn unquoted_hash_without_space_starts_a_comment() {
    let line = "A=unquoted#nospace";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn hash_immediately_after_equals_starts_a_comment() {
    let line = "E=#immediate";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn hash_then_space_after_unquoted_value_starts_a_comment() {
    let line = "F=value# trailing";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

// ── The shapes bash already gets right; we must agree, not differ ───────────

#[test]
fn unquoted_value_then_spaced_hash_starts_a_comment() {
    let line = "A=unquoted #spaced";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn comment_after_closing_double_quote_is_found() {
    let line = "B=\"quoted\" #after";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn comment_after_closing_single_quote_is_found() {
    let line = "B='quoted' #after";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

// ── Quoted values: a `#` inside quotes is value data, never a comment ───────

#[test]
fn hash_inside_double_quotes_is_not_a_comment() {
    assert_eq!(inline_comment_start("C=\"quoted#inside\""), None);
}

#[test]
fn hash_inside_single_quotes_is_not_a_comment() {
    assert_eq!(inline_comment_start("C='quoted#inside'"), None);
}

#[test]
fn hash_inside_double_quotes_with_later_comment_finds_only_the_comment() {
    let line = "C=\"has#hash\" #real";
    // The second `#` — the one outside the quotes.
    let expected = line.rfind('#').unwrap();
    assert_eq!(inline_comment_start(line), Some(expected));
}

#[test]
fn escaped_quote_does_not_close_the_string_early() {
    // The `#` sits immediately after the escaped quote. If `\"` were treated as
    // closing the string, the very next character would be read in the
    // post-quote state and the `#` reported as a comment. It is not: `\"` is
    // value data, so the `#` is still inside the quotes.
    assert_eq!(inline_comment_start(r##"C="a\"#b""##), None);
}

#[test]
fn comment_after_a_string_containing_an_escaped_quote_is_still_found() {
    // The mirror of the above: once the *real* closing quote arrives, a
    // following `#` is a comment. Mishandling `\"` ends the string early and
    // loses this comment entirely.
    let line = r##"C="a\"b" #tag"##;
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn backslash_in_single_quotes_is_literal_and_does_not_escape() {
    // phpdotenv applies no escapes inside single quotes: only `'` closes them,
    // so the `'` here closes and the `#` that follows is a comment.
    let line = r"C='ends\' #after";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

// ── Non-assignment lines ────────────────────────────────────────────────────

#[test]
fn whole_line_comment_is_not_a_value_comment() {
    assert_eq!(inline_comment_start("# just a comment"), None);
}

#[test]
fn indented_whole_line_comment_is_not_a_value_comment() {
    assert_eq!(inline_comment_start("   # indented comment"), None);
}

#[test]
fn whole_line_comment_containing_equals_is_not_parsed_as_an_assignment() {
    // Guards the ordering inside `inline_comment_start`: the whole-line-comment
    // check must run before the `=` scan, or this line is read as an assignment
    // whose value contains no `#` after the `=`.
    assert_eq!(inline_comment_start("# set FOO=bar#baz to enable"), None);
}

#[test]
fn blank_line_has_no_comment() {
    assert_eq!(inline_comment_start(""), None);
    assert_eq!(inline_comment_start("    "), None);
}

#[test]
fn line_without_equals_has_no_value_comment() {
    assert_eq!(inline_comment_start("NOT_AN_ASSIGNMENT"), None);
}

// ── Value shapes that must not be mistaken for comments ─────────────────────

#[test]
fn value_with_no_hash_has_no_comment() {
    assert_eq!(inline_comment_start("APP_NAME=Laravel"), None);
}

#[test]
fn empty_value_has_no_comment() {
    assert_eq!(inline_comment_start("DB_PASSWORD="), None);
}

#[test]
fn export_prefix_does_not_shift_the_comment_offset() {
    let line = "export MAIL_MAILER=smtp#tag";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn first_equals_splits_the_value_so_a_later_equals_is_value_data() {
    // Base64 app keys end in `=` padding; the comment must still be found.
    let line = "APP_KEY=base64:AAAA==#tag";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn interpolated_value_then_comment_is_found() {
    let line = "ASSET_URL=${APP_URL}/assets #cdn";
    assert_eq!(inline_comment_start(line), Some(hash_at(line)));
}

#[test]
fn junk_after_a_closing_quote_stops_the_scan() {
    // phpdotenv rejects this line outright ("unexpected whitespace"); we return
    // None rather than guess where a comment would have begun.
    assert_eq!(inline_comment_start("C=\"quoted\" junk #after"), None);
}

// ── Byte-offset safety ──────────────────────────────────────────────────────

#[test]
fn multibyte_value_yields_a_char_boundary_offset() {
    let line = "GREETING=héllo→wörld#tag";
    let start = inline_comment_start(line).expect("comment found");
    assert!(
        line.is_char_boundary(start),
        "offset {start} splits a character"
    );
    assert_eq!(&line[start..], "#tag");
}

#[test]
fn multibyte_inside_quotes_does_not_shift_a_later_comment() {
    let line = "GREETING=\"héllo→wörld\" #tag";
    let start = inline_comment_start(line).expect("comment found");
    assert!(line.is_char_boundary(start));
    assert_eq!(&line[start..], "#tag");
}

// ── Token emission ──────────────────────────────────────────────────────────

#[test]
fn tokens_use_comment_type_and_span_to_end_of_line() {
    let tokens = extract_env_comment_tokens("A=value#tag\n");
    assert_eq!(tokens.len(), 1);
    let t = &tokens[0];
    assert_eq!(t.token_type, 1, "must be COMMENT (legend index 1)");
    assert_eq!(t.delta_line, 0);
    assert_eq!(t.delta_start, 7, "byte offset of the #");
    assert_eq!(t.length, 4, "#tag");
}

#[test]
fn delta_lines_are_relative_to_the_previous_emitted_token() {
    // Comments sit on lines 1 and 4; lines 0, 2 and 3 carry none. Neither
    // commented line is line 0 — that matters: with the first comment on line
    // 0, `line_no - prev_line` and a plain `line_no` produce identical output
    // and the test could not tell a relative encoding from an absolute one.
    let content = concat!(
        "# header\n",            // line 0 — whole-line comment, not emitted
        "A=one#first\n",         // line 1 — emitted, delta 1 from line 0
        "B=plain\n",             // line 2
        "C=\"quoted#inside\"\n", // line 3 — hash is inside quotes
        "D=two#second\n",        // line 4 — emitted, delta 3 from line 1
    );
    let tokens = extract_env_comment_tokens(content);
    assert_eq!(tokens.len(), 2, "only the two real comments");
    assert_eq!(tokens[0].delta_line, 1, "line 1, relative to the 0 origin");
    assert_eq!(
        tokens[1].delta_line, 3,
        "line 4 minus the previous token's line 1 — not the absolute 4"
    );
}

#[test]
fn buffer_with_no_inline_comments_emits_nothing() {
    let content = "# header\nAPP_NAME=Laravel\nAPP_DEBUG=true\nQ=\"has#hash\"\n";
    assert!(extract_env_comment_tokens(content).is_empty());
}

#[test]
fn token_length_counts_bytes_not_characters() {
    // A multi-byte comment body: 5 chars, more than 5 bytes. LSP columns here
    // are byte offsets, so the length must be the byte count.
    let line = "A=v#→→→";
    let tokens = extract_env_comment_tokens(line);
    assert_eq!(tokens.len(), 1);
    let start = inline_comment_start(line).unwrap();
    assert_eq!(tokens[0].length as usize, line.len() - start);
    assert_eq!(tokens[0].length, 10, "'#' + three 3-byte arrows");
}
