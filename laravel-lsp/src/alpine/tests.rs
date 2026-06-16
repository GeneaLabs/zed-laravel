use super::*;

// ─── catalogs & lookups ──────────────────────────────────────────────────

#[test]
fn directives_cover_the_ac_set() {
    for name in [
        "x-data",
        "x-bind",
        "x-on",
        "x-model",
        "x-text",
        "x-html",
        "x-show",
        "x-if",
        "x-for",
        "x-ref",
        "x-init",
        "x-effect",
        "x-transition",
        "x-cloak",
        "x-teleport",
        "x-ignore",
    ] {
        assert!(directive(name).is_some(), "{name} should be catalogued");
    }
}

#[test]
fn magics_cover_the_ac_set() {
    for name in [
        "$el",
        "$refs",
        "$store",
        "$watch",
        "$dispatch",
        "$nextTick",
        "$root",
        "$data",
        "$id",
        "$persist",
    ] {
        assert!(magic(name).is_some(), "{name} should be catalogued");
    }
}

#[test]
fn lookup_misses_return_none() {
    assert!(directive("x-nope").is_none());
    assert!(directive("class").is_none());
    assert!(magic("$nope").is_none());
    assert!(
        magic("store").is_none(),
        "magic lookup requires the leading $"
    );
}

#[test]
fn directive_and_event_names_are_disjoint() {
    // The disambiguation rests on this: no name is both a Blade-shaped directive
    // and an Alpine event, so a merged completion list never double-suggests.
    for d in ALPINE_DIRECTIVES {
        let bare = d.name.trim_start_matches("x-");
        assert!(
            !ALPINE_EVENTS.contains(&bare),
            "{} collides with an event name",
            d.name
        );
    }
}

#[test]
fn mergeable_events_drops_blade_directive_collisions() {
    // The *real* merge path is Blade directives × Alpine events, and they are
    // NOT disjoint: `error` is both Laravel's `@error … @enderror` directive and
    // a DOM event. `mergeable_events` must drop the Alpine `error` so the merged
    // `@`-completion list shows it once, not twice (issue #61, AC5).
    let blade_names = ["error", "if", "foreach"]; // names already offered by Blade
    let events = mergeable_events("err", &blade_names);

    assert!(
        !events.contains(&"error"),
        "the Alpine `error` event must be dropped — Blade already offers @error: {events:?}",
    );

    // Assemble the merged `@`-name set exactly as the handler does (Blade items
    // that matched the prefix, then the deduped Alpine events) and prove there
    // is exactly one `@error`.
    let mut at_names: Vec<String> = blade_names
        .iter()
        .filter(|n| n.starts_with("err"))
        .map(|n| format!("@{n}"))
        .collect();
    at_names.extend(events.iter().map(|e| format!("@{e}")));
    assert_eq!(
        at_names.iter().filter(|n| *n == "@error").count(),
        1,
        "exactly one @error entry in the merged list, got {at_names:?}",
    );
}

#[test]
fn mergeable_events_keeps_non_colliding_events() {
    // A name that is only an Alpine event (no same-named Blade directive) is
    // still offered. `@cli` → `click`, with no Blade `@click` to shadow it.
    let blade_names = ["if", "foreach"];
    let events = mergeable_events("cli", &blade_names);
    assert!(
        events.contains(&"click"),
        "non-colliding Alpine events must survive the dedup: {events:?}",
    );
}

// ─── is_alpine_event ─────────────────────────────────────────────────────

#[test]
fn recognizes_core_events_and_modifiers() {
    assert!(is_alpine_event("click"));
    assert!(is_alpine_event("submit"));
    assert!(is_alpine_event("change"));
    assert!(is_alpine_event("input"));
    // Modifiers don't defeat recognition.
    assert!(is_alpine_event("submit.prevent"));
    assert!(is_alpine_event("keyup.enter"));
    assert!(is_alpine_event("click.outside"));
}

#[test]
fn rejects_blade_directive_names() {
    // These are real Blade directives used inside tags — must NOT be events.
    assert!(!is_alpine_event("class"));
    assert!(!is_alpine_event("checked"));
    assert!(!is_alpine_event("if"));
    assert!(!is_alpine_event("foreach"));
    assert!(!is_alpine_event(""));
}

// ─── matching_* filters ──────────────────────────────────────────────────

#[test]
fn matching_directives_filters_by_prefix() {
    let names: Vec<_> = matching_directives("x-t").iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["x-text", "x-transition", "x-teleport"]);
}

#[test]
fn matching_directives_empty_prefix_returns_all() {
    assert_eq!(matching_directives("x-").len(), ALPINE_DIRECTIVES.len());
}

#[test]
fn matching_magics_filters_after_the_dollar() {
    // Prefix is the text typed after `$`.
    let names: Vec<_> = matching_magics("re").iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["$refs"]);
    let names: Vec<_> = matching_magics("d").iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["$dispatch", "$data"]);
}

