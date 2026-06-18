use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Build a tempdir containing the given (relative-path, body) files.
fn dir_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for (relpath, body) in files {
        let full = dir.path().join(relpath);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

fn names(candidates: &[ComponentCandidate]) -> Vec<String> {
    candidates.iter().map(|c| c.name.clone()).collect()
}

// ─── name conversion ────────────────────────────────────────────────────

#[test]
fn to_kebab_case_handles_pascal_and_dots() {
    assert_eq!(to_kebab_case("Button"), "button");
    assert_eq!(to_kebab_case("AlertDialog"), "alert-dialog");
    assert_eq!(to_kebab_case("Forms.InputText"), "forms.input-text");
    assert_eq!(to_kebab_case("backstage"), "backstage");
}

#[test]
fn relative_path_to_tag_body_strips_and_dots() {
    assert_eq!(
        relative_path_to_tag_body(Path::new("backstage.blade.php")).as_deref(),
        Some("backstage"),
    );
    assert_eq!(
        relative_path_to_tag_body(Path::new("forms/input.blade.php")).as_deref(),
        Some("forms.input"),
    );
    assert_eq!(
        relative_path_to_tag_body(Path::new("Forms/InputText.php")).as_deref(),
        Some("forms.input-text"),
    );
    assert_eq!(relative_path_to_tag_body(Path::new("readme.md")), None);
}

// ─── anonymous (blade) scan ─────────────────────────────────────────────

#[test]
fn scan_anonymous_dir_emits_namespaced_blade_components() {
    let (_dir, root) = dir_with_files(&[
        ("views/test/components/backstage.blade.php", "x"),
        ("views/test/components/forms/input.blade.php", "x"),
        // A stray PHP class in the dir must be ignored by the anonymous scan.
        ("views/test/components/Ignored.php", "<?php"),
    ]);
    let scan_dir = root.join("views/test/components");

    let mut got = scan_anonymous_dir(&scan_dir, "test::", &root, &root);
    got.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(names(&got), vec!["test::backstage", "test::forms.input"]);
    assert!(got.iter().all(|c| c.kind == CandidateKind::AnonymousView));
}

#[test]
fn scan_anonymous_dir_with_empty_prefix_for_root_components() {
    let (_dir, root) = dir_with_files(&[("components/button.blade.php", "x")]);
    let got = scan_anonymous_dir(&root.join("components"), "", &root, &root);
    assert_eq!(names(&got), vec!["button"]);
}

#[test]
fn scan_missing_dir_is_empty() {
    let (_dir, root) = dir_with_files(&[]);
    assert!(scan_anonymous_dir(&root.join("nope"), "", &root, &root).is_empty());
}

// ─── class scan ─────────────────────────────────────────────────────────

#[test]
fn scan_class_dir_emits_kebab_class_components_and_skips_blade() {
    let (_dir, root) = dir_with_files(&[
        ("app/View/Components/Alert.php", "<?php class Alert {}"),
        ("app/View/Components/Forms/InputText.php", "<?php"),
        // A blade view sitting alongside classes must NOT be picked up here.
        ("app/View/Components/legacy.blade.php", "x"),
    ]);
    let scan_dir = root.join("app/View/Components");

    let mut got = scan_class_dir(&scan_dir, "", &root, &root);
    got.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(names(&got), vec!["alert", "forms.input-text"]);
    assert!(got.iter().all(|c| c.kind == CandidateKind::Class));
}

// ─── flux scan (issue #60) ──────────────────────────────────────────────

#[test]
fn collect_flux_components_scans_published_and_vendor_sources() {
    let (_dir, root) = dir_with_files(&[
        ("resources/views/flux/button.blade.php", "x"),
        ("resources/views/flux/icon/arrow-right.blade.php", "x"),
        (
            "vendor/livewire/flux/stubs/resources/views/flux/badge.blade.php",
            "x",
        ),
        (
            "vendor/livewire/flux-pro/stubs/resources/views/flux/chart.blade.php",
            "x",
        ),
        // A non-blade file in the flux dir must be ignored.
        ("resources/views/flux/notes.md", "x"),
    ]);

    let got = collect_flux_components(&root);

    assert_eq!(
        names(&got),
        vec!["badge", "button", "chart", "icon.arrow-right"],
        "should offer bare dotted Flux names from all sources, sorted",
    );
}

#[test]
fn collect_flux_components_empty_without_flux_dirs() {
    let (_dir, root) = dir_with_files(&[("resources/views/components/button.blade.php", "x")]);
    assert!(collect_flux_components(&root).is_empty());
}

#[test]
fn collect_flux_components_ignores_node_modules_tree() {
    // AC #4: the corrected stub discovers Flux via Composer (`vendor/`), never
    // `node_modules`. A flux-shaped blade sitting under a node_modules tree
    // must NOT be picked up — only the real Composer source counts.
    let (_dir, root) = dir_with_files(&[
        // Decoy: flux-named blade under node_modules — must be ignored.
        (
            "node_modules/@livewire/flux/stubs/resources/views/flux/decoy.blade.php",
            "x",
        ),
        ("node_modules/flux/views/flux/ghost.blade.php", "x"),
        // The genuine Composer source — the only thing that should surface.
        (
            "vendor/livewire/flux/stubs/resources/views/flux/button.blade.php",
            "x",
        ),
    ]);

    let got = collect_flux_components(&root);

    assert_eq!(
        names(&got),
        vec!["button"],
        "only the Composer vendor source is scanned; node_modules is ignored",
    );
}

// ─── dedup ──────────────────────────────────────────────────────────────

#[test]
fn dedup_prefers_class_over_anonymous_view_for_same_tag() {
    let candidates = vec![
        ComponentCandidate {
            name: "alert".into(),
            detail: "resources/views/components/alert.blade.php".into(),
            kind: CandidateKind::AnonymousView,
        },
        ComponentCandidate {
            name: "alert".into(),
            detail: "app/View/Components/Alert.php".into(),
            kind: CandidateKind::Class,
        },
    ];

    let out = dedup_and_sort(candidates);

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, CandidateKind::Class, "class must win the tie");
    assert!(out[0].detail.ends_with("Alert.php"));
}

#[test]
fn dedup_is_order_independent_for_class_precedence() {
    // Same as above but class listed first — result must be identical.
    let candidates = vec![
        ComponentCandidate {
            name: "alert".into(),
            detail: "app/View/Components/Alert.php".into(),
            kind: CandidateKind::Class,
        },
        ComponentCandidate {
            name: "alert".into(),
            detail: "resources/views/components/alert.blade.php".into(),
            kind: CandidateKind::AnonymousView,
        },
    ];
    let out = dedup_and_sort(candidates);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, CandidateKind::Class);
}
