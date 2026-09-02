use super::*;

#[test]
fn extracts_single_quoted_route_name() {
    let src = r#"<?php
Route::get('/login', [LoginController::class, 'show'])->name('login');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    assert_eq!(results.len(), 1);
    let (name, def) = &results[0];
    assert_eq!(name.as_deref(), Some("login"));
    assert_eq!(def.line, 1);
    assert_eq!(def.priority, PRIORITY_APP);
}

#[test]
fn extracts_double_quoted_route_name() {
    let src = r#"<?php
Route::get('/dashboard')->name("dashboard.index");
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("dashboard.index"));
}

#[test]
fn extracts_multiple_routes_per_file() {
    let src = r#"<?php
Route::get('/login')->name('login');
Route::post('/logout')->name('logout');
Route::get('/register')->name('register');
"#;
    let path = PathBuf::from("/fake/routes/auth.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);

    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["login", "logout", "register"]);
}

#[test]
fn tolerates_whitespace_in_call() {
    let src = "<?php\nRoute::get('/x')->name ( 'spaced' );\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("spaced"));
}

#[test]
fn skips_variable_route_names() {
    let src = "<?php\nRoute::get('/x')->name($name);\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert!(results.is_empty(), "should skip variable name arguments");
}

#[test]
fn extracts_routes_inside_macro_body() {
    // Models the Laravel UI AuthRouteMethods pattern.
    let src = r#"<?php
class AuthRouteMethods
{
public function auth()
{
    return function () {
        $this->get('login', ...)->name('login');
        $this->post('logout', ...)->name('logout');
    };
}
}
"#;
    let path = PathBuf::from("/fake/vendor/laravel/ui/src/AuthRouteMethods.php");
    let results = extract_named_routes(src, &path, PRIORITY_PACKAGE, &[]);

    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["login", "logout"]);
}

#[test]
fn route_index_resolves_priority_collision() {
    let mut idx = RouteIndex::new();

    idx.insert(
        "login".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/vendor/laravel/fortify/routes/routes.php"),
            line: 5,
            column: 0,
            end_column: 10,
            priority: PRIORITY_PACKAGE,
            method: None,
            uri: None,
            action: None,
        },
    );
    idx.insert(
        "login".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/routes/auth.php"),
            line: 12,
            column: 0,
            end_column: 10,
            priority: PRIORITY_APP,
            method: None,
            uri: None,
            action: None,
        },
    );

    let def = idx.get("login").expect("should resolve");
    assert!(
        def.file.ends_with("routes/auth.php"),
        "app should win over package"
    );
    assert_eq!(def.priority, PRIORITY_APP);
}

#[test]
fn route_index_keeps_lower_when_higher_does_not_redefine() {
    let mut idx = RouteIndex::new();
    idx.insert(
        "horizon.index".into(),
        RouteDefinition {
            file: PathBuf::from("/fake/vendor/laravel/horizon/routes/web.php"),
            line: 3,
            column: 0,
            end_column: 10,
            priority: PRIORITY_PACKAGE,
            method: None,
            uri: None,
            action: None,
        },
    );
    let def = idx
        .get("horizon.index")
        .expect("package route should index");
    assert_eq!(def.priority, PRIORITY_PACKAGE);
}

#[test]
fn file_registers_named_routes_detects_macro_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("AuthRouteMethods.php");
    let src = "<?php\nclass X {\n  public function auth() {\n    return function () {\n      $this->get('login')->name('login');\n    };\n  }\n}\n";
    std::fs::write(&path, src).unwrap();

    assert!(file_registers_named_routes(&path));
}

#[test]
fn content_registers_named_routes_via_verb_method_call() {
    // Laravel UI's AuthRouteMethods style — uses `$this->get(...)->name(...)`
    // with no `Route::` token at all.
    let src = r#"<?php
$this->get('login')->name('login');
$this->post('logout')->name('logout');
"#;
    assert!(content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_via_route_facade() {
    let src = "<?php\nRoute::get('/x')->name('x');\n";
    assert!(content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_rejects_no_name_call() {
    // `->name(` is required regardless of other tokens.
    let src = "<?php\nRoute::get('/x', [Controller::class, 'index']);\n";
    assert!(!content_registers_named_routes(src));
}

#[test]
fn content_registers_named_routes_rejects_only_name_calls() {
    // `->name(` alone (e.g., builder DSL with no routing context) is not
    // sufficient. We require some route-shape token.
    let src = "<?php\n$builder->name('foo');\n";
    assert!(!content_registers_named_routes(src));
}

#[test]
fn file_registers_named_routes_rejects_unrelated_php() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Plain.php");
    std::fs::write(&path, "<?php\nclass Plain { public $name = 'x'; }\n").unwrap();

    assert!(!file_registers_named_routes(&path));
}

#[test]
fn is_under_routes_dir_recognizes_package_layout() {
    assert!(is_under_routes_dir(Path::new(
        "/project/vendor/laravel/fortify/routes/routes.php"
    )));
    assert!(is_under_routes_dir(Path::new("/project/routes/auth.php")));
    assert!(!is_under_routes_dir(Path::new(
        "/project/vendor/foo/src/Http/Controllers.php"
    )));
}

#[test]
fn priority_for_vendor_path_distinguishes_framework() {
    assert_eq!(
        priority_for_vendor_path(Path::new(
            "/project/vendor/laravel/framework/src/Illuminate/Auth.php"
        )),
        PRIORITY_FRAMEWORK
    );
    assert_eq!(
        priority_for_vendor_path(Path::new(
            "/project/vendor/laravel/fortify/routes/routes.php"
        )),
        PRIORITY_PACKAGE
    );
}

// ============================================================================
// Route metadata extraction (method / URI / action)
// ============================================================================

/// Convenience: run a full extraction over `src` and return the first
/// `RouteDefinition`. Tests are about extraction shape, not file paths.
fn first_def(src: &str) -> RouteDefinition {
    let path = PathBuf::from("/fake/routes/web.php");
    let mut results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert!(
        !results.is_empty(),
        "expected at least one route definition"
    );
    results.remove(0).1
}

#[test]
fn metadata_extracts_array_action() {
    let def = first_def(
        "<?php\nRoute::get('/users', [UserController::class, 'show'])->name('users.show');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/users"));
    assert_eq!(def.action.as_deref(), Some("UserController@show"));
}

#[test]
fn metadata_extracts_legacy_string_action() {
    let def =
        first_def("<?php\nRoute::post('/login', 'LoginController@authenticate')->name('login');\n");
    assert_eq!(def.method.as_deref(), Some("post"));
    assert_eq!(def.uri.as_deref(), Some("/login"));
    assert_eq!(def.action.as_deref(), Some("LoginController@authenticate"));
}

#[test]
fn metadata_extracts_invokable_action() {
    let def =
        first_def("<?php\nRoute::get('/dashboard', DashboardController::class)->name('dash');\n");
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/dashboard"));
    assert_eq!(def.action.as_deref(), Some("DashboardController"));
}

#[test]
fn metadata_extracts_closure_action() {
    let def = first_def(
        "<?php\nRoute::get('/closure', function () { return 'hi'; })->name('closure');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/closure"));
    assert_eq!(def.action.as_deref(), Some("Closure"));
}

#[test]
fn metadata_extracts_arrow_function_action() {
    let def = first_def("<?php\nRoute::get('/arrow', fn() => 'hi')->name('arrow');\n");
    assert_eq!(def.action.as_deref(), Some("Closure"));
}

#[test]
fn metadata_handles_namespaced_controller_in_array() {
    let def = first_def(
        "<?php\nRoute::get('/x', [\\App\\Http\\Controllers\\UserController::class, 'index'])->name('x');\n",
    );
    assert_eq!(def.action.as_deref(), Some("UserController@index"));
}

#[test]
fn metadata_handles_view_route_without_action() {
    let def = first_def("<?php\nRoute::view('/static', 'view.name')->name('view');\n");
    assert_eq!(def.method.as_deref(), Some("view"));
    assert_eq!(def.uri.as_deref(), Some("/static"));
    // 'view.name' is the second argument — but in Route::view, it's a view name,
    // not an action. We pass it through as a string for now; consumers can decide.
    assert_eq!(def.action.as_deref(), Some("view.name"));
}

#[test]
fn metadata_handles_redirect_route_without_action() {
    let def = first_def("<?php\nRoute::redirect('/from', '/to')->name('redir');\n");
    assert_eq!(def.method.as_deref(), Some("redirect"));
    assert_eq!(def.uri.as_deref(), Some("/from"));
    assert_eq!(def.action.as_deref(), Some("/to"));
}

#[test]
fn metadata_handles_chained_middleware_before_name() {
    let def = first_def(
        "<?php\nRoute::get('/admin', [AdminController::class, 'index'])->middleware('auth')->name('admin');\n",
    );
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/admin"));
    assert_eq!(def.action.as_deref(), Some("AdminController@index"));
}

#[test]
fn metadata_handles_multiline_route_declaration() {
    let src = r#"<?php
Route::get(
    '/users/{user}',
    [UserController::class, 'show'],
)->name('users.show');
"#;
    let def = first_def(src);
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/users/{user}"));
    assert_eq!(def.action.as_deref(), Some("UserController@show"));
}

#[test]
fn metadata_handles_this_router_macro_style() {
    let src = r#"<?php
$this->get('login', [LoginController::class, 'show'])->name('login');
"#;
    let def = first_def(src);
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("login"));
    assert_eq!(def.action.as_deref(), Some("LoginController@show"));
}

