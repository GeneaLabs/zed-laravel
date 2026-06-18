//! Emit-safe "Expected at:" containment for the remaining `from_diagnostic →
//! CreateFile` diagnostic surfaces (issue #214), extending the view/component
//! coverage in `view_diagnostic_containment.rs` (#194/#201).
//!
//! Every "not found" diagnostic in this family echoes an `Expected at: <path>`
//! line back to the client, and [`crate::FileAction::from_diagnostic`] parses
//! that line into a `CreateFile` target a client could follow. Issue #201 routed
//! the four view/component surfaces through [`crate::in_root_expected_path_hint`]
//! — the *emit-safe* guard ([`crate::path_containment::path_within_root_emit_safe`])
//! that refuses an out-of-root candidate (and a dangling under-root symlink)
//! while still admitting a genuinely-absent in-root path (the hint is for a
//! *missing* file). This file closes the rest of the family so the invariant
//! holds uniformly across *every* surface:
//!
//! | Surface     | `expected_path` source                              | Escape vector closed |
//! |-------------|-----------------------------------------------------|----------------------|
//! | Translation | `vendor_map.get(namespace)` (user-registered)       | out-of-root vendor lang dir |
//! | Config      | `root.join("config")` (structurally in-root)        | uniform invariant + dangling symlink |
//! | Middleware  | `resolve_class_to_file` (PSR-4 from class name)     | `..`-injected / out-of-root PSR-4 map |
//! | Feature     | `resolve_class_to_file` + `root.join("app/Features")` | as middleware + uniform invariant |
//! | Env         | `root.join(".env")` (structurally in-root)          | dangling `.env` symlink + uniform invariant |
//! | Inertia page | `page_create_path` → `root.join("resources/js/Pages")` (structurally in-root) | dangling page symlink + uniform invariant (issue #220) |
//!
//! Each surface gets two cases, mirroring the #201 component test:
//! a **regression** test (every candidate out-of-root → `"unknown"`, never a
//! leaked absolute path) and a **positive control** (a single in-root but
//! *missing* candidate is returned as-is, so the guard never drops a legitimate
//! "Expected at:" hint for a normal not-yet-created file). All cases drive the
//! shared helper directly, the same style the existing containment tests use.

use crate::in_root_expected_path_hint;

// ---------------------------------------------------------------------------
// Translation "not found" — `check_translation_file` → `create_translation_diagnostic`
// ---------------------------------------------------------------------------
//
// A namespaced key (`pkg::messages.hi`) builds `expected_path` from
// `vendor_map.get("pkg")` — a user-registered path a package can point outside
// the project root. That out-of-root absolute path must never reach the hint.

#[test]
fn translation_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    // The shape a `Lang::addNamespace('pkg', '/abs/out/of/tree')` registration
    // produces: the vendor lang dir resolves outside the project root.
    let escapee = outside.path().join("pkg/en/messages.php");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root vendor-map translation path must never be echoed into the \
         \"Expected at:\" hint — it falls back to \"unknown\""
    );
}

#[test]
fn translation_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    // The everyday case: a published vendor lang file that doesn't exist yet.
    let in_root = root.path().join("lang/vendor/pkg/en/messages.php");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root translation candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root translation path is a legitimate create \
         target and must be surfaced unchanged"
    );
}

// ---------------------------------------------------------------------------
// Config "not found" — `check_config_file` → `create_config_diagnostic`
// ---------------------------------------------------------------------------
//
// `config_path` is `root.join("config")`-rooted, so structurally in-root, but
// routing it keeps the guarantee uniform and refuses a dangling under-root
// symlink the client could follow out of the tree on create.

#[test]
fn config_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("config/app.php");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root config path must never be echoed into the \"Expected \
         at:\" hint — it falls back to \"unknown\""
    );
}

#[test]
fn config_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    let in_root = root.path().join("config/app.php");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root config candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root config path is a legitimate create target \
         and must be surfaced unchanged"
    );
}

// ---------------------------------------------------------------------------
// Middleware "not found" — `resolve_class_to_file` / registry class path
// ---------------------------------------------------------------------------
//
// The class path is mapped from a user-controlled class name through PSR-4
// (`App\Http\Middleware\X` → `app/Http/Middleware/X.php`). A `..`-injected name
// or an out-of-root PSR-4 mapping can escape the root; the hint must not leak it.

#[test]
fn middleware_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("app/Http/Middleware/Authenticate.php");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root middleware class path must never be echoed into the \
         \"Expected at:\" hint — it falls back to \"unknown\""
    );
}

#[test]
fn middleware_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    let in_root = root.path().join("app/Http/Middleware/Authenticate.php");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root middleware candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root middleware class path is a legitimate create \
         target and must be surfaced unchanged"
    );
}

// ---------------------------------------------------------------------------
// Feature "not found" — `resolve_class_to_file` / `root.join("app/Features")`
// ---------------------------------------------------------------------------
//
// A Pennant feature class reference resolves through the same PSR-4 mapping as
// middleware; the string-key/`@feature` forms build `root.join("app/Features")`.
// The hint must hold the same containment guarantee.

