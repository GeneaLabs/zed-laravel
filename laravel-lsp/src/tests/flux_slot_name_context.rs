//! Flux slot-name completion-context detection and enclosing-component
//! resolution (issue #106 — follow-up from Holmes's review of #60 / PR #99).
//!
//! Three pieces feed `<flux:slot>` / `<x-slot:>` slot-name completion:
//! - `get_flux_slot_name_context` — is the cursor inside a slot *name* being
//!   typed (`<flux:slot name="│">` or `<x-slot:│>`), and what's the partial?
//! - `enclosing_flux_component` — which wrapping `<flux:…>` component owns that
//!   slot tag (walked over the full document's open-tag stack)?
//! - `extract_slot_variable_usages` — the owner's named slots, read from its
//!   backing Blade (already covered in `slot_variable_resolution`; re-exercised
//!   here through the full round-trip).
//!
//! `get_blade_component_context`'s slot exclusion is covered too: without it,
//! the `<x-slot:│>` colon form would be swallowed as a Blade *component* name
//! before reaching the slot-name block.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower_lsp::lsp_types::Position;

// ─── context detection: `<flux:slot name="…">` value form ────────────────

#[test]
fn flux_slot_context_at_empty_value() {
    let line = "    <flux:slot name=\"";
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32)
        .expect("cursor inside an empty `name=\"\"` value is a slot-name context");
    assert_eq!(ctx.prefix, "", "no partial typed yet → offer all slots");
    assert_eq!(
        ctx.start_col,
        line.len() as u32,
        "replacement starts right after the opening quote",
    );
}

#[test]
fn flux_slot_context_with_partial_value() {
    let line = "<flux:slot name=\"ti";
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32)
        .expect("a partial `name=\"ti` value is a slot-name context");
    assert_eq!(ctx.prefix, "ti");
    // `<flux:slot name="` is 17 bytes; content (the `t`) starts at col 17.
    assert_eq!(ctx.start_col, 17);
}

#[test]
fn flux_slot_context_accepts_single_quotes() {
    let line = "<flux:slot name='foo";
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32)
        .expect("single-quoted name value is detected too");
    assert_eq!(ctx.prefix, "foo");
    assert_eq!(ctx.quote_char, '\'');
}

// ─── context detection: `<x-slot:│>` colon form ──────────────────────────

#[test]
fn x_slot_colon_context_at_bare_prefix() {
    let line = "<x-slot:";
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32)
        .expect("`<x-slot:` is a slot-name context");
    assert_eq!(ctx.prefix, "");
    assert_eq!(
        ctx.start_col, 8,
        "replacement starts right after `<x-slot:`"
    );
}

#[test]
fn x_slot_colon_context_with_partial_name() {
    let line = "    <x-slot:ti";
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32)
        .expect("partial `<x-slot:ti` is a slot-name context");
    assert_eq!(ctx.prefix, "ti");
}

// ─── no-match cases ───────────────────────────────────────────────────────

#[test]
fn no_context_in_name_attribute_key() {
    // Cursor in the attribute *key*, before the `=` — not inside the value.
    let line = "<flux:slot name";
    assert!(
        LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none(),
        "the `name` key position is not a slot-name value context",
    );
    // Right after `=` but before any quote is still not inside the value.
    let line = "<flux:slot name=";
    assert!(
        LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none(),
        "between `=` and the opening quote is not inside the value",
    );
}

#[test]
fn no_context_in_tag_content() {
    // The tag has closed (`>` before the cursor) — the cursor is in content,
    // not in the `name` value.
    let line = "<flux:slot name=\"title\">";
    assert!(
        LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none(),
        "a closed slot tag puts the cursor in content, not the name value",
    );
    let line = "<flux:slot name=\"title\">{{ ";
    assert!(
        LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none(),
        "inside tag content there is no slot-name completion",
    );
}