#[test]
fn metadata_skips_unrelated_verb_calls_in_other_statements() {
    // Two statements; the first contains `Route::post(...)`, the second contains
    // `->name('x')`. They must not bleed into each other.
    let src = r#"<?php
Route::post('/wrong', [Wrong::class, 'wrong']);
Route::get('/right', [Right::class, 'right'])->name('right');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert_eq!(def.method.as_deref(), Some("get"));
    assert_eq!(def.uri.as_deref(), Some("/right"));
    assert_eq!(def.action.as_deref(), Some("Right@right"));
}

#[test]
fn metadata_returns_none_when_verb_call_missing() {
    // A `->name(` callsite without any verb call upstream — e.g. someone building
    // routes through a builder we don't recognise.
    let src = "<?php\n$builder->name('orphan');\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    // content_registers_named_routes filters this in the discovery pipeline, but
    // the raw extractor still surfaces the name with empty metadata.
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert!(def.method.is_none());
    assert!(def.uri.is_none());
    assert!(def.action.is_none());
}

#[test]
fn metadata_does_not_match_longer_verb_lookalike() {
    // `->getUser(...)` should not match the `get` verb. The extractor must
    // enforce a word boundary after the verb.
    let src = "<?php\n$obj->getUser('/x', SomeController::class)->name('user');\n";
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    assert_eq!(results.len(), 1);
    let def = &results[0].1;
    assert!(
        def.method.is_none(),
        "verb match must require a word boundary"
    );
}

// ============================================================================
// Route group name compositing
// ============================================================================

#[test]
fn route_group_chain_name_prefixes_child_route() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.users.index"]);
}

#[test]
fn route_group_array_as_prefixes_child_route() {
    let src = r#"<?php
Route::group(['as' => 'api.', 'prefix' => 'api'], function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.users.index"]);
}

#[test]
fn route_group_chain_handles_intervening_middleware_call() {
    // `->middleware(...)` sits between `->name('admin.')` and `->group(...)`.
    // The backward chain walk must step over middleware() and still find name().
    let src = r#"<?php
Route::name('admin.')->middleware(['auth', 'verified'])->group(function () {
    Route::get('/users', [UserController::class, 'index'])->name('users.index');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.users.index"]);
}

#[test]
fn route_group_nested_two_levels() {
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::name('v1.')->group(function () {
        Route::get('/users', [UserController::class, 'index'])->name('users.index');
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.v1.users.index"]);
}

#[test]
fn route_group_nested_five_levels_real_world() {
    // Modelled on the case Mike reported during manual testing:
    // `decision-cloud.lead-settings.management-systems.decisioner-settings.edit`
    let src = r#"<?php
Route::name('decision-cloud.')->group(function () {
    Route::name('lead-settings.')->group(function () {
        Route::name('management-systems.')->group(function () {
            Route::name('decisioner-settings.')->group(function () {
                Route::get('/edit', [Controller::class, 'edit'])->name('edit');
            });
        });
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(
        names,
        vec!["decision-cloud.lead-settings.management-systems.decisioner-settings.edit"]
    );
}

#[test]
fn route_group_mixed_chain_and_array_styles() {
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::group(['as' => 'v1.'], function () {
        Route::get('/users', ...)->name('users.index');
    });
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["api.v1.users.index"]);
}

#[test]
fn route_group_does_not_prefix_routes_outside_body() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users')->name('users.index');
});
Route::get('/login')->name('login');
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(
        names,
        vec!["admin.users.index", "login"],
        "route outside the group must NOT be prefixed"
    );
}

#[test]
fn route_group_without_prefix_does_not_contribute() {
    // A group with no `->name(...)` and no array `'as'` — it's a valid
    // grouping but doesn't affect child route names.
    let src = r#"<?php
Route::middleware('auth')->group(function () {
    Route::get('/dashboard')->name('dashboard');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["dashboard"]);
}

#[test]
fn route_group_sibling_groups_do_not_cross_pollinate() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/x')->name('x');
});
Route::name('api.')->group(function () {
    Route::get('/y')->name('y');
});
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let names: Vec<&str> = results.iter().filter_map(|(n, _)| n.as_deref()).collect();
    assert_eq!(names, vec!["admin.x", "api.y"]);
}

// ============================================================================
// External-file route group loads (issue #43)
// ============================================================================
//
// `Route::as('admin.')->group(base_path('routes/web_backstage.php'))` loads an
// external file and applies the group's name prefix to every route inside it.
// These tests exercise `build_route_index` end-to-end over a temp directory,
// because the load resolution + transitive propagation only happen there.

use tempfile::TempDir;

/// Build a `routes/` tree from `(relative_path, contents)` pairs under a fresh
/// temp dir, then run `build_route_index` over the project routes. Returns the
/// temp dir (kept alive) and the resulting index.
fn index_routes(files: &[(&str, &str)]) -> (TempDir, RouteIndex) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut route_files = Vec::new();
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        route_files.push(RouteFile {
            path,
            priority: PRIORITY_APP,
        });
    }
    let index = build_route_index(root, &route_files);
    (tmp, index)
}

#[test]
fn external_group_base_path_applies_prefix() {
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    // Both the prefixed AND the bare name must be present — the loaded file is
    // also scanned directly, so its bare leaf stays in the index.
    assert!(
        index.get("admin.patient.index").is_some(),
        "external group prefix must produce admin.patient.index"
    );
    assert!(
        index.get("patient.index").is_some(),
        "bare leaf name must remain in the index"
    );
}

#[test]
fn external_group_dir_concat_applies_prefix() {
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(__DIR__ . '/web_backstage.php');\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.index").is_some(),
        "__DIR__ . '/file' load form must resolve and apply prefix"
    );
}

