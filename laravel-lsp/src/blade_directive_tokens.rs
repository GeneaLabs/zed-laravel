//! Blade directive semantic-token extraction for LSP highlighting.
//!
//! Zed's tree-sitter Blade grammar highlights a fixed set of directives plus
//! generic *paired* (`@foo … @endfoo`) forms, but it cannot colour custom
//! *inline* directives an app registers via `Blade::directive()`. This module
//! produces LSP semantic tokens (all `FUNCTION`-typed) for the directives in a
//! buffer so that, with `"semantic_tokens": "combined"`, those custom inline
//! directives highlight like first-class ones.
//!
//! Two guards keep it precise — the highlighter is only as good as the names it
//! trusts and the regions it ignores:
//!
//! 1. **Known-set filter** — a `@word` is tokenised only if its name (without
//!    the leading `@`, compared case-insensitively) is in the caller-supplied
//!    set. That set is the standard directives ∪ the names scanned from real
//!    `Blade::directive()` registrations, so non-directive `@`-text is rejected:
//!    `@param` in a PHPDoc block, `@media` in inline CSS, or the `@example` in
//!    an email address.
//! 2. **Comment exclusion** — matches inside Blade `{{-- … --}}` or HTML
//!    `<!-- … -->` comments are skipped, so a commented-out directive stays
//!    dark instead of lighting up.
//!
//! Positions are 0-based throughout (LSP convention). Columns are byte offsets
//! from the line start, matching the rest of the LSP's token handling — Blade
//! directives are ASCII, so this stays correct for the tokens we emit.

use std::collections::HashSet;

use lazy_static::lazy_static;
use regex::Regex;
use tower_lsp::lsp_types::SemanticToken;

lazy_static! {
    /// A `@`-prefixed directive candidate: `@` followed by a PHP-identifier-shaped
    /// name. Widened from `@[a-zA-Z]+` to include digits and underscores (e.g.
    /// `@feature2`, `@my_directive`) so custom names match; the known-set filter
    /// stops the wider pattern from over-matching.
    static ref DIRECTIVE_RE: Regex = Regex::new(r"@[a-zA-Z_][a-zA-Z0-9_]*").unwrap();

    /// Every region Blade does not execute: a Blade comment (`{{-- --}}`), an
    /// HTML comment (`<!-- -->`), or a `@verbatim … @endverbatim` body.
    /// Non-greedy so neighbouring regions don't merge into one; `(?s)` lets a
    /// region span multiple lines.
    ///
    /// **Every form requires its terminator** — see [`dead_region_spans`].
    ///
    /// The `@verbatim` alternative captures its BODY in group 1, because the
    /// `@verbatim` and `@endverbatim` directives themselves are live tokens
    /// that should still highlight. Comments have no such tokens, so their
    /// whole match is dead.
    static ref DEAD_REGION_RE: Regex =
        Regex::new(r"(?s)\{\{--.*?--\}\}|<!--.*?-->|@verbatim(.*?)@endverbatim").unwrap();
}

/// Byte ranges (`start..end`) of every region of `content` that Blade does not
/// execute: Blade comments, HTML comments, and `@verbatim` bodies.
///
/// **The crate's one dead-region scanner.** It exists so callers "can never
/// disagree about which directives exist" — the reason `blade_use_sites`
/// already gives for sharing this scan (`query_chain/use_aliases.rs`). Before
/// issue #369 Part A there were three implementations disagreeing on two axes:
/// whether `<!-- -->` and `@verbatim` count as dead, and what an unterminated
/// opener does.
///
/// # An unterminated opener yields no span
///
/// `{{--` with no `--}}`, `<!--` with no `-->`, and `@verbatim` with no
/// `@endverbatim` all match nothing, so the text stays live.
///
/// Laravel is on this side for comments: `CompilesComments::compileComments`
/// is a single `preg_replace` with `/{{--(.*?)--}}/s`, whose pattern requires
/// the closing `--}}` and which returns the input unchanged without it. Blade
/// has no handling for `<!--` at all. The `@verbatim` form follows the same
/// rule so the three cannot drift apart again.
///
/// Masking to end of input instead — which two of the three old maskers did —
/// is a regression wider than the bug it fixes: one stray `<!--` inside a
/// `<script>` blanks every directive below it for the rest of the file.
///
/// # HTML comments are treated as dead by choice
///
/// Blade genuinely compiles a directive inside `<!-- -->`, because the
/// compiler is a text transform with no HTML awareness. Treating them as dead
/// is this repo's settled decision (issue #369 Part A records it), made once
/// here rather than separately at each call site.
pub fn dead_region_spans(content: &str) -> Vec<(usize, usize)> {
    DEAD_REGION_RE
        .captures_iter(content)
        .filter(|c| {
            let whole = c.get(0).expect("group 0 always matches");
            // `@@verbatim` is escaped: it renders as literal text and opens no
            // region. Comments have no escape form, so this only ever applies
            // to the `@verbatim` alternative, which is the one with a group 1.
            !(c.get(1).is_some() && is_escaped_directive(content, whole.start()))
        })
        .map(|c| {
            // Group 1 is present only for `@verbatim`, where the dead region
            // is the body between the directives rather than the whole match.
            let m = c
                .get(1)
                .unwrap_or_else(|| c.get(0).expect("group 0 always matches"));
            (m.start(), m.end())
        })
        .collect()
}

