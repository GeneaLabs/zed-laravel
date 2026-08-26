//! Panic-hardening for the LSP's line-local cursor helpers (issue #182).
//!
//! Every `*_context` helper used to do `let cursor = character as usize;`
//! followed by `&line_text[..cursor]`, treating the LSP `character` (a
//! code-point column) as a raw byte offset. On a line containing any multibyte
//! character that byte offset can land mid-codepoint, and the slice would
//! `panic!`. The helpers now route the column→offset conversion through
//! `query_chain::cursor::char_col_to_byte_offset`, which always yields a valid
//! char boundary clamped to the line length.
//!
//! The exhaustive sweeps below call each affected handler with **every** byte
//! index `0..=line.len()` on multibyte lines — including the mid-codepoint
//! indices that used to panic — and assert only that the call returns. A test
//! that completes is the assertion: no index panics. The positive cases pin
//! that a correct code-point column on a multibyte line still yields the right
//! context (the conversion is correct, not just panic-safe).

use crate::LaravelLanguageServer;

/// Multibyte lines whose byte ranges include many non-char-boundary indices
/// (`é` is 2 bytes, `🎉` is 4). Sweeping every byte index over these exercises
/// the exact indices the old `character as usize` slice would have panicked on.
const MULTIBYTE_LINES: &[&str] = &[
    "config('app.café🎉')",
    "route('users.café🎉')",
    "<x-café-🎉 ",
    "@if 🎉 café",
    "$café🎉->property",
    "'rule:café🎉|max:5'",
    "view('café.🎉.index')",
];

/// Call a `(&str, u32) -> Option<_>` handler at every byte index of every
/// multibyte line. The macro drops the result — the point is that no index
/// panics inside the helper's slice.
macro_rules! sweep_two_arg {
    ($($handler:path),+ $(,)?) => {
        for line in MULTIBYTE_LINES {
            for byte in 0..=line.len() {
                $( let _ = $handler(line, byte as u32); )+
            }
        }
    };
}

#[test]
fn two_arg_context_helpers_never_panic_on_any_byte_index() {
    sweep_two_arg!(
        LaravelLanguageServer::get_env_call_context,
        LaravelLanguageServer::get_env_interpolation_context,
        LaravelLanguageServer::get_phpunit_env_context,
        LaravelLanguageServer::get_config_call_context,
        LaravelLanguageServer::get_route_call_context,
        LaravelLanguageServer::get_view_call_context,
        LaravelLanguageServer::get_translation_call_context,
        LaravelLanguageServer::get_asset_call_context,
        LaravelLanguageServer::get_vite_call_context,
        LaravelLanguageServer::get_blade_component_context,
        LaravelLanguageServer::get_flux_component_context,
        LaravelLanguageServer::get_flux_slot_name_context,
        LaravelLanguageServer::get_flux_attribute_context,
        LaravelLanguageServer::get_livewire_component_context,
        LaravelLanguageServer::get_path_helper_context,
        LaravelLanguageServer::get_binding_call_context,
        LaravelLanguageServer::get_feature_call_context,
        LaravelLanguageServer::get_variable_name_context,
        LaravelLanguageServer::get_blade_directive_context,
        LaravelLanguageServer::get_inertia_call_context,
    );
}

#[test]
fn multi_arg_context_helpers_never_panic_on_any_byte_index() {
    for line in MULTIBYTE_LINES {
        for byte in 0..=line.len() {
            let c = byte as u32;
            let _ = LaravelLanguageServer::get_model_property_context(line, c, "");
            let _ = LaravelLanguageServer::get_validation_param_context(line, c, &[], &[]);
            let _ = LaravelLanguageServer::get_validation_rule_context(line, c, &[], &[]);
            let _ = LaravelLanguageServer::get_cast_type_context(line, c, &[]);
            let _ = LaravelLanguageServer::get_middleware_call_context(line, c, None);
        }
    }
}

// ---- positive correctness on multibyte lines ----------------------------

#[test]
fn variable_name_context_reads_whole_name_past_multibyte_prefix() {
    // `🎉 $user` has 7 code points; column 7 is end-of-line. The old byte-offset
    // slice would have sliced `&line[..7]` = `🎉 $u` and returned "u"; the
    // boundary-correct conversion reads the full "user".
    let line = "🎉 $user";
    assert_eq!(line.chars().count(), 7);
    let ctx = LaravelLanguageServer::get_variable_name_context(line, 7)
        .expect("`$user` is a variable-name context");
    assert_eq!(ctx, "user");
}

