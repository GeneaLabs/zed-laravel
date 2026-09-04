//! Scope-aware Blade variable rename, plus controller→view binding rename.
//!
//! Two linked capabilities sit on top of the existing rename engine:
//!
//! 1. **Scope-aware rename within a template.** Renaming `$foo` inside a
//!    `.blade.php` file rewrites only the occurrences in `$foo`'s *actual*
//!    scope. A variable introduced by `@foreach ($items as $foo)` (or
//!    `@forelse` / `@for`) is treated as block-scoped: renaming it touches
//!    only occurrences between the loop's open and close directives, and
//!    never an unrelated `$foo` elsewhere in the file. A variable that is
//!    *not* loop-introduced (a controller-passed view variable, an inline
//!    `@php $foo = …; @endphp`) is file-scoped — but a renamed file-scoped
//!    `$foo` still skips any nested loop block that *re-binds* `$foo`, since
//!    that inner `$foo` is a different variable (the "nested scope conflict"
//!    rule).
//!
//! 2. **Cross-file rename from controller into view.** Renaming a view-data
//!    binding key — `view('users.profile', ['name' => $name])` or
//!    `compact('name')` — rewrites the binding key *and* the in-view `$name`
//!    usages in lockstep. For `compact('name')` the controller's own local
//!    `$name` (within the enclosing function) is renamed too, because compact
//!    binds the view key *by the local's name* — leaving it behind would
//!    produce a `compact('newname')` with no matching `$newname` local.
//!
//! Positions are **0-based** throughout, matching the rest of the stack
//! (tree-sitter `Point`, LSP `Position`, every match struct). A [`VarSpan`]
//! covers the identifier *name only* — the leading `$` (for variables) or the
//! surrounding quotes (for binding-key strings) are deliberately excluded, so
//! a `TextEdit` over the span swaps just the name and leaves the sigil intact.
//!
//! The Blade side is line/regex based (consistent with [`crate::blade_loops`]
//! and [`crate::blade_php_block`]); the controller side is tree-sitter based
//! (consistent with [`crate::view_var_index`]). Both sides are pure functions
//! over source text so the wiring in `main.rs` owns all path resolution and
//! I/O, and every rule here is unit-testable without the LSP harness.

use tree_sitter::Node;

use crate::blade_loops::{
    find_loop_blocks, unbalanced_loop_head_lines, BladeLoopBlock, BladeLoopType,
};
use crate::parser::parse_php;

/// A 0-based span of an identifier name to rewrite. `start_col`..`end_col`
/// covers the name only — for a `$foo` variable it starts *after* the `$`;
/// for a `'key'` binding string it starts *inside* the opening quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VarSpan {
    pub line: u32,
    pub start_col: u32,
    pub end_col: u32,
}

impl VarSpan {
    fn new(line: u32, start_col: u32, end_col: u32) -> Self {
        Self {
            line,
            start_col,
            end_col,
        }
    }
}

/// Strip a leading `$` a user may have typed into the rename box (Zed/editors
/// pre-fill the name without the sigil, but a pasted `$bar` should still work)
/// and trim surrounding whitespace. The bare identifier is what gets written
/// at a variable span (the `$` already lives in the source) and inside a
/// binding-key string.
pub fn normalize_new_var_name(new_name: &str) -> String {
    new_name.trim().trim_start_matches('$').to_string()
}

/// Validate that `name` is a legal PHP variable / array-key identifier:
/// a letter or `_` followed by letters, digits, or `_`. Rename should reject
/// anything else rather than emit edits that produce invalid source.
pub fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ── Blade side: scope-aware variable spans ────────────────────────────────

/// Every `$name` variable occurrence in a Blade template, as name-only spans,
/// excluding occurrences inside Blade comments (`{{-- … --}}`) and
/// `@verbatim … @endverbatim` regions. Property accesses (`$x->name`) are not
/// matched — only the variable token itself, because `$name` and `$x->name`
/// are different identifiers.
pub fn variable_spans(source: &str, name: &str) -> Vec<VarSpan> {
    if !is_valid_identifier(name) {
        return Vec::new();
    }
    let masked = mask_non_code(source);
    let pattern = match regex::Regex::new(&format!(r"\${}\b", regex::escape(name))) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    let mut spans = Vec::new();
    for (line_idx, line) in masked.lines().enumerate() {
        for m in pattern.find_iter(line) {
            // `m` covers `$name`; the rewritable span is the name only,
            // so advance past the leading `$`.
            let start = (m.start() + 1) as u32;
            let end = m.end() as u32;
            spans.push(VarSpan::new(line_idx as u32, start, end));
        }
    }
    spans
}

