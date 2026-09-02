//! Locate the source-text position of a Laravel config-array key via
//! tree-sitter — column-accurate, suitable for building a rename
//! `WorkspaceEdit`.
//!
//! The companion to `config_lookup.rs`: that module resolves the *value* for
//! a dotted config key (`"app.name"` → `"env('APP_NAME', 'Laravel')"`),
//! whereas this one finds the (line, start_column, end_column) of the *key*
//! string itself so the rename machinery can rewrite it in place.
//!
//! Walks the PHP AST rather than scanning bytes:
//!
//! 1. Find the top-level `return [...];` array literal (skipping any leading
//!    `<?php`, declarations, or comments — tree-sitter handles that for us).
//! 2. Descend into nested `array_creation_expression`s following the dotted
//!    path. `"database.connections.mysql.host"` walks `database` → array →
//!    `connections` → array → `mysql` → array → `host`.
//! 3. Match keys by their `string_content` text, ignoring quote style.
//! 4. When the path reaches the leaf, return that key string's content
//!    position.
//!
//! The walker is conservative: anything that's not a literal `string =>
//! value` array entry (e.g. dynamic keys built from `env(...)`, spread
//! syntax, numeric keys) is skipped silently. The rename operation simply
//! produces fewer `TextEdit`s — never an incorrect edit.

use std::path::{Path, PathBuf};
use tree_sitter::Node;

/// Position of a config-key string literal's *content* (no quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPosition {
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
    /// How the key is written in the source. Each consumer accepts a
    /// different set — see [`KeyKind`].
    pub kind: KeyKind,
}

/// How an array key is spelled in the source, which decides what each feature
/// may do with it. Every kind is equally *resolvable* — Laravel finds a value
/// for all three — so completion and go-to-definition accept all of them. The
/// distinctions matter only to features that need the key's own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// `'name' => …` or `"name" => …`. The span covers the text inside the
    /// quotes, so a rename can rewrite it in place. The only renameable kind.
    Quoted,
    /// `404 => …`. Real text at a real position, so a code lens can annotate
    /// it, but a rename must not: replacing the bare digits with a name would
    /// turn a string key into a constant lookup.
    BareInteger,
    /// The `0` in `['sm', 'md']` — an index PHP assigns, written nowhere. It
    /// is a navigable key (`sizes.0` resolves), but there is no text to
    /// annotate or rewrite, so the span is zero-width at the value's start.
    SynthesizedIndex,
}

/// Locate the source position of `dotted_key` (e.g. `"app.name"`,
/// `"database.connections.mysql.host"`) under a Laravel project root.
/// Returns `None` if the file or any path segment is missing.
pub fn locate_key(root: &Path, dotted_key: &str) -> Option<KeyPosition> {
    locate_key_all(root, &[], dotted_key)
        .into_iter()
        .next()
        .map(|(_, position)| position)
}

/// Locate `dotted_key` in **every** file contributing to its config group —
/// the project `config/{group}.php` plus each module's config file (see
/// [`crate::config::config_group_files`], which already orders by
/// descending merge precedence — the file whose value wins first), so `.first()`
/// is the primary declaration and the full list feeds find-references and
/// rename across merged declarations.
pub fn locate_key_all(
    root: &Path,
    module_dirs: &[PathBuf],
    dotted_key: &str,
) -> Vec<(PathBuf, KeyPosition)> {
    let mut parts = dotted_key.split('.');
    let Some(file) = parts.next() else {
        return Vec::new();
    };
    let path_segments: Vec<&str> = parts.collect();
    if path_segments.is_empty() {
        return Vec::new();
    }

    crate::config::config_group_files(root, module_dirs, file)
        .into_iter()
        .filter_map(|config_path| {
            let content = std::fs::read_to_string(&config_path).ok()?;
            let position = locate_in_source(&content, &path_segments)?;
            Some((config_path, position))
        })
        .collect()
}

/// Source-only variant for unit tests — operates on a string rather than
/// reading from disk.
pub fn locate_in_source(source: &str, key_path: &[&str]) -> Option<KeyPosition> {
    if key_path.is_empty() {
        return None;
    }
    let tree = crate::parser::parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let array_node = find_return_array(tree.root_node())?;
    locate_at_path(array_node, bytes, key_path)
}

