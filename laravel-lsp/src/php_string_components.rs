//! Blade component tags written inside PHP **string literals**.
//!
//! The Blade tag extractor in [`crate::queries`] only sees tags that
//! tree-sitter-blade parsed out of a `.blade.php` file. Real Laravel code also
//! *builds* Blade markup as PHP strings and renders it later — a job that
//! rewrites verse text into `"<x-reader.cross-reference :id=\"{$id}\" />"`, a
//! mail builder that assembles `'<x-mail::button>'`, a test asserting on
//! rendered output. Those are genuine references to the component, but nothing
//! in the pipeline saw them, so the component indexed as zero-reference and the
//! unused-symbol diagnostic (#59) called a live component "possibly dead".
//!
//! This module closes that gap: given the full-file PHP parse a `.php` file
//! already has, it walks the literal-text nodes and returns the component /
//! Livewire tags inside them, positioned so they merge into the same
//! `components` / `livewire_refs` buckets the Blade path fills. Everything
//! downstream — the symbol index, code lenses, find-references, the
//! unused-symbol warning — then works without further changes.
//!
//! Two precision guards keep this from inventing references:
//!
//! 1. **Literal nodes only** — the scan visits `string_content` (single-quoted,
//!    double-quoted, and heredoc bodies) and `nowdoc_string`. A `<x-…>` in a
//!    docblock, a comment, or raw inline HTML after `?>` is not a literal and
//!    is never matched.
//! 2. **In-fragment terminator required** — the tag name must be followed,
//!    *within the same literal fragment*, by whitespace, `/`, or `>`. An
//!    interpolated name (`"<x-alert-{$type} />"`) splits into a fragment ending
//!    at `<x-alert-` with no terminator, so it is skipped rather than indexed
//!    as a phantom `alert-` component. This mirrors the Blade extractor's
//!    `name_is_runtime_constructed` guard, which likewise refuses to name a
//!    component it cannot resolve statically.
//!
//! Positions are 0-based and columns are byte offsets from the line start —
//! the same convention `ComponentReferenceData` carries from the Blade path.

use lazy_static::lazy_static;
use regex::Regex;
use tree_sitter::{Point, Tree};

lazy_static! {
    /// An opening component tag inside literal text. Capture 1 is the full tag
    /// name; the trailing `[\s/>]` is the in-fragment terminator guard (never
    /// part of the capture). Character classes match the ones
    /// `document_symbols` already uses for the same tag families.
    static ref STRING_TAG_RE: Regex = Regex::new(
        r"<(x-[a-z][a-z0-9._:-]*|livewire:[a-z][a-z0-9._-]*|flux:[a-z][a-z0-9._-]*)[\s/>]",
    )
    .unwrap();
}

/// Which reference bucket a scanned tag belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringTagKind {
    /// `<x-…>` and `<flux:…>` — both land in the component bucket, matching how
    /// `extract_all_blade_patterns` files them.
    Component,
    /// `<livewire:…>`.
    Livewire,
}

/// One component tag found inside a PHP string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringTagMatch {
    pub kind: StringTagKind,
    /// The indexed name, stripped exactly as the Blade extractor strips it:
    /// `x-` removed for components, `livewire:` removed for Livewire, and
    /// `flux:` kept whole (`flux:icon`) because that is the name Flux
    /// components index under.
    pub name: String,
    /// The full tag text (`x-reader.cross-reference`), for
    /// `ComponentReferenceData::tag_name`.
    pub tag_name: String,
    /// 0-based line of the tag name's first character.
    pub line: u32,
    /// 0-based byte column of the tag name's first character. Points at the
    /// name, not the `<`, matching the Blade path's `tag_name` node span.
    pub column: u32,
    /// 0-based byte column one past the tag name's last character.
    pub end_column: u32,
}

/// Scan `tree`'s string literals for component / Livewire tags.
///
/// `tree` must be a PHP parse of `source`. Callers pass the full-file parse
/// they already hold, so this costs a tree walk and no extra parse. Returns
/// matches in source order.
pub fn scan_php_string_tags(tree: &Tree, source: &str) -> Vec<StringTagMatch> {
    // Cheap bail before touching the tree: the overwhelming majority of PHP
    // files contain no component tag anywhere, and warm indexing runs this over
    // every file in the project.
    if !source.contains("<x-") && !source.contains("<livewire:") && !source.contains("<flux:") {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "string_content" | "nowdoc_string") {
            if let Ok(text) = node.utf8_text(source.as_bytes()) {
                collect_from_literal(node.start_position(), text, &mut out);
            }
            // Literal text has no component tags nested as child nodes.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    // The stack walk visits siblings in reverse, so sort back into source
    // order — the index and the position lookup both assume it.
    out.sort_by_key(|m| (m.line, m.column));
    out
}

/// Append every tag in one literal fragment. `start` is the fragment's position
/// in the original source; `text` is the fragment's own text.
fn collect_from_literal(start: Point, text: &str, out: &mut Vec<StringTagMatch>) {
    for caps in STRING_TAG_RE.captures_iter(text) {
        let tag = caps.get(1).expect("capture group 1 is not optional");
        let tag_name = tag.as_str();

        // Slot tags are named-slot syntax, not component usage — the Blade
        // extractor skips them for the same reason (they'd otherwise produce
        // bogus "component not found" results).
        if tag_name == "x-slot" || tag_name.starts_with("x-slot:") {
            continue;
        }

        let (kind, name) = if let Some(rest) = tag_name.strip_prefix("x-") {
            (StringTagKind::Component, rest)
        } else if let Some(rest) = tag_name.strip_prefix("livewire:") {
            (StringTagKind::Livewire, rest)
        } else {
            // `flux:` — indexed under the full tag name.
            (StringTagKind::Component, tag_name)
        };

        let (line, column) = absolute_point(start, text, tag.start());
        let (_, end_column) = absolute_point(start, text, tag.end());
        out.push(StringTagMatch {
            kind,
            name: name.to_string(),
            tag_name: tag_name.to_string(),
            line,
            column,
            end_column,
        });
    }
}

/// Map a byte offset inside a literal fragment to its absolute 0-based
/// (line, byte-column) in the original source.
///
/// A fragment can span lines: tree-sitter-php keeps a multi-line quoted string
/// or nowdoc body as ONE `string_content` / `nowdoc_string` node with embedded
/// newlines (a heredoc body is the exception — it is split one node per
/// physical line). Only an offset on the fragment's first line inherits the
/// fragment's start column; past a newline the column is line-relative.
fn absolute_point(start: Point, text: &str, offset: usize) -> (u32, u32) {
    let before = &text[..offset];
    match before.rfind('\n') {
        None => (start.row as u32, (start.column + offset) as u32),
        Some(nl) => {
            let rows = before.matches('\n').count();
            ((start.row + rows) as u32, (offset - nl - 1) as u32)
        }
    }
}

#[cfg(test)]
mod tests;
