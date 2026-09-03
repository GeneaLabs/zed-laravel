//! Tests for the combined warm-start pass (issues #371 and #373).
//!
//! Two claims to pin from #371, and they pull in opposite directions — which is
//! why both are needed:
//!
//! 1. **Identical output.** Sharing the read must not change either index.
//! 2. **Fewer reads.** If it did not, the module would be pure overhead.
//!
//! #373 parallelized the pass, which adds a third: **order survives it.** The
//! command index resolves a same-tier duplicate to the first file walked and
//! persists `CommandScan::files` in insertion order, so a map that returned its
//! answers out of order, or a fold that ran concurrently, would be wrong in a
//! way no equality-of-contents assertion catches.

use super::*;
use crate::command_index::{build_command_index_with_vendor, CommandIndex, CommandPriority};
use crate::route_discovery::discover_route_files_with_vendor;
use crate::vendor_index::VendorFile;
use std::path::PathBuf;
use tempfile::TempDir;

/// A project where the route and command classifiers both have real work, and
/// where their wanted file sets overlap but do not coincide: a vendor command
/// past the route depth budget, a route file inside `node_modules` (which the
/// command leg prunes), and a file that is neither.
fn seed(root: &Path) {
    let route_src = "<?php\nRoute::get('/x', fn () => 'ok')->name('x');\n";
    // Each command gets a DISTINCT signature. Reusing one name would let the
    // App-tier declaration win the priority merge and silently erase every
    // vendor command this test is about.
    let cmd_src = |class: &str, sig: &str| {
        format!(
            "<?php\nuse Illuminate\\Console\\Command;\nclass {class} extends Command\n{{\n    protected $signature = '{sig}';\n}}\n"
        )
    };
    // One file both classifiers accept: route registration AND a command class
    // in the same PHP block, so a single read has to serve both.
    let both = "<?php\nuse Illuminate\\Console\\Command;\nRoute::get('/b', fn () => 'ok')->name('b');\nclass Both extends Command\n{\n    protected $signature = 'both:cmd';\n}\n";
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    };
    write("routes/web.php", route_src);
    write(
        "app/Console/Commands/App.php",
        &cmd_src("AppCmd", "app:cmd"),
    );
    write("vendor/acme/pkg/routes/web.php", route_src);
    write("vendor/acme/pkg/src/Provider.php", route_src);
    write("vendor/acme/pkg/src/Both.php", both);
    write("vendor/acme/pkg/src/Neither.php", "<?php\nclass N {}\n");
    // Past the route depth budget (8), still inside the command leg.
    write(
        "vendor/acme/pkg/a/b/c/d/e/f/Deep.php",
        &cmd_src("DeepCmd", "deep:cmd"),
    );
    // Pruned by the command leg, still visible to the route leg.
    write("vendor/acme/pkg/node_modules/Bundled.php", route_src);
}

fn route_pairs(files: &[RouteFile]) -> Vec<(PathBuf, u8)> {
    let mut v: Vec<_> = files.iter().map(|f| (f.path.clone(), f.priority)).collect();
    v.sort();
    v
}

fn command_pairs(index: &CommandIndex) -> Vec<(String, PathBuf, CommandPriority)> {
    let mut v: Vec<_> = index
        .entries()
        .map(|e| (e.name.clone(), e.file.clone(), e.priority))
        .collect();
    v.sort();
    v
}

#[test]
fn combined_pass_produces_the_same_two_indexes_as_building_them_separately() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);

    let (routes, commands) = build_route_files_and_command_index(root, &vendor, None);
    let commands = commands.index;
    let routes_alone = discover_route_files_with_vendor(root, &vendor);
    let commands_alone = build_command_index_with_vendor(root, &vendor);

    assert_eq!(
        route_pairs(&routes),
        route_pairs(&routes_alone),
        "sharing the read must not change route discovery"
    );
    assert_eq!(
        command_pairs(&commands),
        command_pairs(&commands_alone),
        "sharing the read must not change the command index"
    );

    // Fixture checks — the equalities are worthless on an inert project.
    assert!(
        routes
            .iter()
            .any(|f| f.path == root.join("vendor/acme/pkg/src/Both.php")),
        "the shared file must reach the route classifier"
    );
    assert!(
        commands
            .entries()
            .any(|e| e.file == root.join("vendor/acme/pkg/src/Both.php")),
        "and the same text must reach the command classifier"
    );
    assert!(
        !routes
            .iter()
            .any(|f| f.path == root.join("vendor/acme/pkg/src/Neither.php")),
        "a file neither classifier accepts stays out of both"
    );
}

