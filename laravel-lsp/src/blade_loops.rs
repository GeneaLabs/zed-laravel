//! Blade loop-block parsing.
//!
//! Extracts `@foreach` / `@forelse` / `@for` / `@while` block boundaries and the
//! variables they introduce, so the LSP can do scope-aware variable resolution
//! inside Blade templates.
//!
//! Types are defined here (rather than in main.rs) so the Salsa actor in
//! `salsa_impl` can return them from a tracked query.

use lazy_static::lazy_static;
use regex::Regex;

/// Represents a loop block in a Blade file for scope-aware variable resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BladeLoopBlock {
    /// The type of loop directive (foreach, forelse, for, while)
    pub loop_type: BladeLoopType,
    /// Variables introduced by this loop (e.g., `$item`, `$key` from `@foreach`).
    /// Tuple is (name_without_dollar, php_type_hint).
    pub variables: Vec<(String, String)>,
    /// Iterable expression (left of `as` for foreach/forelse), e.g. `$this->audits`.
    pub iterable: Option<String>,
    /// Start line (0-indexed).
    pub start_line: usize,
    /// End line (0-indexed). `None` if the loop is unclosed (cursor still inside).
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BladeLoopType {
    Foreach,
    Forelse,
    For,
    While,
}

/// Parse the bound variables from `@foreach` / `@forelse` directive arguments —
/// every name the loop introduces, in source order.
///
/// A foreach target (right of the last ` as `) is a PHP *lvalue*: it has no
/// method calls or expressions, so every `$ident` token in it IS a bound
/// variable. Extracting them all — rather than matching one fixed shape — covers
/// every binding form with one rule:
/// - plain `$item` → `[item]`
/// - key/value `$key => $value` → `[key, value]`
/// - by-reference `&$item` → `[item]` (the `&` carries no name)
/// - list destructuring `[$a, $b]` / `list($a, $b)` → `[a, b]`
/// - key + destructured value `$k => [$a, $b]` → `[k, a, b]`
///
/// A binding the parser can't see any `$ident` in (a truncated/garbled header)
/// yields an empty list — the caller treats such a foreach/forelse as an
/// *opaque* loop and refuses the rename rather than guessing its scope.
pub fn parse_foreach_variables(arguments: &str) -> Vec<(String, String)> {
    lazy_static! {
        static ref BOUND_VAR_RE: Regex = Regex::new(r#"\$(\w+)"#).unwrap();
    }

    let Some(binding) = foreach_binding(arguments) else {
        return Vec::new();
    };
    BOUND_VAR_RE
        .captures_iter(binding)
        .filter_map(|caps| caps.get(1))
        .map(|m| (m.as_str().to_string(), "mixed".to_string()))
        .collect()
}

/// Parse the iterable expression from `@foreach` / `@forelse` arguments.
/// e.g. `($this->audits as $audit)` -> `Some("$this->audits")`,
/// `($users->where('active', true) as $user)` -> `Some("$users->where('active', true)")`.
pub fn parse_foreach_iterable(arguments: &str) -> Option<String> {
    strip_outer_parens(arguments.trim())
        .rsplit_once(" as ")
        .map(|(iterable, _)| iterable.trim().to_string())
}

/// Return the binding portion (right of the LAST ` as `) of a foreach argument
/// list. Splitting on the last keyword — rather than regex-capturing the
/// iterable with a `[^)]` run — keeps iterables that contain their own parens
/// or `as` (method calls like `$users->where('active', true)`) intact, so the
/// loop variable is still recovered. `None` when the directive has no ` as `.
fn foreach_binding(arguments: &str) -> Option<&str> {
    strip_outer_parens(arguments.trim())
        .rsplit_once(" as ")
        .map(|(_, binding)| binding)
}

/// Strip a single matching outer parenthesis pair, if both are present.
/// Only the outermost pair is removed, so nested parens inside the iterable are
/// preserved.
fn strip_outer_parens(s: &str) -> &str {
    s.strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(s)
}

/// Parse variables from `@for` directive arguments.
/// Handles: `@for($i = 0; $i < 10; $i++)`.
pub fn parse_for_variables(arguments: &str) -> Vec<(String, String)> {
    lazy_static! {
        static ref FOR_RE: Regex = Regex::new(r#"\(\s*\$(\w+)\s*="#).unwrap();
    }

    let mut vars = Vec::new();
    if let Some(caps) = FOR_RE.captures(arguments) {
        if let Some(var_match) = caps.get(1) {
            vars.push((var_match.as_str().to_string(), "int".to_string()));
        }
    }
    vars
}

/// The balanced `(…)` argument list of a loop directive whose opening `(` is at
/// byte `open` on line `start_line`, joining continuation lines when the header
/// wraps across physical lines. Returns the argument substring **including** the
/// outer parens (interior runs of whitespace between joined lines collapsed to a
/// single space), or `None` if the parens never balance before end of file.
///
/// Two reasons the parens must be balanced rather than regex-captured:
/// - An iterable containing a call — `@foreach($users->where('active', true) as
///   $user)` — would truncate at the first `)` under a `\([^)]*\)` regex, losing
///   the ` as $user` tail.
/// - A long header is routinely broken across lines:
///   `@foreach($users->where('active', true)` ⏎ `    as $user)`. A per-line scan
///   gives up at the first line end.
///
/// In both cases the binding goes unrecognized, the loop scope turns invisible,
/// and a rename inside it silently clobbers the whole file — the file-scope
/// fallback admits every occurrence (issue #55). Parens inside single/double-
/// quoted strings are ignored so a string argument can't throw off the depth
/// count. Reads forward only as far as needed for the depth to return to zero.
fn balanced_directive_arguments(lines: &[&str], start_line: usize, open: usize) -> Option<String> {
    // Fast path: balanced on the opening line — the overwhelmingly common case.
    // Returns the exact source slice, byte-for-byte as the old per-line scan did.
    if let Some(close) = matching_paren(lines[start_line], open) {
        return Some(lines[start_line][open..=close].to_string());
    }

    // Slow path: the header wraps across physical lines. Walk the continuation
    // lines a single time, carrying the scanner state (paren depth, the active
    // string quote, and any open block comment) forward from one line to the
    // next via [`scan_line_parens`]. Each line's bytes are therefore visited
    // exactly once — total work is O(total bytes across the continuation lines),
    // not the O(N²) of re-scanning the whole accumulated buffer from index 0
    // after every appended line (issue #185).
    //
    // `buf` still accumulates the joined header — lines joined with `\n`, each
    // continuation `trim_start`ed — so the returned argument string is byte-for-
    // byte what the old re-scan produced: the closing `)` truncates it and every
    // `\n` collapses back to a single space, preserving the documented format
    // (and the ` as ` split that recovers a binding wrapped as `… as` ⏎ `$user`).
    let mut buf = lines[start_line][open..].to_string();
    // Seed the running state from the opening line. The fast path above already
    // proved it doesn't balance on its own, so this can only yield `Open(state)`;
    // the `Closed` arm is defensive and resolves correctly rather than panicking.
    let mut state = match scan_line_parens(lines[start_line], open, ParenScanState::default()) {
        LineScan::Closed(close) => return Some(lines[start_line][open..=close].to_string()),
        LineScan::Open(state) => state,
    };
    for line in &lines[start_line + 1..] {
        buf.push('\n');
        // Scan only the bytes just appended (one physical continuation line),
        // resuming from `state`; `scan_line_parens` returns indices absolute in
        // `buf`, so the close index truncates `buf` directly.
        let segment_start = buf.len();
        buf.push_str(line.trim_start());
        match scan_line_parens(&buf, segment_start, state) {
            LineScan::Closed(close) => {
                buf.truncate(close + 1);
                return Some(buf.replace('\n', " "));
            }
            LineScan::Open(next) => state = next,
        }
    }
    None
}

/// Running state of the parenthesis scanner as it walks a loop directive's
/// argument list. [`balanced_directive_arguments`]'s slow path carries this from
/// one continuation line to the next so a wrapped header is scanned in a single
/// linear pass instead of re-scanning the whole accumulated buffer per appended
/// line (issue #185).
///
/// The acceptance criteria name `depth` + `quote` as the resumable state; the
/// open-block-comment flag is carried too because a `/* … */` comment can span a
/// wrapped header. Dropping it across the line break would let a `)` inside a
/// multi-line block comment close the arguments early — re-introducing exactly
/// the truncation that issue #166's comment-awareness fixed. All three must
/// survive a line boundary; a `//`/`#` line comment does *not* (it ends at its
/// own physical line), so it is deliberately absent from the carried state.
#[derive(Debug, Clone, Copy, Default)]
struct ParenScanState {
    /// Open-paren depth. The argument list balances when this returns to 0.
    depth: usize,
    /// The active string-literal quote byte (`b'\''` or `b'"'`), or `None`
    /// outside a string. A string literal spans line boundaries.
    quote: Option<u8>,
    /// Whether we are inside an unterminated `/* … */` block comment. A block
    /// comment spans line boundaries.
    in_block_comment: bool,
}

/// Result of scanning one physical line with [`scan_line_parens`].
enum LineScan {
    /// The arguments balanced: the matching `)` is at this byte index, absolute
    /// within the slice that was scanned.
    Closed(usize),
    /// The line ended without balancing; carry this state to the next line.
    Open(ParenScanState),
}

/// Scan one physical line of a directive's argument list, beginning at byte
/// `start` and resuming from the carried `state`. Returns [`LineScan::Closed`]
/// the instant the open-paren depth returns to 0 (the matching `)` index), or
/// [`LineScan::Open`] with the updated state when the line ends still unbalanced.
///
/// `line` is exactly one physical line (the slice up to the next `\n`, exclusive).
/// That single-line framing is what makes a `//`/`#` comment end at the line
/// boundary: anything after the marker on this line is commentary, but a `)` on a
/// *later* line is still counted when the next line resumes. String literals
/// (honouring `\` escapes) and `/* … */` block comments instead carry across
/// lines through `state`. See [`matching_paren`] for the full rationale on the
/// comment- and string-awareness (issue #166).
fn scan_line_parens(line: &str, start: usize, mut state: ParenScanState) -> LineScan {
    let bytes = line.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];

        if state.in_block_comment {
            // Inside `/* … */`: every byte (including `)`) is skipped until `*/`.
            if c == b'*' && bytes.get(i + 1) == Some(&b'/') {
                state.in_block_comment = false;
                i += 1; // also consume the closing '/'
            }
            i += 1;
            continue;
        }

        match state.quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1; // skip the escaped byte
                } else if c == q {
                    state.quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => state.quote = Some(c),
                // `/*` opens a block comment; consume the `*` too so a lone `/*/`
                // isn't read as open-then-close.
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    state.in_block_comment = true;
                    i += 1;
                }
                // `//` or `#` begins a line comment: in PHP it runs to the end of
                // this physical line. Nothing past the marker on this line can
                // affect the parens, so stop — the next line resumes the scan, so
                // a real closing `)` further down is still found.
                b'#' => return LineScan::Open(state),
                b'/' if bytes.get(i + 1) == Some(&b'/') => return LineScan::Open(state),
                b'(' => state.depth += 1,
                b')' => {
                    state.depth -= 1;
                    if state.depth == 0 {
                        return LineScan::Closed(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    LineScan::Open(state)
}

/// Given the byte index of an opening `(` in `s`, return the byte index of its
/// matching `)`, balancing nested parens and ignoring any parens that sit
/// inside string literals **or PHP comments**. Returns `None` when the parens
/// are unbalanced on this input.
///
/// Skipped spans (a `)` inside any of these never counts toward depth):
/// - single- or double-quoted string literals (`'…'`, `"…"`), honouring `\`
///   escapes;
/// - `/* … */` block comments;
/// - `//` and `#` line comments — from the marker to the end of the physical
///   line (the next `\n`), or to the end of `s` when it has no newline. The
///   multi-line slow path in [`balanced_directive_arguments`] joins lines with
///   `\n` precisely so a line comment on a wrapped header ends at its own line
///   break instead of swallowing the closing `)` further down.
///
/// Without comment-awareness a `)` inside a header comment — e.g.
/// `@foreach ($users /* :) */ as $user)` — would close the depth counter early,
/// truncating the arguments before ` as $user`. The binding would then be lost,
/// the loop treated as opaque, and a legitimate rename refused (issue #166).
///
/// `(`, `)`, `'`, `"`, `\`, `/`, `*`, `#` are all ASCII, so the returned index
/// always lands on a char boundary.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    // The no-underflow of `depth` (a `usize`, decremented on every `)`) and the
    // char-boundary safety of the caller's `buf.truncate(close + 1)` both rely on
    // `open` indexing a literal `(`. Every caller passes the `(` of a loop head;
    // assert it so a future caller that violates the contract fails loudly here.
    debug_assert!(
        bytes.get(open) == Some(&b'('),
        "matching_paren expects `open` to index a literal '('"
    );
    // Walk `s` one physical line at a time from `open`, carrying the scanner
    // state across each `\n` via [`scan_line_parens`] — the same single-line-aware
    // logic the multi-line slow path of [`balanced_directive_arguments`] uses, so
    // the two can never drift. Within a line a `//`/`#` comment ends at the line
    // break (a real `)` on a later line is still counted); a `/* … */` block
    // comment and a string literal both span line breaks. On a single-line input
    // ending in a line comment there is no further line, so the parens can't
    // balance and the function yields `None`.
    let mut state = ParenScanState::default();
    let mut line_start = open;
    loop {
        let line_end = match s[line_start..].find('\n') {
            Some(rel_nl) => line_start + rel_nl,
            None => s.len(),
        };
        match scan_line_parens(&s[..line_end], line_start, state) {
            LineScan::Closed(close) => return Some(close),
            LineScan::Open(next) => state = next,
        }
        if line_end == s.len() {
            return None;
        }
        line_start = line_end + 1; // step past the `\n`
    }
}

lazy_static! {
    /// A loop directive head — `@foreach`/`@forelse`/`@for`/`@while` up to and
    /// including its opening `(`. Shared by block parsing and broken-header
    /// detection.
    static ref LOOP_HEAD_RE: Regex =
        Regex::new(r#"@(foreach|forelse|for|while)\s*\("#).unwrap();
}

/// Find all loop blocks in Blade content and their boundaries.
/// Returns a list of loop blocks with start/end lines and extracted variables.
pub fn find_loop_blocks(content: &str) -> Vec<BladeLoopBlock> {
    lazy_static! {
        static ref LOOP_END_RE: Regex =
            Regex::new(r#"@(endforeach|endforelse|endfor|endwhile)"#).unwrap();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut blocks = Vec::new();
    let mut open_loops: Vec<(BladeLoopType, Vec<(String, String)>, Option<String>, usize)> =
        Vec::new();

    for line_idx in 0..lines.len() {
        let line = lines[line_idx];
        for caps in LOOP_HEAD_RE.captures_iter(line) {
            let Some(directive) = caps.get(1) else {
                continue;
            };
            // `@@foreach` renders the literal text and runs no loop, so it
            // opens no block — and the body below it is therefore not loop
            // scoped, exactly as for a commented-out header.
            if crate::blade_directive_tokens::is_escaped_directive(
                line,
                caps.get(0).expect("group 0 always matches").start(),
            ) {
                continue;
            }
            // The opening paren is the final byte of the whole match; the
            // argument list may continue onto following lines.
            let open = caps.get(0).unwrap().end() - 1;
            let Some(arguments) = balanced_directive_arguments(&lines, line_idx, open) else {
                continue;
            };
            let (loop_type, variables, iterable) = match directive.as_str() {
                "foreach" => (
                    BladeLoopType::Foreach,
                    parse_foreach_variables(&arguments),
                    parse_foreach_iterable(&arguments),
                ),
                "forelse" => (
                    BladeLoopType::Forelse,
                    parse_foreach_variables(&arguments),
                    parse_foreach_iterable(&arguments),
                ),
                "for" => (BladeLoopType::For, parse_for_variables(&arguments), None),
                "while" => (BladeLoopType::While, Vec::new(), None),
                _ => continue,
            };

            open_loops.push((loop_type, variables, iterable, line_idx));
        }

        for caps in LOOP_END_RE.captures_iter(line) {
            // `@@endforeach` is literal text too, so it must not close a live
            // block — otherwise an escaped closer would end a real loop early.
            if crate::blade_directive_tokens::is_escaped_directive(
                line,
                caps.get(0).expect("group 0 always matches").start(),
            ) {
                continue;
            }
            let end_directive = caps.get(1).map(|m| m.as_str()).unwrap_or("");

            let expected_type = match end_directive {
                "endforeach" => Some(BladeLoopType::Foreach),
                "endforelse" => Some(BladeLoopType::Forelse),
                "endfor" => Some(BladeLoopType::For),
                "endwhile" => Some(BladeLoopType::While),
                _ => None,
            };

            if let Some(expected) = expected_type {
                if let Some(pos) = open_loops.iter().rposition(|(t, _, _, _)| *t == expected) {
                    let (loop_type, variables, iterable, start_line) = open_loops.remove(pos);
                    blocks.push(BladeLoopBlock {
                        loop_type,
                        variables,
                        iterable,
                        start_line,
                        end_line: Some(line_idx),
                    });
                }
            }
        }
    }

    // Add any unclosed loops (cursor might be inside them)
    for (loop_type, variables, iterable, start_line) in open_loops {
        blocks.push(BladeLoopBlock {
            loop_type,
            variables,
            iterable,
            start_line,
            end_line: None,
        });
    }

    blocks
}

/// 0-based line of every loop directive head whose parenthesised argument list
/// never balances — a syntactically broken header (typically a missing `)`).
/// [`find_loop_blocks`] silently drops such a head (it can't form a block), so
/// the scope structure below it is unreliable. The scope-aware rename uses this
/// to fail closed — treating everything from a broken header onward as
/// unresolved — rather than renaming file-wide. A header that merely *wraps*
/// across lines still balances (via [`balanced_directive_arguments`]) and is
/// never flagged; only genuinely unclosed parens are.
pub fn unbalanced_loop_head_lines(content: &str) -> Vec<usize> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    for line_idx in 0..lines.len() {
        for caps in LOOP_HEAD_RE.captures_iter(lines[line_idx]) {
            // An escaped head is literal text; it cannot be "unbalanced".
            if crate::blade_directive_tokens::is_escaped_directive(
                lines[line_idx],
                caps.get(0).expect("group 0 always matches").start(),
            ) {
                continue;
            }
            let open = caps.get(0).unwrap().end() - 1;
            if balanced_directive_arguments(&lines, line_idx, open).is_none() {
                out.push(line_idx);
                break;
            }
        }
    }
    out
}

/// Get all loop blocks that enclose the given cursor position.
/// Returns loops ordered innermost-first.
pub fn get_enclosing_loops(content: &str, cursor_line: usize) -> Vec<BladeLoopBlock> {
    let blocks = find_loop_blocks(content);

    let mut enclosing: Vec<BladeLoopBlock> = blocks
        .into_iter()
        .filter(|block| {
            let after_start = cursor_line > block.start_line;
            let before_end = match block.end_line {
                Some(end) => cursor_line < end,
                None => true,
            };
            after_start && before_end
        })
        .collect();

    enclosing.sort_by_key(|b| std::cmp::Reverse(b.start_line));
    enclosing
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── matching_paren: comment-awareness (issue #166) ──────────────────────

    #[test]
    fn matching_paren_ignores_parens_in_a_block_comment() {
        // The `)` inside `/* :) */` must NOT close the depth counter early; the
        // match is the final `)`, not the one inside the comment.
        let s = "@foreach ($users /* :) */ as $user)";
        let open = s.find('(').unwrap();
        let close = matching_paren(s, open).unwrap();
        assert_eq!(
            close,
            s.len() - 1,
            "match is the trailing `)`, not the one in the comment"
        );
    }

    #[test]
    fn matching_paren_still_ignores_parens_in_strings() {
        // Pre-existing quote-awareness is preserved by the comment changes.
        let s = "($users->where('a)b', true) as $user)";
        let close = matching_paren(s, 0).unwrap();
        assert_eq!(close, s.len() - 1);
    }

    #[test]
    fn matching_paren_treats_line_comments_as_skipping_the_rest() {
        // `//` and `#` start a line comment: everything past the marker — the
        // closing `)` included — is skipped, so the parens never balance.
        assert_eq!(matching_paren("($users // ) comment", 0), None);
        assert_eq!(matching_paren("($users # ) comment", 0), None);
    }

    #[test]
    fn matching_paren_ends_a_line_comment_at_the_newline() {
        // On the `\n`-joined multi-line buffer that the slow path of
        // `balanced_directive_arguments` builds, a `//` / `#` comment ends at the
        // next newline — the real closing `)` on a later line is still found,
        // instead of being swallowed as if the comment ran to end of input
        // (the issue #166 review regression).
        let slashes = "($users // active only\nas $user)";
        let close = matching_paren(slashes, 0).expect("the `)` on the next line is found");
        assert_eq!(close, slashes.len() - 1);

        let hash = "($users # active only\nas $user)";
        let close = matching_paren(hash, 0).expect("the `)` on the next line is found");
        assert_eq!(close, hash.len() - 1);
    }

    #[test]
    fn matching_paren_lone_slash_star_is_unterminated() {
        // `/*/` is an open block comment, not open-then-close; it never ends, so
        // the surrounding paren never balances.
        assert_eq!(matching_paren("($x /*/ as $y)", 0), None);
    }

    // ── balanced_directive_arguments + parse_foreach_variables (issue #166) ──

    #[test]
    fn commented_header_yields_full_arguments_and_the_bound_variable() {
        // The header `@foreach ($users /* :) */ as $user)` parses end-to-end:
        // the full argument list survives the comment, and `$user` is recovered.
        let line = "@foreach ($users /* :) */ as $user)";
        let lines = vec![line];
        let open = line.find('(').unwrap();
        let args = balanced_directive_arguments(&lines, 0, open).unwrap();
        assert_eq!(args, "($users /* :) */ as $user)");

        let vars = parse_foreach_variables(&args);
        assert_eq!(
            vars,
            vec![("user".to_string(), "mixed".to_string())],
            "the bound variable survives the comment in the header"
        );
    }

    #[test]
    fn wrapped_header_with_a_first_line_comment_resolves_the_binding() {
        // A `//` (or `#`) line comment on the FIRST physical line of a wrapped
        // header ends at the line break, so the binding on the next line is still
        // parsed — the comment doesn't swallow the rest of the joined buffer
        // (issue #166 review fix). The returned argument string keeps the comment
        // text (like the block-comment case) but collapses the line join to a
        // single space.
        let lines = vec!["@foreach ($users // active only", "    as $user)"];
        let open = lines[0].find('(').unwrap();
        let args = balanced_directive_arguments(&lines, 0, open).unwrap();
        assert_eq!(args, "($users // active only as $user)");
        assert_eq!(
            parse_foreach_variables(&args),
            vec![("user".to_string(), "mixed".to_string())],
            "the `// …`-commented first line doesn't lose the binding"
        );

        let lines = vec!["@foreach ($users # active only", "    as $user)"];
        let open = lines[0].find('(').unwrap();
        let args = balanced_directive_arguments(&lines, 0, open).unwrap();
        assert_eq!(args, "($users # active only as $user)");
        assert_eq!(
            parse_foreach_variables(&args),
            vec![("user".to_string(), "mixed".to_string())],
            "the `# …`-commented first line doesn't lose the binding"
        );
    }

    // ── balanced_directive_arguments: linear-time slow path (issue #185) ─────

    #[test]
    fn header_wrapped_across_many_continuation_lines_resolves_the_binding() {
        // A `@foreach` header split across K = 6 continuation lines, with the
        // closing `)` only on the final one. This drives the slow path — the
        // path that previously re-scanned the whole accumulated buffer from
        // index 0 after every appended line (O(n²)) and now carries the scanner
        // state forward so each line's bytes are visited exactly once. The point
        // of the test is end-to-end correctness after that refactor: the binding
        // on the last continuation line must still be recovered. Each
        // `->where(...)`/`->get()` call opens and closes its own parens on a
        // single line, so depth stays at 1 across the wrap until the final `)`.
        let lines = vec![
            "@foreach ($users",
            "    ->where('active', true)",
            "    ->where('verified', true)",
            "    ->orderBy('name')",
            "    ->get()",
            "    ->filter()",
            "    as $user)",
        ];
        let open = lines[0].find('(').unwrap();
        let args = balanced_directive_arguments(&lines, 0, open)
            .expect("the wrapped header balances on the final continuation line");
        assert_eq!(
            args,
            "($users ->where('active', true) ->where('verified', true) \
             ->orderBy('name') ->get() ->filter() as $user)"
        );
        assert_eq!(
            parse_foreach_variables(&args),
            vec![("user".to_string(), "mixed".to_string())],
            "the binding on the final continuation line is recovered"
        );
    }

    #[test]
    fn wrapped_header_with_a_multi_line_block_comment_still_balances() {
        // A `/* … */` block comment that spans the wrap and contains a `)` must
        // not close the arguments early: the block-comment state has to carry
        // across the line break (issue #166's comment-awareness, preserved by
        // the issue #185 linear-time refactor). Without carrying it, the `)`
        // inside the comment on the second line would truncate before ` as $user`.
        let lines = vec![
            "@foreach ($users /* keep only",
            "    the :) active ones */",
            "    as $user)",
        ];
        let open = lines[0].find('(').unwrap();
        let args = balanced_directive_arguments(&lines, 0, open)
            .expect("the trailing `)` closes the header, not the one in the comment");
        assert_eq!(args, "($users /* keep only the :) active ones */ as $user)");
        assert_eq!(
            parse_foreach_variables(&args),
            vec![("user".to_string(), "mixed".to_string())],
            "the binding survives a `)` buried in a multi-line block comment"
        );
    }
}

// ---- Blade's `@@` escape: literal text opens and closes nothing ----------

#[cfg(test)]
mod escape_tests {
    use super::*;

    #[test]
    fn an_escaped_head_opens_no_block() {
        // `@@foreach` renders the literal text `@foreach (...)` and runs no
        // loop, so nothing below it is loop scoped.
        assert_eq!(
            find_loop_blocks("@foreach ($rows as $row)\n{{ $row }}\n@endforeach").len(),
            1,
            "fixture check — the live head must open a block"
        );
        assert!(
            find_loop_blocks("@@foreach ($rows as $row)\n{{ $row }}\n@endforeach").is_empty(),
            "the escaped head must open none"
        );
    }

    #[test]
    fn an_escaped_closer_does_not_end_a_live_loop() {
        // The dangerous direction: if `@@endforeach` closed the block, the
        // real loop would end two lines early and `$row` would fall out of
        // scope while still inside the loop body.
        let blocks =
            find_loop_blocks("@foreach ($rows as $row)\n@@endforeach\n{{ $row }}\n@endforeach");
        assert_eq!(blocks.len(), 1, "one block, not one truncated by the text");
        assert_eq!(
            blocks[0].end_line,
            Some(3),
            "it must close on the REAL @endforeach at line 3, not the escaped one at line 1"
        );
    }

    #[test]
    fn an_escaped_head_is_never_reported_unbalanced() {
        // An unbalanced head makes `in_scope_spans` refuse the rename outright
        // (fail-closed). Literal text must not be able to trigger that.
        assert_eq!(
            unbalanced_loop_head_lines("@foreach ($rows as $row\n{{ $row }}"),
            vec![0],
            "fixture check — a genuinely broken head is still reported"
        );
        assert!(
            unbalanced_loop_head_lines("@@foreach ($rows as $row\n{{ $row }}").is_empty(),
            "an escaped head is literal text and cannot be unbalanced"
        );
    }
}
