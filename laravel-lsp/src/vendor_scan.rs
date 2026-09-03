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
//! # Shape: map in parallel, fold in order (issue #373)
//!
//! Both halves of the pass — the stat pre-pass and the read pass — run their
//! per-file work across the server's bounded pool and then fold the answers
//! sequentially. The fold is not an implementation detail: the command index
//! resolves a same-tier duplicate to the *first file walked*, so the answers
//! have to be applied in walk order no matter which worker produced them
//! first. See [`map_in_parallel`].
//!
//! Measured on `test-project/` (16,050 vendor files, macOS, 8 workers), the
//! whole pass went from 397.7 ms to 191.3 ms — **2.1x**. The per-stage serial
//! timings in `benches/vendor_parallelism.rs` are unchanged either side of it
//! (384.1 ms against 387.0 ms), which is the control: the work is the same, only
//! its execution changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::command_disk_cache::CommandScanCache;
use crate::command_index::{
    cached_verdict, command_entry_for, consider_project_commands, record_verdict,
    vendor_command_needs_source, CommandEntry, CommandScan, PrepassVerdict,
};
use crate::route_discovery::{
    collect_conventional_vendor_route_files, collect_project_route_files,
    vendor_route_needs_source, vendor_route_verdict, RouteFile, RouteFileSet,
};
use crate::vendor_index::{VendorFile, VendorIndex};

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
    let wanted: Vec<&VendorFile> = vendor
        .files()
        .iter()
        .filter(|file| vendor_command_needs_source(&vendor_root, file))
        .collect();

    let prepass = map_in_parallel(&wanted, |file| cached_verdict(command_cache, &file.path));

    let mut command_needs_read: HashMap<PathBuf, (u64, u32)> = HashMap::new();
    for (file, verdict) in wanted.iter().zip(prepass) {
        match verdict {
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

    // Read pass. Same two classifiers as before, now run per file across the
    // worker pool instead of one at a time.
    let to_read: Vec<&VendorFile> = vendor
        .files()
        .iter()
        .filter(|file| {
            vendor_route_needs_source(file) || command_needs_read.contains_key(&file.path)
        })
        .collect();

    let classified = map_in_parallel(&to_read, |file| {
        // An unreadable file contributes nothing to either index — the same
        // outcome `VendorIndex::for_each_source` produces by skipping its
        // `visit` call, and the behaviour
        // `for_each_source_skips_an_unreadable_file_without_failing` pins.
        let content = std::fs::read_to_string(&file.path).ok()?;
        Some(FileVerdict {
            route: vendor_route_needs_source(file)
                .then(|| vendor_route_verdict(&file.path, &content))
                .flatten(),
            // `Some(None)` and `None` are different answers: the first is "the
            // command side read this file and it declares nothing", which must
            // still be recorded so the next scan can skip it; the second is
            // "the command side never wanted this file".
            command: command_needs_read
                .contains_key(&file.path)
                .then(|| command_entry_for(&file.path, &content)),
        })
    });

    for (file, verdict) in to_read.iter().zip(classified) {
        let Some(verdict) = verdict else { continue };
        if let Some((path, priority)) = verdict.route {
            routes.promote(path, priority);
        }
        if let Some(entry) = verdict.command {
            let mtime = command_needs_read[&file.path];
            record_verdict(&mut commands, &file.path, mtime, entry);
        }
    }

    (routes.into_files(), commands)
}

/// What one file's read pass decided, for both consumers.
struct FileVerdict {
    /// `Some` when the file is a route file, carrying the priority to promote
    /// it at. `None` covers both "not a route file" and "the route side did
    /// not want this file" — the two are indistinguishable to the fold, which
    /// does nothing in either case.
    route: Option<(PathBuf, u8)>,
    /// `Some` when the command side wanted this file, carrying its
    /// classification (`None` inside means "declares no command").
    command: Option<Option<CommandEntry>>,
}

/// Run `classify` over `files` on the server's bounded pool, returning one
/// answer per file **in input order**.
///
/// Order is the whole reason this is a `map` and a separate fold rather than a
/// `for_each` mutating shared state. `CommandIndex::insert_entry` keeps the
/// first file walked on a same-tier duplicate — pinned by
/// `shared_vendor_walk_keeps_the_same_winner_on_a_same_tier_collision` — and
/// `CommandScan::files` is persisted in insertion order. rayon's indexed
/// `collect` preserves input order regardless of which worker finished when,
/// so folding the result sequentially reproduces the walk exactly.
///
/// The route side needs no such care (`RouteFileSet` merges by maximum
/// priority, so it is order-independent by construction), but it rides the
/// same pass because both classifiers want the same bytes.
///
/// **The reads run here too, not just the CPU.** Issue #373 proposed
/// parallelizing only the classifiers, on an estimate that they dominated the
/// pass. Measurement said otherwise — they are 2-5% of it, and the stat and
/// read stages are 93-96% — so parallelizing the classifiers alone would have
/// chased the smallest term. Per-worker memory stays at one file's content
/// because each worker reads, classifies and drops before taking the next.
fn map_in_parallel<T: Send>(
    files: &[&VendorFile],
    classify: impl Fn(&VendorFile) -> T + Sync,
) -> Vec<T> {
    crate::parallelism::install(|| files.par_iter().map(|file| classify(file)).collect())
}

#[cfg(test)]
mod tests;