#[test]
fn external_group_nested_closure_prefix_combines() {
    // An enclosing CLOSURE group `admin.` wraps a `->as('v1.')->group($file)`
    // external load. The loaded file's routes get `admin.v1.`.
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(function () {\n    Route::as('v1.')->group(base_path('routes/web_backstage.php'));\n});\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.v1.patient.index").is_some(),
        "enclosing closure prefix must combine with the load's own prefix"
    );
}

#[test]
fn external_group_transitive_chain_of_loads() {
    // a.php --admin.--> b.php --patient.--> c.php
    let (_tmp, index) = index_routes(&[
        (
            "routes/a.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::as('patient.')->group(base_path('routes/c.php'));\n",
        ),
        (
            "routes/c.php",
            "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.edit").is_some(),
        "transitive load chain must accumulate prefixes"
    );
}

#[test]
fn external_group_array_form_applies_prefix() {
    // Array form: the file path is the SECOND argument; the first is the array.
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::group(['as' => 'admin.'], base_path('routes/web_backstage.php'));\n",
        ),
        (
            "routes/web_backstage.php",
            "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
        ),
    ]);

    assert!(
        index.get("admin.patient.index").is_some(),
        "array-form external group must apply the 'as' prefix"
    );
}

#[test]
fn external_prefixes_for_file_returns_inherited_prefix() {
    // `routes/web.php` loads `routes/web_backstage.php` via an `admin.` group.
    // `external_prefixes_for_file` must return `["", "admin."]` for the loaded
    // file — discovered through `discover_route_files` + the load graph.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("web.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
    )
    .unwrap();
    let backstage = routes.join("web_backstage.php");
    std::fs::write(
        &backstage,
        "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &backstage);
    assert!(
        prefixes.contains(&String::new()),
        "the empty prefix is always present (file scanned directly)"
    );
    assert!(
        prefixes.contains(&"admin.".to_string()),
        "the loaded file inherits the loader group's `admin.` prefix, got {prefixes:?}"
    );
    assert_eq!(
        prefixes.len(),
        2,
        "exactly `\"\"` and `admin.`: {prefixes:?}"
    );
}

#[test]
fn external_prefixes_for_file_loader_has_only_empty() {
    // The LOADER file (web.php) inherits no external prefix itself — only the
    // always-present empty prefix.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    let web = routes.join("web.php");
    std::fs::write(
        &web,
        "<?php\nRoute::as('admin.')->group(base_path('routes/web_backstage.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("web_backstage.php"),
        "<?php\nRoute::get('/patients', fn () => 'ok')->name('patient.index');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &web);
    assert_eq!(
        prefixes,
        vec![String::new()],
        "loader has no inherited prefix"
    );
}

#[test]
fn external_prefixes_for_file_transitive_chain_accumulates() {
    // a.php --admin.--> b.php --patient.--> c.php. The deepest file inherits the
    // accumulated `admin.patient.` prefix.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("a.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("b.php"),
        "<?php\nRoute::as('patient.')->group(base_path('routes/c.php'));\n",
    )
    .unwrap();
    let c = routes.join("c.php");
    std::fs::write(
        &c,
        "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
    )
    .unwrap();

    let prefixes = external_prefixes_for_file(root, &c);
    assert!(prefixes.contains(&String::new()));
    assert!(
        prefixes.contains(&"admin.patient.".to_string()),
        "transitive chain must accumulate: {prefixes:?}"
    );
}

#[test]
fn external_prefixes_for_file_agrees_with_the_built_index() {
    // The premise the whole of issue #80 rests on: every request handler now
    // answers external-prefix questions from `RouteIndex::external_prefixes`
    // instead of calling `external_prefixes_for_file`. That is only sound if
    // the two agree — so compare them directly, file by file, on a project
    // exercising the shapes the propagation has to get right:
    //
    //   a.php   — loader, inherits nothing
    //   b.php   — loaded with `admin.`, and itself a loader
    //   c.php   — loaded transitively, inherits the accumulated `admin.blog.`
    //   solo.php — a route file no load edge ever reaches
    //
    // Comparing only the prefixed files would pass for a cache that returned
    // `[""]` for everything reachable *and* everything else, so the unprefixed
    // files are part of the comparison too.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("a.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("b.php"),
        "<?php\nRoute::get('/dash', fn () => 'ok')->name('dash');\n\
         Route::as('blog.')->group(base_path('routes/c.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("c.php"),
        "<?php\nRoute::get('/posts', fn () => 'ok')->name('posts.index');\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("solo.php"),
        "<?php\nRoute::get('/solo', fn () => 'ok')->name('solo');\n",
    )
    .unwrap();

    let index = build_route_index(root, &discover_route_files(root));

    // Compared as SEQUENCES, not as sets: `compute_effective_prefixes` sorts,
    // so the two sides must agree element for element, in order. (Before that
    // sort this comparison was flaky — see
    // `external_prefixes_are_ordered_deterministically`.)
    for leaf in ["a.php", "b.php", "c.php", "solo.php"] {
        let file = routes.join(leaf);
        assert_eq!(
            index.external_prefixes_for(&file),
            external_prefixes_for_file(root, &file),
            "the cached index and the uncached walk must agree for {leaf}"
        );
    }

    // Pin what "agree" is worth here: the comparison above would be vacuously
    // true if both sides answered `[""]` everywhere, so assert the fixture
    // really does carry an accumulated prefix through the index.
    assert!(
        index
            .external_prefixes_for(&routes.join("c.php"))
            .contains(&"admin.blog.".to_string()),
        "fixture check — the index must carry the transitive `admin.blog.` \
         prefix, otherwise the equality above proves nothing: {:?}",
        index.external_prefixes_for(&routes.join("c.php"))
    );
}

/// `admin.php` and `blog.php` both load `shared.php`, under different name
/// prefixes. That is the shape whose prefix order used to vary run to run.
fn seed_two_loader_project(root: &Path) -> PathBuf {
    let routes = root.join("routes");
    std::fs::create_dir_all(&routes).unwrap();
    std::fs::write(
        routes.join("admin.php"),
        "<?php\nRoute::as('admin.')->group(base_path('routes/shared.php'));\n",
    )
    .unwrap();
    std::fs::write(
        routes.join("blog.php"),
        "<?php\nRoute::as('blog.')->group(base_path('routes/shared.php'));\n",
    )
    .unwrap();
    let shared = routes.join("shared.php");
    std::fs::write(
        &shared,
        "<?php\nRoute::get('/posts', fn () => 'ok')->name('posts.index');\n",
    )
    .unwrap();
    shared
}