#[test]
fn config_call_context_keeps_accented_prefix() {
    // `config('app.café` with the cursor at end-of-line: the prefix must include
    // the accented char intact, proving the slice landed on a boundary.
    let line = "config('app.café";
    let col = line.chars().count() as u32;
    let ctx = LaravelLanguageServer::get_config_call_context(line, col)
        .expect("inside a config('…') string");
    assert_eq!(ctx.prefix, "app.café");
}

#[test]
fn inertia_call_context_keeps_accented_prefix() {
    // `inertia('Café` with the cursor at end-of-line: the page-name prefix must
    // include the accented char intact. The old `character as usize` slice
    // panicked at this column on a multibyte line (issue #10 regression); the
    // boundary-correct conversion reads the full "Café".
    let line = "inertia('Café";
    let col = line.chars().count() as u32;
    let ctx = LaravelLanguageServer::get_inertia_call_context(line, col)
        .expect("inside an inertia('…') page string");
    assert_eq!(ctx.prefix, "Café");
}

#[test]
fn blade_directive_context_resolves_past_emoji() {
    // `🎉 @if` → 5 code points; column 5 is end-of-line. Whitespace precedes `@`.
    let line = "🎉 @if";
    assert_eq!(line.chars().count(), 5);
    let ctx = LaravelLanguageServer::get_blade_directive_context(line, 5)
        .expect("`🎉 @if` is a directive context — space precedes `@`");
    assert_eq!(ctx, "if");
}

#[test]
fn mid_codepoint_byte_index_returns_cleanly_not_panics() {
    // Pass a column that, as a raw byte offset, lands *inside* the 4-byte `🎉`
    // (bytes 8..12 of `config('🎉')`). The old code panicked here; the new code
    // converts column 9 to a boundary and returns a well-defined value.
    let line = "config('🎉')";
    // Byte 9 is mid-`🎉` — not a char boundary.
    assert!(!line.is_char_boundary(9));
    // `config('🎉')` has 11 code points, so column 9 (the closing `'`) is well
    // within the line — but *byte* 9 lands mid-`🎉`, which is exactly where the
    // old `character as usize` slice panicked. The new code treats 9 as a
    // code-point column, converts it to a boundary, and returns cleanly.
    let _ = LaravelLanguageServer::get_config_call_context(line, 9);
}

// ---- detect_method_name_position: multibyte hardening (issue #182) -------
//
// `detect_method_name_position` (in `method_name_completion`) is the one
// hardened slice site outside the `*_context` family, so it gets its own
// coverage. The live caller (`try_method_name_completion`) converts the LSP
// code-point column through `char_col_to_byte_offset` before calling it; these
// tests pin both halves of that contract — the public fn never panics on an
// arbitrary byte index, and a converted column extracts the right receiver
// past a multibyte char (it slices on the correct coordinate, not merely a
// safe one).

/// Lines that put a `::` / `->` operator after a multibyte run, so the byte
/// offset and the code-point column genuinely diverge (`è` is 2 bytes, `🎉`
/// is 4). Sweeping every byte index hits the mid-codepoint indices the old
/// `&line[..cursor_col]` slice would have panicked on.
const METHOD_NAME_MULTIBYTE_LINES: &[&str] = &[
    "Modèl::wher",
    "$café->ba",
    "App\\Modèls\\Café::",
    "🎉::find",
    "$🎉->save",
];

#[test]
fn detect_method_name_position_never_panics_on_any_byte_index() {
    use laravel_lsp::method_name_completion::detect_method_name_position;
    // Every byte index, including the mid-codepoint ones. A completing test is
    // the assertion: the guard returns `None` rather than slicing into a panic.
    for line in METHOD_NAME_MULTIBYTE_LINES {
        for byte in 0..=line.len() {
            let _ = detect_method_name_position(line, byte);
        }
    }
}

#[test]
fn detect_method_name_position_extracts_receiver_past_multibyte_char() {
    use laravel_lsp::method_name_completion::{detect_method_name_position, MethodNameContext};
    use laravel_lsp::query_chain::char_col_to_byte_offset;

    // Mirror the live path: `try_method_name_completion` converts the LSP
    // `position.character` code-point column to a byte offset before calling
    // `detect_method_name_position`. `Modèl::wher` has an `è` (2 bytes) before
    // the `::`, so the column (11) and the byte offset (12) diverge — passing
    // the raw column would slice mid-name and read the wrong receiver.
    let line = "Modèl::wher";
    let col = line.chars().count(); // cursor at end-of-line
    assert_eq!(col, 11);
    let cursor = char_col_to_byte_offset(line, col);
    assert_eq!(cursor, 12); // one extra byte for `è`
    let ctx = detect_method_name_position(line, cursor).expect("static `::` position");
    assert_eq!(
        ctx,
        MethodNameContext::Static {
            receiver: "Modèl".to_string()
        }
    );
}

