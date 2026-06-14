//! Tests for the baseline `extract_all_php_patterns` flow across the
//! canonical Laravel helpers (`view()`, `env()`, `config()`,
//! `Route::middleware()`, etc.).

use super::super::*;
use crate::parser::{language_php, parse_php};

#[test]
fn test_extract_all_php_patterns_views() {
    let php_code = r#"<?php
    return view('users.profile');
    Route::view('/home', 'welcome');
    echo view("admin.dashboard");
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    assert_eq!(patterns.views.len(), 3, "Should find 3 view calls");

    let view_names: Vec<&str> = patterns.views.iter().map(|m| m.view_name).collect();
    assert!(view_names.contains(&"users.profile"));
    assert!(view_names.contains(&"welcome"));
    assert!(view_names.contains(&"admin.dashboard"));

    let welcome = patterns
        .views
        .iter()
        .find(|v| v.view_name == "welcome")
        .unwrap();
    assert!(
        welcome.is_route_view,
        "Route::view() should set is_route_view=true"
    );

    let users = patterns
        .views
        .iter()
        .find(|v| v.view_name == "users.profile")
        .unwrap();
    assert!(
        !users.is_route_view,
        "view() should set is_route_view=false"
    );
}

#[test]
fn test_extract_all_php_patterns_env() {
    let php_code = r#"<?php
    $name = env('APP_NAME', 'Laravel');
    $debug = env("APP_DEBUG");
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    assert_eq!(patterns.env_calls.len(), 2, "Should find 2 env calls");
    assert_eq!(patterns.env_calls[0].var_name, "APP_NAME");
    assert_eq!(patterns.env_calls[1].var_name, "APP_DEBUG");
}

#[test]
fn test_extract_all_php_patterns_middleware() {
    let php_code = r#"<?php
    Route::middleware('auth')->group(function () {});
    Route::middleware(['auth', 'verified'])->get('/dashboard');
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    let middleware_names: Vec<&str> = patterns
        .middleware_calls
        .iter()
        .map(|m| m.middleware_name)
        .collect();

    assert!(
        middleware_names.contains(&"auth"),
        "Should find 'auth' middleware"
    );
    assert!(
        middleware_names.contains(&"verified"),
        "Should find 'verified' middleware"
    );
}

#[test]
fn test_extract_helper_identifiers_fires_for_each_curated_helper() {
    // Every curated helper's NAME token is captured — including arg-less forms
    // (`auth()`, `app()`) and string-arg forms (`route('home')`).
    let php_code = r#"<?php
    route('home');
    view('welcome');
    config('app.name');
    auth();
    app('cache');
    session('key');
    cache('users');
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    let names: Vec<&str> = patterns.helper_identifiers.iter().map(|h| h.name).collect();

    for helper in ["route", "view", "config", "auth", "app", "session", "cache"] {
        assert!(
            names.contains(&helper),
            "Should capture the `{helper}` helper identifier, got {names:?}"
        );
    }
    assert_eq!(
        patterns.helper_identifiers.len(),
        7,
        "Exactly the seven curated helpers, got {names:?}"
    );
}

#[test]
fn test_extract_helper_identifiers_ignores_non_curated_helpers() {
    // `bcrypt`/`abort` are real Laravel helpers but outside the curated set —
    // Intelephense owns them, so we must not capture them (the dedup policy).
    let php_code = r#"<?php
    bcrypt('secret');
    abort(404);
    collect([1, 2, 3]);
    str('x')->upper();
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    assert!(
        patterns.helper_identifiers.is_empty(),
        "Non-curated helpers must not be captured, got {:?}",
        patterns
            .helper_identifiers
            .iter()
            .map(|h| h.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_extract_helper_identifiers_skips_method_and_static_calls() {
    // Only bare global calls match — `$obj->route()` (member call) and
    // `Router::route()` (static call) are different node kinds.
    let php_code = r#"<?php
    $router->route('home');
    Router::route('home');
    $app->config('x');
    "#;

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    assert!(
        patterns.helper_identifiers.is_empty(),
        "Method / static calls must not match the global-helper pattern, got {:?}",
        patterns
            .helper_identifiers
            .iter()
            .map(|h| h.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_extract_helper_identifier_position_is_the_name_span() {
    // The captured span is the identifier itself, not the string argument —
    // hover must fire on `route`, not on `'home'`.
    let php_code = "<?php\nroute('home');\n";

    let tree = parse_php(php_code).expect("Should parse PHP");
    let lang = language_php();
    let patterns =
        extract_all_php_patterns(&tree, php_code, &lang).expect("Should extract patterns");

    let route = patterns
        .helper_identifiers
        .iter()
        .find(|h| h.name == "route")
        .expect("route helper identifier");
    assert_eq!(route.row, 1, "on the second line (0-based)");
    assert_eq!(route.column, 0, "starts at column 0");
    assert_eq!(route.end_column, 5, "ends after the 5-char name `route`");
}
