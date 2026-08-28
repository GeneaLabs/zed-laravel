//! Tests for the file-watcher glob construction. The notification
//! handling itself is wired in `main.rs` against a real `Backend` and
//! exercised via integration paths — these tests just verify we ask
//! for the right things from the client.

use super::*;
use std::path::PathBuf;

#[test]
fn watchers_cover_all_four_indexed_directories() {
    let root = PathBuf::from("/projects/laravel-app");
    let view_paths = vec![root.join("resources/views")];
    let livewire = root.join("app/Livewire");
    let watchers = build_watchers(&root, &view_paths, Some(&livewire), &[], &[]);

    let globs: Vec<String> = watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!("we always emit absolute"),
        })
        .collect();

    // The four indexed-by-Salsa categories must each have a watcher.
    assert!(
        globs
            .iter()
            .any(|g| g.contains("app/Http/Controllers") && g.ends_with("*.php")),
        "missing controllers glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("routes/") && g.ends_with("*.php")),
        "missing routes glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("resources/views") && g.ends_with("*.blade.php")),
        "missing blade-views glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("app/Livewire") && g.ends_with("*.php")),
        "missing livewire glob: {:?}",
        globs
    );
}

#[test]
fn watchers_omit_livewire_when_not_configured() {
    let root = PathBuf::from("/projects/no-livewire-app");
    let view_paths = vec![root.join("resources/views")];
    let watchers = build_watchers(&root, &view_paths, None, &[], &[]);

    let has_livewire = watchers.iter().any(|w| match &w.glob_pattern {
        GlobPattern::String(s) => s.contains("Livewire"),
        _ => false,
    });
    assert!(
        !has_livewire,
        "should not register livewire glob when path is None"
    );
}

#[test]
fn watchers_register_each_configured_view_path() {
    // Themed apps configure multiple view paths — each gets its own
    // pair of watchers (blade + bare php).
    let root = PathBuf::from("/projects/themed-app");
    let view_paths = vec![
        root.join("resources/views"),
        root.join("themes/dark/views"),
        root.join("themes/light/views"),
    ];
    let watchers = build_watchers(&root, &view_paths, None, &[], &[]);

    for view_path in &view_paths {
        // Compare against the same forward-slash rendering the watcher emits.
        // LSP glob patterns are forward-slash by specification, so building the
        // expectation from a platform-separator `Path` would only match on
        // Unix (issue #292).
        let expected_base = super::glob_base(view_path);
        let has_blade = watchers.iter().any(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.contains(&expected_base) && s.ends_with("*.blade.php"),
            _ => false,
        });
        assert!(has_blade, "missing blade watcher for {:?}", view_path);
    }
}

#[test]
fn watchers_request_create_change_and_delete_events() {
    let root = PathBuf::from("/projects/test");
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &[], &[]);

    let all_three = WatchKind::Create | WatchKind::Change | WatchKind::Delete;
    for w in &watchers {
        assert_eq!(
            w.kind,
            Some(all_three),
            "every watcher should request all three event kinds"
        );
    }
}

#[test]
fn registration_has_correct_method_and_id() {
    let root = PathBuf::from("/projects/test");
    let reg = build_registration(&root, &[root.join("resources/views")], None, &[], &[]);
    assert_eq!(reg.method, METHOD);
    assert_eq!(reg.id, REGISTRATION_ID);
    assert!(reg.register_options.is_some(), "options must be serialized");
}

#[test]
fn registration_options_round_trip_through_serde() {
    let root = PathBuf::from("/projects/test");
    let view_paths = vec![root.join("resources/views")];
    let livewire = root.join("app/Livewire");
    let reg = build_registration(&root, &view_paths, Some(&livewire), &[], &[]);

    // The client deserializes our register_options into
    // DidChangeWatchedFilesRegistrationOptions. Verify the value we
    // emit is shaped correctly.
    let json = reg.register_options.unwrap();
    let parsed: DidChangeWatchedFilesRegistrationOptions = serde_json::from_value(json).unwrap();
    assert!(
        !parsed.watchers.is_empty(),
        "must have at least one watcher"
    );

    // We constructed exactly 1 (controllers) + 1 (routes) + 1 (migrations)
    // + 1 (config) + 2 (view blade + php) + 1 (livewire) + 2 (vendor php +
    // blade) + 4 (Inertia page extensions: vue/tsx/jsx/svelte)
    // + 6 (lang catalogues: php/json/vendor-php across both lang roots,
    //   issue #293) = 19 watchers. If the construction changes, this
    // assertion will flag it for review.
    assert_eq!(parsed.watchers.len(), 19);
}

#[test]
fn watchers_include_inertia_page_globs() {
    // Inertia "views" live under resources/js/Pages/ as JS/TS files (issue
    // #10) — one watcher glob per supported extension so external page
    // create/delete invalidates the existence cache.
    let root = PathBuf::from("/projects/laravel-app");
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &[], &[]);

    let globs: Vec<String> = watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!(),
        })
        .collect();

    for ext in ["vue", "tsx", "jsx", "svelte"] {
        let suffix = format!("*.{ext}");
        assert!(
            globs
                .iter()
                .any(|g| g.contains("resources/js/Pages") && g.ends_with(&suffix)),
            "missing inertia {ext} glob: {globs:?}"
        );
    }
}

