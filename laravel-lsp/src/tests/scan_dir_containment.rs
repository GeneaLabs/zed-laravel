//! Discovered-path containment for the component-completion directory walk,
//! extending the `path_within_root` containment lineage
//! (#130 → #143 → #148 → #194 → #199 → #201 → #214 → #218 → #222 → #226) to its
//! discovered-path leg (issue #228).
//!
//! PR #227 (issue #226) gated the *walk root* of `scan_dir`
//! (`ComposerAutoload::resolve_namespace_dirs` → `scan_class_dir`) with the
//! fail-closed `path_within_root` guard, so the directory a walk starts from can
//! no longer be out-of-root. It explicitly deferred the *discovered-path* leg:
//! `scan_dir` walks with `WalkDir::new(dir).follow_links(true)`, so a symlink
//! encountered *inside* an in-root `dir` whose target escapes the project root
//! was still followed and its files emitted as `<x-...>` completion candidates.
//!
//! `scan_dir` (via the public `scan_anonymous_dir` / `scan_class_dir`) now runs
//! every discovered entry through `path_within_root_walk_entry` — the same
//! canonicalize-based, fail-closed semantics — before emitting it, dropping any
//! entry whose real, symlink-resolved path escapes the root.
//!
//! Each negative case is *discriminating*: the escaping file is written to disk
//! OUTSIDE the root and reached through an under-root symlink, so without the
//! gate `follow_links(true)` would surface it and emit its candidate. Its absence
//! from the result can therefore only come from the containment gate, never from
//! the file not existing — the precondition assertions make that explicit. The
//! positive controls prove the gate does not over-refuse: an in-root symlink
//! (target inside the root) and an ordinary subdirectory still yield candidates.

use laravel_lsp::component_completion::{scan_class_dir, ComponentCandidate};
// Only the `#[cfg(unix)]` symlink tests below scan an anonymous dir.
#[cfg(unix)]
use laravel_lsp::component_completion::scan_anonymous_dir;
use tempfile::TempDir;

fn names(candidates: &[ComponentCandidate]) -> Vec<String> {
    candidates.iter().map(|c| c.name.clone()).collect()
}

