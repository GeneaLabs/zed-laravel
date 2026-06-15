use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn captures_single_line_declaration() {
    let src = "<div>\n@props(['user' => null, 'showAvatar' => true])\n<h1>...</h1>\n</div>\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert_eq!(got, "@props(['user' => null, 'showAvatar' => true])");
}

#[test]
fn captures_multiline_declaration() {
    let src = "@props([\n    'user' => null,\n    'showAvatar' => true,\n])\n<div></div>\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.starts_with("@props(["));
    assert!(got.ends_with("])"));
    assert!(got.contains("'user' => null"));
    assert!(got.contains("'showAvatar' => true"));
}

#[test]
fn returns_none_when_directive_absent() {
    let src = "<div>no props here</div>\n";
    assert_eq!(extract_props_directive_from_source(src), None);
}

#[test]
fn ignores_strings_containing_parens() {
    // Embedded `(` / `)` inside a string literal must not throw off the
    // paren balancer — we should capture all the way to the matching close.
    let src = "@props(['note' => 'something (parens) inside', 'count' => 0])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'something (parens) inside'"));
    assert!(got.ends_with("])"));
}

#[test]
fn does_not_match_propsextended_substring() {
    // `@propsExtended(...)` starts with `@props` but is a different directive.
    // The word-boundary check should reject it.
    let src = "@propsExtended(['x' => 1])\n@props(['y' => 2])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'y' => 2"));
    assert!(
        !got.contains("'x' => 1"),
        "must not match @propsExtended: {}",
        got
    );
}

#[test]
fn captures_first_directive_when_multiple_present() {
    // A file can technically have multiple `@props` (rare, but possible
    // across components). We only return the first.
    let src = "@props(['a' => 1])\n@props(['b' => 2])\n";
    let got = extract_props_directive_from_source(src).expect("should find @props");
    assert!(got.contains("'a' => 1"));
}

#[test]
fn reads_from_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("view.blade.php");
    fs::write(&path, "@props(['user' => null])\n").unwrap();
    let got = extract_props_directive(&path).expect("should find @props");
    assert_eq!(got, "@props(['user' => null])");
}

#[test]
fn returns_none_for_nonexistent_file() {
    let nonexistent = std::path::PathBuf::from("/nonexistent/view.blade.php");
    assert_eq!(extract_props_directive(&nonexistent), None);
}

// ─── prop name extraction (issue #60: Flux attribute completion) ─────────

#[test]
fn prop_names_from_keyed_entries() {
    let src = "@props(['user' => null, 'showAvatar' => true])\n<div></div>\n";
    assert_eq!(extract_prop_names(src), vec!["user", "showAvatar"]);
}

#[test]
fn prop_names_from_shorthand_entries() {
    // Bare string entries (no default) are still prop names.
    let src = "@props(['variant', 'size'])\n";
    assert_eq!(extract_prop_names(src), vec!["variant", "size"]);
}

#[test]
fn prop_names_mix_shorthand_and_keyed() {
    let src = "@props([\n    'variant',\n    'size' => 'base',\n    'icon' => null,\n])\n";
    assert_eq!(extract_prop_names(src), vec!["variant", "size", "icon"]);
}

#[test]
fn prop_names_ignore_nested_array_defaults() {
    // Only the first string of each top-level entry is the name — strings
    // inside a nested-array default must not be mistaken for props.
    let src = "@props(['opts' => ['a', 'b'], 'label' => 'Hi'])\n";
    assert_eq!(extract_prop_names(src), vec!["opts", "label"]);
}

#[test]
fn prop_names_ignore_commas_inside_string_defaults() {
    let src = "@props(['note' => 'a, b, c', 'count' => 0])\n";
    assert_eq!(extract_prop_names(src), vec!["note", "count"]);
}

#[test]
fn prop_names_empty_without_directive() {
    assert!(extract_prop_names("<div>no props</div>\n").is_empty());
}

#[test]
fn prop_names_empty_for_empty_props() {
    assert!(extract_prop_names("@props([])\n").is_empty());
}

// ─── string-literal decoding (issue #111: UTF-8 correctness) ─────────────

#[test]
fn first_string_literal_utf8_multibyte() {
    // A multi-byte UTF-8 character inside the literal must be decoded whole,
    // not split into raw Latin-1 byte casts.
    assert_eq!(first_string_literal("'café'"), Some("café".to_string()));
    assert_eq!(
        first_string_literal("\"naïve façade — €\""),
        Some("naïve façade — €".to_string())
    );
}

#[test]
fn first_string_literal_escapes() {
    // Escape sequences take the next char verbatim, even when it is multi-byte.
    assert_eq!(first_string_literal(r"'it\'s'"), Some("it's".to_string()));
    assert_eq!(first_string_literal(r#""a\"b""#), Some("a\"b".to_string()));
    assert_eq!(first_string_literal(r"'a\\b'"), Some("a\\b".to_string()));
    assert_eq!(first_string_literal(r"'caf\é'"), Some("café".to_string()));
}

#[test]
fn first_string_literal_none_cases() {
    assert_eq!(first_string_literal("no quotes here"), None);
    assert_eq!(first_string_literal("'unterminated"), None);
}
