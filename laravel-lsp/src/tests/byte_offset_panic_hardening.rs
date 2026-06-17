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