#[test]
fn external_prefixes_are_ordered_deterministically() {
    // Regression: `compute_effective_prefixes` DFS'd a `HashSet` of start files
    // and pushed each reached prefix in visit order, so a file reachable under
    // two prefixes got them in an order set by the hasher's per-instance seed.
    // Rust seeds each `HashMap`/`HashSet` instance differently, so the order
    // changed between two calls *inside one process* — which is exactly what
    // `classify_with_decl_fallback` reads when it takes the first non-empty
    // prefix as a declaration's project-level name. Find-references and rename
    // could therefore anchor to `admin.posts.index` on one invocation and
    // `blog.posts.index` on the next, on an unedited project.
    //
    // Repeating the computation is the whole point: a single call cannot
    // observe instability, and each iteration builds fresh maps with fresh
    // seeds. 20 rounds made the unsorted version fail every time it was run.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let shared = seed_two_loader_project(root);

    let expected = vec![String::new(), "admin.".to_string(), "blog.".to_string()];

    for round in 0..20 {
        let index = build_route_index(root, &discover_route_files(root));
        assert_eq!(
            index.external_prefixes_for(&shared),
            expected,
            "round {round}: the built index must order prefixes `\"\"` first, \
             then lexicographically — an order that never varies between runs"
        );
        assert_eq!(
            external_prefixes_for_file(root, &shared),
            expected,
            "round {round}: the uncached walk must produce the same stable order"
        );
    }
}

#[test]
fn a_route_reachable_under_two_prefixes_resolves_to_a_stable_name() {
    // The consequence the sort exists for, stated in the terms a user sees.
    // `classify_with_decl_fallback` builds a declaration's project-level name
    // as `<first non-empty prefix><in-file name>`; with two loaders that first
    // element has to be the same on every call or find-references and rename
    // silently target different symbols on successive invocations.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let shared = seed_two_loader_project(root);

    let names: std::collections::HashSet<String> = (0..20)
        .map(|_| {
            let index = build_route_index(root, &discover_route_files(root));
            let primary = index
                .external_prefixes_for(&shared)
                .into_iter()
                .find(|p| !p.is_empty())
                .expect("a doubly-loaded file must inherit at least one prefix");
            format!("{primary}posts.index")
        })
        .collect();

    assert_eq!(
        names,
        std::collections::HashSet::from(["admin.posts.index".to_string()]),
        "20 rounds must all resolve to the one lexicographically-first name; \
         more than one entry here means rename would rewrite a different symbol \
         depending on when it ran: {names:?}"
    );
}

#[test]
fn external_group_cycle_is_guarded() {
    // a.php loads b.php; b.php loads a.php. Must terminate and still index
    // both files' bare names without blowing up.
    let (_tmp, index) = index_routes(&[
        (
            "routes/a.php",
            "<?php\nRoute::as('x.')->group(base_path('routes/b.php'));\nRoute::get('/a', fn () => 'ok')->name('a');\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::as('y.')->group(base_path('routes/a.php'));\nRoute::get('/b', fn () => 'ok')->name('b');\n",
        ),
    ]);

    assert!(index.get("a").is_some(), "bare name a must index");
    assert!(index.get("b").is_some(), "bare name b must index");
    // One hop each direction is fine.
    assert!(index.get("x.b").is_some(), "a loads b with prefix x.");
    assert!(index.get("y.a").is_some(), "b loads a with prefix y.");
}

/// Like [`index_routes`] but lets callers place files at ARBITRARY relative
/// paths (e.g. `app/Custom/admin.php`) while still seeding the working set only
/// with the `routes/` entry points. Files NOT under `routes/` are written to
/// disk but NOT added to `route_files`, so they can only enter the index via a
/// transitive `->group(<path>)` load — exactly the issue #43 scenario.
fn index_with_entrypoints(files: &[(&str, &str)], entrypoints: &[&str]) -> (TempDir, RouteIndex) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut route_files = Vec::new();
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        if entrypoints.contains(rel) {
            route_files.push(RouteFile {
                path,
                priority: PRIORITY_APP,
            });
        }
    }
    let index = build_route_index(root, &route_files);
    (tmp, index)
}

#[test]
fn external_group_indexes_file_outside_routes_dir() {
    // The loaded file lives OUTSIDE routes/ (app/Custom/admin.php) and is NOT a
    // discovered entry point — it can only enter the index transitively.
    let (_tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/web.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/admin.php'));\n",
            ),
            (
                "app/Custom/admin.php",
                "<?php\nRoute::get('/dashboard', fn () => 'ok')->name('dash');\nRoute::resource('widgets', WidgetController::class);\n",
            ),
        ],
        &["routes/web.php"],
    );

    // The ->name('dash') leaf indexes under the load's admin. prefix...
    assert!(
        index.get("admin.dash").is_some(),
        "external file outside routes/ must index its ->name under the load prefix"
    );
    // ...and its bare leaf survives (file scanned directly too).
    assert!(
        index.get("dash").is_some(),
        "bare leaf name from the external file must remain"
    );
    // Resource routes in the external file also compose with the load prefix.
    for action in [
        "index", "create", "store", "show", "edit", "update", "destroy",
    ] {
        assert!(
            index.get(&format!("admin.widgets.{action}")).is_some(),
            "expected admin.widgets.{action} from external file"
        );
    }
}

#[test]
fn source_files_includes_referenced_external_file() {
    let (tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/web.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/admin.php'));\n",
            ),
            (
                "app/Custom/admin.php",
                "<?php\nRoute::get('/dashboard', fn () => 'ok')->name('dash');\n",
            ),
        ],
        &["routes/web.php"],
    );

    let root = tmp.path();
    let web = normalize_path(&root.join("routes/web.php"));
    let admin = normalize_path(&root.join("app/Custom/admin.php"));
    assert!(
        index.source_files.contains(&web),
        "source_files must contain the discovered routes/web.php"
    );
    assert!(
        index.source_files.contains(&admin),
        "source_files must contain the transitively-referenced app/Custom/admin.php"
    );
}

#[test]
fn external_group_transitive_chain_outside_routes_dir() {
    // a (entry, in routes/) --admin.--> b (outside) --patient.--> c (outside)
    let (tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/a.php",
                "<?php\nRoute::as('admin.')->group(base_path('app/Custom/b.php'));\n",
            ),
            (
                "app/Custom/b.php",
                "<?php\nRoute::as('patient.')->group(base_path('app/Custom/c.php'));\n",
            ),
            (
                "app/Custom/c.php",
                "<?php\nRoute::get('/edit', fn () => 'ok')->name('edit');\n",
            ),
        ],
        &["routes/a.php"],
    );

    assert!(
        index.get("admin.patient.edit").is_some(),
        "transitive chain through files outside routes/ must accumulate prefixes"
    );

    let root = tmp.path();
    for rel in ["routes/a.php", "app/Custom/b.php", "app/Custom/c.php"] {
        let key = normalize_path(&root.join(rel));
        assert!(
            index.source_files.contains(&key),
            "source_files must contain {rel}"
        );
    }
}

#[test]
fn external_group_cycle_outside_routes_dir_terminates() {
    // a (entry) <-> b (outside): each loads the other. Must terminate and index
    // a finite set of names.
    let (_tmp, index) = index_with_entrypoints(
        &[
            (
                "routes/a.php",
                "<?php\nRoute::as('x.')->group(base_path('app/Custom/b.php'));\nRoute::get('/a', fn () => 'ok')->name('a');\n",
            ),
            (
                "app/Custom/b.php",
                "<?php\nRoute::as('y.')->group(base_path('routes/a.php'));\nRoute::get('/b', fn () => 'ok')->name('b');\n",
            ),
        ],
        &["routes/a.php"],
    );

    // Bare names from both files index.
    assert!(index.get("a").is_some(), "bare name a must index");
    assert!(index.get("b").is_some(), "bare name b must index");
    // One hop each direction.
    assert!(index.get("x.b").is_some(), "a loads b with prefix x.");
    assert!(index.get("y.a").is_some(), "b loads a with prefix y.");
    // Finite: the index can't have exploded into an unbounded name set.
    assert!(
        index.routes.len() < 50,
        "cycle must produce a finite, small name set, got {}",
        index.routes.len()
    );
}