/// Blank every region Blade does not execute — Blade comments, HTML comments
/// and `@verbatim` bodies — preserving newlines and overall length so byte
/// offsets, and hence line/column positions, are unchanged. Variable scanning
/// runs over the masked copy so a `$foo` inside `{{-- $foo --}}` is never
/// rewritten.
///
/// Delegates to the crate's single dead-region scanner (issue #369 Part A).
/// This used to hand-roll its own, and differed from the other two on two
/// axes: it ignored `<!-- -->`, and it masked an unterminated opener to end of
/// file. Both are now settled in one place —
/// [`crate::blade_directive_tokens::dead_region_spans`] documents why.
fn mask_non_code(source: &str) -> String {
    crate::blade_directive_tokens::blank_dead_regions(source)
}

/// True if `name` surfaces as a Blade *template* variable — at least one
/// `$name` occurrence in template markup (an echo `{{ $name }}`, a directive, a
/// loop header), i.e. OUTSIDE every `@php … @endphp` block, `@verbatim` region,
/// and `{{-- --}}` comment.
///
/// This is the `prepare_rename` admissibility gate (issue #55 AC: rename rejects
/// variables outside a recognized binding context). A `$name` whose only
/// occurrences live inside `@php` blocks — a PHP-block / function-local temp —
/// or that appears nowhere is NOT a renameable Blade variable and returns
/// `false`. Loop variables surface in their headers/bodies, `@php`-assigned
/// variables that are actually used surface at their echo sites, and
/// controller-passed variables surface at their usages, so all legitimately
/// renameable variables pass.
pub fn is_template_variable(source: &str, name: &str) -> bool {
    if !is_valid_identifier(name) {
        return false;
    }
    // `$loop` is Blade's reserved loop-status variable, injected inside every
    // `@foreach`/`@forelse`/`@for`/`@while` body — it never appears in a header,
    // so it has no resolvable binding scope and a rename would clobber every
    // `$loop` across unrelated loops. Renaming it is also semantically always
    // wrong (the name is framework-defined). Refuse at the prepare gate.
    if name == "loop" {
        return false;
    }
    let pattern = match regex::Regex::new(&format!(r"\${}\b", regex::escape(name))) {
        Ok(re) => re,
        Err(_) => return false,
    };
    mask_non_template(source)
        .lines()
        .any(|line| pattern.is_match(line))
}

/// Blank everything that is NOT live template markup: comments, `@verbatim`
/// regions, and closed `@php … @endphp` blocks. Used by [`is_template_variable`]
/// to tell a real Blade variable (surfaces in markup) from a PHP-block-only
/// local. Length/newlines are preserved, like [`mask_non_code`].
fn mask_non_template(source: &str) -> String {
    // Stage 1: blank every dead region, through the shared scanner. An
    // unterminated opener blanks nothing (issue #369 Part A) — masking to EOF
    // here would reject every later variable as out-of-context.
    let stage1 = mask_non_code(source);
    // Stage 2: blank closed `@php … @endphp` blocks, searching the
    // comment-masked text so a `@php` token inside a comment can't anchor a
    // spurious block.
    let mut out: Vec<u8> = stage1.as_bytes().to_vec();
    mask_php_blocks(&stage1, &mut out);
    String::from_utf8(out).unwrap_or(stage1)
}

/// Blank each *closed* `@php … @endphp` block (replacing non-newline bytes with
/// spaces). An unclosed `@php` is the inline `@php(expr)` directive form, not a
/// block — leave it as markup rather than masking the rest of the file, which
/// would wrongly reject every later variable as out-of-context.
fn mask_php_blocks(source: &str, out: &mut [u8]) {
    let mut search_from = 0;
    while let Some(rel) = source[search_from..].find("@php") {
        let start = search_from + rel;
        let after_open = start + "@php".len();
        // Require a word boundary after `@php` so a longer directive like
        // `@phpunit` / `@phpdoc` can't anchor a spurious block. A real `@php`
        // directive is followed by `(` (inline form), whitespace, or EOF.
        if source[after_open..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            search_from = after_open;
            continue;
        }
        // Locate the closing `@endphp` with PHP-string awareness: a literal
        // `@endphp` inside a string or comment in the block body does not close
        // the block (a plain substring search would close it early, leaving the
        // real PHP tail wrongly treated as renameable markup).
        let Some(rel_end) = find_block_terminator(&source[after_open..]) else {
            break; // unclosed `@php` (inline `@php(...)`) — not a block.
        };
        let end = after_open + rel_end + "@endphp".len();
        for b in out.iter_mut().take(end).skip(start) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        search_from = end;
    }
}