// ---------------------------------------------------------------------------
// Negative (unix): a class-dir entry reached through an under-root symlink
// whose target escapes the root is NOT emitted
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn scan_class_dir_drops_entry_through_under_root_symlink_escaping_root() {
    // The walk root `<root>/app/View/Components` is in-root, but it contains a
    // symlink `Escape` -> `<tmp>/outside`. `follow_links(true)` descends it and
    // would surface `<root>/app/View/Components/Escape/Secret.php`, whose real
    // path is `<tmp>/outside/Secret.php` — outside the project root. The gate
    // must drop it. A legitimate in-root `Alert.php` is included so the walk is
    // proven to run and emit: only the escaping candidate must be missing.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let components = root.join("app").join("View").join("Components");
    std::fs::create_dir_all(&components).unwrap();
    std::fs::write(components.join("Alert.php"), "<?php class Alert {}").unwrap();

    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("Secret.php");
    std::fs::write(&secret, "<?php class Secret {}").unwrap();

    let escape_link = components.join("Escape");
    std::os::unix::fs::symlink(&outside, &escape_link).unwrap();

    // Precondition: the escaping entry exists through the symlink and resolves
    // OUTSIDE the root, so its absence can only be the gate refusing it.
    let escaping_entry = components.join("Escape").join("Secret.php");
    assert_eq!(
        escaping_entry.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the discovered entry resolves through the symlink to the \
         out-of-root file"
    );
    assert!(
        !escaping_entry
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the resolved entry escapes the project root"
    );

    let got = names(&scan_class_dir(&components, "", &root, &root));

    assert!(
        got.contains(&"alert".to_string()),
        "the in-root class must still be discovered — the walk ran; got {got:?}"
    );
    assert!(
        !got.contains(&"escape.secret".to_string()),
        "a class-dir entry reached through an under-root symlink resolving outside \
         the root must NOT be emitted as a candidate; got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Negative (unix): an anonymous (blade) entry reached through an under-root
// symlink whose target escapes the root is NOT emitted
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn scan_anonymous_dir_drops_entry_through_under_root_symlink_escaping_root() {
    // Same escape, exercised through the anonymous-blade branch of `scan_dir`.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let views = root.join("resources").join("views").join("components");
    std::fs::create_dir_all(&views).unwrap();
    std::fs::write(views.join("alert.blade.php"), "x").unwrap();

    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.blade.php");
    std::fs::write(&secret, "x").unwrap();

    let escape_link = views.join("escape");
    std::os::unix::fs::symlink(&outside, &escape_link).unwrap();

    // Precondition: the escaping entry exists through the symlink and resolves
    // OUTSIDE the root.
    let escaping_entry = views.join("escape").join("secret.blade.php");
    assert_eq!(
        escaping_entry.canonicalize().unwrap(),
        secret.canonicalize().unwrap(),
        "precondition: the discovered entry resolves through the symlink to the \
         out-of-root file"
    );
    assert!(
        !escaping_entry
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the resolved entry escapes the project root"
    );

    let got = names(&scan_anonymous_dir(&views, "", &root, &root));

    assert!(
        got.contains(&"alert".to_string()),
        "the in-root blade component must still be discovered; got {got:?}"
    );
    assert!(
        !got.contains(&"escape.secret".to_string()),
        "an anonymous entry reached through an under-root symlink resolving outside \
         the root must NOT be emitted as a candidate; got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Positive control (unix): an entry reached through an IN-root symlink (target
// inside the root) is still emitted — the gate does not over-refuse
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn scan_class_dir_still_emits_entry_through_in_root_symlink() {
    // A symlink `<root>/app/View/Components/Linked` -> `<root>/real-components`
    // (inside the root). `follow_links(true)` surfaces
    // `.../Linked/Button.php`, which canonicalizes to
    // `<root>/real-components/Button.php` — inside the root — so it must still be
    // emitted. This pins that the canonicalize-based gate refuses *escapes*, not
    // every symlink.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let components = root.join("app").join("View").join("Components");
    std::fs::create_dir_all(&components).unwrap();

    let real = root.join("real-components");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("Button.php"), "<?php class Button {}").unwrap();

    let link = components.join("Linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Precondition: the entry resolves through the symlink to a path INSIDE the
    // root, so its presence proves the gate admitted an in-root symlink.
    let entry = components.join("Linked").join("Button.php");
    assert!(
        entry
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()),
        "precondition: the entry resolves through the in-root symlink, staying \
         inside the root"
    );

    let got = names(&scan_class_dir(&components, "", &root, &root));
    assert!(
        got.contains(&"linked.button".to_string()),
        "an entry reached through an in-root symlink (target inside the root) must \
         still be emitted — the gate must not over-refuse; got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Positive control: ordinary in-root subdirectory entries are still emitted
// ---------------------------------------------------------------------------

#[test]
fn scan_class_dir_still_emits_ordinary_in_root_subdir() {
    // No symlinks at all: a plain nested subdirectory must still yield its
    // candidate after the gate is wired in (the common case must not regress).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let components = root.join("app").join("View").join("Components");
    std::fs::create_dir_all(components.join("Forms")).unwrap();
    std::fs::write(components.join("Alert.php"), "<?php class Alert {}").unwrap();
    std::fs::write(
        components.join("Forms").join("InputText.php"),
        "<?php class InputText {}",
    )
    .unwrap();

    let mut got = names(&scan_class_dir(&components, "", &root, &root));
    got.sort();
    assert_eq!(
        got,
        vec!["alert".to_string(), "forms.input-text".to_string()],
        "ordinary in-root class entries (including a nested subdir) must still be \
         emitted after the containment gate is applied"
    );
}
