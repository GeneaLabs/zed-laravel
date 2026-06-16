//! Alpine.js Tier-A support: static catalogs plus line-local context detection
//! for directive/magic completion, hover cards, and `@event`-vs-Blade
//! disambiguation.
//!
//! Alpine lives in HTML *attributes* inside Blade markup — directive names like
//! `x-data` / `x-on:click`, the `@`/`:` shorthands, and `$`-prefixed magics
//! inside the JS expressions in attribute values. None of that is PHP or Blade,
//! so the static-analysis the rest of this LSP does doesn't reach it. This
//! module supplies exactly the Tier-A surface: a curated catalog of the core
//! directives and magics (no JS scanning, no goto), the context predicates the
//! completion/hover handlers need, and the predicate the Blade directive
//! highlighter uses to keep `@click="…"` from being mistaken for a Blade
//! `@directive`.
//!
//! Everything here is pure and line-local so it can be unit-tested directly,
//! mirroring `component_completion` — the handlers in `main.rs` only render the
//! results into LSP types.

use crate::hover::{render, source_link, HoverContent};

/// One catalog entry — a directive (`x-data`) or magic (`$store`) with a
/// one-line synopsis and its canonical docs anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlpineEntry {
    /// The token as written in markup, e.g. `x-data` or `$store`.
    pub name: &'static str,
    /// One-line, hover-card synopsis.
    pub summary: &'static str,
    /// Canonical alpinejs.dev documentation URL.
    pub url: &'static str,
}

/// The core Alpine directives (AC Tier A). Names are the bare directive without
/// any `:argument` (`x-on`, not `x-on:click`) — argument forms resolve to their
/// base for lookup.
pub static ALPINE_DIRECTIVES: &[AlpineEntry] = &[
    AlpineEntry {
        name: "x-data",
        summary: "Declare a new Alpine component and its reactive data.",
        url: "https://alpinejs.dev/directives/data",
    },
    AlpineEntry {
        name: "x-bind",
        summary: "Bind an attribute (or class/style) to a JS expression. Shorthand `:`.",
        url: "https://alpinejs.dev/directives/bind",
    },
    AlpineEntry {
        name: "x-on",
        summary: "Listen for a DOM event and run an expression. Shorthand `@`.",
        url: "https://alpinejs.dev/directives/on",
    },
    AlpineEntry {
        name: "x-model",
        summary: "Two-way bind an input's value to a data property.",
        url: "https://alpinejs.dev/directives/model",
    },
    AlpineEntry {
        name: "x-text",
        summary: "Set an element's text content to a JS expression.",
        url: "https://alpinejs.dev/directives/text",
    },
    AlpineEntry {
        name: "x-html",
        summary: "Set an element's innerHTML to a JS expression (trusted content only).",
        url: "https://alpinejs.dev/directives/html",
    },
    AlpineEntry {
        name: "x-show",
        summary: "Toggle an element's visibility via `display` based on an expression.",
        url: "https://alpinejs.dev/directives/show",
    },
    AlpineEntry {
        name: "x-if",
        summary: "Conditionally add or remove an element from the DOM (use on a `<template>`).",
        url: "https://alpinejs.dev/directives/if",
    },
    AlpineEntry {
        name: "x-for",
        summary: "Render a block for each item in an array (use on a `<template>`).",
        url: "https://alpinejs.dev/directives/for",
    },
    AlpineEntry {
        name: "x-ref",
        summary: "Mark an element so it can be reached via the `$refs` magic.",
        url: "https://alpinejs.dev/directives/ref",
    },
    AlpineEntry {
        name: "x-init",
        summary: "Run an expression when the element initializes.",
        url: "https://alpinejs.dev/directives/init",
    },
    AlpineEntry {
        name: "x-effect",
        summary: "Re-run an expression whenever one of its reactive dependencies changes.",
        url: "https://alpinejs.dev/directives/effect",
    },
    AlpineEntry {
        name: "x-transition",
        summary: "Apply transition classes when an element is shown or hidden.",
        url: "https://alpinejs.dev/directives/transition",
    },
    AlpineEntry {
        name: "x-cloak",
        summary: "Hide an element until Alpine has finished initializing it.",
        url: "https://alpinejs.dev/directives/cloak",
    },
    AlpineEntry {
        name: "x-teleport",
        summary: "Render a `<template>`'s content at another place in the DOM.",
        url: "https://alpinejs.dev/directives/teleport",
    },
    AlpineEntry {
        name: "x-ignore",
        summary: "Tell Alpine to skip initializing an element and its children.",
        url: "https://alpinejs.dev/directives/ignore",
    },
];