#[test]
fn combined_pass_keeps_each_consumers_own_limits() {
    // The two consumers disagree about two files by construction. If the
    // combined pass applied one consumer's predicate to both — the easy bug —
    // one of these assertions fails.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);

    let (routes, commands) = build_route_files_and_command_index(root, &vendor, None);
    let commands = commands.index;

    assert!(
        commands
            .entries()
            .any(|e| e.file == root.join("vendor/acme/pkg/a/b/c/d/e/f/Deep.php")),
        "the command leg has no depth budget and must still see a deep file"
    );
    assert!(
        !routes
            .iter()
            .any(|f| f.path == root.join("vendor/acme/pkg/a/b/c/d/e/f/Deep.php")),
        "the route leg's depth-8 budget must still bind in the shared pass"
    );
    assert!(
        routes
            .iter()
            .any(|f| f.path == root.join("vendor/acme/pkg/node_modules/Bundled.php")),
        "route discovery never pruned node_modules"
    );
    assert!(
        !commands
            .entries()
            .any(|e| e.file == root.join("vendor/acme/pkg/node_modules/Bundled.php")),
        "the command leg still prunes it"
    );
}

#[test]
fn combined_pass_reads_each_vendor_file_once() {
    // The reason the module exists. Counted by instrumenting `for_each_source`
    // with the same predicates the production pass uses, then comparing against
    // what the two consumers ask for separately.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);
    let vendor_root = vendor.vendor_root().to_path_buf();

    let route_wants = vendor
        .files()
        .iter()
        .filter(|f| crate::route_discovery::vendor_route_needs_source(f))
        .count();
    let command_wants = vendor
        .files()
        .iter()
        .filter(|f| crate::command_index::vendor_command_needs_source(&vendor_root, f))
        .count();

    let mut shared_reads = 0usize;
    vendor.for_each_source(
        |file| {
            crate::route_discovery::vendor_route_needs_source(file)
                || crate::command_index::vendor_command_needs_source(&vendor_root, file)
        },
        |_, _| shared_reads += 1,
    );

    assert!(
        route_wants > 0 && command_wants > 0,
        "fixture check — both consumers must want something ({route_wants}, {command_wants})"
    );
    assert!(
        shared_reads < route_wants + command_wants,
        "one shared pass ({shared_reads}) must cost fewer reads than two \
         separate ones ({route_wants} + {command_wants})"
    );
    assert_eq!(
        shared_reads,
        vendor
            .files()
            .iter()
            .filter(|f| crate::route_discovery::vendor_route_needs_source(f)
                || crate::command_index::vendor_command_needs_source(&vendor_root, f))
            .count(),
        "and exactly the union of what they want — never a file neither asked for"
    );
}

// ---------------------------------------------------------------------------
// Order survives the parallel map (issue #373)
// ---------------------------------------------------------------------------

