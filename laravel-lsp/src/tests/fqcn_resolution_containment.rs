//! FQCN → file resolution containment for the heuristic class locator (issue
//! #218), extending the `path_within_root` containment lineage
//! (#130 → #143 → #148 → #194 → #199 → #201 → #214) to the last FS-touching
//! resolver that lacked the guard.
//!
//! `find_php_class_file_by_fqcn` (in `class_locator.rs`) maps an FQCN to a
//! candidate path by splitting on `\` and `PathBuf::join`-ing the segments —
//! and `join` does not resolve `..` on a non-absolute segment, it appends it
//! literally. So an FQCN carrying `..` segments (e.g. `App\Models\..\..\etc\X`)
//! yields a path like `<root>/app/Models/../../etc/X.php` that, once stat'd by
//! `path.exists()`, can read a file *outside* the project root — a read
//! primitive that escapes the tree. The same is true for a candidate whose path
//! crosses an under-root symlink resolving outside the root.
//!
//! The fix gates every candidate with the fail-closed
//! [`laravel_lsp::path_containment::path_within_root`] guard before the on-disk
//! check, in both the app (`!search_vendor`) and vendor (`search_vendor`)
//! branches. These tests pin that invariant.
//!
//! ## Why the public entry points, not the private helper
//!
//! `find_php_class_file_by_fqcn` is a *private* heuristic fallback — it has no
//! reason to be part of the crate's public API, and `class_locator`'s existing
//! tests (`class_locator_and_properties.rs`) likewise drive the public
//! `find_php_class_file`. So these tests exercise the guard through the two
//! public callers that reach the two branches:
//!   - [`laravel_lsp::class_locator::find_php_class_file`] → the app branch
//!     (`search_vendor = false`).
//!   - [`laravel_lsp::class_locator::find_php_class_file_in_app_or_vendor`] →
//!     the vendor branch (`search_vendor = true`); for a vendor-shaped FQCN the
//!     app branch returns `None` first (its first namespace segment is not
//!     `app`), so the vendor branch's guard is what decides the outcome.
//!
//! Each case is *discriminating*: the escaping file is written to disk OUTSIDE
//! the root, so without the guard the resolver would `path.exists()` it and
//! return `Some(<out-of-root path>)`. A `None` result can therefore only come
//! from the containment guard, never from absence — the precondition assertions
//! make that explicit.

use laravel_lsp::class_locator::{find_php_class_file, find_php_class_file_in_app_or_vendor};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// App branch (`search_vendor = false`) — `find_php_class_file`
// ---------------------------------------------------------------------------

#[test]
fn app_fqcn_with_dotdot_escaping_root_is_refused() {
    // Layout: an empty project root with an `app/` dir, and a secret PHP file
    // OUTSIDE the root. The FQCN `App\..\..\secret` builds the candidate
    // `<root>/app/../../secret.php`, which `PathBuf::join` leaves literal and
    // canonicalizes to `<tmp>/secret.php` — outside the project root.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("app")).unwrap();
    let secret = tmp.path().join("secret.php");
    std::fs::write(&secret, "<?php\nclass secret {}").unwrap();

    // Precondition: the candidate the app branch builds exists on disk and
    // resolves OUTSIDE the root — so a `None` result proves the guard fired, not
    // mere absence. (Without the guard, `path.exists()` returns true here and
    // the resolver returns `Some`.)
    let candidate = root.join("app").join("..").join("..").join("secret.php");
    assert_eq!(
        candidate.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the `..` candidate resolves to the out-of-root secret file"
    );

    assert_eq!(
        find_php_class_file("App\\..\\..\\secret", &root),
        None,
        "an FQCN whose `..` segments escape the project root must be refused by \
         the fail-closed path_within_root guard, not read"
    );
}

#[test]
fn app_fqcn_in_root_resolves_to_some_path() {
    // Positive control: a normal in-root FQCN with the file present must still
    // resolve — the guard must not drop legitimate in-root candidates.
    let root = TempDir::new().unwrap();
    let user = root.path().join("app").join("Models").join("User.php");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(&user, "<?php\nnamespace App\\Models;\nclass User {}").unwrap();

    let found = find_php_class_file("App\\Models\\User", root.path())
        .expect("a normal in-root FQCN must resolve to its file");
    assert!(
        found.ends_with("app/Models/User.php"),
        "App\\Models\\User must resolve to app/Models/User.php; got {found:?}"
    );
}

