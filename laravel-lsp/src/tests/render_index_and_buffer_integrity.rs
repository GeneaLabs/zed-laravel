//! Actor-side integrity of the backing-class resolution path (#339 review).
//!
//! Three defects live at the seam where the Blade side pushes state into the
//! Salsa actor, and none of them is visible from a pure-function test:
//!
//! 1. the reverse component-usage index has to see an edit that adds or
//!    removes an `<x-…>` tag, or the ancestor walk answers out of a stale map;
//! 2. loading a backing class from disk must not overwrite an open editor
//!    buffer's text, and must drop the caches populated from the old text;
//! 3. two concurrent render-index snapshots must not leave the actor holding
//!    the older one while the caller's gate claims the newer.
//!
//! Every test drives the real `SalsaHandle` API rather than actor internals.

use laravel_lsp::salsa_impl::SalsaHandle;
use std::fs;
use std::path::{Path, PathBuf};

fn write(path: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

/// A handle whose project is `root`, with `resources/views` registered as the
/// view root so the Blade files below land in the actor's view-file list, and
/// `root` registered as the config root.
async fn handle_for(root: &Path) -> SalsaHandle {
    let handle = laravel_lsp::salsa_impl::SalsaActor::spawn();
    // Registers the actor's `config_root`, which the backing-class loader's
    // containment guard needs before it will read anything (#364). Production
    // always sets it first — either here or via `RegisterCachedConfig` — so a
    // fixture that skips it is under-registering, not exercising a real state.
    handle
        .register_config_files(root.to_path_buf(), None, None, None, None)
        .await
        .expect("actor registers the tempdir project root");
    handle
        .register_project_files(
            root.to_path_buf(),
            vec![PathBuf::from("app/Http/Controllers")],
            vec![root.join("resources/views")],
            Some(root.join("resources/views/livewire")),
            PathBuf::from("routes"),
            // The shared vendor walk the production caller passes in
            // (issue #371) — built from the same root, so the actor
            // registers exactly what it would in production.
            laravel_lsp::vendor_index::VendorIndex::build(root)
                .files()
                .iter()
                .map(|f| f.path.clone())
                .collect(),
        )
        .await
        .expect("actor registers the tempdir project");
    handle
}

// === 1. the reverse component-usage index tracks edits ====================

#[tokio::test]
async fn the_usage_index_sees_a_tag_added_by_an_edit() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/dashboard.blade.php"),
        "<div>nothing here yet</div>\n",
    );
    let handle = handle_for(root).await;

    assert!(
        handle
            .files_rendering_component(vec!["save-button".to_string()], Vec::new())
            .await
            .unwrap()
            .is_empty(),
        "nothing renders <x-save-button> yet"
    );

    handle
        .update_file(
            parent.clone(),
            1,
            "<div><x-save-button /></div>\n".to_string(),
        )
        .await
        .unwrap();

    assert_eq!(
        handle
            .files_rendering_component(vec!["save-button".to_string()], Vec::new())
            .await
            .unwrap(),
        vec![parent],
        "the edit added the tag, so the index has to answer with this file"
    );
}

#[tokio::test]
async fn the_usage_index_sees_a_tag_removed_by_an_edit() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/dashboard.blade.php"),
        "<div><x-save-button /></div>\n",
    );
    let handle = handle_for(root).await;

    assert_eq!(
        handle
            .files_rendering_component(vec!["save-button".to_string()], Vec::new())
            .await
            .unwrap(),
        vec![parent.clone()],
    );

    handle
        .update_file(parent, 1, "<div>gone</div>\n".to_string())
        .await
        .unwrap();

    assert!(
        handle
            .files_rendering_component(vec!["save-button".to_string()], Vec::new())
            .await
            .unwrap()
            .is_empty(),
        "re-indexing must withdraw the tag the file no longer renders"
    );
}

/// `<livewire:…>` is the other half of the usage graph, and it is indexed in a
/// separate map — a lookup wired to only one of the two answers this with
/// silence.
#[tokio::test]
async fn the_usage_index_answers_for_livewire_tags_too() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/dashboard.blade.php"),
        "<div><livewire:counter /></div>\n",
    );
    let handle = handle_for(root).await;

    assert_eq!(
        handle
            .files_rendering_component(Vec::new(), vec!["counter".to_string()])
            .await
            .unwrap(),
        vec![parent],
    );
}