// ============================================================================
// Resource route derivation (Route::resource / Route::apiResource)
// ============================================================================

/// Collect just the route names from a direct extraction over `src`.
fn names_of(src: &str) -> Vec<String> {
    let path = PathBuf::from("/fake/routes/web.php");
    extract_named_routes(src, &path, PRIORITY_APP, &[])
        .into_iter()
        .filter_map(|(n, _)| n)
        .collect()
}

#[test]
fn resource_inside_name_group_applies_prefix_and_strips_slash() {
    // `/leads` (leading slash) inside `Route::name('api.')->group(fn)` with an
    // `->only(['store', 'update'])` filter. Must yield `api.leads.store` and
    // `api.leads.update` — and NEVER a slash-prefixed `/leads.*` name.
    let src = r#"<?php
Route::name('api.')->group(function () {
    Route::resource('/leads', LeadController::class)->only(['store', 'update']);
});
"#;
    let names = names_of(src);
    assert!(
        names.contains(&"api.leads.store".to_string()),
        "expected api.leads.store, got {names:?}"
    );
    assert!(
        names.contains(&"api.leads.update".to_string()),
        "expected api.leads.update, got {names:?}"
    );
    // only() must drop the rest.
    assert!(!names.iter().any(|n| n.ends_with(".index")));
    assert!(!names.iter().any(|n| n.ends_with(".show")));
    // No name may carry a literal slash from the stripped resource URI.
    assert!(
        !names.iter().any(|n| n.contains('/')),
        "no name may contain a slash, got {names:?}"
    );
}

#[test]
fn api_resource_yields_five_actions_slash_free() {
    let src = r#"<?php
Route::apiResource('photos', PhotoController::class);
"#;
    let mut names = names_of(src);
    names.sort();
    assert_eq!(
        names,
        vec![
            "photos.destroy",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
        ],
        "apiResource must register exactly the 5 api actions"
    );
    // create/edit are form routes — excluded from apiResource.
    assert!(!names.iter().any(|n| n == "photos.create"));
    assert!(!names.iter().any(|n| n == "photos.edit"));
}

#[test]
fn resource_except_filters_actions() {
    let src = r#"<?php
Route::resource('posts', PostController::class)->except(['create', 'edit', 'destroy']);
"#;
    let mut names = names_of(src);
    names.sort();
    assert_eq!(
        names,
        vec!["posts.index", "posts.show", "posts.store", "posts.update",]
    );
}

#[test]
fn resource_strips_trailing_slash() {
    let src = r#"<?php
Route::resource('photos/', PhotoController::class)->only(['index']);
"#;
    let names = names_of(src);
    assert_eq!(names, vec!["photos.index"]);
}

#[test]
fn resource_skips_non_string_first_arg() {
    // A variable resource URI can't be resolved statically — skip it entirely.
    let src = r#"<?php
Route::resource($name, PhotoController::class);
"#;
    let names = names_of(src);
    assert!(
        names.is_empty(),
        "non-literal resource URI must be skipped, got {names:?}"
    );
}

#[test]
fn resource_in_external_file_composes_with_load_prefix() {
    // A `Route::resource('patient', …)` in a file loaded via
    // `Route::as('admin.')->group(base_path('routes/b.php'))` must yield
    // `admin.patient.*` (resource derivation + external-file prefix compose).
    let (_tmp, index) = index_routes(&[
        (
            "routes/web.php",
            "<?php\nRoute::as('admin.')->group(base_path('routes/b.php'));\n",
        ),
        (
            "routes/b.php",
            "<?php\nRoute::resource('patient', PatientController::class);\n",
        ),
    ]);

    for action in [
        "index", "create", "store", "show", "edit", "update", "destroy",
    ] {
        assert!(
            index.get(&format!("admin.patient.{action}")).is_some(),
            "expected admin.patient.{action} in index"
        );
        // The bare leaf survives too (file scanned directly).
        assert!(
            index.get(&format!("patient.{action}")).is_some(),
            "expected bare patient.{action} in index"
        );
    }
}

#[test]
fn no_indexed_resource_name_ever_starts_with_slash() {
    // Belt-and-suspenders: across a realistic mix, assert the slash bug is gone.
    let (_tmp, index) = index_routes(&[(
        "routes/web.php",
        "<?php\nRoute::name('api.')->group(function () {\n    Route::resource('/leads', LeadController::class);\n    Route::apiResource('/photos/', PhotoController::class);\n});\nRoute::resource('/bare', BareController::class);\nRoute::resource('/admin/account-manager/accounts', AccountsController::class);\n",
    )]);

    for name in index.routes.keys() {
        assert!(
            !name.starts_with('/'),
            "indexed route name must never start with a slash: {name}"
        );
        assert!(
            !name.contains('/'),
            "indexed route name must never contain a slash: {name}"
        );
    }
    // Spot-check a couple of expected names exist.
    assert!(index.get("api.leads.index").is_some());
    assert!(index.get("api.photos.store").is_some());
    assert!(index.get("bare.show").is_some());
    // A multi-segment URI is what actually exercises the invariant above —
    // single-segment fixtures pass it even when interior slashes are kept.
    assert!(index.get("accounts.index").is_some());
}

#[test]
fn resource_with_multi_segment_uri_names_only_the_last_segment() {
    // Laravel sends any slashed resource name through
    // `ResourceRegistrar::prefixedResource`, which keeps only the final segment
    // as the route name and treats the rest as a URI prefix. Indexing the whole
    // path made every CRUD route of such a resource unresolvable.
    let src = r#"<?php
Route::resource('/admin/account-manager/accounts', AccountsController::class)->only(['index', 'show']);
"#;
    let mut names = names_of(src);
    names.sort();
    assert_eq!(names, vec!["accounts.index", "accounts.show"]);
}

#[test]
fn resource_with_multi_segment_uri_keeps_the_full_uri_for_display() {
    // The prefix is dropped from the NAME only — hover still shows the path
    // the developer wrote.
    let path = PathBuf::from("/fake/routes/web.php");
    let results = extract_named_routes(
        "<?php\nRoute::apiResource('admin/photos', PhotoController::class)->only(['index']);\n",
        &path,
        PRIORITY_APP,
        &[],
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.as_deref(), Some("photos.index"));
    assert_eq!(results[0].1.uri.as_deref(), Some("admin/photos"));
}

#[test]
fn resource_with_multi_segment_uri_composes_with_group_prefix() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::resource('account-manager/users', UsersController::class)->only(['index']);
});
"#;
    assert_eq!(names_of(src), vec!["admin.users.index"]);
}

#[test]
fn closure_group_behavior_unchanged_regression() {
    // A plain in-file closure group must behave exactly as before — no external
    // load, no extra prefixes leaking in.
    let (_tmp, index) = index_routes(&[(
        "routes/web.php",
        "<?php\nRoute::name('admin.')->group(function () {\n    Route::get('/users', [UserController::class, 'index'])->name('users.index');\n});\nRoute::get('/login')->name('login');\n",
    )]);

    assert!(index.get("admin.users.index").is_some());
    assert!(index.get("login").is_some());
    assert!(
        index.get("users.index").is_none(),
        "in-file closure group should not also emit the bare leaf"
    );
}

