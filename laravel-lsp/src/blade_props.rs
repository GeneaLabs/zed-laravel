//! Extract `@props([...])` declarations from Blade view source.
//!
//! Hover uses this to surface the variables a view expects. The extractor
//! is paren-balanced and string-aware so multi-line `@props([...])` blocks
//! with embedded parentheses inside string keys/defaults are captured
//! intact — for example:
//!
//! ```text
//! @props([
//!     'note' => 'something (with parens)',
//!     'count' => 0,
//! ])
//! ```
//!
//! …returns the full multi-line declaration without truncation at the first
//! `)` it sees.

use std::path::Path;

/// Read a Blade file and extract its first `@props([...])` declaration.
/// Returns `None` when the file doesn't exist, can't be read, or has no
/// `@props(...)` directive.
///
/// The returned string starts with `@props(` and ends with the matching
/// `)` — multi-line declarations are preserved verbatim so the hover code
/// block reads the same as the source.
pub fn extract_props_directive(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_props_directive_from_source(&content)
}

/// Source-only variant used for unit tests — operates on an in-memory
/// string rather than reading from disk.
pub fn extract_props_directive_from_source(content: &str) -> Option<String> {
    let bytes = content.as_bytes();
    let needle = b"@props";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] != needle {
            i += 1;
            continue;
        }
        // Word boundary after `@props` — reject `@propsExtended` or similar.
        let after = i + needle.len();
        let mut j = after;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i = after;
            continue;
        }
        // Paren-balance to find the matching `)`, tracking string literals
        // so embedded `(` / `)` characters don't throw off the count.
        let mut depth = 1i32;
        let mut k = j + 1;
        let mut in_string: Option<u8> = None;
        while k < bytes.len() {
            let b = bytes[k];
            if let Some(q) = in_string {
                if b == b'\\' && k + 1 < bytes.len() {
                    k += 2;
                    continue;
                }
                if b == q {
                    in_string = None;
                }
                k += 1;
                continue;
            }
            match b {
                b'\'' | b'"' => in_string = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(content[i..=k].to_string());
                    }
                }
                _ => {}
            }
            k += 1;
        }
        return None;
    }
    None
}

/// Parse the prop *names* declared by a `@props([...])` directive in Blade
/// source, in declaration order. Both shorthand entries (`'foo'`) and keyed
/// entries (`'foo' => default`) yield the bare name `foo` (no leading `$`).
///
/// Flux/Blade component attribute completion uses this to offer a resolved
/// component's declared props as attributes. Returns an empty `Vec` when the
/// source has no `@props(...)` directive.
///
/// Robust to nested arrays in defaults — `'opts' => ['a', 'b']` yields just
/// `opts`, because only the *first* string literal of each top-level entry
/// (the key, or the bare name) is taken.
pub fn extract_prop_names(content: &str) -> Vec<String> {
    let directive = match extract_props_directive_from_source(content) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let bytes = directive.as_bytes();
    // Narrow to the array body between the outermost `[` and its match `]`.
    let open = match directive.find('[') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let body = match matching_bracket(bytes, open) {
        Some(close) => &directive[open + 1..close],
        None => return Vec::new(),
    };

    split_top_level_commas(body)
        .into_iter()
        .filter_map(first_string_literal)
        .filter(|name| is_php_identifier(name))
        .collect()
}

/// Index of the `]` matching the `[` at `open`, tracking string literals so
/// brackets inside strings don't throw off the balance. `None` if unbalanced.
fn matching_bracket(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split an array body on top-level commas, ignoring commas nested inside
/// `[]` / `()` or inside string literals.
fn split_top_level_commas(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&body[start..]);
    parts
}

/// Content of the first single- or double-quoted string literal in `entry`,
/// or `None` when there isn't one. Used to pull the prop name (the key, or the
/// bare value) from a `@props` array entry.
fn first_string_literal(entry: &str) -> Option<String> {
    let bytes = entry.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            let quote = b;
            let mut j = i + 1;
            let mut s = String::new();
            while j < bytes.len() {
                let c = bytes[j];
                if c == b'\\' && j + 1 < bytes.len() {
                    s.push(bytes[j + 1] as char);
                    j += 2;
                    continue;
                }
                if c == quote {
                    return Some(s);
                }
                s.push(c as char);
                j += 1;
            }
            return None; // unterminated literal
        }
        i += 1;
    }
    None
}

/// Whether `s` is a valid PHP identifier (the shape a prop name must have to
/// be offered as an attribute). Rejects array spreads, expressions, etc.
fn is_php_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests;
