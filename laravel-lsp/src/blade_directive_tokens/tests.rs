use super::*;
use std::collections::HashSet;

/// Build a lowercased known-directive set from bare names (no leading `@`).
fn known(names: &[&str]) -> HashSet<String> {
    names.iter().map(|n| n.to_lowercase()).collect()
}

#[test]
fn highlights_known_directives() {
    let set = known(&["if", "endif"]);
    let positions = directive_token_positions("@if ($x)\n@endif", &set);
    assert_eq!(positions, vec![(0, 0, 3), (1, 0, 6)]);
}

#[test]
fn highlights_registered_custom_inline_directive() {
    // `@money` stands in for a custom inline directive registered via
    // Blade::directive() — tree-sitter can't colour these, so this is the
    // case the LSP uniquely covers.
    let set = known(&["money"]);
    let positions = directive_token_positions("<span>@money($total)</span>", &set);
    assert_eq!(positions, vec![(0, 6, 6)]);
}

#[test]
fn matches_custom_names_with_digits_and_underscores() {
    let set = known(&["feature2", "my_directive"]);
    let positions = directive_token_positions("@feature2 @my_directive", &set);
    assert_eq!(positions, vec![(0, 0, 9), (0, 10, 13)]);
}

#[test]
fn rejects_unknown_at_words() {
    // A PHPDoc tag, a CSS at-rule, and an email local-part boundary are all
    // `@word` shaped but none are registered directives.
    let set = known(&["if", "foreach"]);
    assert!(directive_token_positions("@param string $name", &set).is_empty());
    assert!(directive_token_positions("@media (min-width: 1px) {}", &set).is_empty());
    assert!(directive_token_positions("mail hello@example.com", &set).is_empty());
}

#[test]
fn skips_directives_inside_blade_comments() {
    let set = known(&["if", "include"]);
    let positions = directive_token_positions("{{-- @if @include('x') --}}\n@if", &set);
    // Only the live `@if` on line 1 survives; the commented ones are dropped.
    assert_eq!(positions, vec![(1, 0, 3)]);
}

#[test]
fn skips_directives_inside_html_comments() {
    let set = known(&["foreach"]);
    let positions = directive_token_positions("<!-- @foreach --> @foreach", &set);
    assert_eq!(positions, vec![(0, 18, 8)]);
}

#[test]
fn matches_directive_names_case_insensitively() {
    let set = known(&["csrf"]);
    let positions = directive_token_positions("@CSRF", &set);
    assert_eq!(positions, vec![(0, 0, 5)]);
}

#[test]
fn comment_spans_cover_blade_and_html() {
    let spans = dead_region_spans("a {{-- x --}} b <!-- y --> c");
    assert_eq!(spans.len(), 2);
}

#[test]
fn delta_encodes_multiple_tokens_on_one_line() {
    let set = known(&["if", "csrf"]);
    let tokens = extract_blade_directive_tokens("@if @csrf", &set);
    assert_eq!(tokens.len(), 2);
    // First token: absolute (line 0, col 0), length of "@if".
    assert_eq!(
        (
            tokens[0].delta_line,
            tokens[0].delta_start,
            tokens[0].length
        ),
        (0, 0, 3)
    );
    // Second token: same line, so delta_start is relative (4 - 0), length "@csrf".
    assert_eq!(
        (
            tokens[1].delta_line,
            tokens[1].delta_start,
            tokens[1].length
        ),
        (0, 4, 5)
    );
}

#[test]
fn empty_when_no_known_directives_present() {
    let set = known(&["if"]);
    assert!(directive_token_positions("plain text with no directives", &set).is_empty());
    assert!(extract_blade_directive_tokens("plain text", &set).is_empty());
}

#[test]
fn skips_alpine_event_bindings(/* issue #61 */) {
    // `@click="…"` is an Alpine event binding, not a Blade directive — even if a
    // same-named custom directive were registered, the `@word=` shape excludes it.
    let set = known(&["click", "submit", "if"]);
    // Plain binding.
    assert!(directive_token_positions("<button @click=\"go()\">", &set).is_empty());
    // Binding with modifiers.
    assert!(directive_token_positions("<form @submit.prevent=\"save()\">", &set).is_empty());
    // A real Blade directive is untouched on the same line.
    let positions = directive_token_positions("<button @click=\"go()\">@if($x)", &set);
    assert_eq!(
        positions,
        vec![(0, 22, 3)],
        "only the @if directive survives"
    );
}