#[test]
fn feature_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("app/Features/NewApi.php");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root feature class path must never be echoed into the \
         \"Expected at:\" hint — it falls back to \"unknown\""
    );
}

#[test]
fn feature_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    let in_root = root.path().join("app/Features/NewApi.php");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root feature candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root feature class path is a legitimate create \
         target and must be surfaced unchanged"
    );
}

// ---------------------------------------------------------------------------
// Environment variable "not found" — `root.join(".env")`
// ---------------------------------------------------------------------------
//
// The `.env` path is `root.join`-rooted (the variable name never enters the
// path), so the path-string itself can't escape — but the emit-safe guard still
// closes a dangling `.env` under-root symlink a malicious repo could ship, and
// routing it keeps the invariant uniform across every surface. The
// out-of-root regression case below documents the helper's contract for this
// surface alongside the others.

#[test]
fn env_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join(".env");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root .env path must never be echoed into the \"Expected at:\" \
         hint — it falls back to \"unknown\""
    );
}

#[test]
fn env_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    let in_root = root.path().join(".env");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root .env candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root .env path is a legitimate create target and \
         must be surfaced unchanged"
    );
}

#[cfg(unix)]
#[test]
fn env_hint_skips_dangling_under_root_symlink() {
    // The narrow escape this surface's guard actually closes: a repo ships
    // `.env` as a dangling under-root symlink whose target was never created.
    // Its path is lexically inside the root, but a client `CreateFile` following
    // the link could write outside the project tree, so the hint must refuse it
    // and fall back to "unknown" rather than leak the dangling link path.
    let root = tempfile::TempDir::new().unwrap();
    let missing_target = root.path().join("../never-created.env");
    let dangling = root.path().join(".env");
    std::os::unix::fs::symlink(&missing_target, &dangling).unwrap();

    assert!(
        dangling.canonicalize().is_err(),
        "precondition: the candidate is a dangling symlink (can't canonicalize)"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&dangling), root.path()),
        "unknown",
        "a dangling under-root .env symlink must not be echoed into the hint — \
         it falls back to \"unknown\""
    );
}

// ---------------------------------------------------------------------------
// Inertia "page not found" — `page_create_path` → `resources/js/Pages/<page>.<ext>`
// ---------------------------------------------------------------------------
//
// `inertia_not_found_diagnostic` builds `expected_path` via
// `inertia::page_create_path`, which `root.join`s an `is_valid_page_name`-validated
// name under `resources/js/Pages` — so `..`-injection and absolute-path escapes are
// already blocked structurally. The one residual the emit-safe guard closes here is
// a dangling under-root symlink at the page path (e.g. `resources/js/Pages/Foo.vue`),
// whose target a client could follow out of the tree on `CreateFile`. Routing it
// keeps the containment invariant uniform across every surface (issue #220). The
// out-of-root regression case below documents the helper's contract for this surface
// alongside the others.

#[test]
fn inertia_page_hint_is_unknown_when_all_candidates_out_of_root() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("resources/js/Pages/Dashboard.vue");

    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&escapee), root.path()),
        "unknown",
        "an out-of-root Inertia page path must never be echoed into the \"Expected \
         at:\" hint — it falls back to \"unknown\""
    );
}

#[test]
fn inertia_page_hint_keeps_in_root_missing_candidate() {
    let root = tempfile::TempDir::new().unwrap();
    // The everyday case: an Inertia page that doesn't exist on disk yet.
    let in_root = root.path().join("resources/js/Pages/Auth/Login.vue");

    assert!(
        in_root.canonicalize().is_err(),
        "precondition: the in-root Inertia page candidate does not exist on disk"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&in_root), root.path()),
        in_root.to_string_lossy(),
        "a genuinely-absent in-root Inertia page path is a legitimate create \
         target and must be surfaced unchanged"
    );
}

#[cfg(unix)]
#[test]
fn inertia_page_hint_skips_dangling_under_root_symlink() {
    // A hostile repo ships `resources/js/Pages/Foo.vue` as a dangling under-root
    // symlink (`→ ../../../outside`) whose target was never created. The link path
    // is lexically inside the root, but a client `CreateFile` following it could
    // write outside the project tree, so the hint must refuse it and fall back to
    // "unknown" rather than leak the symlink's link path.
    let root = tempfile::TempDir::new().unwrap();
    let pages_dir = root.path().join("resources/js/Pages");
    std::fs::create_dir_all(&pages_dir).unwrap();
    let missing_target = std::path::Path::new("../../../outside");
    let dangling = pages_dir.join("Foo.vue");
    std::os::unix::fs::symlink(missing_target, &dangling).unwrap();

    assert!(
        dangling.canonicalize().is_err(),
        "precondition: the candidate is a dangling symlink (can't canonicalize)"
    );
    assert_eq!(
        in_root_expected_path_hint(std::slice::from_ref(&dangling), root.path()),
        "unknown",
        "a dangling under-root Inertia page symlink must not be echoed into the \
         hint — it falls back to \"unknown\""
    );
}