/// The core Alpine magics (AC Tier A). Names include the leading `$`.
pub static ALPINE_MAGICS: &[AlpineEntry] = &[
    AlpineEntry {
        name: "$el",
        summary: "The current DOM element.",
        url: "https://alpinejs.dev/magics/el",
    },
    AlpineEntry {
        name: "$refs",
        summary: "Access elements marked with `x-ref` within the component.",
        url: "https://alpinejs.dev/magics/refs",
    },
    AlpineEntry {
        name: "$store",
        summary: "Access a global Alpine store registered with `Alpine.store()`.",
        url: "https://alpinejs.dev/magics/store",
    },
    AlpineEntry {
        name: "$watch",
        summary: "Watch a component property and run a callback when it changes.",
        url: "https://alpinejs.dev/magics/watch",
    },
    AlpineEntry {
        name: "$dispatch",
        summary: "Dispatch a custom browser event from the current element.",
        url: "https://alpinejs.dev/magics/dispatch",
    },
    AlpineEntry {
        name: "$nextTick",
        summary: "Run a callback after Alpine has next updated the DOM.",
        url: "https://alpinejs.dev/magics/nexttick",
    },
    AlpineEntry {
        name: "$root",
        summary: "The root element of the current component (nearest `x-data`).",
        url: "https://alpinejs.dev/magics/root",
    },
    AlpineEntry {
        name: "$data",
        summary: "The current component's reactive data scope.",
        url: "https://alpinejs.dev/magics/data",
    },
    AlpineEntry {
        name: "$id",
        summary: "Generate a component-scoped unique id (pairs with `x-id`).",
        url: "https://alpinejs.dev/magics/id",
    },
    AlpineEntry {
        name: "$persist",
        summary: "Persist a property to localStorage across page loads.",
        url: "https://alpinejs.dev/magics/persist",
    },
];

/// Common DOM event names Alpine binds via `@event` / `x-on:event`. Used to tell
/// an Alpine event binding (`@click`) apart from a Blade directive (`@class`):
/// the sets are disjoint, so a name in here is never a Blade directive candidate.
/// Tier A covers the common core; arbitrary custom events are a follow-up.
pub static ALPINE_EVENTS: &[&str] = &[
    "click",
    "dblclick",
    "mousedown",
    "mouseup",
    "mouseenter",
    "mouseleave",
    "mouseover",
    "mouseout",
    "mousemove",
    "contextmenu",
    "wheel",
    "submit",
    "change",
    "input",
    "focus",
    "blur",
    "focusin",
    "focusout",
    "keydown",
    "keyup",
    "keypress",
    "scroll",
    "resize",
    "load",
    "error",
    "select",
    "reset",
    "drag",
    "dragstart",
    "dragend",
    "dragover",
    "drop",
    "touchstart",
    "touchend",
    "touchmove",
    "paste",
    "copy",
    "cut",
];

/// Look up a directive by its bare name (`x-data`, `x-on`). An argument form
/// like `x-on:click` must be reduced to its base (`x-on`) by the caller.
pub fn directive(name: &str) -> Option<&'static AlpineEntry> {
    ALPINE_DIRECTIVES.iter().find(|e| e.name == name)
}

/// Look up a magic by its full name including the `$` (`$store`).
pub fn magic(name: &str) -> Option<&'static AlpineEntry> {
    ALPINE_MAGICS.iter().find(|e| e.name == name)
}

/// Whether `name` (a `@`-binding's event name, modifiers allowed) is a known
/// Alpine DOM event. `click`, `submit.prevent`, `keyup.enter` → true; the base
/// before the first `.` is matched, so modifiers don't defeat recognition.
pub fn is_alpine_event(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    !base.is_empty() && ALPINE_EVENTS.contains(&base)
}

/// Directives whose name starts with `prefix` (case-insensitive), for offering
/// completion candidates as the user types an `x-…` attribute.
pub fn matching_directives(prefix: &str) -> Vec<&'static AlpineEntry> {
    let p = prefix.to_lowercase();
    ALPINE_DIRECTIVES
        .iter()
        .filter(|e| e.name.starts_with(&p))
        .collect()
}

/// Magics whose name (compared *without* the leading `$`) starts with `prefix`.
/// The caller passes the text already typed after the `$`, mirroring how Blade
/// variable completion filters after the `$`.
pub fn matching_magics(prefix: &str) -> Vec<&'static AlpineEntry> {
    let p = prefix.to_lowercase();
    ALPINE_MAGICS
        .iter()
        .filter(|e| e.name[1..].to_lowercase().starts_with(&p))
        .collect()
}

