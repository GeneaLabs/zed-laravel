use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Build a Laravel-shaped tempdir with the given (path, body) pairs.
fn project_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for (relpath, body) in files {
        let full = dir.path().join(relpath);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

#[test]
fn resolves_app_classes_via_project_composer_json() {
    // composer.json maps `App\` → `app/`, so App\Models\User must land
    // at app/Models/User.php — same answer as the FQCN heuristic, but
    // now we got there by reading the source of truth.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": "app/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("App\\Models\\User")
        .expect("App PSR-4 hit");
    assert!(
        resolved.ends_with("app/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn resolves_hyphenated_vendor_packages_from_installed_json() {
    // This is the exact crossbible-vapor failure case: the package dir
    // is `bible-models` (with hyphen) but the namespace is
    // `CrossBibleInc\BibleModels\` (no hyphen). The lowercased-namespace
    // heuristic computes `biblemodels` and misses. The real PSR-4 map
    // tells us the truth.
    let installed = r#"{
        "packages": [
            {
                "name": "crossbibleinc/bible-models",
                "autoload": {
                    "psr-4": { "CrossBibleInc\\BibleModels\\": "src/" }
                },
                "install-path": "../crossbibleinc/bible-models"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/crossbibleinc/bible-models/src/Models/Version.php",
            "<?php\nnamespace CrossBibleInc\\BibleModels\\Models;\nclass Version {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("CrossBibleInc\\BibleModels\\Models\\Version")
        .expect("hyphenated vendor package should resolve");
    assert!(
        resolved.ends_with("vendor/crossbibleinc/bible-models/src/Models/Version.php"),
        "got {resolved:?}"
    );
}

#[test]
fn longest_prefix_wins_when_multiple_match() {
    // If both `App\` → app/ and `App\Models\` → custom/models/ are
    // declared, the more specific prefix must win for `App\Models\User`.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": "app/",
                "App\\Models\\": "custom/models/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("custom/models/User.php", "<?php class User {}"),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("custom/models/User.php"),
        "longest prefix should win; got {resolved:?}"
    );
}

#[test]
fn psr4_value_can_be_array_of_paths() {
    // Some packages declare PSR-4 paths as an array — Composer tries
    // each in order. We try them all and return the first that exists.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": ["app/", "src/"]
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("src/Models/User.php", "<?php class User {}"),
        // Note: NO app/Models/User.php — must fall through to src/.
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("src/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn returns_none_for_fqcn_with_no_matching_prefix() {
    // Class lives in a namespace nobody declared autoload for —
    // resolution must return None so the caller can fall back to other
    // strategies (basename walk, etc.).
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);
    assert!(autoload.resolve("Unrelated\\Vendor\\Something").is_none());
}

#[test]
fn returns_none_when_psr4_matches_but_file_doesnt_exist() {
    // PSR-4 says "this is where it should be", but the file isn't there
    // (deleted, renamed, never created). We don't lie — return None.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);
    assert!(autoload.resolve("App\\Models\\Missing").is_none());
}

#[test]
fn autoload_dev_is_included() {
    // Database\Factories\ lives in autoload-dev, not autoload. Tests
    // and seeders need to resolve from there.
    let composer = r#"{
        "autoload-dev": {
            "psr-4": { "Database\\Factories\\": "database/factories/" }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        (
            "database/factories/UserFactory.php",
            "<?php\nnamespace Database\\Factories;\nclass UserFactory {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("Database\\Factories\\UserFactory")
        .expect("autoload-dev should be honored");
    assert!(
        resolved.ends_with("database/factories/UserFactory.php"),
        "got {resolved:?}"
    );
}

#[test]
fn leading_backslash_in_fqcn_is_tolerated() {
    // PHP allows `\App\Models\User` (fully qualified marker). We treat
    // it identically to the version without the leading slash.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php class User {}"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload.resolve("\\App\\Models\\User").expect("resolve");
    assert!(
        resolved.ends_with("app/Models/User.php"),
        "got {resolved:?}"
    );
}

#[test]
fn resolve_namespace_dirs_maps_namespace_to_existing_directory() {
    // Blade::componentNamespace('App\\View\\Components\\Nightshade', 'nightshade')
    // must resolve to the on-disk directory so its class files can be walked
    // for completion candidates.
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        (
            "app/View/Components/Nightshade/Alert.php",
            "<?php class Alert {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);

    let dirs = autoload.resolve_namespace_dirs("App\\View\\Components\\Nightshade");

    assert_eq!(dirs.len(), 1, "expected one resolved dir, got {dirs:?}");
    assert!(
        dirs[0].ends_with("app/View/Components/Nightshade"),
        "got {:?}",
        dirs[0],
    );
}

#[test]
fn resolve_namespace_dirs_returns_empty_for_unknown_or_nonexistent() {
    let composer = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;
    let (_dir, root) = project_with_files(&[("composer.json", composer)]);
    let autoload = ComposerAutoload::load(&root);

    // No matching PSR-4 prefix.
    assert!(autoload
        .resolve_namespace_dirs("Vendor\\Pkg\\Components")
        .is_empty());
    // Matching prefix but the directory doesn't exist on disk.
    assert!(autoload
        .resolve_namespace_dirs("App\\View\\Components")
        .is_empty());
}

// ── project_source_roots (M2 — file-watcher widening) ──────────────────────

/// Assert a `project_source_roots` result contains a root ending in `suffix`,
/// path-separator-agnostic.
fn has_root_ending(roots: &[PathBuf], suffix: &str) -> bool {
    roots.iter().any(|r| r.ends_with(suffix))
}

#[test]
fn source_roots_return_app_and_dev_roots_deduped() {
    // Both `autoload` (App\ → app/) and `autoload-dev` (Tests\ → tests/) roots
    // are surfaced; installed.json vendor roots are NOT.
    let composer = r#"{
        "autoload": { "psr-4": { "App\\": "app/" } },
        "autoload-dev": { "psr-4": { "Tests\\": "tests/" } }
    }"#;
    let installed = r#"{
        "packages": [
            {
                "name": "acme/pkg",
                "autoload": { "psr-4": { "Acme\\Pkg\\": "src/" } },
                "install-path": "../acme/pkg"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php"),
        ("tests/Feature/ExampleTest.php", "<?php"),
        ("vendor/composer/installed.json", installed),
        ("vendor/acme/pkg/src/Thing.php", "<?php"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let roots = autoload.project_source_roots();

    assert!(
        has_root_ending(&roots, "app"),
        "app root missing: {roots:?}"
    );
    assert!(
        has_root_ending(&roots, "tests"),
        "autoload-dev tests root missing: {roots:?}"
    );
    assert!(
        !roots
            .iter()
            .any(|r| r.components().any(|c| c.as_os_str() == "vendor")),
        "vendor package roots must be excluded: {roots:?}"
    );
}

#[test]
fn source_roots_drop_subsumed_nested_root() {
    // `App\Models\` → app/Models is nested under `App\` → app; only the
    // shallowest survives, its recursive glob already covers the nested one.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "App\\": "app/",
                "App\\Models\\": "app/Models/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let roots = autoload.project_source_roots();

    assert!(
        has_root_ending(&roots, "app"),
        "app root missing: {roots:?}"
    );
    assert!(
        !has_root_ending(&roots, "app/Models"),
        "nested app/Models root must be subsumed: {roots:?}"
    );
}

#[test]
fn source_roots_skip_root_equal_to_project_and_nonexistent_dirs() {
    // `Root\\` → "" resolves to the project root itself (whose `**/*.php` would
    // sweep storage/compiled Blade) — skipped. `Ghost\\` → ghost/ doesn't
    // exist on disk — skipped. `App\\` → app/ survives.
    let composer = r#"{
        "autoload": {
            "psr-4": {
                "Root\\": "",
                "Ghost\\": "ghost/",
                "App\\": "app/"
            }
        }
    }"#;
    let (_dir, root) = project_with_files(&[
        ("composer.json", composer),
        ("app/Models/User.php", "<?php"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let roots = autoload.project_source_roots();

    assert_eq!(roots.len(), 1, "only app/ should survive: {roots:?}");
    assert!(
        has_root_ending(&roots, "app"),
        "app root missing: {roots:?}"
    );
}

#[test]
fn source_roots_drop_root_escaping_project_via_containment_guard() {
    // The fail-closed `path_within_root` guard must drop a PSR-4 value that
    // escapes the project — even one pointing at a directory that really exists
    // (so `is_dir` alone wouldn't catch it). `outside` is a sibling tempdir
    // (absolute, on-disk, outside the project root); a `PathBuf::join` of an
    // absolute value replaces the base, so `Evil\\` maps straight to it. A
    // regression removing the containment guard would let it through and fail
    // this test.
    let outside = TempDir::new().unwrap();
    // `\` is an escape character in JSON, so a Windows path interpolated raw
    // makes the whole composer.json unparseable — every autoload root then
    // comes back empty, including the legitimate one this test asserts on
    // (issue #292).
    let outside_json = outside.path().display().to_string().replace('\\', "\\\\");
    let composer = format!(
        r#"{{ "autoload": {{ "psr-4": {{ "Evil\\": "{}", "App\\": "app/" }} }} }}"#,
        outside_json
    );
    let (_dir, root) = project_with_files(&[
        ("composer.json", &composer),
        ("app/Models/User.php", "<?php"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let roots = autoload.project_source_roots();

    assert!(
        has_root_ending(&roots, "app"),
        "app root missing: {roots:?}"
    );
    let outside_canon = outside
        .path()
        .canonicalize()
        .unwrap_or_else(|_| outside.path().to_path_buf());
    assert!(
        !roots.iter().any(|r| {
            let rc = r.canonicalize().unwrap_or_else(|_| r.clone());
            rc.starts_with(&outside_canon)
        }),
        "an escaping PSR-4 root must be dropped by the containment guard: {roots:?}"
    );
}