#[test]
fn matching_events_filters_by_prefix() {
    assert!(matching_events("cl").contains(&"click"));
    assert!(matching_events("sub").contains(&"submit"));
    assert!(matching_events("zzz").is_empty());
}

// ─── hover cards ─────────────────────────────────────────────────────────

#[test]
fn directive_card_has_name_summary_and_docs_link() {
    let card = directive_card("x-data").expect("x-data has a card");
    assert!(card.contains("x-data"), "card shows the directive name");
    assert!(
        card.contains("reactive data"),
        "card shows the synopsis: {card}"
    );
    assert!(
        card.contains("https://alpinejs.dev/directives/data"),
        "card links to the docs: {card}"
    );
}

#[test]
fn magic_card_has_name_summary_and_docs_link() {
    let card = magic_card("$store").expect("$store has a card");
    assert!(card.contains("$store"));
    assert!(card.contains("global Alpine store"), "synopsis: {card}");
    assert!(card.contains("https://alpinejs.dev/magics/store"));
}

#[test]
fn cards_are_none_for_unknown_tokens() {
    assert!(directive_card("x-bogus").is_none());
    assert!(magic_card("$bogus").is_none());
}

// ─── at_attribute_position ───────────────────────────────────────────────

#[test]
fn attribute_position_inside_open_tag() {
    let line = "<div x-data>";
    let at = line.find('x').unwrap();
    assert!(at_attribute_position(line, at));
}

#[test]
fn not_attribute_position_in_plain_text() {
    let line = "some x-data text";
    let at = line.find('x').unwrap();
    assert!(!at_attribute_position(line, at));
}

#[test]
fn not_attribute_position_after_tag_closed() {
    let line = "<div></div> x-data";
    let at = line.rfind('x').unwrap();
    assert!(!at_attribute_position(line, at));
}

#[test]
fn not_attribute_position_inside_quoted_value() {
    let line = "<div class=\"x-data";
    let at = line.rfind('x').unwrap();
    assert!(!at_attribute_position(line, at));
}

#[test]
fn attribute_position_after_apostrophe_in_double_quoted_value() {
    // Regression: an apostrophe inside an already-closed double-quoted value
    // must not throw off attribute-position detection. Independent `'`/`"`
    // parity counts saw an odd `'` here and wrongly returned false, silently
    // killing Alpine directive completion + hover on the rest of the element.
    let line = "<input placeholder=\"Don't\" x-data>";
    let at = line.find("x-data").unwrap();
    assert!(at_attribute_position(line, at));
}

#[test]
fn attribute_position_after_double_quote_in_single_quoted_value() {
    // Symmetric case: a `"` sitting inside an already-closed single-quoted
    // value must not flip parity either.
    let line = "<input data-label='say \"hi\"' x-data>";
    let at = line.find("x-data").unwrap();
    assert!(at_attribute_position(line, at));
}

#[test]
fn not_attribute_position_with_unbalanced_quote_after_apostrophe() {
    // The apostrophe is decoration inside the value; the cursor is genuinely
    // inside the open `class="…"` string, so it is *not* an attribute position.
    let line = "<input placeholder=\"Don't\" class=\"x-data";
    let at = line.rfind("x-data").unwrap();
    assert!(!at_attribute_position(line, at));
}

#[test]
fn at_attribute_position_rejects_non_char_boundary() {
    // Hardening: a byte offset landing inside a multibyte char must return
    // false rather than panicking on the `&line[..byte_pos]` slice.
    let line = "<div café x-data>";
    let cafe = line.find("caf").unwrap();
    let mid_e = cafe + "caf".len() + 1; // inside the 2-byte 'é'
    assert!(!line.is_char_boundary(mid_e));
    assert!(!at_attribute_position(line, mid_e));
}

// ─── directive completion context ────────────────────────────────────────

#[test]
fn directive_context_bare_x() {
    let line = "<div x";
    let ctx = directive_completion_context(line, line.len() as u32)
        .expect("typing `x` in a tag is a directive context");
    assert_eq!(ctx.prefix, "x");
    assert_eq!(ctx.start_col, "<div ".len() as u32);
}

#[test]
fn directive_context_partial_name() {
    let line = "<div x-da";
    let ctx = directive_completion_context(line, line.len() as u32)
        .expect("`x-da` is a directive context");
    assert_eq!(ctx.prefix, "x-da");
    // Filters down to x-data.
    let names: Vec<_> = matching_directives(&ctx.prefix)
        .iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["x-data"]);
}

