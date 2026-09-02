//! Tests for the shared vendor walk (issue #371).
//!
//! The load-bearing claim of this module is an *equivalence*: every consumer
//! that used to run its own `WalkDir` now filters a shared result and must see
//! precisely the same files. So most of these tests compare the shared index,
//! narrowed by a consumer's former limits, against a live `WalkDir` configured
//! the way that consumer used to configure it — rather than against a
//! hand-written expected list, which would only re-state my own assumptions.

use super::*;
use std::collections::HashSet;
use std::fs;
use tempfile::TempDir;

/// A vendor tree with the shapes every consumer's limits key on:
/// deep nesting (past depth 6 and 8), a package `routes/` dir, a service
/// provider, an `Http/Kernel.php`, framework-owned files, a `.blade.php`, a
/// non-PHP file, and subtrees named for each of `command_index`'s pruned dirs.
fn seed_vendor(root: &Path) {
    let files = [
        // depth 2
        "vendor/autoload.php",
        // depth 3
        "vendor/acme/pkg.php",
        // depth 4-5: provider + kernel shapes
        "vendor/acme/pkg/AcmeServiceProvider.php",
        "vendor/acme/pkg/Http/Kernel.php",
        "vendor/acme/pkg/routes/web.php",
        // framework tier
        "vendor/laravel/framework/src/Illuminate/Foo/FooServiceProvider.php",
        // depth 7 — inside the route walk's depth 8 budget
        "vendor/acme/pkg/src/a/b/Deep.php",
        // depth 9 — outside it, inside the command walk's unbounded one
        "vendor/acme/pkg/src/a/b/c/d/Deeper.php",
        // pruned subtrees (command_index skips these, others do not)
        "vendor/acme/pkg/node_modules/Bundled.php",
        "vendor/acme/pkg/public/Asset.php",
        "vendor/acme/pkg/storage/Cached.php",
        "vendor/acme/pkg/.git/Hook.php",
        // a blade template and a non-PHP file
        "vendor/acme/pkg/views/card.blade.php",
        "vendor/acme/pkg/README.md",
    ];
    for rel in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "<?php\n").unwrap();
    }
}

fn paths(index: &VendorIndex) -> HashSet<PathBuf> {
    index.files().iter().map(|f| f.path.clone()).collect()
}

/// What a `WalkDir` configured the old way would have found, as a set.
fn walk_like_consumer(
    dir: &Path,
    max_depth: Option<usize>,
    pruned: &'static [&'static str],
) -> HashSet<PathBuf> {
    let mut w = WalkDir::new(dir);
    if let Some(d) = max_depth {
        w = w.max_depth(d);
    }
    w.into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir() && e.file_name().to_str().is_some_and(|n| pruned.contains(&n)))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[test]
fn build_collects_every_php_file_including_blade_and_excluding_others() {
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());
    let found = paths(&index);

    assert!(
        found.contains(&tmp.path().join("vendor/acme/pkg/views/card.blade.php")),
        "`.blade.php` has extension `php` and was admitted by every former \
         walk, so it must be here too"
    );
    assert!(
        !found.contains(&tmp.path().join("vendor/acme/pkg/README.md")),
        "non-PHP files were never collected"
    );
    assert!(
        found.contains(&tmp.path().join("vendor/acme/pkg/node_modules/Bundled.php")),
        "the shared walk prunes NOTHING — it is the union of all five former \
         walks, and the route/provider walks descended into node_modules"
    );
}

#[test]
fn build_yields_an_empty_index_without_a_vendor_dir() {
    // Every former consumer guarded its walk with an `exists()`/`is_dir()`
    // check and skipped the whole branch. `is_empty()` is how they keep it.
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("app")).unwrap();
    let index = VendorIndex::build(tmp.path());

    assert!(index.is_empty(), "no vendor/ means nothing to share");
    assert_eq!(index.len(), 0);
    assert_eq!(
        index.vendor_root(),
        Path::new(""),
        "an absent vendor/ reports no root rather than a path that does not exist"
    );
}

#[test]
fn depth_matches_walkdir_max_depth_semantics() {
    // The whole per-consumer narrowing rests on this: `depth` must mean what
    // `WalkDir::max_depth` meant, or every restored bound is off by one.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    let depth_of = |rel: &str| {
        index
            .files()
            .iter()
            .find(|f| f.path == tmp.path().join(rel))
            .unwrap_or_else(|| panic!("{rel} not indexed"))
            .depth
    };

    // `vendor` is the walk root at depth 0, so its direct children are 1.
    assert_eq!(depth_of("vendor/autoload.php"), 1);
    assert_eq!(depth_of("vendor/acme/pkg.php"), 2);
    assert_eq!(depth_of("vendor/acme/pkg/src/a/b/Deep.php"), 6);
    assert_eq!(depth_of("vendor/acme/pkg/src/a/b/c/d/Deeper.php"), 8);
}

