use crate::salsa_impl::{LaravelDatabase, TranslationCache, TranslationKeyLocationData};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Drives the **real** production path — `TranslationCache` over a bare
/// `LaravelDatabase` — so these assertions are about the code rename actually
/// runs, containment guard included. The LSP actor adds only channel plumbing.
#[derive(Default)]
struct Locator {
    db: LaravelDatabase,
    cache: TranslationCache,
}

impl Locator {
    fn locate(&mut self, root: &Path, dotted_key: &str) -> Vec<TranslationKeyLocationData> {
        self.cache
            .locate_key_across_locales(&mut self.db, root, dotted_key)
    }
}

/// Build a fake Laravel project with a `lang/` directory and a list of
/// (locale, file_stem, content) entries seeded as locale lang files.
fn fake_project_with_lang(entries: &[(&str, &str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let lang = dir.path().join("lang");
    fs::create_dir_all(&lang).unwrap();
    for (locale, file_stem, content) in entries {
        let locale_dir = lang.join(locale);
        fs::create_dir_all(&locale_dir).unwrap();
        fs::write(locale_dir.join(format!("{file_stem}.php")), content).unwrap();
    }
    dir
}

const AUTH_EN: &str = r#"<?php
return [
    'failed' => 'These credentials do not match our records.',
    'password' => 'The provided password is incorrect.',
];
"#;

const AUTH_ES: &str = r#"<?php
return [
    'failed' => 'Estas credenciales no coinciden.',
    'password' => 'La contraseña proporcionada es incorrecta.',
];
"#;

const AUTH_NESTED_EN: &str = r#"<?php
return [
    'throttle' => [
        'message' => 'Too many attempts. Try again in :seconds seconds.',
    ],
];
"#;

#[test]
fn locates_key_across_multiple_locales() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN), ("es", "auth", AUTH_ES)]);
    let mut locs = Locator::default().locate(project.path(), "auth.failed");
    // Sort by file path so the assertion is order-independent.
    locs.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    assert_eq!(locs.len(), 2, "key exists in both locales");
    assert!(locs[0].file_path.ends_with("lang/en/auth.php"));
    assert!(locs[1].file_path.ends_with("lang/es/auth.php"));
    for loc in &locs {
        let content = fs::read_to_string(&loc.file_path).unwrap();
        let line = content.lines().nth(loc.location.line as usize).unwrap();
        let slice = &line[loc.location.start_column as usize..loc.location.end_column as usize];
        assert_eq!(slice, "failed");
    }
}

#[test]
fn skips_locale_without_the_key() {
    // en has 'auth.failed', es has 'auth.password' but no 'auth.failed' would
    // be impossible to express in this format — instead model a locale that
    // simply doesn't define the auth file at all.
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN)]);
    fs::create_dir_all(project.path().join("lang/es")).unwrap();

    let locs = Locator::default().locate(project.path(), "auth.failed");
    assert_eq!(locs.len(), 1, "only en defines auth.php");
    assert!(locs[0].file_path.ends_with("lang/en/auth.php"));
}

#[test]
fn handles_nested_keys() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_NESTED_EN)]);
    let locs = Locator::default().locate(project.path(), "auth.throttle.message");
    assert_eq!(locs.len(), 1);
    let loc = &locs[0];
    let content = fs::read_to_string(&loc.file_path).unwrap();
    let line = content.lines().nth(loc.location.line as usize).unwrap();
    let slice = &line[loc.location.start_column as usize..loc.location.end_column as usize];
    assert_eq!(slice, "message");
}

#[test]
fn returns_empty_when_no_lang_dir() {
    let dir = TempDir::new().unwrap();
    assert!(Locator::default()
        .locate(dir.path(), "auth.failed")
        .is_empty());
}

#[test]
fn returns_empty_for_missing_key_in_all_locales() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN), ("es", "auth", AUTH_ES)]);
    assert!(Locator::default()
        .locate(project.path(), "auth.missing")
        .is_empty());
}

#[test]
fn returns_empty_when_dotted_key_has_no_segments() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN)]);
    // A bare "auth" without anything after the dot can't reach a leaf.
    assert!(Locator::default().locate(project.path(), "auth").is_empty());
}

// ---------------------------------------------------------------------------
// Caching (issue #293)
//
// Containment is NOT re-tested here. This read is now fenced by the shared
// guard in `TranslationCache::ensure_file` — where it previously had none at
// all — and that guard's five regression tests live in
// `translation_lookup::tests`, which go red the moment it is removed. A
// traversal test aimed at *this* entry point would not discriminate anyway:
// the key is split on `.` before the stem is used, so a `../` escape leaves the
// stem empty and never reaches a read. The guard here is defence in depth
// against a stem naming an absolute path, not a closable hole.
// ---------------------------------------------------------------------------

#[test]
fn a_locale_file_is_read_once_across_repeated_renames() {
    let project = fake_project_with_lang(&[("en", "auth", AUTH_EN), ("es", "auth", AUTH_ES)]);
    let mut locator = Locator::default();

    let first = locator.locate(project.path(), "auth.failed");
    let after_first = locator.cache.disk_reads();
    let second = locator.locate(project.path(), "auth.failed");
    let after_second = locator.cache.disk_reads();

    assert_eq!(first.len(), 2);
    assert_eq!(second, first);
    assert!(
        after_first > 0,
        "the first walk must actually read the catalogues"
    );
    assert_eq!(
        after_second, after_first,
        "a rename must not re-read and re-parse every locale's catalogue"
    );
}
