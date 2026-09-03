//! Warm-start composition: build the route-file list and the command index
//! from **one** pass over the vendor tree (issue #371).
//!
//! [`crate::route_discovery::discover_route_files_with_vendor`] and
//! [`crate::command_index::build_command_index_with_vendor`] each share the
//! *walk* via [`VendorIndex`], but calling them in sequence still reads every
//! vendor file twice — ~31,400 reads on this repo's `test-project/`, where a
//! single pass costs ~16,200. This module is the one place that knows both
//! consumers, so it can offer each file's text to both classifiers while it is
//! in hand.
//!
//! It lives in its own module rather than inside either consumer to keep the
//! layering acyclic: `vendor_index` is a leaf, `route_discovery` and
//! `command_index` depend on it, and only this module depends on all three.
//!
//! # Shape: classify, then fold in walk order
//!
//! Each file is classified by a pure function ([`cached_verdict`],
//! [`vendor_route_verdict`], [`command_entry_for`]) and the answer is folded
//! into the accumulators by [`record_verdict`] and `RouteFileSet::promote`.
//! The fold order is not an implementation detail: `CommandIndex::insert_entry`
//! resolves a same-tier duplicate to the **first file walked**, and
//! `CommandScan::files` is persisted in insertion order and replayed into
//! `insert_entry` on the next startup. Both are pinned by tests in this module.
//!
//! # This pass is deliberately NOT parallel (issue #373)
//!
//! #373 asked for the classifiers to be parallelized, estimating ~0.5 s. It was
//! built, measured, and removed. Keep it serial unless the numbers below change:
//!
//! * **In isolation it looks like a clear win** — 397.7 ms → 191.3 ms, 2.1x, on
//!   16,050 vendor files. `benches/vendor_parallelism.rs` still measures the
//!   stage split that predicts it.
//! * **It bought nothing on cold start, because this pass is not on the
//!   critical path.** It finishes about a second before the warm parse pass
//!   does, and startup ends when that parse ends. Swept against real startup,
//!   serial was 2,701 ms and the best parallel width 2,684 ms — 17 ms, against
//!   a ~137 ms run-to-run noise floor.
//! * **It cost 33-236 ms on warm start**, where the command disk cache settles
//!   nearly every file in the pre-pass. With almost nothing left to read, the
//!   parallelized work is ~16k `metadata` calls, which get *worse* with more
//!   threads and contend with `pattern_disk_cache`'s own load.
//! * **On the shared bounded pool it also stole ~139 ms from the parse pass**
//!   it runs beside.
//!
//! The bench misleads here because it passes `None` for the command scan cache,
//! so it always measures the all-reads shape — which production sees only on a
//! cold start, the one regime where this pass's duration does not matter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::command_disk_cache::CommandScanCache;
use crate::command_index::{
    cached_verdict, command_entry_for, consider_project_commands, record_verdict,
    vendor_command_needs_source, CommandScan, PrepassVerdict,
};
use crate::route_discovery::{
    collect_conventional_vendor_route_files, collect_project_route_files,
    vendor_route_needs_source, vendor_route_verdict, RouteFile, RouteFileSet,
};
use crate::vendor_index::VendorIndex;

/// Build both vendor-derived indexes, reading each vendor file at most once.
///
/// Output is identical to calling
/// [`crate::route_discovery::discover_route_files_with_vendor`] and
/// [`crate::command_index::build_command_index_with_vendor`] separately —
/// each consumer applies its own predicates to the same shared walk, and
/// neither classifier can observe the other. Only the read count differs.
///
/// The non-vendor legs run first, exactly as each consumer runs them, so the
/// project/vendor ordering the command index's same-tier tie-break depends on
/// is preserved.
pub fn build_route_files_and_command_index(
    root: &Path,
    vendor: &VendorIndex,
    command_cache: Option<&CommandScanCache>,
) -> (Vec<RouteFile>, CommandScan) {
    let mut routes = RouteFileSet::default();
    let mut commands = CommandScan::default();

    collect_project_route_files(root, &mut routes);
    collect_conventional_vendor_route_files(vendor, &mut routes);
    consider_project_commands(root, command_cache, &mut commands);

    let vendor_root = vendor.vendor_root().to_path_buf();

    // Pre-pass: settle the command side from the scan cache wherever it can be
    // settled, so the read pass below knows which files still have to be
    // opened for it. Costs one `metadata` per command-wanted file, which the
    // verdict check needs anyway, and saves the read for the overwhelming
    // majority whose mtime has not moved (issue #371).
    //
    // The mtime is KEPT, not just the decision, and carried to the fold below.
    // The command side used to stat each file again to stamp its entry, so
    // every file that needed a read was stat'd twice — 225 ms of this pass's
    // 962 ms on a Windows CI runner, and about a fifth of it on Linux and macOS
    // (issue #373, measured by `benches/vendor_parallelism.rs`). Carrying the
    // value across removes the second stat outright.
    let mut command_needs_read: HashMap<PathBuf, (u64, u32)> = HashMap::new();
    for file in vendor.files() {
        if !vendor_command_needs_source(&vendor_root, file) {
            continue;
        }
        match cached_verdict(command_cache, &file.path) {
            // Nothing recorded, and deliberately not read either.
            PrepassVerdict::Unstattable => {}
            PrepassVerdict::Settled { mtime, entry } => {
                record_verdict(&mut commands, &file.path, mtime, entry)
            }
            PrepassVerdict::NeedsSource { mtime } => {
                command_needs_read.insert(file.path.clone(), mtime);
            }
        }
    }

    // Read pass: classify each file, then fold the verdict, in walk order.
    vendor.for_each_source(
        |file| vendor_route_needs_source(file) || command_needs_read.contains_key(&file.path),
        |file, content| {
            if vendor_route_needs_source(file) {
                if let Some((path, priority)) = vendor_route_verdict(&file.path, content) {
                    routes.promote(path, priority);
                }
            }
            // Reached only for a file the command side asked for, so the
            // "declares nothing" verdict is recorded here too — that is what
            // lets the next scan skip the file rather than re-read it.
            if let Some(mtime) = command_needs_read.get(&file.path) {
                record_verdict(
                    &mut commands,
                    &file.path,
                    *mtime,
                    command_entry_for(&file.path, content),
                );
            }
        },
    );

    (routes.into_files(), commands)
}

#[cfg(test)]
mod tests;