#[test]
fn external_prefixes_for_defaults_to_empty_prefix() {
    // An index with no cached prefixes still yields the always-applicable "".
    let index = RouteIndex::new();
    assert_eq!(
        index.external_prefixes_for(&PathBuf::from("/fake/routes/web.php")),
        vec![String::new()]
    );
}

#[test]
fn external_prefixes_for_returns_cached_prefixes() {
    let mut index = RouteIndex::new();
    let path = PathBuf::from("/fake/routes/admin.php");
    index.external_prefixes.insert(
        normalize_path(&path),
        vec![String::new(), "admin.".to_string()],
    );
    assert_eq!(
        index.external_prefixes_for(&path),
        vec![String::new(), "admin.".to_string()]
    );
    // Lookup is normalized-path based, so a non-canonical spelling still hits.
    assert_eq!(
        index.external_prefixes_for(&PathBuf::from("/fake/routes/../routes/admin.php")),
        vec![String::new(), "admin.".to_string()]
    );
}

// ---------------------------------------------------------------------------
// normalize_path — lexical `.`/`..` resolution (regression cover for #117,
// where salsa_impl's divergent copy popped `RootDir` and relativized absolute
// paths; salsa_impl now delegates to this canonical function).
// ---------------------------------------------------------------------------

#[test]
fn normalize_path_resolves_interior_parent_dir_and_stays_absolute() {
    // A `..` after a `Normal` segment pops that segment; the path stays absolute.
    let result = normalize_path(&PathBuf::from("/app/models/../views"));
    assert_eq!(result, PathBuf::from("/app/views"));
    // `has_root`, not `is_absolute`: on Windows a leading separator with no
    // drive prefix is rooted but *not* absolute, and rootedness is the
    // invariant this test exists for (issue #292).
    assert!(result.has_root(), "rooted input must stay rooted");
}

#[test]
fn normalize_path_never_pops_root_dir() {
    // Regression for #117: a `..` walking past root must NOT pop `RootDir` and
    // silently turn an absolute path relative. The escaping `..` is preserved
    // and the result stays rooted (the buggy copy returned a relative `escape`).
    let result = normalize_path(&PathBuf::from("/app/../../escape"));
    assert!(
        result.has_root(),
        "rooted input must stay rooted, got {result:?}"
    );
    assert_eq!(result, PathBuf::from("/../escape"));
}

#[test]
fn normalize_path_preserves_leading_parent_dir() {
    // A leading `..` on a relative path can't be resolved lexically, so it's kept.
    assert_eq!(
        normalize_path(&PathBuf::from("../sibling/file.php")),
        PathBuf::from("../sibling/file.php")
    );
}

#[test]
fn normalize_path_unchanged_without_parent_dir() {
    // No `..` segments: behavior is unchanged (only `CurDir` collapses).
    assert_eq!(
        normalize_path(&PathBuf::from("/app/views/home.blade.php")),
        PathBuf::from("/app/views/home.blade.php")
    );
    assert_eq!(
        normalize_path(&PathBuf::from("/app/./views/./home.php")),
        PathBuf::from("/app/views/home.php")
    );
}

// ============================================================================
// Folio pages surface through the handler-level index (issue #101)
// ============================================================================
//
// `folio_discovery::inject_folio_routes` is unit-tested in
// `folio_discovery/tests.rs`, but nothing exercised the layer the completion
// handler actually reads: `build_route_index` over `discover_route_files`, the
// same pair `rebuild_route_index` calls at runtime. `build_route_index`
// injects Folio routes as its final step, so a named Folio page must surface in
// the resulting `RouteIndex` — name, URI, and method — without any `Route::`
// call. This guards the wiring above the unit test: a regression that dropped
// the `inject_folio_routes` call (or mangled the entry) would fail here.

#[test]
fn folio_named_page_surfaces_in_handler_route_index() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // A project that uses Folio, with one named page at the default mount.
    std::fs::write(
        root.join("composer.json"),
        r#"{"require": {"laravel/folio": "^1.0"}}"#,
    )
    .unwrap();
    let page = root.join("resources/views/pages/users/[id].blade.php");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, "<?php name('users.show'); ?>").unwrap();

    // Exactly what `rebuild_route_index` runs at runtime — discover the route
    // files, then build the index over them. Folio injection happens inside
    // `build_route_index`, so the page surfaces even though no `routes/` file
    // (and thus no discovered route file) exists.
    let index = build_route_index(root, &discover_route_files(root));

    let route = index
        .get("users.show")
        .expect("named Folio route must surface via build_route_index");
    // The name would be offered by `get_all_route_names()`, and goto resolves
    // to the page file.
    assert!(
        route.file.ends_with("users/[id].blade.php"),
        "Folio route must point at the page file, got {:?}",
        route.file
    );
    // URI and method must be carried so a future regression that omits either
    // is caught at this layer.
    assert_eq!(route.uri.as_deref(), Some("/users/{id}"));
    assert_eq!(route.method.as_deref(), Some("get"));
}

// ============================================================================
// Comment / heredoc blindness (the byte-scan bug the AST parse retired)
// ============================================================================
//
// The previous scanner matched braces, parens and quotes over raw bytes with no
// notion of PHP comments. Real route files carry commented-out route code and
// English prose, both of which silently corrupted every group boundary that
// followed: a stray `{` from `// Route::get('/x', function () {` pushed a
// group's closing brace hundreds of lines late, and a lone apostrophe in
// "it's" opened a phantom string literal that swallowed the braces after it.
// Routes then inherited a prefix from a group they were never inside.

#[test]
fn commented_out_route_is_not_indexed() {
    let src = r#"<?php
// Route::get('/ghost', [GhostController::class, 'index'])->name('ghost');
Route::get('/real')->name('real');
"#;
    assert_eq!(
        names_of(src),
        vec!["real"],
        "a commented-out route is not a route"
    );
}

#[test]
fn commented_out_resource_is_not_indexed() {
    let src = r#"<?php
// Route::resource('ghosts', GhostController::class);
Route::resource('reals', RealController::class)->only(['index']);
"#;
    assert_eq!(names_of(src), vec!["reals.index"]);
}

#[test]
fn commented_out_closure_brace_does_not_extend_group_body() {
    // The decisioncloud shape: a commented-out route leaves an unmatched `{`
    // inside the group. Byte-scanning counted it, so the group's closing brace
    // was never found and its prefix vanished from the routes inside it.
    let src = r#"<?php
Route::name('admin.')->group(function () {
    // Route::get('/legacy', function () {
    Route::get('/users')->name('users.index');
});
Route::name('api.')->group(function () {
    Route::get('/posts')->name('posts.index');
});
"#;
    assert_eq!(
        names_of(src),
        vec!["admin.users.index", "api.posts.index"],
        "an unmatched brace in a comment must not move the group boundary"
    );
}

#[test]
fn apostrophe_in_comment_does_not_shift_group_boundary() {
    // A lone `it's` opens a phantom string literal for a quote-tracking byte
    // scanner, hiding every brace up to the next quote — here, the group's own
    // closing `}`, which then swallowed the sibling group below it.
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::get('/users')->name('users.index');
    // the closing brace is below, it's important
});
Route::name('api.')->group(function () {
    Route::get('/posts')->name('posts.index');
});
"#;
    assert_eq!(
        names_of(src),
        vec!["admin.users.index", "api.posts.index"],
        "a prose apostrophe must not shift the group boundary"
    );
}

#[test]
fn block_comment_braces_do_not_shift_group_boundary() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    /*
     * Route::get('/old', function () {
     */
    Route::get('/users')->name('users.index');
});
Route::get('/login')->name('login');
"#;
    assert_eq!(names_of(src), vec!["admin.users.index", "login"]);
}