/// Alpine event names starting with `prefix` (the text after the `@`,
/// case-insensitive), for merging into `@`-directive completion.
pub fn matching_events(prefix: &str) -> Vec<&'static str> {
    let p = prefix.to_lowercase();
    ALPINE_EVENTS
        .iter()
        .filter(|e| e.starts_with(&p))
        .copied()
        .collect()
}

/// Alpine events matching `prefix` that are safe to *add* to a Blade
/// `@`-directive completion list, given the Blade directive names already
/// offered (`blade_names`, matched case-insensitively).
///
/// A name can be **both** a Blade directive and an Alpine event — `error` is
/// the live example: Laravel's compiler exposes `@error … @enderror`, and
/// `error` is also a DOM event. Without deduping, `@err` would surface two
/// `@error` entries (issue #61, AC5). The Blade directive wins — it reflects a
/// directive that is actually registered in the project — so its event twin is
/// dropped here, leaving exactly one `@error` in the merged list.
pub fn mergeable_events(prefix: &str, blade_names: &[&str]) -> Vec<&'static str> {
    let taken: std::collections::HashSet<String> =
        blade_names.iter().map(|n| n.to_lowercase()).collect();
    // `ALPINE_EVENTS` are already lowercase, so comparing against the lowercased
    // Blade names is a straight set membership test.
    matching_events(prefix)
        .into_iter()
        .filter(|ev| !taken.contains(*ev))
        .collect()
}

/// Render a hover card for a directive by bare name, or `None` if unknown.
pub fn directive_card(name: &str) -> Option<String> {
    entry_card(directive(name)?)
}

/// Render a hover card for a magic by full `$`-name, or `None` if unknown.
pub fn magic_card(name: &str) -> Option<String> {
    entry_card(magic(name)?)
}

fn entry_card(e: &AlpineEntry) -> Option<String> {
    let link = source_link("Alpine.js documentation", e.url, None);
    Some(render(&HoverContent {
        header: Some(e.name),
        detail: Some(e.summary),
        source_link: Some(&link),
        ..Default::default()
    }))
}

/// The replacement target + filter prefix for an Alpine completion, mirroring
/// the `StringContext` shape used by the other completion-context helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlpineContext {
    /// Text already typed (the filter prefix).
    pub prefix: String,
    /// 0-based column where the replacement starts.
    pub start_col: u32,
    /// 0-based column where the replacement ends (the cursor).
    pub end_col: u32,
}

/// Characters that can appear in an Alpine attribute name (`x-on:click.prevent`,
/// `@keyup.enter`, `:class`).
fn is_attr_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '@' || c == '.'
}

/// Whether `byte_pos` sits inside an open HTML/Blade tag at an attribute
/// position — there is an unclosed `<` before it and the cursor isn't inside a
/// quoted attribute value. This is what tells an Alpine `@click` attribute apart
/// from a Blade `@directive` written in text or a PHP block.
pub fn at_attribute_position(line: &str, byte_pos: usize) -> bool {
    if byte_pos > line.len() {
        return false;
    }
    let before = &line[..byte_pos];
    match before.rfind('<') {
        Some(lt) if before.rfind('>').is_none_or(|gt| gt < lt) => {
            let region = &before[lt..];
            region.matches('"').count().is_multiple_of(2)
                && region.matches('\'').count().is_multiple_of(2)
        }
        _ => false,
    }
}

/// The maximal run of `is_attr_name_char` ending at `cursor`, plus the byte
/// index where it starts. Empty run → `(cursor, "")`.
fn attr_word_before(before: &str, cursor: usize) -> (usize, &str) {
    let mut start = cursor;
    for (i, c) in before.char_indices().rev() {
        if is_attr_name_char(c) {
            start = i;
        } else {
            break;
        }
    }
    (start, &before[start..])
}

/// Detect the cursor typing an Alpine `x-…` directive name at an attribute
/// position inside an open tag (`<div x-da│`, `<x-card x-│>`). Returns the
/// partial word and replacement range, or `None` when not an `x-` directive
/// attribute position.
pub fn directive_completion_context(line: &str, cursor_col: u32) -> Option<AlpineContext> {
    let cursor = cursor_col as usize;
    if cursor == 0 || cursor > line.len() {
        return None;
    }
    let before = &line[..cursor];
    let (word_start, word) = attr_word_before(before, cursor);

    // The attribute must be a separate token (preceded by whitespace) inside an
    // open tag — otherwise it's part of the tag name or free text.
    if !before[..word_start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_whitespace())
    {
        return None;
    }
    if !at_attribute_position(line, word_start) {
        return None;
    }
    // Only the `x`/`x-…` family completes to a directive list. `@`/`:` shorthands
    // bind arbitrary attributes — recognized elsewhere, nothing to enumerate.
    if !word.starts_with('x') {
        return None;
    }

    Some(AlpineContext {
        prefix: word.to_string(),
        start_col: word_start as u32,
        end_col: cursor as u32,
    })
}

