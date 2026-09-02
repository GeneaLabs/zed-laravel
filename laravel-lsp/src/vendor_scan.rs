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

use std::path::Path;

use crate::command_index::{
    index_command_file, index_project_commands, vendor_command_needs_source, CommandIndex,
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
) -> (Vec<RouteFile>, CommandIndex) {
    let mut routes = RouteFileSet::default();
    let mut commands = CommandIndex::default();

    collect_project_route_files(root, &mut routes);
    collect_conventional_vendor_route_files(vendor, &mut routes);
    index_project_commands(root, &mut commands);

    let vendor_root = vendor.vendor_root().to_path_buf();
    vendor.for_each_source(
        |file| vendor_route_needs_source(file) || vendor_command_needs_source(&vendor_root, file),
        |file, content| {
            if vendor_route_needs_source(file) {
                accept_vendor_route_source(&mut routes, &file.path, content);
            }
            if vendor_command_needs_source(&vendor_root, file) {
                index_command_file(&mut commands, &file.path, content);
            }
        },
    );

    (routes.into_files(), commands)
}

#[cfg(test)]
mod tests;