#[test]
fn no_context_outside_any_slot_tag() {
    let line = "<flux:button variant=\"pri";
    assert!(
        LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none(),
        "a non-slot Flux tag is not a slot-name context",
    );
    let line = "    just some text";
    assert!(LaravelLanguageServer::get_flux_slot_name_context(line, line.len() as u32).is_none());
}

#[test]
fn blade_component_context_excludes_slot_syntax() {
    // The coupled fix: `<x-slot:` / `<x-slot` must NOT register as a Blade
    // component-name context, or the earlier component block would swallow the
    // colon form before the slot-name block runs.
    assert!(
        LaravelLanguageServer::get_blade_component_context("<x-slot:", 8).is_none(),
        "`<x-slot:` is slot syntax, not a component-name context",
    );
    assert!(
        LaravelLanguageServer::get_blade_component_context("<x-slot:ti", 10).is_none(),
        "`<x-slot:ti` is slot syntax, not a component-name context",
    );
    assert!(
        LaravelLanguageServer::get_blade_component_context("<x-slot", 7).is_none(),
        "bare `<x-slot` is slot syntax, not a component-name context",
    );
    // Regression guard: a real component whose name merely starts with "slot"
    // is still a component context.
    assert!(
        LaravelLanguageServer::get_blade_component_context("<x-slot-machine", 15).is_some(),
        "a component named `slot-machine` is unaffected by the exclusion",
    );
}

// ─── enclosing-Flux-component resolution ──────────────────────────────────

/// `(line, character)` of the caret marker `│` in `doc`, with the marker
/// stripped. The column is counted in Unicode code points, matching what an
/// LSP client sends — and what `position_to_byte_offset` (the converter
/// `enclosing_flux_component` now routes through) expects. On ASCII lines this
/// equals the byte offset, so existing ASCII fixtures are unaffected; on a line
/// with multibyte characters before the caret the two diverge, and the
/// code-point count is the correct one.
fn caret(doc: &str) -> (String, Position) {
    let idx = doc.find('│').expect("fixture must contain a `│` caret");
    let before = &doc[..idx];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let character = before[line_start..].chars().count() as u32;
    (doc.replace('│', ""), Position { line, character })
}

#[test]
fn enclosing_resolves_direct_flux_parent() {
    let (doc, pos) =
        caret("<flux:modal>\n    <flux:slot name=\"│\">\n    </flux:slot>\n</flux:modal>\n");
    assert_eq!(
        LaravelLanguageServer::enclosing_flux_component(&doc, pos).as_deref(),
        Some("flux:modal"),
    );
}

#[test]
fn enclosing_skips_self_closed_and_closed_siblings() {
    // A self-closing Flux tag and a fully-closed Flux sibling before the slot
    // must NOT be mistaken for the owner — only the still-open `<flux:card>` is.
    let (doc, pos) = caret(
        "<flux:icon name=\"x\" />\n\
         <flux:button>Go</flux:button>\n\
         <flux:card>\n    <flux:slot name=\"│\">\n</flux:card>\n",
    );
    assert_eq!(
        LaravelLanguageServer::enclosing_flux_component(&doc, pos).as_deref(),
        Some("flux:card"),
    );
}

#[test]
fn enclosing_returns_none_without_flux_ancestor() {
    // `<x-slot:title>` inside a non-Flux `<x-card>` — AC: no Flux parent → None,
    // so bare `<x-slot>` usage outside Flux yields no completions.
    let (doc, pos) = caret("<x-card>\n    <x-slot:│>\n</x-card>\n");
    assert!(
        LaravelLanguageServer::enclosing_flux_component(&doc, pos).is_none(),
        "no wrapping `<flux:…>` component → no owner",
    );
}