/// `content` with every [`dead_region_spans`] region blanked to spaces.
/// Newlines are kept, so byte offsets, line numbers and columns are all
/// unchanged and a caller can scan the masked copy and report positions
/// against the original.
pub fn blank_dead_regions(content: &str) -> String {
    let mut out = content.as_bytes().to_vec();
    for (start, end) in dead_region_spans(content) {
        for b in &mut out[start..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    // Only non-newline bytes are replaced, and only with ASCII spaces, so this
    // cannot fail. Fall back to the original rather than panicking.
    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}

/// Whether the `@` at byte offset `at` in `content` is **escaped**, so Blade
/// emits it as literal text instead of compiling a directive.
///
/// `@@foreach` renders the characters `@foreach` and executes nothing, so an
/// escaped directive binds no variables, opens no block, declares no alias and
/// is not a directive token.
///
/// # The rule, from Blade's own compiler
///
/// `BladeCompiler::compileStatements` matches
/// `/\B@(@?\w+(?:::\w+)?)([ \t]*)(\( [\S\s]*? \))?/x`, and `compileStatement`
/// begins `if (str_contains($match[1], '@'))` — when group 1 carries a leading
/// `@`, the match is replaced by its own text rather than compiled.
///
/// So the test is simply **"is the preceding byte another `@`"**. A run of
/// three or more behaves the same way: for `@@@foreach` the `\B` anchor makes
/// the match start at the second `@`, group 1 is `@foreach`, and it is emitted
/// literally. Two or more `@` never execute, so there is no parity rule to
/// track.
///
/// `@` is ASCII, so indexing the previous byte cannot split a codepoint.
pub fn is_escaped_directive(content: &str, at: usize) -> bool {
    at > 0 && content.as_bytes()[at - 1] == b'@'
}

/// Whether byte offset `pos` falls inside any of the given dead-region spans.
fn in_comment(pos: usize, spans: &[(usize, usize)]) -> bool {
    spans.iter().any(|&(start, end)| pos >= start && pos < end)
}

/// Whether the `@word` ending at `word_end` is an attribute *binding* —
/// `@word`, optionally followed by `.modifier` segments, then `=`. That is the
/// Alpine event-binding shape (`@click="…"`, `@submit.prevent="…"`), which
/// collides syntactically with Blade's `@directive`. No Blade directive is ever
/// written as `@word=`, so this guard keeps Alpine bindings from lighting up as
/// directives without risking a real directive (issue #61).
fn is_attribute_binding(content: &str, word_end: usize) -> bool {
    let bytes = &content.as_bytes()[word_end..];
    let mut i = 0;
    // Consume zero or more `.modifier` segments (`.prevent`, `.enter`, …).
    while i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let mod_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
        {
            i += 1;
        }
        // A lone `.` with no modifier name isn't a binding suffix.
        if i == mod_start {
            return false;
        }
    }
    i < bytes.len() && bytes[i] == b'='
}

/// Find the directives to highlight as 0-based `(line, start_column, length)`
/// triples. A `@word` is included only when it is not inside a comment and its
/// name is present in `known` (which must already be lowercased).
pub fn directive_token_positions(content: &str, known: &HashSet<String>) -> Vec<(u32, u32, u32)> {
    let comment_spans = dead_region_spans(content);

    // Byte offset of each line start, for mapping a match offset to line/column.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in content.as_bytes().iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    let mut positions = Vec::new();

    for mat in DIRECTIVE_RE.find_iter(content) {
        let start_byte = mat.start();

        // Skip directives sitting inside a Blade/HTML comment.
        if in_comment(start_byte, &comment_spans) {
            continue;
        }

        // Skip an escaped directive: `@@csrf` renders the literal text
        // `@csrf` and compiles nothing, so it is not a directive token.
        if is_escaped_directive(content, start_byte) {
            continue;
        }

        // Skip Alpine-style attribute bindings (`@click="…"`): a `@word(.mod)*=`
        // is an event binding, not a Blade directive (issue #61).
        if is_attribute_binding(content, mat.end()) {
            continue;
        }

        // Keep only names we recognise (standard or registered custom). The
        // match is ASCII, so slicing past the leading '@' is byte-safe.
        let name = &content[start_byte + 1..mat.end()];
        if !known.contains(&name.to_lowercase()) {
            continue;
        }

        let line = line_starts
            .iter()
            .position(|&start| start > start_byte)
            .map(|i| i - 1)
            .unwrap_or(line_starts.len() - 1) as u32;
        let col = (start_byte - line_starts[line as usize]) as u32;
        let length = mat.len() as u32;

        positions.push((line, col, length));
    }

    positions
}

/// Build delta-encoded LSP semantic tokens (all `FUNCTION`-typed — index 0 in
/// the server's legend) for the Blade directives in `content` that are present
/// in `known`. See the module docs for the filtering rules.
pub fn extract_blade_directive_tokens(
    content: &str,
    known: &HashSet<String>,
) -> Vec<SemanticToken> {
    let positions = directive_token_positions(content, known);

    let mut tokens = Vec::with_capacity(positions.len());
    let mut prev_line: u32 = 0;
    let mut prev_col: u32 = 0;

    for (line, col, length) in positions {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            col - prev_col
        } else {
            col // absolute column on a new line
        };

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: 0, // FUNCTION (index 0 in the server legend)
            token_modifiers_bitset: 0,
        });

        prev_line = line;
        prev_col = col;
    }

    tokens
}

#[cfg(test)]
mod tests;