/// Within a `@php` block body, return the byte offset of the real `@endphp`
/// terminator — the first `@endphp` token that lies in PHP *code*, not inside a
/// string literal or comment. A literal `@endphp` inside a single-/double-quoted
/// string, a heredoc/nowdoc body, or a `//`, `#`, or `/* … */` comment does not
/// close the block. A symmetric word boundary (mirroring the `@php` opener guard)
/// keeps `@endphpunit` from matching. Returns `None` when the block is unclosed.
/// Single forward pass over the body — O(n), no backtracking.
fn find_block_terminator(body: &str) -> Option<usize> {
    let b = body.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        match b[i] {
            b'\'' => i = skip_php_quoted(b, i, b'\''),
            b'"' => i = skip_php_quoted(b, i, b'"'),
            b'/' if b[i..].starts_with(b"//") => i = skip_to_line_end(b, i),
            b'/' if b[i..].starts_with(b"/*") => i = skip_block_comment(b, i),
            b'#' if !b[i..].starts_with(b"#[") => i = skip_to_line_end(b, i),
            b'<' if b[i..].starts_with(b"<<<") => i = skip_heredoc(b, i).unwrap_or(i + 1),
            b'@' if b[i..].starts_with(b"@endphp") => {
                let after = i + "@endphp".len();
                let at_boundary =
                    after >= n || !(b[after].is_ascii_alphanumeric() || b[after] == b'_');
                if at_boundary {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Index just past the closing `quote` of a PHP string opening at `start`
/// (`b[start] == quote`). A backslash escapes the next byte — covering `\'`/`\\`
/// in single-quoted and `\"`/`\\`/… in double-quoted strings — so an escaped
/// quote does not terminate the string. The ASCII delimiters never collide with
/// UTF-8 continuation bytes, so byte scanning is multi-byte safe. Unterminated →
/// end of input.
fn skip_php_quoted(b: &[u8], start: usize, quote: u8) -> usize {
    let n = b.len();
    let mut i = start + 1;
    while i < n {
        if b[i] == b'\\' {
            i += 2; // skip the escaped byte
        } else if b[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    n
}

/// Index of the next line feed at or after `i` (or end of input) — skips a `//`
/// or `#` line comment so an `@endphp` inside it isn't mistaken for a string
/// opener's apostrophe (`// don't …`) or treated as the terminator.
fn skip_to_line_end(b: &[u8], i: usize) -> usize {
    match b[i..].iter().position(|&c| c == b'\n') {
        Some(off) => i + off,
        None => b.len(),
    }
}

/// Index just past the closing `*/` of a block comment opening at `i`
/// (`b[i..]` begins with `/*`). Unterminated → end of input.
fn skip_block_comment(b: &[u8], i: usize) -> usize {
    let n = b.len();
    let mut j = i + 2;
    while j + 1 < n {
        if b[j] == b'*' && b[j + 1] == b'/' {
            return j + 2;
        }
        j += 1;
    }
    n
}

/// If a heredoc/nowdoc opens at `start` (`b[start..]` begins with `<<<`), return
/// the index just past its closing label, skipping the whole body — any
/// `@endphp` inside it is literal text. Mirrors PHP: optional spaces/tabs, an
/// optional `"`/`'` wrapping the label (nowdoc uses `'`), then the label; the
/// body runs until a line whose first non-whitespace content is the label
/// followed by a non-identifier byte (PHP 7.3+ allows the closer to be
/// indented). Returns `None` when `<<<` is not a valid opener, so the caller
/// treats it as ordinary code.
fn skip_heredoc(b: &[u8], start: usize) -> Option<usize> {
    let n = b.len();
    let mut i = start + 3; // past "<<<"
    while i < n && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let quote = match b.get(i) {
        Some(&q) if q == b'"' || q == b'\'' => {
            i += 1;
            Some(q)
        }
        _ => None,
    };
    let label_start = i;
    if b.get(i)
        .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_')
    {
        i += 1;
        while b
            .get(i)
            .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_')
        {
            i += 1;
        }
    } else {
        return None; // no valid label → not a heredoc opener
    }
    let label = &b[label_start..i];
    if let Some(q) = quote {
        if b.get(i) == Some(&q) {
            i += 1;
        } else {
            return None; // mismatched opening quote → not a heredoc opener
        }
    }
    if b.get(i) == Some(&b'\r') {
        i += 1;
    }
    if b.get(i) == Some(&b'\n') {
        i += 1;
    } else {
        return None; // label not terminated by a newline → not a heredoc opener
    }
    // Scan body lines for the closing label.
    loop {
        let mut j = i;
        while j < n && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if b[j..].starts_with(label) {
            let after = j + label.len();
            let at_boundary = after >= n || !(b[after].is_ascii_alphanumeric() || b[after] == b'_');
            if at_boundary {
                return Some(after);
            }
        }
        match b[i..].iter().position(|&c| c == b'\n') {
            Some(off) => i += off + 1,
            None => return Some(n), // unterminated heredoc → consume to EOF
        }
    }
}

/// The set of variable spans a rename should rewrite when the user invokes
/// rename on the `$name` occurrence at `cursor_line` in a Blade template.
///
/// Scope resolution:
/// - If `cursor_line` falls inside the innermost `@foreach`/`@forelse`/`@for`
///   block that *introduces* `name`, the rename is scoped to that block's
///   `[start_line, end_line]` (inclusive of both directive lines).
/// - Otherwise the rename is file-scoped.
///
/// In both cases, spans that fall inside a more-deeply-nested loop block that
/// *re-binds* `name` are excluded — that inner `$name` is a distinct variable
/// (nested scope conflict). The returned spans always include the cursor's own
/// occurrence and are sorted by position.
pub fn in_scope_spans(source: &str, name: &str, cursor_line: u32) -> Vec<VarSpan> {
    let all = variable_spans(source, name);
    if all.is_empty() {
        return all;
    }

    // Fail-closed: a rename whose cursor sits in an *unresolved* loop region —
    // an opaque loop (binding unparseable) or below a broken (unbalanced-paren)
    // loop header — is refused rather than risk a file-wide clobber. Safe over
    // complete, mirroring the `global`/`compact` refusals in the PHP engine.
    let unresolved = unresolved_loop_ranges(source);
    if range_contains_line(&unresolved, cursor_line) {
        return Vec::new();
    }

    // Loop blocks are resolved over the comment/`@verbatim`-masked copy, exactly
    // like `variable_spans`. Without this, a `@foreach` inside a `{{-- --}}`
    // comment is a phantom binding block, and a paren inside such a comment
    // desyncs the header parse — both collapse the scope and clobber the file.
    let binding_blocks: Vec<(u32, u32)> = find_loop_blocks(&mask_non_code(source))
        .iter()
        .filter(|b| b.variables.iter().any(|(v, _)| v == name))
        .map(block_range)
        .collect();

    // The cursor's binding scope: the innermost (largest start_line) binding
    // block that contains the cursor line. `None` ⇒ file scope.
    let cursor_scope: Option<(u32, u32)> = binding_blocks
        .iter()
        .filter(|(start, end)| cursor_line >= *start && cursor_line <= *end)
        .max_by_key(|(start, _)| *start)
        .copied();

    all.into_iter()
        .filter(|span| {
            match cursor_scope {
                // Loop-scoped: keep spans inside the binding block, but drop
                // any that sit in a strictly-nested block that re-binds `name`.
                Some((start, end)) => {
                    if span.line < start || span.line > end {
                        return false;
                    }
                    !in_nested_shadow(span.line, (start, end), &binding_blocks)
                }
                // File-scoped: keep every span EXCEPT those inside a loop that
                // re-binds `name` (a distinct scope) or inside an unresolved
                // loop region (whose scope is unknown — never clobber into it).
                None => {
                    !range_contains_line(&binding_blocks, span.line)
                        && !range_contains_line(&unresolved, span.line)
                }
            }
        })
        .collect()
}

/// File-scoped variable spans: every `$name` occurrence EXCEPT those inside a
/// loop block that re-binds `name` (a distinct scope) or an unresolved loop
/// region (an opaque loop or a broken header — never clobber into one). This is
/// the set a controller→view rename rewrites in the template — a
/// controller-passed variable is file-scoped, but must never clobber a loop's
/// same-named iteration variable. Equivalent to [`in_scope_spans`] for a cursor
/// that sits outside every binding block.
pub fn file_scope_spans(source: &str, name: &str) -> Vec<VarSpan> {
    let all = variable_spans(source, name);
    if all.is_empty() {
        return all;
    }
    let mut exclude: Vec<(u32, u32)> = find_loop_blocks(&mask_non_code(source))
        .iter()
        .filter(|b| b.variables.iter().any(|(v, _)| v == name))
        .map(block_range)
        .collect();
    exclude.extend(unresolved_loop_ranges(source));
    all.into_iter()
        .filter(|span| !range_contains_line(&exclude, span.line))
        .collect()
}

/// Inclusive `[start_line, end_line]` range of a loop block. An unclosed loop
/// extends to `u32::MAX`.
fn block_range(b: &BladeLoopBlock) -> (u32, u32) {
    let start = b.start_line as u32;
    let end = b.end_line.map(|e| e as u32).unwrap_or(u32::MAX);
    (start, end)
}

/// A `@foreach`/`@forelse` whose binding yielded no variable — a header the
/// parser couldn't resolve (a garbled or commented binding, an unsupported
/// destructuring shape). Such a loop's scope is unknown, so a rename touching it
/// must fail closed. `@for`/`@while` legitimately bind nothing (`@for(;;)`,
/// `@while($cond)`), so they are never opaque.
fn is_opaque_loop(b: &BladeLoopBlock) -> bool {
    matches!(b.loop_type, BladeLoopType::Foreach | BladeLoopType::Forelse) && b.variables.is_empty()
}

/// Whether `line` falls within any of the inclusive `[start, end]` ranges.
fn range_contains_line(ranges: &[(u32, u32)], line: u32) -> bool {
    ranges
        .iter()
        .any(|(start, end)| line >= *start && line <= *end)
}

/// Inclusive line ranges where loop scope is *unresolved*, so a rename must fail
/// closed rather than guess. Two sources:
/// - **Opaque loops** — a `@foreach`/`@forelse` whose binding the parser
///   couldn't resolve into any `$ident` (a garbled or commented binding, an
///   unsupported destructuring shape). Range = the block.
/// - **Broken headers** — a loop directive whose parentheses never balance
///   (e.g. a missing `)`). [`find_loop_blocks`] can't form a block and the
///   scope structure below it is unreliable, so the range runs to EOF.
///
/// Computed over the comment/`@verbatim`-masked copy, so a paren or `@foreach`
/// inside a comment never counts. A rename whose cursor sits in one of these is
/// refused; a file-scoped rename never rewrites occurrences inside one.
fn unresolved_loop_ranges(source: &str) -> Vec<(u32, u32)> {
    let masked = mask_non_code(source);
    let mut ranges: Vec<(u32, u32)> = find_loop_blocks(&masked)
        .iter()
        .filter(|b| is_opaque_loop(b))
        .map(block_range)
        .collect();
    for line in unbalanced_loop_head_lines(&masked) {
        ranges.push((line as u32, u32::MAX));
    }
    ranges
}

/// Whether `cursor_line` sits in an unresolved loop region — an opaque loop or
/// below a broken loop header (see [`unresolved_loop_ranges`]). A rename there
/// would risk a file-wide clobber, so it is refused. Backs the `prepare_rename`
/// gate so F2 is never offered there; [`in_scope_spans`] enforces the same
/// refusal on the edit path.
pub fn cursor_in_unresolved_loop(source: &str, cursor_line: u32) -> bool {
    range_contains_line(&unresolved_loop_ranges(source), cursor_line)
}

/// True if `line` falls inside a binding block that is strictly nested within
/// `outer` (i.e. a different block whose range is contained in `outer`). Used
/// to carve nested shadows out of a loop-scoped rename.
fn in_nested_shadow(line: u32, outer: (u32, u32), binding_blocks: &[(u32, u32)]) -> bool {
    binding_blocks.iter().any(|&(start, end)| {
        let is_outer = start == outer.0 && end == outer.1;
        let nested_within_outer = start >= outer.0 && end <= outer.1;
        !is_outer && nested_within_outer && line >= start && line <= end
    })
}

// ── Controller side: view-data binding key under the cursor ───────────────

/// How a view variable was bound at the controller render site. Determines the
/// extra controller-local edits a key rename needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingForm {
    /// `view('v', ['key' => $expr])` or `->with(['key' => $expr])`. The value
    /// expression is independent of the key, so only the key string moves.
    ArrayKey,
    /// `compact('key')`. The key *is* the controller-local variable name, so
    /// the enclosing-function local `$key` is renamed alongside the string.
    Compact,
}

/// A view-data binding key located under the cursor in a controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewBinding {
    /// The rendered view name (`users.profile`), for resolving the template.
    pub view_name: String,
    /// The current binding key / view-variable name (`name`).
    pub key: String,
    /// Span of the key text inside its quotes — the rewrite target.
    pub key_span: VarSpan,
    pub form: BindingForm,
}

/// If the cursor sits on the **key** of a view-data binding in a PHP
/// controller, return the binding. Recognized shapes (cursor on the key):
/// - `view('users.profile', ['name' => $expr])`
/// - `view('users.profile', compact('name'))`
///
/// The `->with(['name' => …])` / `->with('name', …)` chained forms are not yet
/// routed (out of scope for #55) and return `None`.
///
/// Returns `None` when the cursor is anywhere else (the view name, a value
/// expression, an unrelated string), or when the view name can't be resolved
/// to a single string literal.
pub fn view_binding_key_at(php_source: &str, line: u32, col: u32) -> Option<ViewBinding> {
    let tree = parse_php(php_source).ok()?;
    let bytes = php_source.as_bytes();
    let root = tree.root_node();

    // Collect every `view(...)` call with a resolvable view name, then probe
    // its data argument(s) for a key string under the cursor.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_call_expression" {
            if let Some(binding) = binding_in_view_call(node, bytes, line, col) {
                return Some(binding);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Probe a single `view(...)` call expression (and any chained `->with(...)`)
/// for a binding key under the cursor.
fn binding_in_view_call(call: Node, bytes: &[u8], line: u32, col: u32) -> Option<ViewBinding> {
    if call_function_name(call, bytes)? != "view" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let arg_nodes = positional_args(args);

    // First argument is the view name (a single string literal).
    let view_name = string_literal_text(*arg_nodes.first()?, bytes)?;

    // Second argument carries the data: an array literal or `compact(...)`.
    if let Some(second) = arg_nodes.get(1) {
        if let Some(binding) = binding_in_data_arg(*second, bytes, &view_name, line, col) {
            return Some(binding);
        }
    }
    None
}

/// Inspect a `view()` data argument — an array literal or a `compact(...)`
/// call — for a key string under the cursor.
fn binding_in_data_arg(
    arg: Node,
    bytes: &[u8],
    view_name: &str,
    line: u32,
    col: u32,
) -> Option<ViewBinding> {
    match arg.kind() {
        "array_creation_expression" => {
            array_key_at(arg, bytes, view_name, line, col, BindingForm::ArrayKey)
        }
        "function_call_expression" if call_function_name(arg, bytes) == Some("compact") => {
            compact_key_at(arg, bytes, view_name, line, col)
        }
        _ => None,
    }
}

/// Find an `'key' => …` array element whose key string contains the cursor.
fn array_key_at(
    array: Node,
    bytes: &[u8],
    view_name: &str,
    line: u32,
    col: u32,
    form: BindingForm,
) -> Option<ViewBinding> {
    let mut cursor = array.walk();
    for element in array.children(&mut cursor) {
        if element.kind() != "array_element_initializer" {
            continue;
        }
        // `array_element_initializer` for `'k' => v` has the key as its first
        // named child (the `=>` makes it a two-child initializer).
        let mut ec = element.walk();
        let named: Vec<Node> = element.children(&mut ec).filter(|n| n.is_named()).collect();
        if named.len() < 2 {
            continue; // value-only element (no key) — not a binding key.
        }
        let key_node = named[0];
        if let Some(span) = string_content_span_at(key_node, bytes, line, col) {
            let key = string_literal_text(key_node, bytes)?;
            return Some(ViewBinding {
                view_name: view_name.to_string(),
                key,
                key_span: span,
                form,
            });
        }
    }
    None
}

/// Find a `compact('key', …)` string argument containing the cursor.
fn compact_key_at(
    call: Node,
    bytes: &[u8],
    view_name: &str,
    line: u32,
    col: u32,
) -> Option<ViewBinding> {
    let args = call.child_by_field_name("arguments")?;
    for arg in positional_args(args) {
        if let Some(span) = string_content_span_at(arg, bytes, line, col) {
            let key = string_literal_text(arg, bytes)?;
            return Some(ViewBinding {
                view_name: view_name.to_string(),
                key,
                key_span: span,
                form: BindingForm::Compact,
            });
        }
    }
    None
}

/// Unwrap a call's `arguments` node into the positional expression nodes,
/// peeling the grammar's `argument` wrapper where present (mirrors the helper
/// in [`crate::view_var_index`]).
fn positional_args(arguments: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = arguments.walk();
    for arg in arguments.named_children(&mut cursor) {
        if arg.kind() == "argument" {
            let mut ac = arg.walk();
            if let Some(expr) = arg.named_children(&mut ac).last() {
                out.push(expr);
            }
        } else {
            out.push(arg);
        }
    }
    out
}

/// Controller-local `$name` spans within the function/method enclosing the
/// byte offset of `anchor` — used for the `compact('name')` case, where the
/// view key is bound by the local's name and must be renamed alongside it.
/// Returns name-only spans (after the `$`).
pub fn enclosing_function_local_spans(
    php_source: &str,
    name: &str,
    anchor: VarSpan,
) -> Vec<VarSpan> {
    let tree = match parse_php(php_source) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let bytes = php_source.as_bytes();
    let root = tree.root_node();

    // Find the innermost function-like node containing the anchor line.
    let mut best: Option<Node> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "function_definition"
                | "method_declaration"
                // tree-sitter-php 0.24 names the closure node `anonymous_function`;
                // the `_creation_expression` alias is the older grammar's name. Match
                // both (repo convention, e.g. `route_chain.rs`, `query_chain/flow.rs`)
                // so a `compact()` inside a route closure elects the closure as its
                // scope instead of falling back to the whole file.
                | "anonymous_function"
                | "anonymous_function_creation_expression"
                | "arrow_function"
        ) {
            let s = node.start_position().row as u32;
            let e = node.end_position().row as u32;
            if anchor.line >= s && anchor.line <= e {
                // Prefer the innermost (latest, smallest) enclosing scope.
                best = Some(match best {
                    Some(prev) if prev.start_position().row >= node.start_position().row => prev,
                    _ => node,
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    let scope = best.unwrap_or(root);
    let mut spans = Vec::new();
    collect_variable_name_spans(scope, bytes, name, &mut spans);
    spans.sort();
    spans
}

/// Every rewrite span a controller→view binding-key rename produces, split by
/// file. Pure data — the caller (`main.rs`) owns view-file location, name
/// validation, and turning spans into LSP `TextEdit`s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BindingRenameSpans {
    /// Spans inside the controller: the binding key string, plus (for
    /// `compact`) the enclosing-function local `$key`. Sorted by position.
    pub controller: Vec<VarSpan>,
    /// File-scoped `$key` spans inside the resolved view template.
    pub view: Vec<VarSpan>,
}

/// Cross-file orchestration core: given a located [`ViewBinding`], the
/// controller source, and the resolved view source, compute every rewrite span
/// across BOTH files. This is the I/O-free heart of `view_binding_rename_edit`,
/// unit-tested independently of LSP file location and config.
///
/// - The controller always gets the key-string span. For [`BindingForm::Compact`]
///   it also gets the enclosing-function local `$key` spans (compact binds the
///   view variable BY the local's name, so the local must move too).
/// - The view gets the file-scoped `$key` usages
///   ([`file_scope_spans`] — minus any loop block that re-binds `key`, a
///   separate scope). Pass `view_source = None` when the view can't be located;
///   only the controller spans are then returned.
///
/// Because the view spans are restricted to `binding.key`, a different binding
/// key bound to the same view from another controller (e.g. `$other`) is never
/// touched — its spans don't intersect this rename's.
pub fn binding_rename_spans(
    binding: &ViewBinding,
    controller_source: &str,
    view_source: Option<&str>,
) -> BindingRenameSpans {
    let mut controller = vec![binding.key_span];
    if binding.form == BindingForm::Compact {
        controller.extend(enclosing_function_local_spans(
            controller_source,
            &binding.key,
            binding.key_span,
        ));
    }
    controller.sort();

    let view = view_source
        .map(|src| file_scope_spans(src, &binding.key))
        .unwrap_or_default();

    BindingRenameSpans { controller, view }
}

/// Walk `node`, pushing a name-only [`VarSpan`] for every `variable_name`
/// whose identifier equals `name` (`$name`) — but NOT descending into a nested
/// closure that captures `name` in a separate scope. A plain
/// `function () { … }` doesn't see the enclosing `$name` (PHP closures capture
/// nothing without a `use` clause), so its body `$name` is a *different*
/// variable and must be left untouched; an arrow function or a `use ($name)`
/// closure shares the outer variable, so we keep descending.
fn collect_variable_name_spans(node: Node, bytes: &[u8], name: &str, out: &mut Vec<VarSpan>) {
    if node.kind() == "variable_name" {
        if let Ok(text) = node.utf8_text(bytes) {
            if text == format!("${name}") {
                let start = node.start_position();
                let end = node.end_position();
                // Skip the leading `$` so the span covers the name only.
                out.push(VarSpan::new(
                    start.row as u32,
                    start.column as u32 + 1,
                    end.column as u32,
                ));
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_isolated_closure(child, bytes, name) {
            continue;
        }
        collect_variable_name_spans(child, bytes, name, out);
    }
}

/// True if `node` is an anonymous function that does NOT see the enclosing
/// `$name` — a `function () { … }` (or its `_creation_expression` form) with no
/// `use ($name)` clause. Such a closure is an independent scope, so a rename of
/// the enclosing-function `$name` must not descend into it (issue #55: renaming
/// a `compact()` binding once clobbered an unrelated closure variable). Arrow
/// functions auto-capture, and `use ($name)` closures explicitly capture, so
/// both return `false` and the caller keeps descending. Mirrors the capture
/// rule in [`crate::query_chain::flow`].
fn is_isolated_closure(node: Node, bytes: &[u8], name: &str) -> bool {
    if !matches!(
        node.kind(),
        "anonymous_function" | "anonymous_function_creation_expression"
    ) {
        return false;
    }
    // A `use (…)` clause that lists `$name` (by value or `&`-reference) binds
    // the body `$name` to the outer variable's name, so the closure is NOT
    // isolated — descend so the capture and its uses rename together.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "anonymous_function_use_clause" {
            let mut inner = child.walk();
            for v in child.children(&mut inner) {
                if v.kind() == "variable_name" {
                    if let Ok(t) = v.utf8_text(bytes) {
                        if t.trim_start_matches('$') == name {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

// ── tree-sitter helpers ───────────────────────────────────────────────────

/// The bare callee name of a `function_call_expression` (`view`, `compact`),
/// or `None` for method calls / dynamic callees.
fn call_function_name<'a>(call: Node<'a>, bytes: &'a [u8]) -> Option<&'a str> {
    let func = call.child_by_field_name("function")?;
    if func.kind() == "name" {
        func.utf8_text(bytes).ok()
    } else {
        None
    }
}

/// The text content of a PHP string-literal node (`'users.profile'`), with the
/// surrounding quotes stripped. `None` for interpolated / non-string nodes.
fn string_literal_text(node: Node, bytes: &[u8]) -> Option<String> {
    let raw = node.utf8_text(bytes).ok()?;
    let trimmed = raw.trim();
    // Require the SAME quote on both ends (`'foo'` / `"foo"`), not just any
    // quote on each end — otherwise a mismatched `"foo'` would slip through.
    let open = trimmed.chars().next()?;
    let close = trimmed.chars().next_back()?;
    if trimmed.len() >= 2 && (open == '\'' || open == '"') && open == close {
        Some(trimmed[1..trimmed.len() - 1].to_string())
    } else {
        None
    }
}

/// If `node` is a single-line string literal whose *content* (inside the
/// quotes) contains `(line, col)`, return the content span. Used to detect the
/// cursor landing on a binding key, and to target the rewrite at the key text
/// without disturbing the quotes.
fn string_content_span_at(node: Node, bytes: &[u8], line: u32, col: u32) -> Option<VarSpan> {
    string_literal_text(node, bytes)?; // ensure it's a quoted string
    let start = node.start_position();
    let end = node.end_position();
    if start.row != end.row {
        return None; // multi-line strings can't be a simple binding key
    }
    let content_start = start.column as u32 + 1; // inside opening quote
    let content_end = end.column as u32 - 1; // before (== position of) closing quote
                                             // `content_end` is the closing quote's column; the key text occupies
                                             // `content_start..content_end`, so accept the cursor only strictly before
                                             // the closing quote (landing ON the quote isn't on the key).
    if line == start.row as u32 && col >= content_start && col < content_end {
        Some(VarSpan::new(line, content_start, content_end))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