#[test]
fn within_depth_reproduces_the_route_walks_file_set() {
    // `discover_route_files` used `WalkDir::new(vendor).max_depth(8)` with no
    // pruning. Narrowing the shared index must give the identical set.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    let shared: HashSet<PathBuf> = index.within_depth(8).map(|p| p.to_path_buf()).collect();
    let live = walk_like_consumer(&tmp.path().join("vendor"), Some(8), &[]);

    assert_eq!(
        shared, live,
        "depth-8 narrowing must equal a live max_depth(8) walk"
    );
}

#[test]
fn within_depth_excludes_what_the_bound_excludes() {
    // Discriminates the equality above: if `within_depth` ignored its argument
    // and returned everything, the comparison would still need this to fail.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    let shallow: HashSet<PathBuf> = index.within_depth(6).map(|p| p.to_path_buf()).collect();

    assert!(
        shallow.contains(&tmp.path().join("vendor/acme/pkg/src/a/b/Deep.php")),
        "depth 6 is inside a max_depth(6) budget"
    );
    assert!(
        !shallow.contains(&tmp.path().join("vendor/acme/pkg/src/a/b/c/d/Deeper.php")),
        "depth 8 is outside it — the bound must actually bind"
    );
    assert!(
        index.within_depth(usize::MAX).count() > shallow.len(),
        "an unbounded narrowing must return strictly more than a depth-6 one"
    );
}

#[test]
fn pruned_ancestor_predicate_reproduces_filter_entry_pruning() {
    // `build_command_index` pruned SKIP_DIRS with `filter_entry`. Replacing a
    // subtree prune with an ancestor-name test is only valid if the two agree
    // on every file — including files deep inside a pruned subtree.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    let shared: HashSet<PathBuf> = index
        .files()
        .iter()
        .filter(|f| {
            !has_pruned_ancestor(
                &f.path,
                index.vendor_root(),
                crate::command_index::SKIP_DIRS,
            )
        })
        .map(|f| f.path.clone())
        .collect();
    let live = walk_like_consumer(
        &tmp.path().join("vendor"),
        None,
        crate::command_index::SKIP_DIRS,
    );

    assert_eq!(
        shared, live,
        "ancestor-name filtering must equal live filter_entry pruning"
    );
    assert!(
        !shared.contains(&tmp.path().join("vendor/acme/pkg/node_modules/Bundled.php")),
        "fixture check — the comparison is worthless if nothing was pruned"
    );
}

#[test]
fn pruned_ancestor_ignores_the_files_own_name() {
    // `filter_entry` in this crate only ever rejects directories, so a FILE
    // named `public` was always kept. An ancestor test that looked at the whole
    // path would wrongly drop it.
    let base = Path::new("/p/vendor");
    assert!(
        !has_pruned_ancestor(Path::new("/p/vendor/acme/public"), base, &["public"]),
        "a file named `public` is not inside a pruned directory"
    );
    assert!(
        has_pruned_ancestor(Path::new("/p/vendor/acme/public/x.php"), base, &["public"]),
        "a file inside `public/` is"
    );
    assert!(
        has_pruned_ancestor(
            Path::new("/p/vendor/public/deep/nested/x.php"),
            base,
            &["public"]
        ),
        "pruning removes the whole subtree, not just direct children"
    );
    assert!(
        !has_pruned_ancestor(Path::new("/p/vendor/acme/x.php"), base, &["public"]),
        "an unrelated path is untouched"
    );
}

#[test]
fn pruned_ancestor_ignores_components_above_the_base() {
    // A `WalkDir` only ever sees components below its own root, so the
    // predicate must too. Before this was scoped, a project checked out under
    // a directory that happened to be called `public` — or `storage`, or
    // `node_modules` — had its ENTIRE vendor tree classified as pruned, and
    // the command index would have silently indexed nothing from it.
    let base = Path::new("/home/me/public/app/vendor");
    assert!(
        !has_pruned_ancestor(
            Path::new("/home/me/public/app/vendor/acme/Cmd.php"),
            base,
            crate::command_index::SKIP_DIRS
        ),
        "a `public` ancestor ABOVE the walk root was never visible to WalkDir"
    );
    assert!(
        has_pruned_ancestor(
            Path::new("/home/me/public/app/vendor/acme/public/Cmd.php"),
            base,
            crate::command_index::SKIP_DIRS
        ),
        "a `public` directory BELOW the walk root still prunes"
    );
}

#[test]
fn pruned_ancestor_fails_closed_outside_the_base() {
    // A path that is not under `base` cannot be described by a walk rooted at
    // `base`. Reporting it as pruned keeps a caller from silently indexing
    // something the shared walk never covered.
    assert!(
        has_pruned_ancestor(
            Path::new("/somewhere/else/x.php"),
            Path::new("/p/vendor"),
            &["public"]
        ),
        "an out-of-base path is treated as pruned"
    );
}

