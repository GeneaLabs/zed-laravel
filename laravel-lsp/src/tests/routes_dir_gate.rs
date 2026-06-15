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

#[cfg(unix)]
#[test]
fn matches_through_symlinked_root() {
    // `root` and `path` can resolve through different symlink states (issue
    // #122) — e.g. a stored root reached via a symlink vs. a per-request file
    // addressed by its real path. A raw textual `starts_with` would return a
    // false negative; canonicalizing both sides makes the gate survive it.
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    // Real project with a conventional routes/ dir and a route file on disk.
    let real_root = tmp.path().join("real_project");
    std::fs::create_dir_all(real_root.join("routes")).unwrap();
    let route_file = real_root.join("routes").join("web.php");
    std::fs::write(&route_file, "<?php\n").unwrap();

    // A symlink standing in for the stored project root.
    let link_root = tmp.path().join("link_project");
    symlink(&real_root, &link_root).unwrap();

    // Sanity: the raw textual prefix check fails here — `link_project/routes`
    // is not a textual prefix of `real_project/routes/web.php`. Canonicalizing
    // both is what makes the gate return true.
    assert!(!route_file.starts_with(link_root.join("routes")));
    assert!(is_in_routes_dir(Some(&link_root), &route_file));
}

#[test]
fn rejects_real_path_outside_routes_dir() {
    // Both the file and the routes/ dir exist on disk, so the canonical branch
    // runs — a path outside routes/ must still return false in canonical form.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("routes")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    let file = root.join("app").join("Model.php");
    std::fs::write(&file, "<?php\n").unwrap();

    assert!(!is_in_routes_dir(Some(&root), &file));
}

#[test]
fn falls_back_to_textual_when_path_missing() {
    // A path that doesn't exist can't be canonicalized; the gate must fall back
    // to the textual component check rather than panic (issue #122). Create a
    // real `routes/` dir on disk so `routes_dir` canonicalizes but the missing
    // `path` does not — genuinely driving the `(Err, Ok)` arm. This is the
    // realistic case the production doc comment calls out: a brand-new route
    // file still in the editor buffer, not yet saved to disk, sitting inside a
    // real `routes/` directory.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("routes")).unwrap();
    let missing = root.join("routes").join("does_not_exist.php");

    // No panic, and the textual prefix still matches the project routes/ dir.
    assert!(is_in_routes_dir(Some(&root), &missing));

    // False-negative side: a missing path *outside* routes/ must still return
    // false through the same textual fallback (routes_dir canonicalizes, the
    // path does not — again the `(Err, Ok)` arm).
    let missing_outside = root.join("app").join("does_not_exist.php");
    assert!(!is_in_routes_dir(Some(&root), &missing_outside));
}