/// Enumerate every string-keyed entry in a config/lang `return [...]` array,
/// in document order, as `(in-file dotted path, key position)`. Both leaf and
/// intermediate keys are emitted (`database.connections` AND
/// `database.connections.mysql.host`), since each is a referenceable
/// `config()`/`__()` key. Non-string keys (numeric list entries, dynamic keys)
/// are skipped. The caller prepends the file stem to form the full dotted key
/// (`database.` / `auth.`). Powers config + translation code lenses (#59).
pub fn enumerate_keys_in_source(source: &str) -> Vec<(String, KeyPosition)> {
    enumerate_entries_in_source(source)
        .into_iter()
        .map(|(key, _value, position)| (key, position))
        .collect()
}

/// [`enumerate_keys_in_source`] plus each entry's scalar value text, as
/// `(in-file dotted path, value text, key position)`.
///
/// The value is for display only — translation completion shows it beside the
/// key. It is the string's content without quotes, the raw source text for any
/// other scalar, and empty for an array-valued (intermediate) key, which has
/// no scalar of its own to show.
pub fn enumerate_entries_in_source(source: &str) -> Vec<(String, String, KeyPosition)> {
    let Ok(tree) = crate::parser::parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let Some(array) = find_return_array(tree.root_node()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_keys(array, bytes, "", &mut out);
    out
}

/// Recurse an array literal, accumulating the dotted key path. `prefix` is the
/// path to `array` (empty at the top level).
fn collect_keys(
    array: Node,
    source: &[u8],
    prefix: &str,
    out: &mut Vec<(String, String, KeyPosition)>,
) {
    for entry in array_entries(array, source) {
        let dotted = if prefix.is_empty() {
            entry.key_text.clone()
        } else {
            format!("{prefix}.{}", entry.key_text)
        };
        let nested = find_array_in_expression(entry.value_node);
        let value = match nested {
            Some(_) => String::new(),
            None => scalar_value_text(entry.value_node, source),
        };
        out.push((dotted.clone(), value, entry.key_position));
        if let Some(nested) = nested {
            collect_keys(nested, source, &dotted, out);
        }
    }
}

/// Walk the AST looking for the top-level `return <array>;` statement and
/// return the array literal node. We scan the file root's children so the
/// usual `<?php` opener, `use` statements, etc. don't confuse us.
fn find_return_array(root: Node) -> Option<Node> {
    // tree-sitter-php wraps PHP at the file root in `program > php_tag …
    // statements`. Walk every descendant looking for a `return_statement`
    // whose expression is `array_creation_expression`. This handles both
    // top-level and namespaced files uniformly.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "return_statement" {
            // The expression is typically a direct child, but tree-sitter
            // can wrap it in `expression`/`primary_expression` indirections.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(arr) = find_array_in_expression(child) {
                    return Some(arr);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Recurse through `expression` / `primary_expression` wrappers to reach
/// the underlying `array_creation_expression`, if present.
fn find_array_in_expression(node: Node) -> Option<Node> {
    if node.kind() == "array_creation_expression" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(arr) = find_array_in_expression(child) {
            return Some(arr);
        }
    }
    None
}

/// Walk an `array_creation_expression` along the given path. At each step,
/// find the entry whose key matches the path segment. If it's the last
/// segment, return the key position; otherwise descend into the value (which
/// must itself be an array) and recurse.
fn locate_at_path<'a>(array: Node<'a>, source: &[u8], path: &[&str]) -> Option<KeyPosition> {
    let (head, tail) = path.split_first()?;
    for entry in array_entries(array, source) {
        if entry.key_text != *head {
            // Try the next entry; we only act on a key match. An entry we
            // cannot parse is skipped by `array_entries`, never fatal — it
            // used to abort the whole lookup, so a single list entry hid
            // every key declared after it.
            continue;
        }
        if tail.is_empty() {
            return Some(entry.key_position);
        }
        // Descend into the value, which must be another array.
        if let Some(nested) = find_array_in_expression(entry.value_node) {
            return locate_at_path(nested, source, tail);
        }
        // Path expects more nesting but the value isn't an array.
        return None;
    }
    None
}

struct ParsedEntry<'a> {
    key_text: String,
    key_position: KeyPosition,
    value_node: Node<'a>,
}