#[test]
fn directive_context_in_component_tag_includes_x_bind() {
    // AC #7: x-data completes in a `<x-…>` component tag, and x-bind (the `:`
    // shorthand directive) is among the offered directives.
    let line = "<x-card x-";
    let ctx = directive_completion_context(line, line.len() as u32)
        .expect("`x-` in a component tag is a directive context");
    assert_eq!(ctx.prefix, "x-");
    assert_eq!(ctx.start_col, "<x-card ".len() as u32);
    let names: Vec<_> = matching_directives(&ctx.prefix)
        .iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"x-data"), "x-data offered: {names:?}");
    assert!(names.contains(&"x-bind"), "x-bind offered: {names:?}");
}

#[test]
fn directive_context_none_in_tag_name() {
    // Still inside the tag name (no whitespace yet) — not an attribute position.
    assert!(directive_completion_context("<xyz", 4).is_none());
}

#[test]
fn directive_context_none_in_plain_text() {
    assert!(directive_completion_context("just x-da here", 8).is_none());
}

#[test]
fn directive_context_none_inside_attribute_value() {
    let line = "<div class=\"x-da";
    assert!(directive_completion_context(line, line.len() as u32).is_none());
}

// ─── magic completion context ────────────────────────────────────────────

#[test]
fn magic_context_inside_event_expression() {
    let line = "<button @click=\"$di";
    let ctx = magic_completion_context(line, line.len() as u32)
        .expect("`$di` inside @click value is a magic context");
    assert_eq!(ctx.prefix, "di");
    assert_eq!(ctx.start_col, line.len() as u32 - 2);
    let names: Vec<_> = matching_magics(&ctx.prefix)
        .iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["$dispatch"]);
}

#[test]
fn magic_context_inside_x_data_expression() {
    let line = "<div x-data=\"{ open: false, total: $st";
    let ctx = magic_completion_context(line, line.len() as u32)
        .expect("`$st` inside x-data value is a magic context");
    assert_eq!(ctx.prefix, "st");
}

#[test]
fn magic_context_inside_bound_attribute() {
    let line = "<span :title=\"$re";
    let ctx = magic_completion_context(line, line.len() as u32)
        .expect("`$re` inside a :bound value is a magic context");
    assert_eq!(ctx.prefix, "re");
}

#[test]
fn magic_context_none_in_plain_blade_echo() {
    // A normal Blade `$variable` must NOT be treated as an Alpine magic.
    assert!(magic_completion_context("{{ $us", 6).is_none());
}

#[test]
fn magic_context_none_in_non_alpine_attribute() {
    // A `$` inside a plain HTML attribute value isn't an Alpine expression.
    let line = "<input value=\"$st";
    assert!(magic_completion_context(line, line.len() as u32).is_none());
}

// ─── hover ───────────────────────────────────────────────────────────────

#[test]
fn hover_on_directive_name() {
    let line = "<div x-data=\"{}\">";
    let at = line.find("data").unwrap() as u32; // cursor on the directive
    let card = hover_at(line, at).expect("hover over x-data");
    assert!(card.contains("x-data"));
}

#[test]
fn hover_on_event_argument_resolves_to_x_on() {
    // Cursor on `click` in `x-on:click` → the x-on directive card.
    let line = "<button x-on:click=\"go()\">";
    let at = line.find("click").unwrap() as u32;
    let card = hover_at(line, at).expect("hover over x-on:click");
    assert!(card.contains("x-on"), "resolves to x-on: {card}");
}

#[test]
fn hover_on_at_event_shorthand_resolves_to_x_on() {
    let line = "<button @click=\"go()\">";
    let at = line.find("click").unwrap() as u32;
    let card = hover_at(line, at).expect("hover over @click");
    assert!(card.contains("x-on"), "@event maps to x-on: {card}");
}

#[test]
fn hover_on_colon_bind_shorthand_resolves_to_x_bind() {
    let line = "<img :src=\"url\">";
    let at = line.find("src").unwrap() as u32;
    let card = hover_at(line, at).expect("hover over :src");
    assert!(card.contains("x-bind"), ":attr maps to x-bind: {card}");
}

#[test]
fn hover_on_magic_in_expression() {
    let line = "<button @click=\"$dispatch('x')\">";
    let at = line.find("dispatch").unwrap() as u32;
    let card = hover_at(line, at).expect("hover over $dispatch");
    assert!(card.contains("$dispatch"));
}

#[test]
fn hover_none_on_at_blade_directive() {
    // `@if` is a Blade directive, not an Alpine event — no Alpine hover.
    let line = "<div>@if($x)";
    let at = line.find("if").unwrap() as u32;
    assert!(hover_at(line, at).is_none());
}

#[test]
fn hover_none_on_plain_php_variable() {
    let line = "<?php $store = 1;";
    let at = line.find("store").unwrap() as u32;
    assert!(
        hover_at(line, at).is_none(),
        "a plain $variable is not an Alpine magic"
    );
}

#[test]
fn hover_none_on_unknown_attribute() {
    let line = "<div data-foo=\"x\">";
    let at = line.find("data-foo").unwrap() as u32;
    assert!(hover_at(line, at).is_none());
}
