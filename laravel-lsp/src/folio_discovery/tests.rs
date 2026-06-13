use super::*;

use std::fs;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// derive_uri — static, dynamic, catch-all, and index segments
// ---------------------------------------------------------------------------

#[test]
fn derive_uri_static_segment() {
    assert_eq!(derive_uri("about.blade.php"), "about");
    assert_eq!(derive_uri("users/profile.blade.php"), "users/profile");
}

#[test]
fn derive_uri_dynamic_segment() {
    assert_eq!(derive_uri("users/[id].blade.php"), "users/{id}");
    assert_eq!(derive_uri("[post].blade.php"), "{post}");
}

#[test]
fn derive_uri_catch_all_segment() {
    assert_eq!(derive_uri("docs/[...slug].blade.php"), "docs/{slug}");
    assert_eq!(derive_uri("[...path].blade.php"), "{path}");
}

#[test]
fn derive_uri_drops_trailing_index() {
    assert_eq!(derive_uri("index.blade.php"), "");
    assert_eq!(derive_uri("users/index.blade.php"), "users");
}

#[test]
fn derive_uri_keeps_non_trailing_index() {
    // `index` only collapses when it's the final segment.
    assert_eq!(derive_uri("index/show.blade.php"), "index/show");
}

#[test]
fn derive_uri_mixed_segments() {
    assert_eq!(
        derive_uri("users/[id]/posts/[...rest].blade.php"),
        "users/{id}/posts/{rest}"
    );
}

// ---------------------------------------------------------------------------
// extract_page_name — the Folio `name('...')` helper
// ---------------------------------------------------------------------------

#[test]
fn extract_page_name_finds_helper_call() {
    let src = "<?php\nuse function Laravel\\Folio\\name;\nname('users.show');\n?>\n<div></div>";
    assert_eq!(extract_page_name(src), Some("users.show".to_string()));
}

#[test]
fn extract_page_name_double_quotes() {
    let src = r#"<?php name("dashboard"); ?>"#;
    assert_eq!(extract_page_name(src), Some("dashboard".to_string()));
}

#[test]
fn extract_page_name_none_when_absent() {
    let src = "<div>plain page, no name</div>";
    assert_eq!(extract_page_name(src), None);
}

#[test]
fn extract_page_name_ignores_route_chain_name() {
    // `->name(` must not be mistaken for the Folio helper.
    let src = "<?php Route::get('/x')->name('not.folio'); ?>";
    assert_eq!(extract_page_name(src), None);
}

#[test]
fn extract_page_name_ignores_static_name_call() {
    let src = "<?php Foo::name('nope'); ?>";
    assert_eq!(extract_page_name(src), None);
}

// ---------------------------------------------------------------------------
// parse_folio_mounts — non-default mount paths and chained prefixes
// ---------------------------------------------------------------------------

#[test]
fn parse_folio_mounts_default_when_none() {
    let mounts = parse_folio_mounts("<?php // nothing here", Path::new("/app"));
    assert!(mounts.is_empty());
}

#[test]
fn parse_folio_mounts_custom_path() {
    let src =
        "<?php Folio::path(resource_path('views/folio'));\nFolio::path('resources/views/admin');";
    let mounts = parse_folio_mounts(src, Path::new("/app"));
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].directory, Path::new("/app/resources/views/admin"));
    assert_eq!(mounts[0].uri_prefix, "");
    assert_eq!(mounts[0].name_prefix, "");
}

#[test]
fn parse_folio_mounts_with_uri_and_name_prefixes() {
    let src = "<?php Folio::path('resources/views/admin')->uri('/admin')->name('admin.');";
    let mounts = parse_folio_mounts(src, Path::new("/app"));
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].uri_prefix, "admin");
    assert_eq!(mounts[0].name_prefix, "admin.");
}

#[test]
fn parse_folio_mounts_prefixes_do_not_bleed_across_statements() {
    let src = "<?php Folio::path('a')->name('a.');\nFolio::path('b');";
    let mounts = parse_folio_mounts(src, Path::new("/app"));
    assert_eq!(mounts.len(), 2);
    assert_eq!(mounts[0].name_prefix, "a.");
    assert_eq!(mounts[1].name_prefix, "");
}

// ---------------------------------------------------------------------------
// compose_uri / compose_name
// ---------------------------------------------------------------------------

#[test]
fn compose_uri_collapses_empty_to_root() {
    assert_eq!(compose_uri("", ""), "/");
    assert_eq!(compose_uri("", "users/{id}"), "/users/{id}");
    assert_eq!(compose_uri("admin", "users"), "/admin/users");
}

