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

    // Slow path: the header wraps. Accumulate continuation lines until the
    // parens balance. Lines are joined with `\n` (not a space) so `matching_paren`
    // can see the physical line boundaries — a `//`/`#` comment on one line must
    // end at its own line break, not swallow the closing `)` further down
    // (issue #166). The `\n` join is internal: the returned argument string
    // collapses each `\n` back to a single space, preserving the documented
    // format (and the ` as ` split that recovers a binding wrapped as
    // `… as` ⏎ `$user`).
    let mut buf = lines[start_line][open..].to_string();
    for line in &lines[start_line + 1..] {
        buf.push('\n');
        buf.push_str(line.trim_start());
        if let Some(close) = matching_paren(&buf, 0) {
            buf.truncate(close + 1);
            return Some(buf.replace('\n', " "));
        }
    }
    None
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
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut in_block_comment = false;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];

        if in_block_comment {
            // Inside `/* … */`: every byte (including `)`) is skipped until the
            // closing `*/`.
            if c == b'*' && bytes.get(i + 1) == Some(&b'/') {
                in_block_comment = false;
                i += 1; // also consume the closing '/'
            }
            i += 1;
            continue;
        }

        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1; // skip the escaped byte
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                // `/*` opens a block comment; its bytes are skipped above. Also
                // consume the `*` so a lone `/*/` isn't read as open-then-close.
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    in_block_comment = true;
                    i += 1;
                }
                // `//` or `#` begins a line comment. In PHP it runs to the end
                // of the *physical* line, so on the `\n`-joined multi-line buffer
                // (the slow path) it ends at the next `\n` and scanning resumes
                // on the following line — the real closing `)` further down is
                // still counted. On a single-line input there is no `\n`, so the
                // rest is commentary and the parens can't balance: stop.
                b'#' | b'/' if c == b'#' || bytes.get(i + 1) == Some(&b'/') => {
                    match s[i..].find('\n') {
                        // Jump to the `\n`; the `i += 1` below steps past it.
                        Some(rel_nl) => i += rel_nl,
                        None => return None,
                    }
                }
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
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
}
