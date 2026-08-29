//! Rendering primitives that stop untrusted text from *acting* as markdown.
//!
//! Hover cards and completion-documentation panels are declared
//! `MarkupKind::Markdown`, and the client renders whatever they contain —
//! links included, which Zed makes click-to-open (`docs/hover.md`). Most text
//! these panels carry is a PHP identifier and cannot spell a markdown
//! construct. `.env` text can: [`crate::env_key_locator`] defines a key as
//! everything before the first `=`, with no character restriction, and a value
//! as everything after it. A line like
//!
//! ```text
//! [Update your credentials here](https://evil.example/harvest)=1
//! ```
//!
//! is a well-formed declaration whose key is the whole bracket-paren
//! expression, and `.env.example` ships in public repositories, so opening an
//! unfamiliar project is enough to reach it.
//!
//! Both renderers ([`crate::hover::render`] and
//! [`crate::completion_format::CompletionDoc::render`]) route their untrusted
//! fields through this module rather than escaping at the call sites that
//! happen to be known-unsafe today. The invariant is "text this renderer was
//! handed renders as itself", and a renderer is the only place that can hold
//! it for callers that do not exist yet.
//!
//! The guarantee is per-field, not per-renderer, and it covers exactly two
//! fields: the bold header ([`escape_inline`]) and the code block
//! ([`fenced_block`]). Every other field — `detail`, `description`, `lines`,
//! `tags`, `source_link` and `trailer` on a hover card, `summary` and
//! `sections` on a completion panel — is rendered verbatim on purpose,
//! because each exists to carry markdown its caller wrote (PHPDoc prose, a
//! `[label](file:line)` link, an italic `*(commented out)*` note).
//!
//! So a call site putting *untrusted* text in one of those owns the escaping
//! and calls [`escape_inline`] itself. The `.env` branch of `completion` is
//! the one that does today: it puts a variable's value in `summary`.

use std::borrow::Cow;

/// Backslash-escape `text` so it renders as itself in an inline markdown
/// context, instead of being parsed as links, emphasis, code spans or raw
/// HTML.
///
/// The escape set is [`char::is_ascii_punctuation`], whose ranges
/// (`U+0021..=U+002F`, `U+003A..=U+0040`, `U+005B..=U+0060`,
/// `U+007B..=U+007E`) are exactly CommonMark's "ASCII punctuation character"
/// — the set the spec guarantees a backslash escape works on. Deferring to
/// the standard library's predicate is the point: a hand-listed set of "the
/// characters that can start a construct" is an enumeration someone has to
/// keep in step with the spec, and it is wrong the first time markdown grows
/// a syntax. Escaping the whole punctuation class costs a few backslashes in
/// the wire text and renders identically.
///
/// Text with no punctuation borrows, so the common case (a PHP class name, a
/// bare `APP_NAME`) allocates nothing.
///
/// ```
/// # use laravel_lsp::markdown_safety::escape_inline;
/// assert_eq!(escape_inline("APP_NAME"), "APP\\_NAME");
/// assert_eq!(escape_inline("PLAIN"), "PLAIN");
/// ```
pub fn escape_inline(text: &str) -> Cow<'_, str> {
    if !text.contains(|c: char| c.is_ascii_punctuation()) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    Cow::Owned(out)
}

/// Build a fenced code block around `content` whose fence is always longer
/// than the longest backtick run inside it, per CommonMark's fenced-code
/// rule: a closing fence must be at least as long as the opening one, so a
/// fence of `longest + 1` backticks cannot be closed by anything in the
/// content.
///
/// A fixed ```` ``` ```` fence is closed early by a value that contains three
/// backticks, and everything after that point renders as markdown — the same
/// escape the [`escape_inline`] docs describe, reached through the code block
/// instead of the header. `info` is the fence's language hint (`"php"`,
/// `""` for a plain fence); it comes from the renderer, never from user text.
///
/// ```
/// # use laravel_lsp::markdown_safety::fenced_block;
/// assert_eq!(fenced_block("", "plain"), "```\nplain\n```");
/// assert_eq!(fenced_block("", "a ``` b"), "````\na ``` b\n````");
/// ```
pub fn fenced_block(info: &str, content: &str) -> String {
    // Splitting on every non-backtick character leaves the backtick runs (and
    // empty strings between adjacent separators), so the longest remaining
    // piece is the longest run. `.max(2) + 1` floors the fence at three.
    let longest_run = content
        .split(|c: char| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest_run.max(2) + 1);
    format!("{fence}{info}\n{content}\n{fence}")
}

#[cfg(test)]
mod tests;
