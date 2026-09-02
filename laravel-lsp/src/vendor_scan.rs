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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::command_disk_cache::CommandScanCache;
use crate::command_index::{
    consider_project_commands, record_source, try_cached, vendor_command_needs_source, CommandScan,
};
use crate::route_discovery::{
    accept_vendor_route_source, collect_conventional_vendor_route_files,
    collect_project_route_files, vendor_route_needs_source, RouteFile, RouteFileSet,
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
    let mut command_needs_read: HashSet<PathBuf> = HashSet::new();
    for file in vendor.files() {
        if vendor_command_needs_source(&vendor_root, file)
            && !try_cached(&mut commands, command_cache, &file.path)
        {
            command_needs_read.insert(file.path.clone());
        }
    }

    vendor.for_each_source(
        |file| vendor_route_needs_source(file) || command_needs_read.contains(&file.path),
        |file, content| {
            if vendor_route_needs_source(file) {
                accept_vendor_route_source(&mut routes, &file.path, content);
            }
            if command_needs_read.contains(&file.path) {
                record_source(&mut commands, &file.path, content);
            }
        },
    );

    (routes.into_files(), commands)
}

#[cfg(test)]
mod tests;