/// Answers are sorted, because the ancestor walk takes the first entry of a
/// level and two equidistant parents must not swap places between calls.
#[tokio::test]
async fn the_usage_index_answers_in_sorted_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let tag = "<div><x-icon /></div>\n";
    let a = write(&root.join("resources/views/aaa.blade.php"), tag);
    let b = write(&root.join("resources/views/mmm.blade.php"), tag);
    let c = write(&root.join("resources/views/zzz.blade.php"), tag);
    let handle = handle_for(root).await;

    assert_eq!(
        handle
            .files_rendering_component(vec!["icon".to_string()], Vec::new())
            .await
            .unwrap(),
        vec![a, b, c],
    );
}

// === 2. a disk reload must not clobber an open buffer =====================

/// A Blade hover resolves its backing class through the actor, which loads
/// that `.php` from disk. When the class is also open and edited, the last
/// SAVED bytes must not replace the buffer the editor pushed.
#[tokio::test]
async fn resolving_a_backing_class_leaves_a_dirty_buffer_alone() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let class = write(
        &root.join("app/Livewire/Counter.php"),
        "<?php\nclass Counter\n{\n    public function saved(): void {}\n}\n",
    );
    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(root).await;

    // The editor's unsaved text — a renamed method that is not on disk.
    let buffer = "<?php\nclass Counter\n{\n    public function unsaved(): void {}\n}\n";
    handle
        .update_file(class.clone(), 7, buffer.to_string())
        .await
        .unwrap();

    // Resolving the template's backing class is what reads that `.php` from
    // disk; the resolution hands back the source it read.
    let resolved = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![class.clone()],
            None,
        )
        .await
        .unwrap();

    let source = resolved
        .sources
        .iter()
        .find(|(path, _)| path == &class)
        .map(|(_, source)| source.clone())
        .expect("the backing class resolves");
    assert_eq!(
        source, buffer,
        "the last-saved bytes must not come back over the live buffer"
    );
}

/// The other half of the same defect: when the loader DOES write — no buffer
/// involved, the file changed on disk — every cache populated from the old
/// text has to go. `pattern_cache` compares nothing on lookup, so a surviving
/// entry is served forever.
#[tokio::test]
async fn reloading_a_changed_backing_class_drops_its_cached_patterns() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let class = write(
        &root.join("app/Livewire/Counter.php"),
        "<?php\nclass Counter\n{\n    public function show() { return view('before'); }\n}\n",
    );
    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(root).await;

    // Populate `pattern_cache` from the original text.
    let before = handle
        .get_patterns(class.clone())
        .await
        .unwrap()
        .expect("the class parses");
    assert!(
        before.views.iter().any(|v| v.name == "before"),
        "the original text is parsed and cached"
    );

    // Change it on disk. The sleep gives the mtime room to advance on
    // filesystems with coarse timestamps — the loader reloads on mtime.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(
        &class,
        "<?php\nclass Counter\n{\n    public function show() { return view('after'); }\n}\n",
    )
    .unwrap();

    handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![class.clone()],
            None,
        )
        .await
        .unwrap();

    let after = handle
        .get_patterns(class)
        .await
        .unwrap()
        .expect("the class parses");
    let names: Vec<&str> = after.views.iter().map(|v| v.name.as_str()).collect();
    assert!(
        names.contains(&"after"),
        "the reload replaced the text, so the cached patterns must go too: {names:?}"
    );
}

/// The guard is not allowed to depend on a filesystem call succeeding. When
/// the buffer is pushed while its file is momentarily absent — a branch
/// switch, a stash, an `artisan make:*` regeneration — a stamp built from
/// `fs::metadata` records nothing, and "nothing recorded" reads downstream as
/// "never loaded, read disk unconditionally": the clobber the stamp exists to
/// prevent, re-armed by its own failure path.
#[tokio::test]
async fn a_buffer_pushed_while_its_file_is_missing_is_not_clobbered() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let class = root.join("app/Livewire/Counter.php");
    fs::create_dir_all(class.parent().unwrap()).unwrap();
    let handle = handle_for(root).await;

    // The editor pushes its buffer while the path does not exist on disk, so
    // any `fs::metadata(&class)` inside the push fails.
    let buffer = "<?php\nclass Counter\n{\n    public function unsaved(): void {}\n}\n";
    handle
        .update_file(class.clone(), 7, buffer.to_string())
        .await
        .unwrap();

    // The file comes back, with different content — the regenerated or
    // checked-out version the user has NOT accepted into their buffer.
    write(
        &class,
        "<?php\nclass Counter\n{\n    public function regenerated(): void {}\n}\n",
    );

    let resolved = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![class.clone()],
            None,
        )
        .await
        .unwrap();

    let source = resolved
        .sources
        .iter()
        .find(|(path, _)| path == &class)
        .map(|(_, source)| source.clone())
        .expect("the backing class resolves from the pushed buffer");
    assert_eq!(
        source, buffer,
        "a push that could not stat its file still owns the text"
    );
}

