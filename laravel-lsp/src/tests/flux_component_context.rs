//! Flux tag completion-context detection (issue #60).
//!
//! Two contexts feed `<flux:…>` completion:
//! - `get_flux_component_context` — the cursor is in the component *name*
//!   (`<flux:butt│`), offering component names.
//! - `get_flux_attribute_context` — the cursor is past the name, at an
//!   *attribute* position (`<flux:button │>`), offering the component's props.

use crate::LaravelLanguageServer;

// ─── name context ───────────────────────────────────────────────────────

#[test]
fn name_context_at_bare_prefix() {
    let line = "<flux:";
    let ctx = LaravelLanguageServer::get_flux_component_context(line, line.len() as u32)
        .expect("`<flux:` should be a name context");
    assert_eq!(ctx.prefix, "");
    assert_eq!(ctx.start_col, 6, "replacement starts right after `<flux:`");
}

#[test]
fn name_context_with_partial_name() {
    let line = "    <flux:butt";
    let ctx = LaravelLanguageServer::get_flux_component_context(line, line.len() as u32)
        .expect("partial Flux name should be a name context");
    assert_eq!(ctx.prefix, "butt");
}

#[test]
fn name_context_allows_dotted_names() {
    let line = "<flux:icon.arrow-right";
    let ctx = LaravelLanguageServer::get_flux_component_context(line, line.len() as u32)
        .expect("dotted Flux name should be a name context");
    assert_eq!(ctx.prefix, "icon.arrow-right");
}

#[test]
fn name_context_ends_at_first_space() {
    // Once attributes begin, the name context is over.
    let line = "<flux:button ";
    assert!(
        LaravelLanguageServer::get_flux_component_context(line, line.len() as u32).is_none(),
        "a space (start of attributes) must end the name context",
    );
}

#[test]
fn name_context_none_for_non_flux_tag() {
    let line = "<x-button";
    assert!(LaravelLanguageServer::get_flux_component_context(line, line.len() as u32).is_none());
}

// ─── attribute context ──────────────────────────────────────────────────

#[test]
fn attribute_context_after_name_space() {
    let line = "<flux:button ";
    let (name, ctx) = LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32)
        .expect("a space after the name opens the attribute context");
    assert_eq!(name, "flux:button");
    assert_eq!(ctx.prefix, "", "no partial typed yet → offer all props");
    assert_eq!(ctx.start_col, line.len() as u32);
}

#[test]
fn attribute_context_with_partial_attribute() {
    let line = "<flux:button vari";
    let (name, ctx) = LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32)
        .expect("partial attribute should be an attribute context");
    assert_eq!(name, "flux:button");
    assert_eq!(ctx.prefix, "vari");
    assert_eq!(ctx.start_col, "<flux:button ".len() as u32);
}

#[test]
fn attribute_context_preserves_dotted_component_name() {
    let line = "<flux:icon.arrow-right cl";
    let (name, ctx) = LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32)
        .expect("dotted component still resolves in attribute context");
    assert_eq!(name, "flux:icon.arrow-right");
    assert_eq!(ctx.prefix, "cl");
}

#[test]
fn attribute_context_strips_leading_colon_for_bound_attribute() {
    // `:variant="..."` is a bound attribute — filter on the name, but replace
    // only the part after the `:`.
    let line = "<flux:button :vari";
    let (_name, ctx) = LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32)
        .expect("bound-attribute prefix should still be an attribute context");
    assert_eq!(ctx.prefix, "vari", "leading `:` dropped from the filter");
    assert_eq!(
        ctx.start_col,
        "<flux:button :".len() as u32,
        "replacement starts after the `:`",
    );
}

#[test]
fn attribute_context_skips_second_attribute() {
    let line = "<flux:button variant=\"primary\" si";
    let (name, ctx) = LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32)
        .expect("attribute context after a completed attribute");
    assert_eq!(name, "flux:button");
    assert_eq!(ctx.prefix, "si");
}

#[test]
fn attribute_context_none_inside_attribute_value() {
    // Cursor inside the quoted value — props complete attribute *names* only.
    let line = "<flux:button variant=\"pri";
    assert!(
        LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32).is_none(),
        "inside an attribute value there is no prop-name completion",
    );
}

#[test]
fn attribute_context_none_while_still_in_name() {
    // No space yet → still the name context, not attributes.
    let line = "<flux:butt";
    assert!(
        LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32).is_none(),
        "the name position must not be treated as an attribute position",
    );
}

#[test]
fn attribute_context_none_after_tag_closed() {
    let line = "<flux:button variant=\"primary\"> ";
    assert!(
        LaravelLanguageServer::get_flux_attribute_context(line, line.len() as u32).is_none(),
        "a closed tag (`>` before cursor) is not an attribute position",
    );
}