#[cfg(unix)]
#[test]
fn app_fqcn_through_under_root_symlink_escaping_root_is_refused() {
    // An under-root path component is a symlink whose target is OUTSIDE the
    // root. The FQCN `App\Evil\Secret` builds `<root>/app/Evil/Secret.php`,
    // where `<root>/app/Evil` -> `<tmp>/outside`, so the candidate canonicalizes
    // to `<tmp>/outside/Secret.php` — outside the root, the #55/#134 symlink
    // escape leg the lexical fallback would admit.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("app")).unwrap();

    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("Secret.php");
    std::fs::write(&secret, "<?php\nclass Secret {}").unwrap();

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

    assert_eq!(
        find_php_class_file("App\\Evil\\Secret", &root),
        None,
        "an FQCN candidate whose path crosses an under-root symlink resolving \
         outside the root must be refused — the canonicalize-based guard catches \
         the escape the lexical fallback would admit"
    );
}

// ---------------------------------------------------------------------------
// Vendor branch (`search_vendor = true`) — `find_php_class_file_in_app_or_vendor`
// ---------------------------------------------------------------------------

#[test]
fn vendor_fqcn_with_dotdot_escaping_root_is_refused() {
    // The vendor branch builds `<root>/vendor/<vendor>/<pkg>[/src]/<rest>/<X>.php`.
    // A vendor-shaped FQCN with enough `..` segments climbs out of the vendor
    // tree and the root entirely: `Vendor\Pkg\..\..\..\..\secret` yields, for the
    // no-`src` candidate, `<root>/vendor/vendor/pkg/../../../../secret.php`
    // = `<tmp>/secret.php` — outside the root. (The app branch returns None
    // first: the FQCN's first segment is `vendor`, not `app`.)
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    // The vendor candidate's base must exist so the `..` path canonicalizes.
    std::fs::create_dir_all(root.join("vendor").join("vendor").join("pkg")).unwrap();
    let secret = tmp.path().join("secret.php");
    std::fs::write(&secret, "<?php\nclass secret {}").unwrap();

    // Precondition: the no-`src` vendor candidate exists on disk and resolves
    // OUTSIDE the root — so `None` proves the vendor-branch guard fired.
    let candidate = root
        .join("vendor")
        .join("vendor")
        .join("pkg")
        .join("..")
        .join("..")
        .join("..")
        .join("..")
        .join("secret.php");
    assert_eq!(
        candidate.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the vendor `..` candidate resolves to the out-of-root file"
    );

    assert_eq!(
        find_php_class_file_in_app_or_vendor("Vendor\\Pkg\\..\\..\\..\\..\\secret", &root),
        None,
        "a vendor-shaped FQCN whose `..` segments escape the project root must be \
         refused by the fail-closed guard in the vendor branch"
    );
}

#[test]
fn vendor_fqcn_in_root_resolves_to_some_path() {
    // Positive control for the vendor branch: a normal in-root vendor PSR-4 path
    // must still resolve. (`find_php_class_file_in_app_or_vendor` is the only
    // entry point that searches vendor.)
    let root = TempDir::new().unwrap();
    let token = root
        .path()
        .join("vendor")
        .join("laravel")
        .join("passport")
        .join("src")
        .join("Token.php");
    std::fs::create_dir_all(token.parent().unwrap()).unwrap();
    std::fs::write(
        &token,
        "<?php\nnamespace Laravel\\Passport;\nclass Token {}",
    )
    .unwrap();

    let found = find_php_class_file_in_app_or_vendor("Laravel\\Passport\\Token", root.path())
        .expect("a normal in-root vendor FQCN must resolve to its file");
    assert!(
        found.ends_with("vendor/laravel/passport/src/Token.php"),
        "Laravel\\Passport\\Token must resolve to its PSR-4 path; got {found:?}"
    );
}
