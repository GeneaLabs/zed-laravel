//! Containment of the external-PHP backing-class loader, driven through the
//! real pipeline (#364).
//!
//! `SalsaActor::ensure_external_php_source_loaded` reads a `.php` file from
//! disk and registers it as a Salsa input; `handle_blade_backing_class_resolution`
//! then maps every returned handle into `BladeBackingResolutionData::files`,
//! which is the goto-definition target list. Every candidate reaching it today
//! is pre-vetted by its caller — the render index walks the project's own
//! tree, and `livewire_resolver::resolve_component` gates each path segment
//! through `naming::is_safe_path_segment` — so this is a guard on the
//! primitive, closing the class rather than trusting the next caller to
//! remember (the failure shape of #294 and both rounds of #348).
//!
//! These tests enter through `SalsaHandle::blade_backing_class_resolution`,
//! the same entry point `main.rs` uses, so the candidate travels the real
//! `blade_backing_class_files` path rather than being handed to the loader
//! directly. The companion unit tests in `salsa_impl/tests.rs` assert the
//! actor's internal maps, which no handle message exposes.

use laravel_lsp::salsa_impl::SalsaHandle;
use std::fs;
use std::path::{Path, PathBuf};

fn write(path: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

/// A handle whose config root and project files are both `root` — the same
/// pairing production performs before any resolution request can arrive.
async fn handle_for(root: &Path) -> SalsaHandle {
    let handle = laravel_lsp::salsa_impl::SalsaActor::spawn();
    handle
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("actor registers the tempdir project root");
    handle
        .register_project_files(
            root.to_path_buf(),
            vec![PathBuf::from("app/Http/Controllers")],
            vec![root.join("resources/views")],
            Some(root.join("resources/views/livewire")),
            PathBuf::from("routes"),
        )
        .await
        .expect("actor registers the tempdir project");
    handle
}

/// Assert `path` really resolves outside `root`, so a mis-built fixture can't
/// let a rejection test pass vacuously.
fn assert_outside_root(path: &Path, root: &Path) {
    let real = path
        .canonicalize()
        .expect("the fixture target must exist on disk");
    let real_root = root.canonicalize().expect("the project root must exist");
    assert!(
        !real.starts_with(&real_root),
        "fixture is not actually out of root: {real:?} is under {real_root:?}"
    );
}

/// The paths in a resolution result, as the client would receive them.
fn resolved_paths(data: &laravel_lsp::salsa_impl::BladeBackingResolutionData) -> Vec<PathBuf> {
    data.files.clone()
}

#[tokio::test]
async fn an_out_of_root_backing_class_candidate_is_not_resolved() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();

    // A real, readable file — the rejection must come from containment, not
    // from a read that would have failed anyway.
    let escapee = write(
        &outside.join("Counter.php"),
        "<?php\nclass Counter { public $secret = 'escaped'; }\n",
    );
    assert_outside_root(&escapee, &root);

    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(&root).await;

    let resolved = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![escapee.clone()],
            None,
        )
        .await
        .unwrap();

    assert!(
        !resolved_paths(&resolved).contains(&escapee),
        "an out-of-root candidate must never surface as a goto target: {:?}",
        resolved.files,
    );
    assert!(
        !resolved.sources.iter().any(|(path, _)| path == &escapee),
        "and its source must not be read into the resolution either",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_under_root_symlink_escaping_the_root_is_not_resolved() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("app/Livewire")).unwrap();
    fs::create_dir_all(&outside).unwrap();

    // Distinguishable, non-empty content shared by both fixtures below, so the
    // contrast isolates containment as the only difference.
    let source = "<?php\nclass Counter { public $marker = 'distinguishable'; }\n";
    let target = write(&outside.join("Counter.php"), source);

    let link = root.join("app/Livewire/Counter.php");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_outside_root(&link, &root);

    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(&root).await;

    let escaping = handle
        .blade_backing_class_resolution(
            blade.clone(),
            Some("livewire.counter".to_string()),
            vec![link.clone()],
            None,
        )
        .await
        .unwrap();
    assert!(
        !resolved_paths(&escaping).contains(&link),
        "a path lexically under the root whose target escapes it must be refused: {:?}",
        escaping.files,
    );

    // The contrast: identical bytes at a genuine in-root path DO resolve, so
    // the refusal above tracks containment rather than a coincidental failure
    // to read a symlinked file.
    let genuine = write(&root.join("app/Livewire/Real.php"), source);
    let contained = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![genuine.clone()],
            None,
        )
        .await
        .unwrap();
    assert!(
        resolved_paths(&contained).contains(&genuine),
        "the same content at an in-root path must still resolve: {:?}",
        contained.files,
    );
}

#[tokio::test]
async fn an_in_root_backing_class_resolves_to_its_exact_disk_content() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();

    let source = "<?php\nclass Counter\n{\n    public function saved(): void {}\n}\n";
    let class = write(&root.join("app/Livewire/Counter.php"), source);
    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(root).await;

    let resolved = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![class.clone()],
            None,
        )
        .await
        .unwrap();

    assert!(
        resolved_paths(&resolved).contains(&class),
        "an in-root backing class must resolve: {:?}",
        resolved.files,
    );
    let text = resolved
        .sources
        .iter()
        .find(|(path, _)| path == &class)
        .map(|(_, text)| text.clone())
        .expect("the backing class contributes its source");
    // Compared against the bytes the test WROTE, read back off disk — not a
    // literal duplicated on both sides of the assertion.
    assert_eq!(
        text,
        fs::read_to_string(&class).unwrap(),
        "the guard must return the file's real content, unchanged",
    );
    assert_eq!(text, source);
}
