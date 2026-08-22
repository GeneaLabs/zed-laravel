use super::*;
use crate::parser::parse_php;

fn scan(src: &str) -> Vec<StringTagMatch> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    scan_php_string_tags(&tree, &wrapped)
}

/// The reported case (#59 false positive): a job builds the component tag as a
/// double-quoted string with an interpolated attribute value.
#[test]
fn finds_component_in_double_quoted_string_with_interpolation() {
    let found = scan(r#"$r = "<x-reader.cross-reference :id=\"{$id}\" />";"#);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].kind, StringTagKind::Component);
    assert_eq!(found[0].name, "reader.cross-reference");
    assert_eq!(found[0].tag_name, "x-reader.cross-reference");
}

#[test]
fn finds_component_in_single_quoted_string() {
    let found = scan(r#"$r = '<x-mail::button url="x">';"#);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].name, "mail::button");
}

#[test]
fn finds_livewire_tag_and_files_it_separately() {
    let found = scan(r#"$r = '<livewire:counter />';"#);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].kind, StringTagKind::Livewire);
    assert_eq!(
        found[0].name, "counter",
        "the `livewire:` prefix is stripped"
    );
}

#[test]
fn flux_tag_keeps_its_prefix_in_the_indexed_name() {
    let found = scan(r#"$r = '<flux:icon.trash />';"#);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].kind, StringTagKind::Component);
    assert_eq!(found[0].name, "flux:icon.trash");
}

#[test]
fn finds_tags_in_a_heredoc_body() {
    let found = scan("$r = <<<HTML\n<x-alert type=\"warn\" />\nHTML;");

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].name, "alert");
}

#[test]
fn finds_tags_in_a_nowdoc_body() {
    let found = scan("$r = <<<'HTML'\n<x-alert />\nHTML;");

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    assert_eq!(found[0].name, "alert");
}

// ---------------------------------------------------------------------------
// Precision guards — each of these must stay empty.
// ---------------------------------------------------------------------------

/// Guard 1: only literal text is scanned. A tag in a comment is not a
/// reference, and neither is one in raw inline HTML outside PHP.
#[test]
fn ignores_tags_outside_string_literals() {
    assert!(
        scan("// <x-button /> mentioned in a comment\n$x = 1;").is_empty(),
        "a line comment is not a literal"
    );
    assert!(
        scan("/** Renders <x-button /> for you. */\nfunction f() {}").is_empty(),
        "a docblock is not a literal"
    );
    assert!(
        scan("$x = 1;\n?>\n<x-button />").is_empty(),
        "inline HTML after `?>` is not a literal"
    );
}

/// Guard 2: an interpolated tag name splits the literal, leaving a fragment
/// with no terminator. Indexing it would invent a phantom `alert-` component.
#[test]
fn ignores_runtime_constructed_tag_names() {
    assert!(
        scan(r#"$r = "<x-alert-{$type} />";"#).is_empty(),
        "brace-interpolated name must not be indexed"
    );
    assert!(
        scan(r#"$r = "<x-alert-$type />";"#).is_empty(),
        "variable-interpolated name must not be indexed"
    );
}

/// The terminator guard also rejects a name truncated by concatenation — we
/// cannot know the full name, so we claim nothing.
#[test]
fn ignores_tag_name_truncated_at_end_of_fragment() {
    assert!(
        scan(r#"$r = '<x-butt' . 'on />';"#).is_empty(),
        "a name running to the fragment end is unresolvable"
    );
}

/// Slot syntax is not component usage — same exclusion the Blade extractor makes.
#[test]
fn ignores_slot_tags() {
    assert!(scan(r#"$r = '<x-slot:title>';"#).is_empty());
    assert!(scan(r#"$r = '<x-slot name="title">';"#).is_empty());
}

/// A closing tag is not a second reference; only opening tags carry `<name`.
#[test]
fn ignores_closing_tags() {
    let found = scan(r#"$r = '<x-card>body</x-card>';"#);

    assert_eq!(found.len(), 1, "only the opening tag counts, got {found:?}");
    assert_eq!(found[0].name, "card");
}

// ---------------------------------------------------------------------------
// Positions — 0-based line, byte column, anchored on the name not the `<`.
// ---------------------------------------------------------------------------

#[test]
fn positions_point_at_the_tag_name_on_a_single_line() {
    // `<?php\n` is line 0, so the assignment is line 1.
    let src = r#"$r = '<x-card />';"#;
    let found = scan(src);

    let m = &found[0];
    assert_eq!(m.line, 1);
    // `$r = '<` is 7 bytes, so the name starts at column 7.
    assert_eq!(m.column, 7);
    assert_eq!(m.end_column, 7 + "x-card".len() as u32);

    let line = format!("<?php\n{src}").lines().nth(1).unwrap().to_string();
    assert_eq!(
        &line[m.column as usize..m.end_column as usize],
        "x-card",
        "the reported span must slice back to the tag name"
    );
}

/// A quoted string that spans lines is ONE `string_content` node holding an
/// embedded newline, so a tag past that newline must take its column from the
/// enclosing line's start — not from the fragment's own start column — and its
/// line must advance by the newlines in between.
///
/// Deliberately a quoted string rather than a heredoc: tree-sitter-php emits
/// one `string_content` per physical line inside a heredoc body, so a heredoc
/// never exercises this arithmetic.
#[test]
fn positions_are_line_relative_in_a_multiline_quoted_string() {
    let src = "$r = \"intro\n  <x-alert />\";";
    let found = scan(src);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    let m = &found[0];
    assert_eq!(m.line, 2, "line 0 is `<?php`, line 1 opens the string");
    assert_eq!(m.column, 3, "two spaces of indent, then `<`, then the name");
    assert_eq!(m.end_column, 3 + "x-alert".len() as u32);

    let line = format!("<?php\n{src}")
        .lines()
        .nth(m.line as usize)
        .unwrap()
        .to_string();
    assert_eq!(
        &line[m.column as usize..m.end_column as usize],
        "x-alert",
        "the reported span must slice back to the tag name"
    );
}

/// The heredoc counterpart: its body is split per physical line, so the tag's
/// fragment starts at column 0 of its own line. Pins the split behaviour the
/// test above relies on.
#[test]
fn positions_are_correct_on_a_later_heredoc_line() {
    let src = "$r = <<<HTML\n<p>intro</p>\n  <x-alert />\nHTML;";
    let found = scan(src);

    assert_eq!(found.len(), 1, "expected one tag, got {found:?}");
    let m = &found[0];
    assert_eq!(m.line, 3, "line 2 is `<p>intro</p>`");
    assert_eq!(m.column, 3);
    assert_eq!(m.end_column, 3 + "x-alert".len() as u32);
}

#[test]
fn multiple_tags_are_returned_in_source_order() {
    let found = scan("$a = '<x-one />';\n$b = '<x-two />';\n$c = '<x-three />';");

    let names: Vec<&str> = found.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["one", "two", "three"]);
}

/// The early-bail fast path must not change results for a file that has none.
#[test]
fn file_without_any_tag_yields_nothing() {
    assert!(scan("$x = 'plain string';\nclass Foo {}").is_empty());
}
