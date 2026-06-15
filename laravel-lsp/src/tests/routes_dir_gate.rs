//! Unit tests for `is_in_routes_dir` — the gate deciding whether the
//! declaration-fallback route walk runs for a given file. Only the project
//! root's own `routes/` directory qualifies; a `routes` component nested
//! deeper (a package's `vendor/.../routes/`, or a Folio mount) must not be
//! mistaken for it (issue #98).

use crate::is_in_routes_dir;
use std::path::Path;

#[test]
fn matches_project_root_routes_dir() {
    assert!(is_in_routes_dir(
        Some(Path::new("/project")),
        Path::new("/project/routes/web.php")
    ));
}

#[test]
fn rejects_package_routes_below_root() {
    // A package's own `routes/` under vendor/ is not the project's route dir.
    assert!(!is_in_routes_dir(
        Some(Path::new("/project")),
        Path::new("/project/vendor/somepackage/routes/web.php")
    ));
}

#[test]
fn rejects_folio_style_nested_routes_component() {
    // A Folio page path with `routes` as a non-root component must not be
    // treated as the conventional routes dir (issue #98).
    assert!(!is_in_routes_dir(
        Some(Path::new("/project")),
        Path::new("/project/pages/routes/index.php")
    ));
}

#[test]
fn rejects_false_prefix_sibling() {
    // A sibling directory whose name merely *starts with* `routes` (e.g.
    // `routesX/`) is not the routes dir. The gate matches path components,
    // not string prefixes — a future "optimization" to a raw prefix check
    // would silently reintroduce the bug (issue #98).
    assert!(!is_in_routes_dir(
        Some(Path::new("/project")),
        Path::new("/project/routesX/web.php")
    ));
}

#[test]
fn rejects_when_root_unknown() {
    // No project root to anchor against → never trigger the fallback walk.
    assert!(!is_in_routes_dir(None, Path::new("/any/routes/file.php")));
}