#[test]
fn walk_order_is_preserved() {
    // `command_index::insert_entry` keeps the FIRST file walked on a same-tier
    // duplicate, so a consumer replaying the shared list must see vendor files
    // in the order a `WalkDir` produced them.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    let live: Vec<PathBuf> = WalkDir::new(tmp.path().join("vendor"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "php"))
        .map(|e| e.path().to_path_buf())
        .collect();
    let shared: Vec<PathBuf> = index.files().iter().map(|f| f.path.clone()).collect();

    assert_eq!(shared, live, "the index must keep walk order, not sort");
    assert!(
        shared.len() > 1,
        "fixture check — order needs >1 file to mean anything"
    );
}

#[test]
fn for_each_source_reads_only_what_a_consumer_wants() {
    // The read budget claim: a file no consumer wants is never opened. Counted
    // via the `wants` predicate, which is called for every file, against the
    // `visit` callback, which fires only after a successful read.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    // `wants` is an `Fn`, deliberately — a predicate with side effects would
    // make the read budget unauditable. `Cell` is how a test counts calls
    // through one without needing `FnMut`.
    let considered = std::cell::Cell::new(0usize);
    let mut read = Vec::new();
    index.for_each_source(
        |f| {
            considered.set(considered.get() + 1);
            f.depth <= 2
        },
        |f, text| {
            read.push(f.path.clone());
            assert_eq!(text, "<?php\n", "the visitor receives the file's text");
        },
    );

    assert_eq!(
        considered.get(),
        index.len(),
        "every file is offered to the predicate"
    );
    assert_eq!(
        read.into_iter().collect::<HashSet<_>>(),
        index.within_depth(2).map(|p| p.to_path_buf()).collect(),
        "exactly the wanted files were read — no more, no fewer"
    );
}

#[test]
fn for_each_source_reads_a_file_shared_by_two_consumers_once() {
    // The reason this method exists: two consumers with overlapping subsets
    // must cost one read, not two.
    let tmp = TempDir::new().unwrap();
    seed_vendor(tmp.path());
    let index = VendorIndex::build(tmp.path());

    // Consumer A wants depth <= 8; consumer B wants everything unpruned. Most
    // files satisfy both.
    let wants_a = |f: &VendorFile| f.depth <= 8;
    let wants_b = |f: &VendorFile| {
        !has_pruned_ancestor(
            &f.path,
            index.vendor_root(),
            crate::command_index::SKIP_DIRS,
        )
    };

    let mut reads = 0usize;
    let mut seen_a = 0usize;
    let mut seen_b = 0usize;
    index.for_each_source(
        |f| wants_a(f) || wants_b(f),
        |f, _| {
            reads += 1;
            if wants_a(f) {
                seen_a += 1;
            }
            if wants_b(f) {
                seen_b += 1;
            }
        },
    );

    let overlap = index
        .files()
        .iter()
        .filter(|f| wants_a(f) && wants_b(f))
        .count();
    assert!(
        overlap > 0,
        "fixture check — the consumers must actually overlap"
    );
    assert!(
        reads < seen_a + seen_b,
        "sharing must cost fewer reads ({reads}) than the two consumers \
         separately ({seen_a} + {seen_b})"
    );
    assert_eq!(
        reads,
        index
            .files()
            .iter()
            .filter(|f| wants_a(f) || wants_b(f))
            .count(),
        "reads equal the union of the two consumers' sets"
    );
}

#[test]
fn for_each_source_skips_an_unreadable_file_without_failing() {
    // Every former call site used `if let Ok(content) = read_to_string(..)`, so
    // a directory-shaped or vanished entry was skipped, not fatal. A path in
    // the index that no longer exists on disk is the reachable version of that.
    let tmp = TempDir::new().unwrap();
    let vendor = tmp.path().join("vendor");
    fs::create_dir_all(&vendor).unwrap();
    fs::write(vendor.join("real.php"), "<?php\n").unwrap();

    let index = VendorIndex::from_files(
        vendor.clone(),
        vec![
            VendorFile {
                path: vendor.join("gone.php"),
                depth: 1,
            },
            VendorFile {
                path: vendor.join("real.php"),
                depth: 1,
            },
        ],
    );

    let mut visited = Vec::new();
    index.for_each_source(|_| true, |f, _| visited.push(f.path.clone()));

    assert_eq!(
        visited,
        vec![vendor.join("real.php")],
        "the missing file is skipped and the walk continues past it"
    );
}

#[test]
fn depth_below_matches_a_walk_rooted_at_the_base() {
    // The provider scan's framework leg used `WalkDir::new(<framework>)
    // .max_depth(10)`, so its budget counts from the framework directory, not
    // from `vendor/`. Measuring against the wrong base would move the cutoff
    // by however deep the framework happens to sit.
    let base = Path::new("/p/vendor/laravel/framework/src/Illuminate");
    assert_eq!(depth_below(&base.join("Foo.php"), base), Some(1));
    assert_eq!(depth_below(&base.join("Foo/Bar/Baz.php"), base), Some(3));
    assert_eq!(
        depth_below(base, base),
        Some(0),
        "the base itself is depth 0"
    );
    assert_eq!(
        depth_below(Path::new("/p/vendor/acme/Foo.php"), base),
        None,
        "a path outside the base has no depth in that walk"
    );
}
