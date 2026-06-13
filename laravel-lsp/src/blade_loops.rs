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

/// Parse variables from `@foreach` / `@forelse` directive arguments.
/// Handles: `@foreach($items as $item)`, `@foreach($items as $key => $value)`,
/// `@foreach($category->items as $item)`, and iterables containing their own
/// parens — `@foreach($users->where('active', true) as $user)`.
pub fn parse_foreach_variables(arguments: &str) -> Vec<(String, String)> {
    lazy_static! {
        // Match only the binding (right of the LAST ` as `), so a parenthesized
        // iterable in the arg list can't truncate the capture.
        static ref BINDING_RE: Regex =
            Regex::new(r#"^\s*(?:\$(\w+)\s*=>\s*)?\$(\w+)\s*$"#).unwrap();
    }

    let mut vars = Vec::new();
    let Some(binding) = foreach_binding(arguments) else {
        return vars;
    };
    if let Some(caps) = BINDING_RE.captures(binding) {
        if let Some(key_match) = caps.get(1) {
            vars.push((key_match.as_str().to_string(), "mixed".to_string()));
        }
        if let Some(value_match) = caps.get(2) {
            vars.push((value_match.as_str().to_string(), "mixed".to_string()));
        }
    }
    vars
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

/// Find every loop directive on a single line, returning
/// `(directive_keyword, full_argument_substring_including_parens)` for each.
///
/// The argument list is captured by *balancing* parentheses rather than a
/// `\([^)]*\)` regex, which stops at the first `)`. Without balancing, an
/// iterable containing a call — `@foreach($users->where('active', true) as
/// $user)` — truncates to `($users->where('active', true)`, the ` as $user`
/// tail is lost, and the loop's variable goes unrecognized. That made the loop
/// scope invisible and the rename clobber the whole file (issue #55). Parens
/// inside single/double-quoted strings are ignored so a string argument can't
/// throw off the depth count.
fn loop_directives_on_line(line: &str) -> Vec<(&str, &str)> {
    lazy_static! {
        static ref LOOP_HEAD_RE: Regex =
            Regex::new(r#"@(foreach|forelse|for|while)\s*\("#).unwrap();
    }

    let mut out = Vec::new();
    for caps in LOOP_HEAD_RE.captures_iter(line) {
        let Some(directive) = caps.get(1) else {
            continue;
        };
        // The opening paren is the final byte of the whole match.
        let open = caps.get(0).unwrap().end() - 1;
        if let Some(close) = matching_paren(line, open) {
            out.push((directive.as_str(), &line[open..=close]));
        }
    }
    out
}

/// Given the byte index of an opening `(` in `s`, return the byte index of its
/// matching `)`, balancing nested parens and ignoring any parens that sit
/// inside single- or double-quoted string literals. Returns `None` when the
/// parens are unbalanced on this line. `(`, `)`, `'`, `"`, `\` are ASCII, so
/// the returned indices always land on char boundaries.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];
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

/// Find all loop blocks in Blade content and their boundaries.
/// Returns a list of loop blocks with start/end lines and extracted variables.
pub fn find_loop_blocks(content: &str) -> Vec<BladeLoopBlock> {
    lazy_static! {
        static ref LOOP_END_RE: Regex =
            Regex::new(r#"@(endforeach|endforelse|endfor|endwhile)"#).unwrap();
    }

    let mut blocks = Vec::new();
    let mut open_loops: Vec<(BladeLoopType, Vec<(String, String)>, Option<String>, usize)> =
        Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        for (directive, arguments) in loop_directives_on_line(line) {
            let (loop_type, variables, iterable) = match directive {
                "foreach" => (
                    BladeLoopType::Foreach,
                    parse_foreach_variables(arguments),
                    parse_foreach_iterable(arguments),
                ),
                "forelse" => (
                    BladeLoopType::Forelse,
                    parse_foreach_variables(arguments),
                    parse_foreach_iterable(arguments),
                ),
                "for" => (BladeLoopType::For, parse_for_variables(arguments), None),
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