#[test]
fn watchers_include_vendor_php_and_blade_globs() {
    let root = PathBuf::from("/projects/laravel-app");
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &[], &[]);

    let globs: Vec<String> = watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!(),
        })
        .collect();

    assert!(
        globs
            .iter()
            .any(|g| g.contains("/vendor/") && g.ends_with("*.php")),
        "missing vendor php glob: {:?}",
        globs
    );
    assert!(
        globs
            .iter()
            .any(|g| g.contains("/vendor/") && g.ends_with("*.blade.php")),
        "missing vendor blade glob: {:?}",
        globs
    );
}

fn globs_of(watchers: &[FileSystemWatcher]) -> Vec<String> {
    watchers
        .iter()
        .map(|w| match &w.glob_pattern {
            GlobPattern::String(s) => s.clone(),
            GlobPattern::Relative(_) => unreachable!("we always emit absolute"),
        })
        .collect()
}

#[test]
fn watchers_include_a_recursive_glob_per_psr4_root() {
    // Each first-party PSR-4 source root (M2) gets its own recursive `**/*.php`
    // glob so an external edit anywhere under it converges the magic index.
    let root = PathBuf::from("/projects/laravel-app");
    let psr4 = vec![root.join("app"), root.join("src")];
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &psr4, &[]);

    let globs = globs_of(&watchers);
    for src in &psr4 {
        let expected = format!("{}/**/*.php", super::glob_base(src));
        assert!(
            globs.iter().any(|g| g == &expected),
            "missing PSR-4 glob {expected}: {globs:?}"
        );
    }
}

#[test]
fn psr4_glob_that_exactly_duplicates_an_existing_one_is_skipped() {
    // A PSR-4 root whose recursive glob is byte-identical to one already
    // emitted must not produce a duplicate watcher. The Inertia pages dir
    // isn't `*.php`, and the fixed globs are all more specific, so the only
    // way to collide is to hand in a root that reproduces a fixed glob — here
    // we prove the dedup by counting: two identical roots yield one glob.
    let root = PathBuf::from("/projects/laravel-app");
    let psr4 = vec![root.join("app"), root.join("app")];
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &psr4, &[]);

    let expected = format!("{}/app/**/*.php", super::glob_base(&root));
    let count = globs_of(&watchers)
        .into_iter()
        .filter(|g| g == &expected)
        .count();
    assert_eq!(count, 1, "duplicate PSR-4 roots must collapse to one glob");
}

#[test]
fn overlapping_psr4_glob_coexists_with_fixed_controllers_glob() {
    // `app/**/*.php` overlaps — but is not byte-identical to — the fixed
    // `app/Http/Controllers/**/*.php`. Both must be present: the overlap is
    // harmless (the watched-files handler is idempotent), and dropping the
    // fixed one would narrow coverage on a project whose composer.json we
    // couldn't read.
    let root = PathBuf::from("/projects/laravel-app");
    let psr4 = vec![root.join("app")];
    let watchers = build_watchers(&root, &[root.join("resources/views")], None, &psr4, &[]);

    let globs = globs_of(&watchers);
    assert!(
        globs
            .iter()
            .any(|g| g == &format!("{}/app/**/*.php", super::glob_base(&root))),
        "missing widened app glob: {globs:?}"
    );
    assert!(
        globs
            .iter()
            .any(|g| g == &format!("{}/app/Http/Controllers/**/*.php", root.display())),
        "fixed controllers glob must survive alongside the widened app glob: {globs:?}"
    );
}

#[test]
fn module_dirs_get_their_own_watcher_pair() {
    // A module tree outside every PSR-4 source root (the `Modules/*`
    // shape) must still be watched: config, providers, views, and lang
    // catalogues all live under it.
    let root = std::path::PathBuf::from("/proj");
    let module = root.join("Modules/Blog");
    let watchers = build_watchers(
        &root,
        &[root.join("resources/views")],
        None,
        &[],
        std::slice::from_ref(&module),
    );
    let globs: Vec<String> = watchers
        .iter()
        .filter_map(|w| match &w.glob_pattern {
            GlobPattern::String(g) => Some(g.clone()),
            GlobPattern::Relative(_) => None,
        })
        .collect();
    // Expectations built the same way `build_watchers` builds its globs —
    // `display()` yields backslashes on Windows, the glob base never does.
    let base = module.display().to_string().replace('\\', "/");
    assert!(
        globs.contains(&format!("{base}/**/*.php")),
        "php glob covers config/providers/lang: {globs:?}"
    );
    assert!(
        globs.contains(&format!("{base}/**/*.blade.php")),
        "blade glob covers the module views: {globs:?}"
    );
}

#[test]
fn module_dir_under_a_watched_source_root_adds_nothing() {
    let root = std::path::PathBuf::from("/proj");
    let app = root.join("app");
    let module = root.join("app");
    let base = build_watchers(&root, &[], None, std::slice::from_ref(&app), &[]).len();
    let with_module = build_watchers(
        &root,
        &[],
        None,
        std::slice::from_ref(&app),
        std::slice::from_ref(&module),
    )
    .len();
    assert_eq!(
        with_module,
        base + 1,
        "the php glob dedups against the PSR-4 watcher; only the blade glob is new"
    );
}
