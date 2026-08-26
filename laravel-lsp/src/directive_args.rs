//! Argument-list parsing for Blade directives.
//!
//! A directive's arguments are PHP expressions, and the view name a directive
//! points at is identified by its *position* in the argument list — not by
//! being the first quoted string in it. Condition-first directives
//! (`@includeWhen`, `@includeUnless`) put a boolean expression first, and
//! that expression frequently contains a string literal of its own:
//!
//! ```text
//! @includeWhen($type === 'admin', 'pages.admin')
//! ```
//!
//! A "first quoted string wins" scan answers `admin` there — a wrong
//! goto-definition target and a wrong outline label. Splitting the list at
//! its first *top-level* comma — one that is not inside a quoted string, a
//! nested `(...)`, or a `[...]` array literal — and reading the argument at
//! the position the directive actually uses answers `pages.admin`.
//!
//! Bracket tracking is load-bearing rather than defensive:
//! `@includeWhen(in_array($k, ['a', 'b']), 'pages.list')` carries two commas
//! inside its condition, and any split that ignores nesting picks one of them.

/// Directives whose first argument is a condition, putting the view name in
/// the *second*.
///
/// Single-sourced because three independent call sites need this list —
/// goto-definition (`main.rs`), find-references (`references.rs`), and the
/// document outline (`document_symbols.rs`) — and drift between private
/// copies is exactly how `@includeUnless` came to be handled by
/// goto-definition while find-references never recognised it at all.
pub fn is_condition_first(directive: &str) -> bool {
    matches!(directive, "includeWhen" | "includeUnless")
}

/// The string literal at top-level argument position `index`.
///
/// `None` when that argument is absent, is not a string literal (`$view`,
/// `$a ?: $b`, `'partials.' . $name`), or is the empty literal — an empty
/// name resolves to no file, so it is reported absent rather than passed on.
pub fn nth_literal(args: &str, index: usize) -> Option<String> {
    quoted_literal(split_top_level(args).get(index)?).map(str::to_string)
}

/// The first string literal anywhere in `args`, at any nesting depth.
///
/// The rule for directives that are *not* condition-first, whose name is
/// routinely nested inside an array literal rather than sitting at a
/// top-level argument position: `@props(['title', 'count' => 0])` and
/// `@includeFirst(['custom.layout', 'default.layout'])` both carry it one
/// level down.
pub fn first_literal(args: &str) -> Option<String> {
    let mut scan = ArgScanner::default();
    let mut start = 0usize;
    for (i, ch) in args.char_indices() {
        let was_open = scan.in_string();
        scan.step(ch);
        match (was_open, scan.in_string()) {
            (false, true) => start = i + ch.len_utf8(),
            (true, false) => {
                let literal = &args[start..i];
                return (!literal.is_empty()).then(|| literal.to_string());
            }
            _ => {}
        }
    }
    None
}

/// Split an argument list into its top-level arguments, with surrounding
/// whitespace trimmed from each.
///
/// The directive-call parentheses are removed first when present. Always
/// returns at least one element, so an empty list yields `[""]`.
fn split_top_level(args: &str) -> Vec<&str> {
    let inner = unwrap_parens(args);
    let mut scan = ArgScanner::default();
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, ch) in inner.char_indices() {
        let top = scan.at_top_level();
        scan.step(ch);
        if top && ch == ',' {
            out.push(inner[start..i].trim());
            start = i + ch.len_utf8();
        }
    }
    out.push(inner[start..].trim());
    out
}

/// The contents of `arg` when it is exactly a quoted string literal.
///
/// Anything trailing the closing quote disqualifies it: `'partials.' . $name`
/// is a concatenation whose value is not knowable from source, and reporting
/// its first fragment as a name — which the loose quote-trimming this
/// replaced did — produces a target that can never resolve.
fn quoted_literal(arg: &str) -> Option<&str> {
    let trimmed = arg.trim();
    let mut chars = trimmed.chars();
    let quote = chars.next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = chars.as_str();

    let mut scan = ArgScanner::default();
    scan.step(quote);
    for (i, ch) in rest.char_indices() {
        scan.step(ch);
        if !scan.in_string() {
            let literal = &rest[..i];
            return rest[i + ch.len_utf8()..]
                .trim()
                .is_empty()
                .then_some(literal)
                .filter(|l| !l.is_empty());
        }
    }
    None
}

/// The argument list with the directive-call parentheses removed.
///
/// `queries.rs` documents both shapes reaching the extractors — `('view')`
/// and a bare `'view'` — so the wrapper is detected rather than assumed: a
/// leading `(` only wraps the list when its match is the final character.
fn unwrap_parens(args: &str) -> &str {
    let trimmed = args.trim();
    let Some(body) = trimmed.strip_prefix('(') else {
        return trimmed;
    };
    let mut scan = ArgScanner::default();
    for (i, ch) in body.char_indices() {
        let top = scan.at_top_level();
        scan.step(ch);
        if top && ch == ')' {
            return if body[i + ch.len_utf8()..].trim().is_empty() {
                &body[..i]
            } else {
                trimmed
            };
        }
    }
    // Unclosed args — everything after the opening paren is the list.
    body
}

/// Tracks whether a scan position sits inside a string literal or inside
/// nested brackets, so that commas and parens belonging to sub-expressions
/// are not mistaken for structure of the outer argument list.
#[derive(Default)]
struct ArgScanner {
    depth: usize,
    quote: Option<char>,
    escaped: bool,
}

impl ArgScanner {
    /// Advance over one character.
    fn step(&mut self, ch: char) {
        if let Some(quote) = self.quote {
            if self.escaped {
                self.escaped = false;
            } else if ch == '\\' {
                self.escaped = true;
            } else if ch == quote {
                self.quote = None;
            }
            return;
        }
        match ch {
            '\'' | '"' => self.quote = Some(ch),
            '(' | '[' => self.depth += 1,
            // `saturating_sub` rather than `-`: half-typed source reaches the
            // parser constantly, and a stray closer must not panic the server.
            ')' | ']' => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
    }

    fn in_string(&self) -> bool {
        self.quote.is_some()
    }

    fn at_top_level(&self) -> bool {
        self.quote.is_none() && self.depth == 0
    }
}

#[cfg(test)]
mod tests;