#[test]
fn heredoc_braces_do_not_shift_group_boundary() {
    // A `}` inside heredoc text closed the group early, so the route below it
    // lost the `admin.` prefix entirely.
    let src = r#"<?php
Route::name('admin.')->group(function () {
    $sql = <<<'SQL'
} this brace is data, not code
SQL;
    Route::get('/users')->name('users.index');
});
Route::get('/login')->name('login');
"#;
    assert_eq!(
        names_of(src),
        vec!["admin.users.index", "login"],
        "heredoc content must not be parsed as code"
    );
}

#[test]
fn group_name_setter_is_not_itself_a_route() {
    // `->name('admin.')` on a chain that ends in `->group(...)` configures the
    // children's prefix; it does not register a route of its own. The byte scan
    // indexed it as the bogus route `admin.`.
    let src = r#"<?php
Route::prefix('/admin')->name('admin.')->group(function () {
    Route::get('/users')->name('users.index');
});
"#;
    assert_eq!(names_of(src), vec!["admin.users.index"]);
}

// ============================================================================
// Singleton route derivation (Route::singleton / Route::apiSingleton)
// ============================================================================

/// Route names from a direct extraction, sorted so asserts don't depend on
/// emission order. Sorting also makes a duplicated name visible as an extra
/// element rather than silently collapsing.
fn sorted_names_of(src: &str) -> Vec<String> {
    let mut names = names_of(src);
    names.sort();
    names
}

#[test]
fn singleton_yields_show_edit_update() {
    let src = r#"<?php
Route::singleton('profile', ProfileController::class);
"#;
    assert_eq!(
        sorted_names_of(src),
        vec!["profile.edit", "profile.show", "profile.update"],
        "a bare singleton registers exactly show/edit/update"
    );
}

#[test]
fn singleton_creatable_adds_create_store_destroy() {
    let src = r#"<?php
Route::singleton('profile', ProfileController::class)->creatable();
"#;
    assert_eq!(
        sorted_names_of(src),
        vec![
            "profile.create",
            "profile.destroy",
            "profile.edit",
            "profile.show",
            "profile.store",
            "profile.update",
        ],
        "creatable() adds create/store/destroy to the singleton defaults"
    );
}

#[test]
fn singleton_destroyable_adds_only_destroy() {
    let src = r#"<?php
Route::singleton('profile', ProfileController::class)->destroyable();
"#;
    let names = sorted_names_of(src);
    assert_eq!(
        names,
        vec![
            "profile.destroy",
            "profile.edit",
            "profile.show",
            "profile.update",
        ],
        "destroyable() adds destroy and nothing else"
    );
    // The create/store pair belongs to creatable() alone.
    assert!(!names.iter().any(|n| n == "profile.create"));
    assert!(!names.iter().any(|n| n == "profile.store"));
}

#[test]
fn singleton_creatable_and_destroyable_union_without_duplicates() {
    // creatable() already implies destroy; stacking destroyable() on top must
    // not emit it twice, and neither chain order may drop an action.
    let expected = vec![
        "profile.create",
        "profile.destroy",
        "profile.edit",
        "profile.show",
        "profile.store",
        "profile.update",
    ];
    let creatable_first = r#"<?php
Route::singleton('profile', ProfileController::class)->creatable()->destroyable();
"#;
    let destroyable_first = r#"<?php
Route::singleton('profile', ProfileController::class)->destroyable()->creatable();
"#;
    assert_eq!(sorted_names_of(creatable_first), expected);
    assert_eq!(sorted_names_of(destroyable_first), expected);
}

#[test]
fn api_singleton_yields_show_and_update_only() {
    let src = r#"<?php
Route::apiSingleton('profile', ProfileController::class);
"#;
    let names = sorted_names_of(src);
    assert_eq!(
        names,
        vec!["profile.show", "profile.update"],
        "a bare apiSingleton registers exactly show/update"
    );
    // `edit` renders a form — an API singleton never registers it.
    assert!(!names.iter().any(|n| n == "profile.edit"));
}

#[test]
fn api_singleton_creatable_adds_store_and_destroy_but_never_create() {
    let src = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->creatable();
"#;
    let names = sorted_names_of(src);
    assert_eq!(
        names,
        vec![
            "profile.destroy",
            "profile.show",
            "profile.store",
            "profile.update",
        ],
        "apiSingleton()->creatable() adds store/destroy only"
    );
    // Both form-rendering actions stay out of the API set.
    assert!(!names.iter().any(|n| n == "profile.create"));
    assert!(!names.iter().any(|n| n == "profile.edit"));
}

#[test]
fn api_singleton_destroyable_adds_destroy() {
    let src = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->destroyable();
"#;
    assert_eq!(
        sorted_names_of(src),
        vec!["profile.destroy", "profile.show", "profile.update"],
    );
}

#[test]
fn singleton_only_and_except_filter_the_bare_action_set() {
    let only = r#"<?php
Route::singleton('profile', ProfileController::class)->only(['show', 'update']);
"#;
    assert_eq!(
        sorted_names_of(only),
        vec!["profile.show", "profile.update"]
    );

    let except = r#"<?php
Route::singleton('profile', ProfileController::class)->except(['edit']);
"#;
    assert_eq!(
        sorted_names_of(except),
        vec!["profile.show", "profile.update"]
    );
}

#[test]
fn api_singleton_only_and_except_filter_the_bare_action_set() {
    let only = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->only(['show']);
"#;
    assert_eq!(sorted_names_of(only), vec!["profile.show"]);

    let except = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->except(['update']);
"#;
    assert_eq!(sorted_names_of(except), vec!["profile.show"]);
}

#[test]
fn singleton_only_filters_against_the_expanded_not_the_bare_action_set() {
    // `create` exists only once creatable() has widened the set. Filtering for
    // it without creatable() must yield nothing — proof that only() runs
    // against the expanded set rather than a base set with extras appended
    // after the filter.
    let bare = r#"<?php
Route::singleton('profile', ProfileController::class)->only(['create']);
"#;
    assert!(
        sorted_names_of(bare).is_empty(),
        "create is not in the bare singleton set, so only(['create']) yields nothing"
    );

    let creatable = r#"<?php
Route::singleton('profile', ProfileController::class)->creatable()->only(['create', 'destroy']);
"#;
    assert_eq!(
        sorted_names_of(creatable),
        vec!["profile.create", "profile.destroy"],
    );

    let destroyable = r#"<?php
Route::singleton('profile', ProfileController::class)->destroyable()->only(['destroy']);
"#;
    assert_eq!(sorted_names_of(destroyable), vec!["profile.destroy"]);
}

#[test]
fn singleton_except_filters_against_the_expanded_not_the_bare_action_set() {
    let creatable = r#"<?php
Route::singleton('profile', ProfileController::class)->creatable()->except(['create', 'store', 'edit']);
"#;
    assert_eq!(
        sorted_names_of(creatable),
        vec!["profile.destroy", "profile.show", "profile.update"],
    );

    let destroyable = r#"<?php
Route::singleton('profile', ProfileController::class)->destroyable()->except(['destroy']);
"#;
    assert_eq!(
        sorted_names_of(destroyable),
        vec!["profile.edit", "profile.show", "profile.update"],
        "excepting destroy must undo destroyable(), not be ignored"
    );
}

