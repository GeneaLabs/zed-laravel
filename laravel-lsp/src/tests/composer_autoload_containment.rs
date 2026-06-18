//! PSR-4 FQCN → file resolution containment for the Composer autoload resolver
//! (issue #222), extending the `path_within_root` containment lineage
//! (#130 → #143 → #148 → #194 → #199 → #201 → #214 → #218) to the
//! higher-priority FS-touching resolver that still lacked the guard.
//!
//! `ComposerAutoload::resolve` (in `composer_autoload.rs`) maps a PSR-4 FQCN to
//! a candidate file by splitting the post-prefix remainder on `\` and
//! `PathBuf::push`-ing each segment onto the mapped `source_root`, then returns
//! the candidate on a bare `candidate.exists()`. `push`/`join` appends a `..`
//! segment literally (it does not resolve it), and `source_root` is itself
//! derived from a PSR-4 mapping value in `composer.json` /
//! `vendor/composer/installed.json`. So a `..`-bearing FQCN — or a mapping /
//! under-root symlink pointing outside the tree — yields a candidate that
//! escapes the project root and is then `stat`'d and returned: the same
//! out-of-root read-primitive shape the lineage exists to close.
//!
//! `resolve` is the *higher-priority* branch in `class_locator.rs` — it runs
//! before the heuristic `find_php_class_file_by_fqcn` that #218/PR #221 guarded
//! — yet was the one FS-touching resolver in the lineage with no containment
//! guard. The fix gates every candidate with the fail-closed
//! [`laravel_lsp::path_containment::path_within_root`] guard before the on-disk
//! check; the project root is stored on `ComposerAutoload` at construction
//! (`load` / `for_project` both already receive it), so resolution is bound to
//! exactly the root the PSR-4 mappings were resolved against. These tests pin
//! that invariant.
//!
//! Each case is *discriminating*: the escaping file is written to disk OUTSIDE
//! the root, so without the guard the resolver would `candidate.exists()` it and
//! return `Some(<out-of-root path>)`. A `None` result can therefore only come
//! from the containment guard, never from absence — the precondition assertions
//! make that explicit.

use laravel_lsp::composer_autoload::ComposerAutoload;
use std::path::Path;
use tempfile::TempDir;

/// Write a file, creating parent directories as needed.
fn write_file(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

// ---------------------------------------------------------------------------
// Negative: `..` segments in the FQCN escape the root
// ---------------------------------------------------------------------------

#[test]
fn psr4_fqcn_with_dotdot_escaping_root_is_refused() {
    // composer.json maps `App\` → `app/`. The FQCN `App\..\..\secret` builds the
    // candidate `<root>/app/../../secret.php`, which `PathBuf::push` leaves
    // literal and canonicalizes to `<tmp>/secret.php` — outside the project root.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    // `app/` must exist so the `..` candidate canonicalizes (canonicalize
    // resolves each component, so `app/..` requires `app` to be a real dir).
    std::fs::create_dir_all(root.join("app")).unwrap();
    write_file(
        &root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    );

    let secret = tmp.path().join("secret.php");
    std::fs::write(&secret, "<?php\nclass secret {}").unwrap();

    // Precondition: the candidate `resolve` builds exists on disk and resolves
    // OUTSIDE the root — so a `None` result proves the guard fired, not mere
    // absence. (Without the guard, `candidate.exists()` is true and `resolve`
    // returns `Some`.)
    let candidate = root.join("app").join("..").join("..").join("secret.php");
    assert_eq!(
        candidate.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the `..` candidate resolves to the out-of-root secret file"
    );

    let autoload = ComposerAutoload::load(&root);
    assert_eq!(
        autoload.resolve("App\\..\\..\\secret"),
        None,
        "a PSR-4 FQCN whose `..` segments escape the project root must be refused \
         by the fail-closed path_within_root guard, not read"
    );
}

// ---------------------------------------------------------------------------
// Positive control: a normal in-root PSR-4 FQCN still resolves
// ---------------------------------------------------------------------------

#[test]
fn psr4_fqcn_in_root_resolves_to_some_path() {
    // The guard must not drop legitimate in-root candidates: a normal `App\`
    // PSR-4 mapping with the file present must still resolve.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_file(
        &root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    );
    write_file(
        &root.join("app").join("Models").join("User.php"),
        "<?php\nnamespace App\\Models;\nclass User {}",
    );

    let autoload = ComposerAutoload::load(&root);
    let resolved = autoload
        .resolve("App\\Models\\User")
        .expect("a normal in-root PSR-4 FQCN must resolve to its file");
    assert!(
        resolved.ends_with("app/Models/User.php"),
        "App\\Models\\User must resolve to app/Models/User.php; got {resolved:?}"
    );
}

// ---------------------------------------------------------------------------
// Negative (unix): an under-root symlink in the PSR-4 source path escapes
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn psr4_fqcn_through_under_root_symlink_escaping_root_is_refused() {
    // An under-root path component is a symlink whose target is OUTSIDE the
    // root. composer.json maps `App\` → `app/`; the FQCN `App\Evil\Secret`
    // builds `<root>/app/Evil/Secret.php`, where `<root>/app/Evil` -> `<tmp>/outside`,
    // so the candidate canonicalizes to `<tmp>/outside/Secret.php` — the #55/#134
    // symlink escape leg a lexical fallback would admit but the canonicalize-based
    // fail-closed guard catches.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("app")).unwrap();
    write_file(
        &root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    );

    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("Secret.php");
    std::fs::write(&secret, "<?php\nnamespace App\\Evil;\nclass Secret {}").unwrap();

    let evil_link = root.join("app").join("Evil");
    std::os::unix::fs::symlink(&outside, &evil_link).unwrap();

    // Precondition: the candidate exists through the symlink and resolves
    // outside the root — so `None` can only be the guard refusing it.
    let candidate = root.join("app").join("Evil").join("Secret.php");
    assert_eq!(
        candidate.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the candidate resolves through the symlink to the \
         out-of-root file"
    );
    assert!(
        !candidate
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the resolved target escapes the project root"
    );

    let autoload = ComposerAutoload::load(&root);
    assert_eq!(
        autoload.resolve("App\\Evil\\Secret"),
        None,
        "a PSR-4 FQCN candidate whose path crosses an under-root symlink resolving \
         outside the root must be refused — the canonicalize-based guard catches \
         the escape the lexical fallback would admit"
    );
}