// ---- one dead-region scanner for the whole crate (issue #369 Part A) ------

#[test]
fn a_verbatim_body_is_dead_but_its_own_directives_are_not() {
    // Blade emits a `@verbatim` body literally and compiles nothing inside it,
    // so a directive there must not tokenise. The `@verbatim` and
    // `@endverbatim` directives themselves ARE compiled and must still light
    // up — which is why the span is the body, not the whole match.
    let set = known(&["verbatim", "endverbatim", "csrf"]);
    let positions = directive_token_positions("@verbatim @csrf @endverbatim", &set);
    let columns: Vec<u32> = positions.iter().map(|(_, col, _)| *col).collect();
    assert_eq!(
        columns,
        vec![0, 16],
        "only @verbatim (col 0) and @endverbatim (col 16) tokenise; @csrf at col 10 does not"
    );
}

#[test]
fn an_unterminated_opener_yields_no_dead_span() {
    // Blade requires the closer: `CompilesComments::compileComments` returns
    // the input unchanged without `--}}`, and Blade never handles `<!--`.
    // Masking to end of input would blank every directive below one typo.
    for unterminated in [
        "{{-- never closed @csrf",
        "<!-- never closed @csrf",
        "@verbatim never closed @csrf",
    ] {
        assert!(
            dead_region_spans(unterminated).is_empty(),
            "no span for {unterminated:?}"
        );
    }

    let set = known(&["csrf"]);
    assert_eq!(
        directive_token_positions("<!-- typo, closer deleted\n@csrf", &set).len(),
        1,
        "a directive below an unterminated opener still tokenises"
    );
}

#[test]
fn blanking_preserves_offsets_and_newlines() {
    // Every caller scans the masked copy and reports positions against the
    // original, so length and newline positions must be identical.
    let source = "a{{-- x\ny --}}b\n@verbatim\n$foo\n@endverbatim\n";
    let masked = blank_dead_regions(source);
    assert_eq!(masked.len(), source.len(), "byte length must not change");
    assert_eq!(
        masked.match_indices('\n').collect::<Vec<_>>(),
        source.match_indices('\n').collect::<Vec<_>>(),
        "newline positions must not change"
    );
    assert!(!masked.contains("$foo"), "the verbatim body is blanked");
    assert!(masked.contains("@verbatim"), "its own directives are not");
}

// ---- `@@` escaping: Blade renders the text and compiles nothing -----------

#[test]
fn an_escaped_directive_is_not_a_token() {
    // `BladeCompiler::compileStatement` starts `if (str_contains($match[1],
    // '@'))` and replaces the match with its own text, so `@@csrf` renders the
    // literal `@csrf` and executes nothing.
    let set = known(&["csrf"]);
    assert_eq!(
        directive_token_positions("@csrf", &set),
        vec![(0, 0, 5)],
        "the live directive still tokenises"
    );
    assert!(
        directive_token_positions("@@csrf", &set).is_empty(),
        "the escaped one does not"
    );
    // Three or more behave the same way: `\B` makes the match start at the
    // second `@`, group 1 is `@csrf`, and it is emitted literally. There is no
    // parity rule — two or more `@` never execute.
    assert!(
        directive_token_positions("@@@csrf", &set).is_empty(),
        "and neither does a longer run"
    );
}

#[test]
fn an_escaped_verbatim_opens_no_dead_region() {
    assert_eq!(
        dead_region_spans("@verbatim $x @endverbatim").len(),
        1,
        "a live @verbatim still marks its body dead"
    );
    assert!(
        dead_region_spans("@@verbatim $x @endverbatim").is_empty(),
        "an escaped one renders as text and opens nothing"
    );
}

#[test]
fn is_escaped_directive_reads_the_preceding_byte_only() {
    assert!(
        !is_escaped_directive("@csrf", 0),
        "nothing precedes offset 0"
    );
    assert!(is_escaped_directive("@@csrf", 1));
    assert!(is_escaped_directive("@@@csrf", 2));
    assert!(
        !is_escaped_directive("a@csrf", 1),
        "an ordinary character before the @ is not an escape"
    );
}