#[test]
fn enclosing_correct_with_multibyte_prefix() {
    // Regression for issue #206: the cursor line opens with multibyte chars
    // (six `🎉`, 4 bytes each) before a self-contained `<flux:badge></flux:badge>`
    // pair, then a `<flux:slot name="│">` holding the cursor. The owner is the
    // outer `<flux:modal>`, because the inner `<flux:badge>` is opened *and*
    // closed before the cursor.
    //
    // The old `character as usize` byte-offset shortcut computed a cursor offset
    // too small (it counted each `🎉` as one byte, not four), so `</flux:badge>`'s
    // `m.end()` landed *after* that bogus cursor and was skipped — leaving
    // `flux:badge` on the open-tag stack and wrongly returned as the owner.
    // Routing through `position_to_byte_offset` (code-point aware) places the
    // cursor correctly, so the close tag pops the child and `flux:modal` wins.
    let (doc, pos) = caret(
        "<flux:modal>\n🎉🎉🎉🎉🎉🎉<flux:badge></flux:badge><flux:slot name=\"│\">\n</flux:modal>\n",
    );
    assert_eq!(
        LaravelLanguageServer::enclosing_flux_component(&doc, pos).as_deref(),
        Some("flux:modal"),
        "the multibyte prefix must not cause the closed inner `<flux:badge>` to \
         be mistaken for the enclosing component",
    );
}

// ─── integration round-trip: slot tag → owner → its named slots ───────────

/// A `LaravelConfigData` whose `flux` anonymous-component directory is `dir`.
/// Mirrors `flux_config` in `flux_component_hover` — every other field empty.
fn flux_config(dir: PathBuf) -> LaravelConfigData {
    let mut anonymous_component_paths = HashMap::new();
    anonymous_component_paths.insert("flux".to_string(), dir.clone());
    LaravelConfigData {
        root: dir,
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A `<flux:modal>` backing Blade that fills two named slots (`$title`,
/// `$footer`) and the default `$slot`. The default slot must be excluded.
const MODAL_BLADE: &str = "@props(['name' => null])\n\
    <div class=\"modal\">\n\
    <header>{{ $title }}</header>\n\
    <div class=\"body\">{{ $slot }}</div>\n\
    @if($footer->isNotEmpty())<footer>{{ $footer }}</footer>@endif\n\
    </div>\n";

#[test]
fn round_trip_offers_owner_named_slots_excluding_default_slot() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("modal.blade.php"), MODAL_BLADE).unwrap();
    let config = flux_config(dir.path().to_path_buf());

    // The consumer document: a `<flux:slot name="│">` nested in `<flux:modal>`.
    let (doc, pos) = caret(
        "<flux:modal name=\"edit\">\n    <flux:slot name=\"│\">\n    </flux:slot>\n</flux:modal>\n",
    );

    // 1. The cursor is recognised as a slot-name context.
    let line = doc.lines().nth(pos.line as usize).unwrap();
    let ctx = LaravelLanguageServer::get_flux_slot_name_context(line, pos.character)
        .expect("cursor sits in the `name` value → slot-name context");
    assert_eq!(ctx.prefix, "", "empty value → all slots offered");

    // 2. The wrapping Flux component is resolved to `flux:modal`.
    let owner = LaravelLanguageServer::enclosing_flux_component(&doc, pos)
        .expect("the `<flux:modal>` ancestor owns the slot");
    assert_eq!(owner, "flux:modal");

    // 3. `flux:modal` resolves to the backing Blade on disk.
    let path = config
        .resolve_component_path(&owner)
        .into_iter()
        .find(|p| p.exists())
        .expect("the registered Flux modal file resolves");
    assert_eq!(path, dir.path().join("modal.blade.php"));

    // 4. Its named slots are enumerated — `title` and `footer`, never `$slot`.
    let content = fs::read_to_string(&path).unwrap();
    let slots = LaravelLanguageServer::extract_slot_variable_usages(&content);
    assert!(
        slots.iter().any(|(n, _)| n == "title"),
        "named slot `title` is offered",
    );
    assert!(
        slots.iter().any(|(n, _)| n == "footer"),
        "named slot `footer` is offered",
    );
    assert!(
        !slots.iter().any(|(n, _)| n == "slot"),
        "the default `$slot` is excluded",
    );
}