#[test]
fn api_singleton_only_filters_against_the_expanded_not_the_bare_action_set() {
    let bare = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->only(['store']);
"#;
    assert!(
        sorted_names_of(bare).is_empty(),
        "store is not in the bare apiSingleton set"
    );

    let creatable = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->creatable()->only(['store', 'destroy']);
"#;
    assert_eq!(
        sorted_names_of(creatable),
        vec!["profile.destroy", "profile.store"],
    );

    let destroyable = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->destroyable()->only(['destroy']);
"#;
    assert_eq!(sorted_names_of(destroyable), vec!["profile.destroy"]);
}

#[test]
fn api_singleton_except_filters_against_the_expanded_not_the_bare_action_set() {
    let creatable = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->creatable()->except(['store']);
"#;
    assert_eq!(
        sorted_names_of(creatable),
        vec!["profile.destroy", "profile.show", "profile.update"],
    );

    let destroyable = r#"<?php
Route::apiSingleton('profile', ProfileController::class)->destroyable()->except(['destroy']);
"#;
    assert_eq!(
        sorted_names_of(destroyable),
        vec!["profile.show", "profile.update"],
    );
}

#[test]
fn singleton_with_multi_segment_uri_names_only_the_last_segment() {
    // Laravel detours a slashed singleton name through `prefixedSingleton`,
    // whose `getResourcePrefix` keeps only the final segment as the name.
    let src = r#"<?php
Route::singleton('admin/profile', ProfileController::class);
Route::apiSingleton('admin/settings/theme', ThemeController::class);
"#;
    let names = sorted_names_of(src);
    assert!(
        names.contains(&"profile.show".to_string()),
        "expected profile.show, got {names:?}"
    );
    assert!(
        names.contains(&"theme.show".to_string()),
        "expected theme.show, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains('/')),
        "no singleton name may carry a slash, got {names:?}"
    );
}

#[test]
fn singleton_with_multi_segment_uri_keeps_the_full_uri_for_display() {
    let src = r#"<?php
Route::singleton('admin/profile', ProfileController::class)->only(['show']);
"#;
    let path = PathBuf::from("/fake/routes/web.php");
    let routes = extract_named_routes(src, &path, PRIORITY_APP, &[]);
    let (name, definition) = routes
        .into_iter()
        .find(|(n, _)| n.as_deref() == Some("profile.show"))
        .expect("profile.show must be indexed");
    assert_eq!(name.as_deref(), Some("profile.show"));
    assert_eq!(definition.uri.as_deref(), Some("admin/profile"));
}

#[test]
fn singleton_inside_name_group_composes_the_prefix() {
    let src = r#"<?php
Route::name('admin.')->group(function () {
    Route::singleton('profile', ProfileController::class);
});
"#;
    assert_eq!(
        sorted_names_of(src),
        vec![
            "admin.profile.edit",
            "admin.profile.show",
            "admin.profile.update",
        ],
    );
}

#[test]
fn singleton_skips_non_string_and_empty_names() {
    for src in [
        "<?php\nRoute::singleton($name, ProfileController::class);\n",
        "<?php\nRoute::singleton('', ProfileController::class);\n",
        "<?php\nRoute::singleton('/', ProfileController::class);\n",
        "<?php\nRoute::apiSingleton($name, ProfileController::class);\n",
        "<?php\nRoute::apiSingleton('', ProfileController::class);\n",
        "<?php\nRoute::apiSingleton('/', ProfileController::class);\n",
    ] {
        assert!(
            names_of(src).is_empty(),
            "unresolvable singleton name must be skipped: {src:?}"
        );
    }
}

#[test]
fn resource_forms_are_unaffected_by_the_singleton_code_path() {
    // Regression guard: adding singleton/apiSingleton to the dispatch must not
    // shift what resource/apiResource emit.
    let resource = r#"<?php
Route::resource('photos', PhotoController::class);
"#;
    assert_eq!(
        sorted_names_of(resource),
        vec![
            "photos.create",
            "photos.destroy",
            "photos.edit",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
        ],
    );

    let api_resource = r#"<?php
Route::apiResource('photos', PhotoController::class);
"#;
    assert_eq!(
        sorted_names_of(api_resource),
        vec![
            "photos.destroy",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
        ],
    );

    // `creatable()`/`destroyable()` are not valid on a resource chain, but the
    // shared dispatch makes them a plausible copy-paste. They must be inert.
    let stray = r#"<?php
Route::resource('photos', PhotoController::class)->creatable()->destroyable();
Route::apiResource('videos', VideoController::class)->creatable();
"#;
    let names = sorted_names_of(stray);
    assert_eq!(
        names,
        vec![
            "photos.create",
            "photos.destroy",
            "photos.edit",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
            "videos.destroy",
            "videos.index",
            "videos.show",
            "videos.store",
            "videos.update",
        ],
        "stray singleton modifiers must not alter a resource action set"
    );
}

#[test]
fn container_singleton_is_not_indexed_as_a_route() {
    // `singleton(...)` is the service container's binding method too. A package
    // provider that binds into the container *and* registers a named route
    // passes the file-discovery gate, so without a receiver check every binding
    // key was indexed as a phantom `<key>.show`/`.edit`/`.update` route.
    let src = r#"<?php
class TelescopeServiceProvider extends ServiceProvider {
    public function register(): void
    {
        $this->app->singleton('telescope.limiter', fn ($app) => new Limiter($app));
        $app->singleton('flare.logger', fn ($app) => new Logger($app));
        App::singleton('metrics.recorder', fn ($app) => new Recorder($app));
        Container::singleton('audit.trail', fn ($app) => new Trail($app));
    }
    public function boot(): void
    {
        Route::get('/health', HealthController::class)->name('health');
    }
}
"#;
    assert!(
        content_registers_named_routes(src),
        "this file must reach extraction — the gate is not what protects us here"
    );
    assert_eq!(
        names_of(src),
        vec!["health"],
        "container bindings must contribute no route names"
    );
}

#[test]
fn router_receivers_other_than_the_route_facade_index_singletons() {
    // The allow-list is not "static calls on `Route`" — Laravel registers
    // routes through a router instance in provider `map()` methods, and through
    // the fully-qualified facade in files without a `use` import.
    let cases = [
        (
            "<?php\n$router->singleton('profile', ProfileController::class);\n",
            "$router",
        ),
        (
            "<?php\n$this->router->singleton('profile', ProfileController::class);\n",
            "$this->router",
        ),
        (
            "<?php\n\\Illuminate\\Support\\Facades\\Route::singleton('profile', ProfileController::class);\n",
            "fully-qualified facade",
        ),
        (
            "<?php\nRoute::middleware('auth')->singleton('profile', ProfileController::class);\n",
            "receiver behind a chained router call",
        ),
    ];
    for (src, label) in cases {
        assert_eq!(
            sorted_names_of(src),
            vec!["profile.edit", "profile.show", "profile.update"],
            "{label} must still index singleton routes"
        );
    }
}

#[test]
fn resource_forms_do_not_require_a_router_receiver() {
    // The receiver gate is scoped to the singleton forms, where a real name
    // collision exists. `resource(...)` collides with nothing, so gating it
    // would risk dropping registrations that index correctly today.
    let src = r#"<?php
$this->app->resource('photos', PhotoController::class);
"#;
    assert_eq!(
        sorted_names_of(src),
        vec![
            "photos.create",
            "photos.destroy",
            "photos.edit",
            "photos.index",
            "photos.show",
            "photos.store",
            "photos.update",
        ],
        "resource() must keep its current receiver-agnostic behavior",
    );
}