/// The same rule under the trigger that does not need a failed `metadata()`
/// at all: the file changes on disk while the buffer is open and dirty. An
/// mtime comparison says "reload"; ownership says the pusher's text stands
/// until the pusher replaces it.
#[tokio::test]
async fn a_disk_change_under_a_dirty_buffer_does_not_reload() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let class = write(
        &root.join("app/Livewire/Counter.php"),
        "<?php\nclass Counter\n{\n    public function saved(): void {}\n}\n",
    );
    let blade = write(
        &root.join("resources/views/livewire/counter.blade.php"),
        "<div>{{ $count }}</div>\n",
    );
    let handle = handle_for(root).await;

    let buffer = "<?php\nclass Counter\n{\n    public function unsaved(): void {}\n}\n";
    handle
        .update_file(class.clone(), 7, buffer.to_string())
        .await
        .unwrap();

    // Disk moves on underneath the dirty buffer. The sleep gives the mtime
    // room to advance on filesystems with coarse timestamps, so the test
    // exercises the mtime-differs branch rather than an accidental tie.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(
        &class,
        "<?php\nclass Counter\n{\n    public function checked_out(): void {}\n}\n",
    )
    .unwrap();

    let resolved = handle
        .blade_backing_class_resolution(
            blade,
            Some("livewire.counter".to_string()),
            vec![class.clone()],
            None,
        )
        .await
        .unwrap();

    let source = resolved
        .sources
        .iter()
        .find(|(path, _)| path == &class)
        .map(|(_, source)| source.clone())
        .expect("the backing class resolves");
    assert_eq!(
        source, buffer,
        "an advancing mtime must not evict the editor's unsaved text"
    );
}

// === 3. the render index never goes backwards =============================

/// Two tasks snapshot generations 1 and 2 and reach the actor in either order.
/// The actor installs the newest and refuses the older, so the data it holds
/// always matches the generation the caller's gate advanced to.
#[tokio::test]
async fn an_out_of_order_render_index_push_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let newer = write(
        &root.join("app/Http/Controllers/NewerController.php"),
        "<?php\nclass NewerController\n{\n    public function show() { return view('page'); }\n}\n",
    );
    let older = write(
        &root.join("app/Http/Controllers/OlderController.php"),
        "<?php\nclass OlderController\n{\n    public function show() { return view('page'); }\n}\n",
    );
    let blade = write(
        &root.join("resources/views/page.blade.php"),
        "<div></div>\n",
    );
    let handle = handle_for(root).await;

    handle
        .set_render_index(2, vec![("page".to_string(), newer.clone())])
        .await
        .unwrap();
    handle
        .set_render_index(1, vec![("page".to_string(), older.clone())])
        .await
        .unwrap();

    let resolved = handle
        .blade_backing_class_resolution(blade, Some("page".to_string()), Vec::new(), None)
        .await
        .unwrap();
    assert_eq!(
        resolved.files,
        vec![newer],
        "generation 2 was installed first; generation 1 must not replace it"
    );
}

/// …and a genuinely newer snapshot still wins, so the guard rejects only
/// what is stale rather than everything after the first push.
#[tokio::test]
async fn a_newer_render_index_push_replaces_the_installed_one() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let first = write(
        &root.join("app/Http/Controllers/FirstController.php"),
        "<?php\nclass FirstController\n{\n    public function show() { return view('page'); }\n}\n",
    );
    let second = write(
        &root.join("app/Http/Controllers/SecondController.php"),
        "<?php\nclass SecondController\n{\n    public function show() { return view('page'); }\n}\n",
    );
    let blade = write(
        &root.join("resources/views/page.blade.php"),
        "<div></div>\n",
    );
    let handle = handle_for(root).await;

    handle
        .set_render_index(1, vec![("page".to_string(), first)])
        .await
        .unwrap();
    handle
        .set_render_index(2, vec![("page".to_string(), second.clone())])
        .await
        .unwrap();

    let resolved = handle
        .blade_backing_class_resolution(blade, Some("page".to_string()), Vec::new(), None)
        .await
        .unwrap();
    assert_eq!(resolved.files, vec![second]);
}