/// Every resolvable entry of an array literal, in document order, with PHP's
/// own index rules applied to keyless entries.
///
/// PHP gives a keyless entry the next free integer index: the counter starts
/// at 0 and jumps to `n + 1` whenever an explicit integer key `n` appears.
/// `['a', 5 => 'b', 'c']` is therefore `0`, `5`, `6`. A spread (`...$other`)
/// contributes an unknown number of elements, so every later position is
/// unknowable — positional entries after one are dropped rather than guessed.
fn array_entries<'a>(array: Node<'a>, source: &[u8]) -> Vec<ParsedEntry<'a>> {
    let mut out = Vec::new();
    let mut next_index: Option<i64> = Some(0);
    let mut cursor = array.walk();
    for child in array.children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        if is_spread(child) {
            next_index = None;
            continue;
        }
        let Some((key_node, value_node)) = entry_key_and_value(child) else {
            continue;
        };
        match key_node {
            Some(key_node) => {
                if let Some(n) = integer_literal_value(key_node, source) {
                    next_index = Some(next_index.map_or(n + 1, |c| c.max(n + 1)));
                }
                if let Some((key_text, key_position)) = literal_key(key_node, source) {
                    out.push(ParsedEntry {
                        key_text,
                        key_position,
                        value_node,
                    });
                }
            }
            None => {
                let Some(index) = next_index else { continue };
                next_index = Some(index + 1);
                out.push(ParsedEntry {
                    key_text: index.to_string(),
                    key_position: synthesized_index_position(value_node),
                    value_node,
                });
            }
        }
    }
    out
}

/// Split one `array_element_initializer` into `(key, value)`. The key is
/// `None` for a keyless (list) entry. Handles both the field-named grammar
/// revision and the older positional one.
fn entry_key_and_value<'a>(node: Node<'a>) -> Option<(Option<Node<'a>>, Node<'a>)> {
    if let Some(value) = node.child_by_field_name("value") {
        return Some((node.child_by_field_name("key"), value));
    }
    let mut cursor = node.walk();
    let named: Vec<Node<'a>> = node.named_children(&mut cursor).collect();
    match named.len() {
        0 => None,
        1 => Some((None, named[0])),
        _ => Some((Some(named[0]), named[1])),
    }
}

/// True for a `...$other` spread element, whose element count is unknown.
fn is_spread(node: Node) -> bool {
    if node.kind().contains("variadic") {
        return true;
    }
    let mut cursor = node.walk();
    let spread = node
        .children(&mut cursor)
        .any(|child| child.kind() == "..." || child.kind().contains("variadic"));
    spread
}

/// The integer a literal integer key denotes, e.g. `404 => …`.
fn integer_literal_value(node: Node, source: &[u8]) -> Option<i64> {
    (node.kind() == "integer")
        .then(|| node.utf8_text(source).ok()?.parse().ok())
        .flatten()
}

/// The dotted-path text a literal key contributes, and where it is declared.
///
/// A quoted key reports `is_literal_key: true` — rename can rewrite it inside
/// its quotes. An unquoted integer key (`404 => …`) reports `false`: it is a
/// real, resolvable key, but replacing the bare digits with a name would
/// produce a constant lookup rather than a string key.
fn literal_key(node: Node, source: &[u8]) -> Option<(String, KeyPosition)> {
    if let Some(text) = string_literal_text(node, source) {
        return Some((text, string_body_position(node)));
    }
    let value = integer_literal_value(node, source)?;
    let start = node.start_position();
    let end = node.end_position();
    Some((
        value.to_string(),
        KeyPosition {
            line: start.row as u32,
            start_column: start.column as u32,
            end_column: if end.row == start.row {
                end.column as u32
            } else {
                start.column as u32
            },
            kind: KeyKind::BareInteger,
        },
    ))
}

/// Where a synthesized list index points: the start of the entry's value.
/// Zero-width, because the index has no text of its own in the source.
fn synthesized_index_position(value_node: Node) -> KeyPosition {
    let start = value_node.start_position();
    KeyPosition {
        line: start.row as u32,
        start_column: start.column as u32,
        end_column: start.column as u32,
        kind: KeyKind::SynthesizedIndex,
    }
}