#[test]
fn detect_method_name_position_detects_instance_past_multibyte_char() {
    use laravel_lsp::method_name_completion::{detect_method_name_position, MethodNameContext};
    use laravel_lsp::query_chain::char_col_to_byte_offset;

    // `$café->ba` — the `é` (2 bytes) sits before the `->`. Converting the
    // code-point column lands the slice on a boundary so the `->` is seen.
    let line = "$café->ba";
    let cursor = char_col_to_byte_offset(line, line.chars().count());
    let ctx = detect_method_name_position(line, cursor).expect("instance `->` position");
    assert_eq!(ctx, MethodNameContext::Instance);
}

// ---- display-value extraction: no truncation, at any length --------------
//
// `extract_translation_value` and `extract_config_value` both used to
// truncate their hover/completion display value with a byte slice
// (`&s[..47]`), which panicked whenever a multibyte character straddled
// index 47 — reachable via any root `lang/` translation value over 50 bytes
// with a multibyte char at the boundary, not just an exotic namespaced-
// catalogue case. #319 replaced that with the shared, char-safe
// `display_truncate::truncate_for_display` at a 200-char limit.
//
// #326 moved the cut out of these two functions entirely: the completion
// list line and the documentation panel want different budgets, so each
// render site now truncates the full value itself (see
// `completion_display`). The char-boundary property is still pinned — at
// the render sites, in `completion_display::tests` — while these four
// assert the property that replaced it here: **whatever the length, and
// whatever the encoding, the value comes back exactly as written**. An
// inequality would pass with truncation quietly reinstated, so these are
// exact-equality against multibyte inputs well past the old 200 limit.
//
// The four names below are kept verbatim from #319 so this coverage stays
// traceable to the panic it guards, even though what they now assert is the
// *absence* of a cut rather than a safe one.
//
// `extract_translation_value` moved into `salsa_impl` when translation reads
// were routed through Salsa (issue #293); it is the same function, and this
// coverage follows it rather than being dropped.

#[test]
fn translation_value_truncation_is_char_boundary_safe() {
    // 199 ASCII chars, then a two-byte 'č', then padding well past the old
    // 200-char limit — the cut point the byte slice used to panic on.
    let value: String = "a".repeat(199) + "č" + &"é".repeat(50);
    let line = format!("'key' => '{}',", value);
    let display = laravel_lsp::salsa_impl::extract_translation_value(&line);
    assert_eq!(display, value, "the extractor must not truncate at all");
}

#[test]
fn translation_value_under_two_hundred_chars_is_not_truncated() {
    // A 30-char/60-byte Czech string used to get truncated (and could
    // panic) under the old byte-slice/50-char threshold.
    let value = "č".repeat(30);
    let line = format!("'key' => '{}',", value);
    let display = laravel_lsp::salsa_impl::extract_translation_value(&line);
    assert_eq!(display, value);
}

#[test]
fn config_value_truncation_is_char_boundary_safe() {
    let value: String = "a".repeat(199) + "ü" + &"ö".repeat(50);
    let line = format!("'key' => '{}',", value);
    let env_vars = std::collections::HashMap::new();
    let display = LaravelLanguageServer::extract_config_value(&line, &env_vars);
    assert_eq!(display, value, "the extractor must not truncate at all");
}

#[test]
fn config_value_under_two_hundred_chars_is_not_truncated() {
    let value = "ü".repeat(30);
    let line = format!("'key' => '{}',", value);
    let env_vars = std::collections::HashMap::new();
    let display = LaravelLanguageServer::extract_config_value(&line, &env_vars);
    assert_eq!(display, value);
}

#[test]
fn a_five_thousand_char_translation_value_survives_extraction_whole() {
    let value = "ž".repeat(5_000);
    let line = format!("'key' => '{}',", value);
    assert_eq!(
        laravel_lsp::salsa_impl::extract_translation_value(&line),
        value
    );
}

#[test]
fn a_five_thousand_char_config_value_survives_extraction_whole() {
    let value = "ö".repeat(5_000);
    let line = format!("'key' => '{}',", value);
    let env_vars = std::collections::HashMap::new();
    assert_eq!(
        LaravelLanguageServer::extract_config_value(&line, &env_vars),
        value
    );
}
