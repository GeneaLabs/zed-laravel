//! Tests for the combined warm-start pass (issue #371).
//!
//! Two claims to pin, and they pull in opposite directions — which is why both
//! are needed:
//!
//! 1. **Identical output.** Sharing the read must not change either index.
//! 2. **Fewer reads.** If it did not, the module would be pure overhead.

use super::*;
use crate::command_index::{build_command_index_with_vendor, CommandIndex, CommandPriority};
use crate::route_discovery::discover_route_files_with_vendor;
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
