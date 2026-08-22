//! Phase 1 reference tests. Each pattern kind gets at least one positive case
//! (parser-classified positions are returned) plus a shared negative case
//! (random PHP strings with the same shape but NOT in a classified position
//! are not returned).

use super::*;
use crate::salsa_impl::{
    BindingReferenceData, ComponentReferenceData, ConfigReferenceData, DirectiveReferenceData,
    EnvReferenceData, LivewireReferenceData, MiddlewareReferenceData, ParsedPatternsData,
    RouteReferenceData, TranslationReferenceData, ViewReferenceData,
};
use std::sync::Arc;

fn empty_patterns() -> ParsedPatternsData {
    ParsedPatternsData::default()
}

/// Build a `ParsedPatternsData` containing a single classified pattern at the
/// given position. Each helper wires `build_position_index()` so the test can
/// query via [`classify_pattern_at_cursor`].
fn with_view(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.views.push(Arc::new(ViewReferenceData {
        name: name.to_string(),
        line,
        column: col,
        end_column: col + name.len() as u32,
        is_route_view: false,
    }));
    p.build_position_index();
    p
}

fn with_route(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: name.to_string(),
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_config(key: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.config_refs.push(Arc::new(ConfigReferenceData {
        key: key.to_string(),
        line,
        column: col,
        end_column: col + key.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_translation(key: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.translation_refs.push(Arc::new(TranslationReferenceData {
        key: key.to_string(),
        line,
        column: col,
        end_column: col + key.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_env(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.env_refs.push(Arc::new(EnvReferenceData {
        name: name.to_string(),
        has_fallback: false,
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_component(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.components.push(Arc::new(ComponentReferenceData {
        name: name.to_string(),
        tag_name: name.to_string(),
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_livewire(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.livewire_refs.push(Arc::new(LivewireReferenceData {
        name: name.to_string(),
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_middleware(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.middleware_refs.push(Arc::new(MiddlewareReferenceData {
        name: name.to_string(),
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

fn with_binding(name: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.binding_refs.push(Arc::new(BindingReferenceData {
        name: name.to_string(),
        is_class_reference: false,
        line,
        column: col,
        end_column: col + name.len() as u32,
    }));
    p.build_position_index();
    p
}

#[test]
fn classifies_view_at_cursor() {
    let p = with_view("users.profile", 3, 10);
    let got = classify_pattern_at_cursor(&p, 3, 12);
    assert_eq!(got, Some(SymbolRef::View("users.profile".into())));
}

#[test]
fn classifies_route_at_cursor() {
    let p = with_route("users.index", 1, 7);
    let got = classify_pattern_at_cursor(&p, 1, 10);
    assert_eq!(got, Some(SymbolRef::Route("users.index".into())));
}

#[test]
fn classifies_config_at_cursor() {
    let p = with_config("app.name", 0, 8);
    let got = classify_pattern_at_cursor(&p, 0, 9);
    assert_eq!(got, Some(SymbolRef::Config("app.name".into())));
}

#[test]
fn classifies_translation_at_cursor() {
    let p = with_translation("auth.failed", 5, 4);
    let got = classify_pattern_at_cursor(&p, 5, 8);
    assert_eq!(got, Some(SymbolRef::Translation("auth.failed".into())));
}

#[test]
fn classifies_env_at_cursor() {
    let p = with_env("APP_KEY", 0, 0);
    let got = classify_pattern_at_cursor(&p, 0, 3);
    assert_eq!(got, Some(SymbolRef::Env("APP_KEY".into())));
}

#[test]
fn classifies_component_at_cursor() {
    let p = with_component("button", 2, 2);
    let got = classify_pattern_at_cursor(&p, 2, 4);
    assert_eq!(got, Some(SymbolRef::Component("button".into())));
}

#[test]
fn classifies_livewire_at_cursor() {
    let p = with_livewire("counter", 4, 6);
    let got = classify_pattern_at_cursor(&p, 4, 10);
    assert_eq!(got, Some(SymbolRef::Livewire("counter".into())));
}

#[test]
fn classifies_middleware_at_cursor() {
    let p = with_middleware("auth", 1, 16);
    let got = classify_pattern_at_cursor(&p, 1, 18);
    assert_eq!(got, Some(SymbolRef::Middleware("auth".into())));
}

#[test]
fn classifies_binding_at_cursor() {
    let p = with_binding("cache.store", 2, 4);
    let got = classify_pattern_at_cursor(&p, 2, 7);
    assert_eq!(got, Some(SymbolRef::Binding("cache.store".into())));
}

#[test]
fn classifies_include_directive_as_view() {
    let mut p = empty_patterns();
    let args = "('partials.header')".to_string();
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "include".to_string(),
        arguments: Some(args),
        line: 0,
        column: 0,
        end_column: 30,
        string_column: 10,
        string_end_column: 27,
    }));
    p.build_position_index();
    let got = classify_pattern_at_cursor(&p, 0, 5);
    assert_eq!(got, Some(SymbolRef::View("partials.header".into())));
}

#[test]
fn classifies_each_directive_first_arg_as_view() {
    let mut p = empty_patterns();
    // @each('view.row', $rows, 'row') — only the first argument is the view.
    let args = "('view.row', $rows, 'row')".to_string();
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "each".to_string(),
        arguments: Some(args),
        line: 0,
        column: 0,
        end_column: 40,
        string_column: 7,
        string_end_column: 15,
    }));
    p.build_position_index();
    let got = classify_pattern_at_cursor(&p, 0, 3);
    assert_eq!(got, Some(SymbolRef::View("view.row".into())));
}

#[test]
fn returns_none_when_cursor_misses_all_patterns() {
    // Empty pattern set + any cursor position → nothing classified.
    // This is the negative-test guarantee: a same-shape string in source code
    // that the parser hasn't classified is NOT returned as a reference.
    let p = empty_patterns();
    assert_eq!(classify_pattern_at_cursor(&p, 0, 0), None);
    assert_eq!(classify_pattern_at_cursor(&p, 99, 99), None);
}

#[test]
fn returns_none_when_cursor_outside_classified_range() {
    // Parser classified `view('users.profile')` at column 10-22. Cursor at 5
    // sits in an unrelated PHP string with the same shape — must NOT classify.
    let p = with_view("users.profile", 3, 10);
    assert_eq!(classify_pattern_at_cursor(&p, 3, 5), None);
    assert_eq!(classify_pattern_at_cursor(&p, 3, 50), None);
    assert_eq!(classify_pattern_at_cursor(&p, 4, 12), None);
}

#[test]
fn symbol_ref_to_data_round_trip() {
    // Sanity-check the data-transfer conversion since the Salsa actor sees
    // only the *Data variant.
    let s = SymbolRef::Route("home".into());
    assert!(matches!(s.to_data(), SymbolRefData::Route(n) if n == "home"));
    assert_eq!(s.name(), "home");
}

// ---------------------------------------------------------------------------
// `@use` — a Blade file's class imports. Classification runs through the
// `class_refs` position index, not through the directive entry: `@use` has no
// directive entry, so one route serves Blade and PHP alike.
// ---------------------------------------------------------------------------

/// End-to-end from real Blade source, so the directive scan, the span
/// computation, and the position index are all exercised together.
fn classify_in_blade(src: &str, line: u32, column: u32) -> Option<SymbolRef> {
    let path = std::path::PathBuf::from("/fixture/resources/views/page.blade.php");
    let data = crate::pattern_indexer::parse_owned(&path, src);
    classify_pattern_at_cursor(&data, line, column)
}

#[test]
fn blade_use_classifies_as_a_class_symbol() {
    let src = "@use(\"App\\Support\\Reader\\VerseMarkerResolver\")\n";
    let col = src.find("Reader").expect("offset") as u32;

    assert_eq!(
        classify_in_blade(src, 0, col),
        Some(SymbolRef::Class(
            r"App\Support\Reader\VerseMarkerResolver".into()
        ))
    );
}

#[test]
fn blade_use_with_an_alias_classifies_on_the_imported_class() {
    let src = "@use('App\\Models\\Flight', 'FlightModel')\n";
    let col = src.find("Flight'").expect("offset") as u32;

    assert_eq!(
        classify_in_blade(src, 0, col),
        Some(SymbolRef::Class(r"App\Models\Flight".into())),
        "the alias names the binding, not the class being referenced"
    );
}

/// Each member of a group import classifies as its own class.
#[test]
fn blade_group_import_classifies_per_member() {
    let src = "@use('App\\Models\\{Flight, Airport}')\n";

    assert_eq!(
        classify_in_blade(src, 0, src.find("Flight").expect("offset") as u32),
        Some(SymbolRef::Class(r"App\Models\Flight".into()))
    );
    assert_eq!(
        classify_in_blade(src, 0, src.find("Airport").expect("offset") as u32),
        Some(SymbolRef::Class(r"App\Models\Airport".into()))
    );
}

#[test]
fn blade_function_import_classifies_as_nothing() {
    let src = "@use('function App\\Helpers\\fmt')\n";
    let col = src.find("Helpers").expect("offset") as u32;

    assert_eq!(classify_in_blade(src, 0, col), None, "binds no class");
}

/// A `@use` must never fall through to the view classifier — a class FQCN is
/// not a dotted view name and resolving it as one would navigate somewhere
/// wrong.
#[test]
fn blade_use_never_classifies_as_a_view() {
    let src = "@use('App\\Models\\Flight')\n";
    let got = classify_in_blade(src, 0, src.find("Models").expect("offset") as u32);

    assert!(!matches!(got, Some(SymbolRef::View(_))), "got {got:?}");
}

// ---------------------------------------------------------------------------
// Class import sites — PHP `use` resolves through the position index; a Blade
// `@use` resolves through its enclosing directive. Both must answer `Class`.
// ---------------------------------------------------------------------------

fn with_class_ref(fqcn: &str, line: u32, col: u32) -> ParsedPatternsData {
    let mut p = empty_patterns();
    p.class_refs
        .push(Arc::new(crate::salsa_impl::ClassReferenceData {
            name: fqcn.to_string(),
            line,
            column: col,
            end_column: col + fqcn.len() as u32,
        }));
    p.build_position_index();
    p
}

#[test]
fn php_use_statement_classifies_as_a_class_symbol() {
    let p = with_class_ref(r"App\Models\Flight", 3, 4);

    assert_eq!(
        classify_pattern_at_cursor(&p, 3, 8),
        Some(SymbolRef::Class(r"App\Models\Flight".into()))
    );
}

#[test]
fn a_cursor_off_the_import_span_classifies_as_nothing() {
    let p = with_class_ref(r"App\Models\Flight", 3, 4);

    assert_eq!(classify_pattern_at_cursor(&p, 3, 2), None);
    assert_eq!(classify_pattern_at_cursor(&p, 4, 8), None);
}

/// `@use` must contribute no directive entry to the position index. It spans
/// the whole call and starts at column 0, so it would win `find_at_position`
/// for every cursor inside the parentheses and shadow the class ref — and a
/// single directive entry cannot tell the members of a group import apart.
#[test]
fn use_directive_does_not_shadow_the_class_ref_in_the_position_index() {
    let args = r#"('App\Models\Flight')"#;
    let mut p = empty_patterns();
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "use".to_string(),
        arguments: Some(args.to_string()),
        line: 0,
        column: 0,
        end_column: 4 + args.len() as u32,
        string_column: 6,
        string_end_column: 6 + r"App\Models\Flight".len() as u32,
    }));
    p.class_refs
        .push(Arc::new(crate::salsa_impl::ClassReferenceData {
            name: r"App\Models\Flight".to_string(),
            line: 0,
            column: 6,
            end_column: 6 + r"App\Models\Flight".len() as u32,
        }));
    p.build_position_index();

    assert_eq!(
        classify_pattern_at_cursor(&p, 0, 10),
        Some(SymbolRef::Class(r"App\Models\Flight".into())),
        "whichever entry wins, the classified symbol is the same"
    );
}
