use super::*;
use tempfile::TempDir;

/// Build `<temp>/<name>` and drop a marker file inside it.
fn seeded_dir(base: &Path, name: &str, marker: &str) -> PathBuf {
    let dir = base.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("marker.txt"), marker).unwrap();
    dir
}

#[test]
fn moves_the_legacy_cache_when_only_the_legacy_cache_exists() {
    let temp = TempDir::new().unwrap();
    let legacy = seeded_dir(temp.path(), "org.mike-bronner.laravel-lsp", "warm");
    let new = temp.path().join("org.mike-bronner.laravel-ce-lsp");

    assert_eq!(migrate_legacy_cache(&legacy, &new), Migration::Moved);

    assert_eq!(
        fs::read_to_string(new.join("marker.txt")).unwrap(),
        "warm",
        "the legacy cache contents must land under the new name"
    );
    assert!(
        !legacy.exists(),
        "a rename leaves nothing behind at the old path"
    );
}

#[test]
fn migrates_nested_contents_not_just_the_top_level() {
    let temp = TempDir::new().unwrap();
    let legacy = temp.path().join("legacy");
    let project = legacy.join("deadbeef");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("pattern_cache.bin"), b"cached").unwrap();
    let new = temp.path().join("new");

    assert_eq!(migrate_legacy_cache(&legacy, &new), Migration::Moved);

    assert_eq!(
        fs::read(new.join("deadbeef").join("pattern_cache.bin")).unwrap(),
        b"cached",
        "per-project subdirectories must survive the move"
    );
}

#[test]
fn fresh_install_migrates_nothing_and_creates_nothing() {
    let temp = TempDir::new().unwrap();
    let legacy = temp.path().join("legacy");
    let new = temp.path().join("new");

    assert_eq!(
        migrate_legacy_cache(&legacy, &new),
        Migration::NoLegacyCache
    );

    assert!(
        !new.exists(),
        "migration must not fabricate a cache directory; the write paths create it on demand"
    );
    assert!(!legacy.exists());
}

#[test]
fn prefers_the_new_cache_and_leaves_the_legacy_one_alone_when_both_exist() {
    let temp = TempDir::new().unwrap();
    let legacy = seeded_dir(temp.path(), "legacy", "stale");
    let new = seeded_dir(temp.path(), "new", "current");

    assert_eq!(
        migrate_legacy_cache(&legacy, &new),
        Migration::AlreadyPresent
    );

    assert_eq!(
        fs::read_to_string(new.join("marker.txt")).unwrap(),
        "current",
        "an existing new cache must not be overwritten or merged"
    );
    assert_eq!(
        fs::read_to_string(legacy.join("marker.txt")).unwrap(),
        "stale",
        "the legacy cache is left untouched rather than destroyed"
    );
}

#[test]
fn creates_the_destination_parent_when_it_is_missing() {
    let temp = TempDir::new().unwrap();
    let legacy = seeded_dir(temp.path(), "legacy", "warm");
    // On Windows the new cache nests under its own application folder, which
    // won't exist yet on a machine that only ever ran the pre-rebrand build.
    let new = temp.path().join("mike-bronner").join("laravel-ce-lsp");

    assert_eq!(migrate_legacy_cache(&legacy, &new), Migration::Moved);

    assert_eq!(
        fs::read_to_string(new.join("marker.txt")).unwrap(),
        "warm",
        "a missing destination parent must be created, not treated as a failure"
    );
}

#[test]
fn reports_failure_when_the_destination_is_unusable() {
    let temp = TempDir::new().unwrap();
    let legacy = seeded_dir(temp.path(), "legacy", "warm");

    // A regular file where the destination's parent directory needs to be:
    // `create_dir_all` cannot turn a file into a directory.
    let blocker = temp.path().join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();
    let new = blocker.join("cache");

    assert_eq!(migrate_legacy_cache(&legacy, &new), Migration::Failed);

    assert_eq!(
        fs::read_to_string(legacy.join("marker.txt")).unwrap(),
        "warm",
        "a failed migration must leave the legacy cache recoverable"
    );
}

#[cfg(unix)]
#[test]
fn reports_failure_when_the_rename_itself_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    // Renaming a directory needs write permission on its *parent*. Stripping
    // it makes the rename fail while `create_dir_all` on the destination side
    // still succeeds, so this exercises the rename arm specifically.
    let jail = temp.path().join("jail");
    fs::create_dir_all(&jail).unwrap();
    let legacy = seeded_dir(&jail, "legacy", "warm");
    let new = temp.path().join("new");

    let original = fs::metadata(&jail).unwrap().permissions();
    fs::set_permissions(&jail, fs::Permissions::from_mode(0o500)).unwrap();

    let outcome = migrate_legacy_cache(&legacy, &new);

    // Restore before asserting so a failed assertion can't leave the
    // TempDir undeletable.
    fs::set_permissions(&jail, original).unwrap();

    assert_eq!(outcome, Migration::Failed);
    assert!(
        !new.exists(),
        "a refused rename must not leave a half-migrated directory"
    );
    assert_eq!(
        fs::read_to_string(legacy.join("marker.txt")).unwrap(),
        "warm",
        "a failed migration must leave the legacy cache recoverable"
    );
}

#[test]
fn cache_root_resolves_to_the_rebranded_name_and_is_stable() {
    let root = cache_root().expect("home directory is resolvable in the test environment");

    let path = root.to_string_lossy().into_owned();
    assert!(
        path.contains("laravel-ce-lsp"),
        "cache root should use the rebranded application name, got: {path}"
    );
    assert!(
        !path.contains("laravel-lsp"),
        "cache root must not still carry the pre-rebrand name, got: {path}"
    );
    assert_eq!(
        cache_root().unwrap(),
        root,
        "the root is resolved once and reused"
    );
}