/// Byte index of the quote that is *currently open* at the end of `s`, or `None`
/// when `s` ends outside any string.
fn open_quote(s: &str) -> Option<usize> {
    let mut q: Option<(char, usize)> = None;
    for (i, c) in s.char_indices() {
        match q {
            None if c == '"' || c == '\'' => q = Some((c, i)),
            Some((qc, _)) if c == qc => q = None,
            _ => {}
        }
    }
    q.map(|(_, i)| i)
}

/// Whether the byte offset `pos` sits inside the quoted value of an Alpine
/// attribute (`x-…`, `@…`, or `:…`) — i.e. inside an Alpine JS expression, where
/// `$`-magics are valid. `before` is the line text up to `pos`.
fn in_alpine_expression(before: &str) -> bool {
    let Some(oq) = open_quote(before) else {
        return false;
    };
    let head = before[..oq].trim_end();
    let Some(name_region) = head.strip_suffix('=') else {
        return false;
    };
    let (_, attr) = attr_word_before(name_region, name_region.len());
    attr.starts_with("x-") || attr.starts_with('@') || attr.starts_with(':')
}

/// Detect the cursor typing a `$`-magic inside an Alpine attribute expression
/// (`@click="$dis│"`, `x-data="{ x: $st│ }"`). Returns the partial typed *after*
/// the `$` and the replacement range. Returns `None` outside an Alpine
/// expression, so ordinary Blade/PHP `$variables` are never hijacked.
pub fn magic_completion_context(line: &str, cursor_col: u32) -> Option<AlpineContext> {
    let cursor = cursor_col as usize;
    if cursor == 0 || cursor > line.len() {
        return None;
    }
    let before = &line[..cursor];

    let mut dollar = None;
    for (i, c) in before.char_indices().rev() {
        if c == '$' {
            dollar = Some(i);
            break;
        }
        if !(c.is_alphanumeric() || c == '_') {
            break;
        }
    }
    let dollar = dollar?;
    let after = &before[dollar + 1..];
    if !after.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if !in_alpine_expression(&before[..dollar]) {
        return None;
    }

    Some(AlpineContext {
        prefix: after.to_string(),
        start_col: (dollar + 1) as u32,
        end_col: cursor as u32,
    })
}

/// The maximal token containing `cursor`, where membership is decided by `pred`.
/// Returns `(start_byte, token)`.
fn token_at(line: &str, cursor: usize, pred: impl Fn(char) -> bool) -> (usize, &str) {
    let mut start = cursor;
    for (i, c) in line[..cursor].char_indices().rev() {
        if pred(c) {
            start = i;
        } else {
            break;
        }
    }
    let mut end = cursor;
    for (i, c) in line[cursor..].char_indices() {
        if pred(c) {
            end = cursor + i + c.len_utf8();
        } else {
            break;
        }
    }
    (start, &line[start..end])
}

/// Build the hover card for whatever Alpine token the cursor is on, or `None`.
/// Recognizes `x-…` directive names (including `x-on:click` → `x-on` and the
/// `@event` / `:bind` shorthands) and `$`-magics inside Alpine expressions.
pub fn hover_at(line: &str, character: u32) -> Option<String> {
    let cursor = character as usize;
    if cursor > line.len() {
        return None;
    }
    directive_hover_at(line, cursor).or_else(|| magic_hover_at(line, cursor))
}

fn directive_hover_at(line: &str, cursor: usize) -> Option<String> {
    let (start, token) = token_at(line, cursor, is_attr_name_char);
    if token.is_empty() || !at_attribute_position(line, start) {
        return None;
    }
    // Reduce an argument form to its base directive: `x-on:click` → `x-on`.
    let base = token.split(':').next().unwrap_or(token);

    if base.starts_with("x-") {
        return directive_card(base);
    }
    // `@event` is the `x-on` shorthand; `:attr` is the `x-bind` shorthand.
    if let Some(event) = base.strip_prefix('@') {
        if is_alpine_event(event) {
            return directive_card("x-on");
        }
    }
    if token.starts_with(':') {
        return directive_card("x-bind");
    }
    None
}

fn magic_hover_at(line: &str, cursor: usize) -> Option<String> {
    let (start, token) = token_at(line, cursor, |c| {
        c == '$' || c.is_alphanumeric() || c == '_'
    });
    if !token.starts_with('$') {
        return None;
    }
    magic(token)?;
    if !in_alpine_expression(&line[..start]) {
        return None;
    }
    magic_card(token)
}

#[cfg(test)]
mod tests;