/// A vendor tree of `n` packages that all declare the SAME command name at the
/// SAME tier, plus a route registration in each. One same-tier collision is
/// enough to state the rule; `n` of them is what makes an out-of-order fold
/// almost certain to show, rather than merely possible.
fn seed_colliding_packages(root: &Path, n: usize) {
    let body = "<?php\nuse Illuminate\\Console\\Command;\n\
                Route::get('/c', fn () => 'ok')->name('c');\n\
                class Dup extends Command\n{\n    protected $signature = 'dup:cmd';\n}\n";
    for i in 0..n {
        let p = root.join(format!("vendor/acme/pkg{i:03}/src/Dup.php"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
}

#[test]
fn the_parallel_pass_keeps_the_first_walked_winner_on_a_same_tier_collision() {
    // `CommandIndex::insert_entry` keeps the FIRST entry inserted when two
    // declarations share a name and a tier. Folding the parallel map's output
    // in input order is what preserves that; a `for_each` over the pool, or an
    // unordered collect, would hand the win to whichever worker finished first.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed_colliding_packages(root, 200);
    let vendor = VendorIndex::build(root);

    let first_walked = vendor
        .files()
        .iter()
        .map(|f| f.path.clone())
        .find(|p| p.ends_with("Dup.php"))
        .expect("fixture check — the walk must see the colliding files");

    let (_, commands) = build_route_files_and_command_index(root, &vendor, None);

    assert_eq!(
        commands.index.resolve("dup:cmd").map(|e| e.file.clone()),
        Some(first_walked),
        "the first file walked must win the same-tier collision"
    );
    assert_eq!(
        commands.index.resolve("dup:cmd").map(|e| e.priority),
        Some(CommandPriority::Package),
        "fixture check — the collisions must really be same-tier, or this \
         test proves nothing about order"
    );
}

#[test]
fn the_parallel_pass_is_deterministic_across_repeated_runs() {
    // The failure mode a single run cannot see: a pass that resolves the
    // collision by whichever worker won the race is right about half the time.
    // Ten runs over 200 colliding files would have to lose every race the same
    // way to pass by luck.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed_colliding_packages(root, 200);
    let vendor = VendorIndex::build(root);

    let run = || {
        let (routes, commands) = build_route_files_and_command_index(root, &vendor, None);
        (
            route_pairs(&routes),
            commands.index.resolve("dup:cmd").map(|e| e.file.clone()),
            commands
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
        )
    };

    let first = run();
    for i in 1..10 {
        assert_eq!(run(), first, "run {i} disagreed with the first run");
    }
}

#[test]
fn scanned_files_are_recorded_in_walk_order() {
    // `CommandScan::files` is what `command_disk_cache` persists, and the next
    // scan replays it into `insert_entry` — so its ORDER carries the same
    // first-wins rule the index does. Sorting it, or letting the pool append
    // as workers finish, would survive every contents-equality assertion here
    // and corrupt the collision winner one startup later.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed_colliding_packages(root, 200);
    let vendor = VendorIndex::build(root);
    let vendor_root = vendor.vendor_root().to_path_buf();

    let (_, commands) = build_route_files_and_command_index(root, &vendor, None);

    let expected: Vec<PathBuf> = vendor
        .files()
        .iter()
        .filter(|f| crate::command_index::vendor_command_needs_source(&vendor_root, f))
        .map(|f| f.path.clone())
        .collect();
    let recorded: Vec<PathBuf> = commands.files.iter().map(|f| f.path.clone()).collect();

    assert!(
        expected.len() >= 200,
        "fixture check — {} files is too few to expose an ordering bug",
        expected.len()
    );
    assert_eq!(
        recorded, expected,
        "scanned files must be recorded in walk order, not completion order"
    );
}

// ---------------------------------------------------------------------------
// The two "no answer" paths (issue #373)
// ---------------------------------------------------------------------------

#[test]
fn a_file_that_vanishes_between_the_walk_and_the_read_contributes_nothing() {
    // The walk lists files; the stat and the read happen later. A file deleted
    // in between is unstattable for the pre-pass and unreadable for the read
    // pass, and BOTH must leave the scan untouched: an entry stored without a
    // usable mtime could never be invalidated, so it would be trusted forever.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let vendor_root = root.join("vendor");
    std::fs::create_dir_all(vendor_root.join("acme/pkg/src")).unwrap();
    std::fs::write(
        vendor_root.join("acme/pkg/src/Real.php"),
        "<?php\nRoute::get('/r', fn () => 'ok')->name('r');\n",
    )
    .unwrap();

    let gone = vendor_root.join("acme/pkg/src/Gone.php");
    let vendor = VendorIndex::from_files(
        vendor_root.clone(),
        vec![
            VendorFile {
                path: gone.clone(),
                depth: 4,
            },
            VendorFile {
                path: vendor_root.join("acme/pkg/src/Real.php"),
                depth: 4,
            },
        ],
    );

    let (routes, commands) = build_route_files_and_command_index(root, &vendor, None);

    assert!(
        !routes.iter().any(|f| f.path == gone),
        "a vanished file must not enter route discovery"
    );
    assert!(
        !commands.files.iter().any(|f| f.path == gone),
        "and must not be recorded as scanned — a verdict with no valid mtime \
         would be trusted forever"
    );
    assert!(
        routes
            .iter()
            .any(|f| f.path == vendor_root.join("acme/pkg/src/Real.php")),
        "fixture check — the pass must continue past the missing file"
    );
}

#[test]
fn a_command_wanted_file_declaring_nothing_is_still_recorded_but_a_route_only_one_is_not() {
    // `Some(None)` and `None` are different answers in the read pass, and the
    // difference is the whole point of the scan cache's schema: "read, and
    // declares nothing" must be recorded so the next scan can skip the file,
    // while "the command side never wanted this file" must not be, or the
    // cache would claim coverage it never had.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    };
    // Wanted by the command leg, declares no command.
    write("vendor/acme/pkg/src/Plain.php", "<?php\nclass Plain {}\n");
    // Pruned by the command leg, still read for routes.
    write(
        "vendor/acme/pkg/node_modules/Bundled.php",
        "<?php\nRoute::get('/b', fn () => 'ok')->name('b');\n",
    );
    let vendor = VendorIndex::build(root);

    let (routes, commands) = build_route_files_and_command_index(root, &vendor, None);
    let recorded = |rel: &str| commands.files.iter().any(|f| f.path == root.join(rel));

    assert!(
        recorded("vendor/acme/pkg/src/Plain.php"),
        "a file the command leg read must be recorded even when it declares \
         nothing — that verdict is what lets the next scan skip it"
    );
    assert!(
        commands
            .files
            .iter()
            .find(|f| f.path == root.join("vendor/acme/pkg/src/Plain.php"))
            .is_some_and(|f| f.entry.is_none()),
        "and recorded as declaring nothing, not as a command"
    );
    assert!(
        !recorded("vendor/acme/pkg/node_modules/Bundled.php"),
        "a file only the route leg wanted must not be recorded as scanned"
    );
    assert!(
        routes
            .iter()
            .any(|f| f.path == root.join("vendor/acme/pkg/node_modules/Bundled.php")),
        "fixture check — that file must really have been read, for routes"
    );
}
