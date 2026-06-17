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
    // slice would have sliced `&line[..7]` = `🎉 $us` and returned "us"; the
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
    // As a *column*, 9 is past the 11 code points only sometimes; either way the
    // call must not panic. We assert it simply returns (Some or None).
    let _ = LaravelLanguageServer::get_config_call_context(line, 9);
}
