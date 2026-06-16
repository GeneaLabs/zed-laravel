use crate::LaravelLanguageServer;

// `get_blade_directive_context` finds the `@` that starts a Blade directive and
// then checks the char immediately before it (must be whitespace / `>` / `(` /
// `{` / `;` / tab) to avoid firing inside an email, string, etc.
//
// `at_pos` (the `@`'s position) is a *byte* offset from `char_indices()`. The
// guard used to read that char with `chars().nth(at_pos - 1)`, treating the byte
// offset as a char count — so any line with a multibyte char before `@` inspected
// the wrong character and a valid directive context was wrongly rejected. These
// tests pin the byte-bounded behaviour (issue #180).

/// Regression: an emoji (4-byte char) before `@` must not break detection.
/// Old code: `chars().nth(at_pos - 1)` landed on `'f'` → `None`.
#[test]
fn emoji_before_at_is_still_a_directive_context() {
    let line = "🎉 @if";
    let ctx = LaravelLanguageServer::get_blade_directive_context(line, line.len() as u32)
        .expect("`🎉 @if` should be a directive context — space precedes `@`");
    assert_eq!(ctx, "if");
}

/// Regression: an accented (2-byte) char before `@` must not break detection.
/// Old code: `chars().nth(at_pos - 1)` landed on `'@'` → `None`.
#[test]
fn accented_char_before_at_is_still_a_directive_context() {
    let line = "é @foreach";
    let ctx = LaravelLanguageServer::get_blade_directive_context(line, line.len() as u32)
        .expect("`é @foreach` should be a directive context — space precedes `@`");
    assert_eq!(ctx, "foreach");
}

/// The ASCII guard path is unaffected: a word char immediately before `@`
/// (e.g. an email-like `foo@if`) must still be rejected.
#[test]
fn word_char_before_at_is_not_a_directive_context() {
    let line = "foo@if";
    assert!(
        LaravelLanguageServer::get_blade_directive_context(line, line.len() as u32).is_none(),
        "a word char immediately before `@` must not be a directive context",
    );
}

/// Baseline happy path: plain ASCII whitespace before `@` is a directive context.
#[test]
fn ascii_whitespace_before_at_is_a_directive_context() {
    let line = "  @if";
    let ctx = LaravelLanguageServer::get_blade_directive_context(line, line.len() as u32)
        .expect("`  @if` should be a directive context");
    assert_eq!(ctx, "if");
}