/// The span of a string literal's body — inside the quotes.
///
/// Derived from the literal's own extent rather than from its `string_content`
/// child, because an escape splits that child in two: `'it\'s'` is
/// `string_content("it")` + `escape_sequence` + `string_content("s")`, and the
/// first run alone spans only `it`.
fn string_body_position(node: Node) -> KeyPosition {
    let start = node.start_position();
    let end = node.end_position();
    let start_column = start.column as u32 + 1;
    KeyPosition {
        line: start.row as u32,
        start_column,
        end_column: if end.row == start.row && end.column as u32 > start_column {
            end.column as u32 - 1
        } else {
            start_column
        },
        kind: KeyKind::Quoted,
    }
}

/// A string literal's text with PHP's escape sequences resolved. `None` for
/// any node that is not a single- or double-quoted literal.
fn string_literal_text(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" && node.kind() != "encapsed_string" {
        return None;
    }
    let raw = node.utf8_text(source).ok()?;
    let quote = raw.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = raw
        .strip_prefix(quote)
        .and_then(|rest| rest.strip_suffix(quote))
        .unwrap_or(&raw[quote.len_utf8()..]);
    Some(unescape_php(body, quote))
}

/// Resolve PHP's escape sequences for the given quote style.
///
/// A single-quoted literal resolves only `\'` and `\\`; every other backslash
/// stands for itself, so `'a\nb'` really does contain a backslash and an `n`.
/// A double-quoted literal resolves the usual control escapes too. Variable
/// interpolation is not attempted — see [`fold_value`].
fn unescape_php(body: &str, quote: char) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            None => out.push('\\'),
            Some(next) if quote == '\'' => {
                if next == '\'' || next == '\\' {
                    out.push(next);
                } else {
                    out.push('\\');
                    out.push(next);
                }
            }
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('v') => out.push('\u{0B}'),
            Some('f') => out.push('\u{0C}'),
            Some('e') => out.push('\u{1B}'),
            Some('0') => out.push('\0'),
            Some(next @ ('\\' | '"' | '$')) => out.push(next),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// The display text of a non-array value. Statically knowable values are
/// resolved to what PHP would produce; anything else falls back to its source
/// text, which at least shows the reader where the value comes from.
fn scalar_value_text(node: Node, source: &[u8]) -> String {
    fold_value(node, source)
        .unwrap_or_else(|| node.utf8_text(source).unwrap_or_default().to_string())
}

/// The runtime text of a value expression when it can be known without
/// running PHP: a string literal, a heredoc or nowdoc body, or a `.`
/// concatenation whose every operand is one of those.
///
/// `None` for anything needing runtime state — `env(…)`, a constant, an
/// interpolated variable — so the caller shows the source text instead of a
/// confidently wrong answer.
fn fold_value(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "string" => string_literal_text(node, source),
        "encapsed_string" => {
            // `"Hi $name"` has no static value. Only a literal run (plus its
            // escapes) can be folded.
            let mut cursor = node.walk();
            let interpolated = node.children(&mut cursor).any(|child| {
                !matches!(
                    child.kind(),
                    "string_content" | "escape_sequence" | "\"" | "'"
                )
            });
            (!interpolated).then(|| string_literal_text(node, source))?
        }
        "heredoc" | "nowdoc" => {
            let mut cursor = node.walk();
            let body = node
                .children(&mut cursor)
                .find(|child| child.kind().ends_with("_body"))?;
            // The body node starts at the newline that ends the `<<<EOT`
            // opener; that newline is a delimiter, not content.
            let text = body.utf8_text(source).ok()?;
            let text = text
                .strip_prefix("\r\n")
                .or_else(|| text.strip_prefix('\n'))
                .unwrap_or(text);
            // A nowdoc (`<<<'EOT'`) is single-quote semantics; a heredoc is
            // double-quote semantics.
            Some(match node.kind() {
                "nowdoc" => text.to_string(),
                _ => unescape_php(text, '"'),
            })
        }
        "binary_expression" => {
            let operator = node.child_by_field_name("operator")?;
            if operator.utf8_text(source).ok()? != "." {
                return None;
            }
            let left = fold_value(node.child_by_field_name("left")?, source)?;
            let right = fold_value(node.child_by_field_name("right")?, source)?;
            Some(left + &right)
        }
        "expression" | "primary_expression" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            let folded = node
                .named_children(&mut cursor)
                .find_map(|child| fold_value(child, source));
            folded
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