#[test]
fn compose_name_applies_prefix() {
    assert_eq!(compose_name("", "users.show"), "users.show");
    assert_eq!(compose_name("admin.", "users"), "admin.users");
}

// ---------------------------------------------------------------------------
// End-to-end discovery against a temp project
// ---------------------------------------------------------------------------

/// Build a temp project that enables Folio (via composer.json) and write the
/// given `(relative-page-path, contents)` pages under `mount`.
fn make_project(mount: &str, pages: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(
        root.join("composer.json"),
        r#"{"require": {"laravel/folio": "^1.0"}}"#,
    )
    .unwrap();
    for (rel, contents) in pages {
        let full = root.join(mount).join(rel);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, contents).unwrap();
    }
    dir
}

#[test]
fn folio_in_use_detects_composer_dependency() {
    let dir = make_project(DEFAULT_FOLIO_MOUNT, &[]);
    assert!(folio_in_use(dir.path()));
}

#[test]
fn folio_in_use_false_without_folio() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("composer.json"), r#"{"require": {}}"#).unwrap();
    assert!(!folio_in_use(dir.path()));
}

#[test]
fn discover_folio_routes_resolves_named_pages_default_mount() {
    let dir = make_project(
        DEFAULT_FOLIO_MOUNT,
        &[
            ("users/[id].blade.php", "<?php name('users.show'); ?>"),
            ("about.blade.php", "<?php name('about'); ?>"),
            ("anonymous.blade.php", "<div>no name</div>"),
        ],
    );
    let routes = discover_folio_routes(dir.path());

    let show = routes
        .iter()
        .find(|r| r.name.as_deref() == Some("users.show"))
        .expect("named users.show route");
    assert_eq!(show.uri, "/users/{id}");
    assert!(show.file.ends_with("users/[id].blade.php"));

    // The unnamed page is still discovered (with a URI) but carries no name.
    assert!(routes
        .iter()
        .any(|r| r.uri == "/anonymous" && r.name.is_none()));
}

#[test]
fn discover_folio_routes_honours_non_default_mount() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app/Providers")).unwrap();
    fs::write(
        root.join("app/Providers/FolioServiceProvider.php"),
        "<?php Folio::path('resources/views/folio')->name('site.');",
    )
    .unwrap();
    let page = root.join("resources/views/folio/contact.blade.php");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(&page, "<?php name('contact'); ?>").unwrap();

    let routes = discover_folio_routes(root);
    let contact = routes
        .iter()
        .find(|r| r.file.ends_with("contact.blade.php"))
        .expect("contact page discovered under custom mount");
    assert_eq!(contact.uri, "/contact");
    // The mount's `->name('site.')` prefix is applied.
    assert_eq!(contact.name.as_deref(), Some("site.contact"));
}

#[test]
fn discover_folio_routes_empty_without_folio() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("composer.json"), r#"{"require": {}}"#).unwrap();
    assert!(discover_folio_routes(dir.path()).is_empty());
}

#[test]
fn inject_folio_routes_adds_named_routes_to_index() {
    let dir = make_project(
        DEFAULT_FOLIO_MOUNT,
        &[
            ("docs/[...slug].blade.php", "<?php name('docs.show'); ?>"),
            ("index.blade.php", "<?php name('home'); ?>"),
        ],
    );
    let mut index = RouteIndex::new();
    inject_folio_routes(dir.path(), &mut index);

    let docs = index.get("docs.show").expect("docs.show injected");
    assert_eq!(docs.uri.as_deref(), Some("/docs/{slug}"));
    assert!(docs.file.ends_with("docs/[...slug].blade.php"));
    assert_eq!(docs.method.as_deref(), Some("get"));

    let home = index.get("home").expect("home injected");
    assert_eq!(home.uri.as_deref(), Some("/"));
}

#[test]
fn inject_folio_routes_does_not_clobber_conventional_route() {
    let dir = make_project(
        DEFAULT_FOLIO_MOUNT,
        &[("dashboard.blade.php", "<?php name('dashboard'); ?>")],
    );
    let mut index = RouteIndex::new();
    // A conventional route already owns the name (inserted first, app priority).
    index.insert(
        "dashboard".to_string(),
        RouteDefinition {
            file: PathBuf::from("/app/routes/web.php"),
            line: 4,
            column: 0,
            end_column: 10,
            priority: PRIORITY_APP,
            method: Some("get".to_string()),
            uri: Some("/dashboard".to_string()),
            action: None,
        },
    );
    inject_folio_routes(dir.path(), &mut index);

    let def = index.get("dashboard").unwrap();
    assert!(
        def.file.ends_with("routes/web.php"),
        "conventional route must win over the Folio page of the same name"
    );
}
