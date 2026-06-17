//! Tests for the root-containment guard in the **diagnostic-validation** view
//! loops (issue #194, the secondary surface — extending #130/#143/#148), and
//! the remaining diagnostic surfaces in the same family (issue #201): the
//! Livewire "component not found" fallback (which now shares the
//! `any_in_root_candidate_exists` decision) and the "Expected at:" message-hint
//! selection (`in_root_expected_path_hint`).
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
//! containment cases in `view_navigation_containment.rs`), confirming the
//! helper's behavior: an out-of-root candidate that exists on disk is treated
//! as absent, while a speculative in-root candidate still reports "not found"
//! (no false suppression of a genuinely missing view).
//!
//! Note the helper's boolean output is *equivalent* under lexical and
//! fail-closed containment — a missing in-root candidate yields `false` either
//! way (lexical keeps it and `.exists()` is `false`; fail-closed drops it so
//! `.any()` runs over an empty set). The lexical choice is for consistency with
//! the navigation-side filter (`salsa_impl.rs`, issue #156) and to avoid a
//! wasted stat, *not* because it changes any of these outcomes — so these tests
//! assert the helper's contract, not a distinction between the two policies.

use crate::{any_in_root_candidate_exists, in_root_expected_path_hint};
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
    // "View file not found" correctly fires. (The fail-closed `path_within_root`
    // would reach the same boolean: it drops the missing candidate so `.any()`
    // runs over an empty set → also `false`. The two policies are equivalent
    // here.) This case guards against a future filter that wrongly suppressed
    // the diagnostic for a genuinely missing in-root view.
    let root = tempfile::TempDir::new().unwrap();
    let ghost = root.path().join("resources/views/ghost.blade.php");

    assert!(
        ghost.canonicalize().is_err(),
        "precondition: the in-root candidate does not exist on disk"
    );
    assert!(
        !any_in_root_candidate_exists(&[ghost], root.path()),
        "a missing in-root view must still report absent — the containment \
         filter must not suppress the diagnostic for a speculative in-root view"
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

// ---------------------------------------------------------------------------
// Livewire diagnostic fallback (issue #201)
// ---------------------------------------------------------------------------
//
// The Livewire "component not found" diagnostic falls back to view-path
// resolution when no Livewire kind resolves. That fallback now shares the same
// `any_in_root_candidate_exists` decision as the `view()` and
// `@extends`/`@include` loops, so an out-of-root `loadViewsFrom`/
// `component_namespaces`-registered view can't make it stat-probe outside the
// project tree, and an out-of-root view that exists on disk can't silently
// satisfy the check.

#[test]
fn livewire_fallback_out_of_root_view_still_reports_not_found() {
    // The Livewire component's fallback view resolves OUTSIDE the project root
    // (the shape a `component_namespaces`/`loadViewsFrom` registration pointing
    // at an absolute out-of-tree directory produces). The file exists on disk,
    // so without the containment filter the fallback would see `exists = true`
    // and suppress the diagnostic — having stat-probed a file outside the
    // project. The filter refuses it on root grounds, so the helper reports the
    // candidate absent and a "Livewire component not found" diagnostic fires.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee = outside.path().join("counter.blade.php");
    write(&escapee, VIEW_BODY);

    assert!(
        escapee.exists(),
        "precondition: the out-of-root Livewire view candidate exists on disk"
    );
    assert!(
        !any_in_root_candidate_exists(&[escapee], root.path()),
        "an out-of-root Livewire-fallback view candidate must be treated as \
         absent even though it exists on disk — the containment filter refuses \
         it before probing, so the \"component not found\" diagnostic fires"
    );
}

#[test]
fn livewire_fallback_in_root_view_suppresses_diagnostic() {
    // Positive control: a Livewire component whose fallback view exists in-root
    // passes containment and is found, so no false "Livewire component not
    // found" diagnostic fires (e.g. a vendor-registered component view at a
    // non-conventional in-root path like Jetstream's `resources/views/api/`).
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/api/counter.blade.php");
    write(&view, VIEW_BODY);

    assert!(
        any_in_root_candidate_exists(&[view], root.path()),
        "an in-root Livewire-fallback view that exists must be found — the \
         diagnostic must not fire for a real in-root component view"
    );
}

// ---------------------------------------------------------------------------
// "Expected at:" message-hint containment (issue #201)
// ---------------------------------------------------------------------------
//
// The "not found" diagnostics echo an "Expected at:" path back to the client.
// `in_root_expected_path_hint` sources it from the first *in-root* candidate so
// a maliciously-registered out-of-root namespace path is never leaked into the
// message text — it considers only lexical containment, never disk existence.

#[test]
fn expected_path_hint_is_unknown_when_all_candidates_out_of_root() {
    // Every resolved candidate is outside the project root (the shape an
    // out-of-root `loadViewsFrom`/namespace registration produces with no
    // in-root fallback). The hint must NOT leak an out-of-root absolute path —
    // it falls back to "unknown".
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let escapee_a = outside.path().join("pkg/card.blade.php");
    let escapee_b = outside.path().join("other/card.blade.php");

    assert_eq!(
        in_root_expected_path_hint(&[escapee_a, escapee_b], root.path()),
        "unknown",
        "when every candidate is out-of-root the hint must be \"unknown\", never \
         a leaked out-of-root absolute path"
    );
}

#[test]
fn expected_path_hint_picks_first_in_root_candidate() {
    // Positive control: with a mix of out-of-root and in-root candidates, the
    // hint is the first *in-root* one — the out-of-root candidate ordered ahead
    // of it is skipped, so a real, safe expected path is still surfaced. The
    // in-root candidate need not exist on disk (the hint is for a *missing*
    // view); only lexical containment is checked.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();

    let out_of_root = outside.path().join("pkg/card.blade.php");
    let in_root = root.path().join("resources/views/card.blade.php");

    assert_eq!(
        in_root_expected_path_hint(&[out_of_root, in_root.clone()], root.path()),
        in_root.to_string_lossy().to_string(),
        "the hint must be the first in-root candidate, skipping the out-of-root \
         one ordered ahead of it"
    );
}
