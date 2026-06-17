//! Tests for the root-containment guard in the **diagnostic-validation** view
//! loops (issue #194, the secondary surface — extending #130/#143/#148).
//!
//! `validate_and_publish_diagnostics` walks both the PHP `view()` references and
//! the Blade `@extends`/`@include` directives, resolving each to candidate paths
//! via `config.resolve_view_path` and deciding whether a "View file not found"
//! diagnostic should fire. Both loops make the *identical* decision through the
//! free helper [`crate::any_in_root_candidate_exists`]: a candidate is only
//! stat-probed if it lexically resolves within the project root.
//!
//! A `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')`-style registration (or a
//! `component_namespaces` entry) can map a namespace to an absolute directory
//! that escapes the project root. Without the containment filter, an out-of-root
//! candidate whose file happens to *exist on disk* would leave `exists = true`
//! and suppress the diagnostic, and — more importantly — the loop would
//! `stat`-probe a path outside the project tree during diagnostics. The filter
//! refuses such candidates *before* probing them.
//!
//! These tests drive the shared helper directly (mirroring the
//! `crate::is_in_routes_dir` style in `routes_dir_gate.rs` and the disk-backed
//! containment cases in `view_navigation_containment.rs`), pinning the exact
//! fork: out-of-root-but-exists is treated as missing, while a speculative
//! in-root candidate is still probed so a genuinely missing in-root view keeps
//! reporting "not found".

use crate::any_in_root_candidate_exists;
use std::fs;
use std::path::Path;

/// The Blade view contents are irrelevant — only that the file exists on disk,
/// so a `false` result can only come from the containment filter, never a
/// missing file.
const VIEW_BODY: &str = "<div>card</div>\n";

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn out_of_root_candidate_that_exists_is_treated_as_missing() {
    // The `pkg` namespace points OUTSIDE the project root (the shape a
    // `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')` registration produces). The
    // resolved candidate exists on disk, so without the containment filter the
    // diagnostic loop would see `exists = true` and stay quiet — having probed a
    // file outside the project tree. The filter must refuse it on root grounds
    // alone, so the helper reports the candidate absent and a "View file not
    // found" diagnostic fires.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("card.blade.php");
    write(&escapee, VIEW_BODY);

    assert!(
        escapee.exists(),
        "precondition: the out-of-root candidate exists on disk"
    );
    assert!(
        !any_in_root_candidate_exists(&[escapee], root.path()),
        "an out-of-root candidate must be treated as absent even though it \
         exists on disk — the containment filter refuses it before probing, so \
         the diagnostic fires"
    );
}

#[test]
fn in_root_candidate_that_exists_is_found() {
    // Positive control: an in-root candidate that exists on disk passes
    // containment and is found, so no false-positive "View file not found"
    // diagnostic fires — no regression to the normal happy path.
    let root = tempfile::TempDir::new().unwrap();
    let card = root.path().join("resources/views/card.blade.php");
    write(&card, VIEW_BODY);

    assert!(
        any_in_root_candidate_exists(&[card], root.path()),
        "an in-root candidate that exists on disk must be found — the diagnostic \
         must not fire for a real in-root view"
    );
}

#[test]
fn speculative_in_root_candidate_reports_missing() {
    // A not-yet-created in-root candidate (the everyday case: the developer
    // referenced a view they haven't built). Lexical containment keeps it — it
    // is *not* filtered out as an escape — so it is probed, found absent, and
    // "View file not found" correctly fires. This proves the filter uses lexical
    // (not fail-closed) containment: a fail-closed guard would drop every
    // not-yet-created candidate and break the missing-view diagnostic entirely.
    let root = tempfile::TempDir::new().unwrap();
    let ghost = root.path().join("resources/views/ghost.blade.php");

    assert!(
        ghost.canonicalize().is_err(),
        "precondition: the in-root candidate does not exist on disk"
    );
    assert!(
        !any_in_root_candidate_exists(&[ghost], root.path()),
        "a missing in-root view must still report absent — lexical containment \
         keeps speculative in-root candidates so the diagnostic still fires"
    );
}

#[test]
fn out_of_root_existing_does_not_mask_missing_in_root() {
    // Belt-and-braces over the realistic multi-candidate shape: `resolve_view_path`
    // returns several candidates. When the only one that exists on disk is
    // out-of-root and the in-root candidate is missing, the helper must still
    // report absent — the out-of-root hit is filtered out and cannot mask the
    // genuinely-missing in-root view, so the diagnostic fires.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let in_root_missing = root.path().join("resources/views/card.blade.php");
    let out_of_root_present = outside.path().join("card.blade.php");
    write(&out_of_root_present, VIEW_BODY);

    assert!(
        !any_in_root_candidate_exists(&[in_root_missing, out_of_root_present], root.path()),
        "an existing out-of-root candidate must not mask a missing in-root one — \
         it is filtered out before probing, so the diagnostic fires"
    );
}
