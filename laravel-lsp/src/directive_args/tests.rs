use super::*;

/// The bug from #327: a condition that compares a string donated its own
/// literal as the "view name". Every row here is a real Blade shape.
#[test]
fn second_argument_is_read_after_the_top_level_comma() {
    let cases = [
        ("($cond, 'view')", Some("view")),
        (
            "($boolean, 'view.name', ['status' => 'complete'])",
            Some("view.name"),
        ),
        (
            "($cond, \"double.quoted\", ['a' => $b])",
            Some("double.quoted"),
        ),
        ("($type === 'admin', 'pages.admin')", Some("pages.admin")),
        (
            "($user->role == 'editor', 'panels.editor', ['x' => 1])",
            Some("panels.editor"),
        ),
        ("($cond)", None),
        ("($cond, '')", None),
        ("($cond1, $cond2)", None),
    ];
    for (args, expected) in cases {
        assert_eq!(nth_literal(args, 1).as_deref(), expected, "args: {args}");
    }
}

/// Bracket tracking is load-bearing: `in_array($k, ['a', 'b'])` puts two
/// commas inside the condition, both of them before the real split point.
#[test]
fn nested_brackets_do_not_supply_the_split_comma() {
    assert_eq!(
        nth_literal("(in_array($k, ['a', 'b']), 'pages.list')", 1).as_deref(),
        Some("pages.list")
    );
    assert_eq!(
        nth_literal("(match_it($a, $b), 'pages.match')", 1).as_deref(),
        Some("pages.match")
    );
}

/// A comma living inside the condition's own string literal is not
/// structure. Without quote suppression the split lands inside `'a,b'` and
/// argument one becomes the fragment `b'`.
#[test]
fn a_comma_inside_a_string_literal_is_not_the_split_point() {
    assert_eq!(
        nth_literal("($status === 'a,b', 'view.name')", 1).as_deref(),
        Some("view.name")
    );
}

/// A closing paren inside a string literal must not end the argument list
/// early — `unwrap_parens` would otherwise treat the args as unwrapped and
/// hand the whole raw string back.
#[test]
fn a_paren_inside_a_string_literal_does_not_close_the_argument_list() {
    assert_eq!(
        nth_literal("($label === \"a)b\", 'view.name')", 1).as_deref(),
        Some("view.name")
    );
}

/// PHP escapes its quote characters, and a mis-tracked `\'` flips the
/// scanner's in-string state for the rest of the list — after which the real
/// top-level comma looks like it is inside a string and is never found.
#[test]
fn an_escaped_quote_does_not_end_the_condition_string() {
    assert_eq!(
        nth_literal(r"($label === 'it\'s', 'view.name')", 1).as_deref(),
        Some("view.name")
    );
}

/// `queries.rs` documents both shapes reaching these extractors: wrapped in
/// the directive-call parens, and bare.
#[test]
fn arguments_parse_with_or_without_the_wrapping_parens() {
    assert_eq!(nth_literal("$cond, 'view'", 1).as_deref(), Some("view"));
    assert_eq!(nth_literal("'view'", 0).as_deref(), Some("view"));
    // A condition that is itself parenthesised must not be mistaken for the
    // call's own wrapper — stripping it would leave the list malformed.
    assert_eq!(
        nth_literal("(($a || $b), 'view.name')", 1).as_deref(),
        Some("view.name")
    );
}

/// Half-typed source reaches the parser on every keystroke.
#[test]
fn unbalanced_arguments_still_yield_what_was_typed() {
    assert_eq!(nth_literal("('view'", 0).as_deref(), Some("view"));
    assert_eq!(nth_literal("($cond, 'view'", 1).as_deref(), Some("view"));
    assert_eq!(nth_literal("(", 0), None);
    assert_eq!(nth_literal("", 0), None);
}

/// Argument zero is read with the same strictness as argument one: the
/// argument must *be* a literal. The loose quote-trimming this replaced
/// reported `$view` and `partials.` as names — targets that never resolve.
#[test]
fn a_non_literal_argument_is_not_a_name() {
    assert_eq!(nth_literal("('view')", 0).as_deref(), Some("view"));
    assert_eq!(nth_literal("($view)", 0), None);
    assert_eq!(nth_literal("('partials.' . $name)", 0), None);
    assert_eq!(nth_literal("($a ?: $b)", 0), None);
    assert_eq!(nth_literal("('')", 0), None);
    assert_eq!(
        nth_literal("('view', ['a' => 1])", 0).as_deref(),
        Some("view")
    );
}

/// The outline label for directives that are not condition-first: the name
/// sits inside an array literal, one level below any top-level argument.
#[test]
fn first_literal_reaches_into_nested_array_literals() {
    assert_eq!(
        first_literal("(['title', 'count' => 0])").as_deref(),
        Some("title")
    );
    assert_eq!(
        first_literal("(['custom.layout', 'default.layout'])").as_deref(),
        Some("custom.layout")
    );
    assert_eq!(first_literal("('content')").as_deref(), Some("content"));
    assert_eq!(first_literal("($variable)"), None);
    assert_eq!(first_literal("('')"), None);
    assert_eq!(first_literal("()"), None);
}

/// The condition-first list is what routes a directive to argument one. It
/// is asserted member by member: a set that is correct today but unpinned
/// degrades silently the next time someone edits it.
#[test]
fn condition_first_directives_are_exactly_include_when_and_include_unless() {
    assert!(is_condition_first("includeWhen"));
    assert!(is_condition_first("includeUnless"));
    for other in [
        "include",
        "includeIf",
        "includeFirst",
        "extends",
        "component",
        "each",
        "livewire",
        "props",
        "section",
    ] {
        assert!(!is_condition_first(other), "directive: {other}");
    }
}
