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

use crate::blade_loops::find_loop_blocks;
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

/// Replace the bytes of Blade comments and `@verbatim` regions with spaces,
/// preserving newlines and overall length so that byte offsets — and hence
/// line/column positions — are unchanged. Variable scanning runs over the
/// masked copy so a `$foo` inside `{{-- $foo --}}` is never rewritten.
fn mask_non_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();

    // Blade comments: `{{-- … --}}` (may span lines, non-nesting).
    mask_delimited(source, &mut out, "{{--", "--}}");
    // `@verbatim … @endverbatim`: Blade emits the body literally, so any
    // `$foo` inside is template text, not a live variable.
    mask_delimited(source, &mut out, "@verbatim", "@endverbatim");

    // `out` only ever had non-newline bytes replaced with spaces, so it
    // remains valid UTF-8 (we never split a multi-byte char: the delimiters
    // are ASCII and we blank ASCII-or-continuation bytes uniformly to b' '
    // only between ASCII delimiters — see the guard in `mask_delimited`).
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// Blank (replace with spaces, keeping `\n`) every region of `source` from an
/// `open` delimiter to the next `close` delimiter, inclusive. Operates on the
/// shared `out` buffer in place.
fn mask_delimited(source: &str, out: &mut [u8], open: &str, close: &str) {
    let mut search_from = 0;
    while let Some(rel) = source[search_from..].find(open) {
        let start = search_from + rel;
        let after_open = start + open.len();
        let end = match source[after_open..].find(close) {
            Some(rel_end) => after_open + rel_end + close.len(),
            None => source.len(), // unclosed: mask to end of file
        };
        for b in out.iter_mut().take(end).skip(start) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        search_from = end;
    }
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
    // Stage 1: blank comments + verbatim (these genuinely extend to EOF when
    // unclosed, matching `mask_non_code`).
    let stage1 = {
        let mut out: Vec<u8> = source.as_bytes().to_vec();
        mask_delimited(source, &mut out, "{{--", "--}}");
        mask_delimited(source, &mut out, "@verbatim", "@endverbatim");
        String::from_utf8(out).unwrap_or_else(|_| source.to_string())
    };
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
        let Some(rel_end) = source[after_open..].find("@endphp") else {
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

    let binding_blocks = loop_binding_ranges(source, name);

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
                // File-scoped: keep every span EXCEPT those that belong to a
                // loop block that binds `name` (a separate scope).
                None => !binding_blocks
                    .iter()
                    .any(|(start, end)| span.line >= *start && span.line <= *end),
            }
        })
        .collect()
}

/// File-scoped variable spans: every `$name` occurrence EXCEPT those inside a
/// loop block that re-binds `name`. This is the set a controller→view rename
/// rewrites in the template — a controller-passed variable is file-scoped, but
/// must never clobber a loop's same-named iteration variable (a distinct
/// scope). Equivalent to [`in_scope_spans`] for a cursor that sits outside
/// every binding block.
pub fn file_scope_spans(source: &str, name: &str) -> Vec<VarSpan> {
    let all = variable_spans(source, name);
    if all.is_empty() {
        return all;
    }
    let binding_blocks = loop_binding_ranges(source, name);
    all.into_iter()
        .filter(|span| {
            !binding_blocks
                .iter()
                .any(|(start, end)| span.line >= *start && span.line <= *end)
        })
        .collect()
}

/// Inclusive `[start_line, end_line]` ranges of every loop block that binds a
/// variable named `name`. An unclosed loop extends to `u32::MAX`.
fn loop_binding_ranges(source: &str, name: &str) -> Vec<(u32, u32)> {
    find_loop_blocks(source)
        .iter()
        .filter(|b| b.variables.iter().any(|(v, _)| v == name))
        .map(|b| {
            let start = b.start_line as u32;
            let end = b.end_line.map(|e| e as u32).unwrap_or(u32::MAX);
            (start, end)
        })
        .collect()
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
