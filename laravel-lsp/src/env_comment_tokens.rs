//! Inline-comment semantic tokens for `.env` files.
//!
//! Zed classifies every env file as **Shell Script** and highlights it with the
//! bash grammar (see `docs/environment.md`). That is the right trade overall,
//! but bash and dotenv disagree about one thing: where an inline comment starts.
//!
//! Inline comments are standard dotenv syntax — `vlucas/phpdotenv` (Laravel),
//! `motdotla/dotenv`, `bkeepers/dotenv`, and `python-dotenv` all support them,
//! and all agree that in an **unquoted** value a `#` begins a comment with no
//! preceding whitespace required. Bash follows shell rules, where `#` mid-word
//! is literal, so it paints those comments as part of the value:
//!
//! | Line | bash | dotenv (and this module) |
//! |---|---|---|
//! | `A=unquoted #spaced`   | value + comment ✅ | value + comment |
//! | `B="quoted" #after`    | value + comment ✅ | value + comment |
//! | `C="quoted#inside"`    | all value ✅       | all value |
//! | `D=unquoted#nospace`   | **all value** ❌   | value + comment |
//! | `E=#immediate`         | **all value** ❌   | empty value + comment |
//! | `F=value# trailing`    | **all value** ❌   | value + comment |
//!
//! With `"semantic_tokens": "combined"` the `COMMENT` tokens emitted here
//! overlay bash's colours, correcting the three broken rows. The three rows
//! bash already gets right are emitted too — painting a comment as a comment is
//! idempotent, and re-deriving them from one state machine is less code and
//! fewer edges than special-casing which ones to skip.
//!
//! The state machine mirrors `EntryParser::processToken()` in phpdotenv, which
//! is what Laravel actually runs. Deviating from it would mean colouring a
//! value differently from how the framework reads it.
//!
//! Positions are 0-based and columns are **byte** offsets from the line start,
//! matching the rest of the LSP's token handling. Iteration is over
//! `char_indices`, so a multi-byte value never yields an offset that splits a
//! character.

use tower_lsp::lsp_types::SemanticToken;

/// Where the parser is within a value, mirroring phpdotenv's states.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Immediately after `=`, before any value character.
    Initial,
    /// Inside a bare (unquoted) value.
    Unquoted,
    /// Inside `'…'`. phpdotenv applies no escapes here — only `'` closes it.
    SingleQuoted,
    /// Inside `"…"`.
    DoubleQuoted,
    /// The character after a `\` inside `"…"`, consumed literally.
    Escape,
    /// After a closing quote, where only whitespace or a comment may follow.
    Whitespace,
}

/// Byte offset of the `#` that begins an inline comment in `line`, if any.
///
/// Returns `None` when the line has no value-level comment: a blank line, a
/// whole-line comment (bash already colours those correctly, and they carry no
/// `=` to split on), a line with no `=` at all, or a value whose `#` characters
/// are all inside quotes.
///
/// The offset is relative to the start of `line`.
pub fn inline_comment_start(line: &str) -> Option<usize> {
    // A whole-line comment is not a *value* comment. Checked before the `=`
    // scan below, because a comment may itself contain an `=`
    // (`# set FOO=bar to enable`) and must not be read as an assignment.
    let trimmed_start = line.len() - line.trim_start().len();
    if crate::env_key_locator::commented_declaration_body(line).is_some() || line.trim().is_empty()
    {
        return None;
    }

    // The value begins after the first `=`. phpdotenv splits on the first one,
    // so `KEY=a=b` has the value `a=b`. Anything before it (whitespace, an
    // `export ` prefix, the name) cannot contain a comment.
    let eq = line[trimmed_start..].find('=')? + trimmed_start;
    let value_start = eq + 1;

    let mut state = State::Initial;
    for (offset, ch) in line[value_start..].char_indices() {
        let abs = value_start + offset;
        state = match state {
            State::Initial => match ch {
                '\'' => State::SingleQuoted,
                '"' => State::DoubleQuoted,
                '#' => return Some(abs),
                _ => State::Unquoted,
            },
            State::Unquoted => match ch {
                '#' => return Some(abs),
                c if c.is_whitespace() => State::Whitespace,
                _ => State::Unquoted,
            },
            State::SingleQuoted => {
                if ch == '\'' {
                    State::Whitespace
                } else {
                    State::SingleQuoted
                }
            }
            State::DoubleQuoted => match ch {
                '"' => State::Whitespace,
                '\\' => State::Escape,
                _ => State::DoubleQuoted,
            },
            // Whatever follows a backslash is consumed as part of the value,
            // including a `"` that would otherwise close the string.
            State::Escape => State::DoubleQuoted,
            State::Whitespace => match ch {
                '#' => return Some(abs),
                c if c.is_whitespace() => State::Whitespace,
                // Trailing junk after a closing quote is a phpdotenv parse
                // error. It is not our job to report it, and we cannot know
                // where a comment would have started, so stop scanning.
                _ => return None,
            },
        };
    }

    None
}

/// LSP semantic tokens marking every inline comment in an env-file buffer.
///
/// One `COMMENT` token per line that has a value-level comment, spanning the
/// `#` through end of line.
pub fn extract_env_comment_tokens(content: &str) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let mut prev_line: u32 = 0;

    for (index, line) in content.lines().enumerate() {
        let Some(start) = inline_comment_start(line) else {
            continue;
        };
        let line_no = index as u32;
        let col = start as u32;

        tokens.push(SemanticToken {
            delta_line: line_no - prev_line,
            // Every token here is the last on its line, so a same-line
            // predecessor is impossible and `delta_start` is always absolute.
            delta_start: col,
            length: (line.len() - start) as u32,
            token_type: 1, // COMMENT (index 1 in the server legend)
            token_modifiers_bitset: 0,
        });

        prev_line = line_no;
    }

    tokens
}

#[cfg(test)]
mod tests;
