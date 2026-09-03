//! One walk of `<root>/vendor`, shared by every subsystem that used to walk it
//! for itself (issue #371).
//!
//! Five independent `WalkDir` passes over the Composer tree existed, none
//! sharing results. On this repo's `test-project/` (16,051 vendor `.php`
//! files) they cost, per startup:
//!
//! | Walk | Depth | Pruned | Files walked | Files read |
//! |---|---|---|---|---|
//! | `salsa_impl::handle_register_project_files` | unbounded | `.git` | 16,051 | 0 |
//! | `main::register_service_provider_files_with_salsa` | 10 + 6 | none | 10,374 | 73 |
//! | `main::rescan_vendor_providers` | 10 + 6 | none | 10,374 | 73 |
//! | `command_index::build_command_index` | unbounded | `SKIP_DIRS` | 16,202 | 16,202 |
//! | `route_discovery::discover_route_files` | 8 | none | 15,225 | ~15,219 |
//!
//! A bare stat walk of that tree costs ~0.25 s warm; reading all 16,051
//! contents adds ~0.15 s on top, before any per-file parsing. So both the
//! duplicated traversals and the ~31,400 duplicated reads were worth removing.
//!
//! # How the sharing preserves behaviour exactly
//!
//! The walk here is the **union** of all five: unbounded depth, nothing
//! pruned. Each consumer then re-applies its own former limits as a *path
//! predicate* over the shared result, so every consumer sees precisely the
//! file set it saw before. That substitution is only sound because none of
//! those limits depended on traversal state:
//!
//! * Depth is a function of the path alone ([`VendorFile::depth`] counts
//!   components below `vendor/`, matching `WalkDir`'s convention where the
//!   walk root is depth 0).
//! * `WalkDir::filter_entry` pruning of a directory is equivalent, for files,
//!   to "no ancestor component is named X" — see [`has_pruned_ancestor`].
//! * Filtering preserves relative order, and [`VendorIndex`] keeps the walk
//!   order it discovered. That matters because `command_index::insert_entry`
//!   breaks a same-tier duplicate in favour of the first file walked. Tier is
//!   itself path-derived (`App` ⟺ not under `vendor/`), so a same-tier
//!   collision can never span the project/vendor split, and preserving order
//!   *within* the vendor leg is enough to preserve the winner.
//!
//! `route_discovery`'s vendor predicates (`is_under_routes_dir`,
//! `priority_for_vendor_path`) are likewise path-only, and `promote` merges by
//! maximum priority rather than by arrival, so it is order-independent too.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// One vendor PHP file, with the walk depth that former callers bounded on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorFile {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Components below the `vendor/` directory, so `vendor/a/b.php` is 2.
    /// Matches `WalkDir::max_depth`, where the walk root itself is depth 0 —
    /// a caller that used `.max_depth(8)` keeps `depth <= 8`.
    pub depth: usize,
}

/// Every `*.php` file under `<root>/vendor`, discovered once.
///
/// `*.blade.php` is included: its extension is still `php`, which is exactly
/// how the former walks admitted it.
#[derive(Debug, Clone, Default)]
pub struct VendorIndex {
    /// The `<root>/vendor` directory this index describes. Empty-pathed when
    /// the project has no `vendor/` at all.
    vendor_root: PathBuf,
    /// Discovered files, in walk order. Order is load-bearing — see the module
    /// docs on same-tier command duplicates.
    files: Vec<VendorFile>,
}

/// True when any directory component of `path` **below `base`** is named in
/// `pruned`.
///
/// This is the path-predicate form of `WalkDir::filter_entry`: pruning a
/// directory removes that directory and its whole subtree, so a file survives
/// exactly when none of its ancestors was pruned. Each consumer passes its own
/// former `filter_entry` list, so there is no copy of anyone's list here to
/// drift.
///
/// Two details the equivalence depends on:
///
/// * **`base` is not optional.** A `WalkDir` only ever sees components below
///   its own root, so components of the absolute prefix must not be considered
///   — otherwise a project living in `/home/me/public/app` would have its
///   entire tree "pruned" by a `public` entry it never contained. A `path`
///   outside `base` is treated as pruned, which fails closed.
/// * **The file's own name is excluded.** `filter_entry` predicates in this
///   crate only ever reject directories, so a *file* named `public` was always
///   kept.
pub fn has_pruned_ancestor(path: &Path, base: &Path, pruned: &[&str]) -> bool {
    let Ok(relative) = path.strip_prefix(base) else {
        return true;
    };
    let mut components: Vec<_> = relative.components().collect();
    components.pop(); // the file name itself is never a pruned directory
    components
        .iter()
        .any(|c| c.as_os_str().to_str().is_some_and(|n| pruned.contains(&n)))
}

impl VendorIndex {
    /// Walk `<root>/vendor` once, collecting every `*.php` file.
    ///
    /// Unbounded depth and no pruning, deliberately: this is the union of what
    /// the five former walks each saw, and consumers narrow it themselves.
    /// A project with no `vendor/` yields an empty index rather than an error —
    /// every consumer's former code simply skipped the walk in that case.
    pub fn build(root: &Path) -> Self {
        let vendor_root = root.join("vendor");
        if !vendor_root.is_dir() {
            return Self::default();
        }
        let files = WalkDir::new(&vendor_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
            .map(|e| VendorFile {
                depth: e.depth(),
                path: e.path().to_path_buf(),
            })
            .collect();
        Self { vendor_root, files }
    }

    /// Build an index directly from `(path, depth)` pairs. Test seam only —
    /// production always goes through [`VendorIndex::build`].
    pub fn from_files(vendor_root: PathBuf, files: Vec<VendorFile>) -> Self {
        Self { vendor_root, files }
    }

    /// The `<root>/vendor` directory, or an empty path when absent.
    pub fn vendor_root(&self) -> &Path {
        &self.vendor_root
    }

    /// True when the project has no `vendor/`, or it holds no PHP at all.
    /// Consumers use this to keep their former "skip the whole branch" guard.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Every discovered file, in walk order.
    pub fn files(&self) -> &[VendorFile] {
        &self.files
    }

    /// Files at or below `max_depth`, in walk order — the replacement for a
    /// former `WalkDir::new(vendor).max_depth(max_depth)`.
    pub fn within_depth(&self, max_depth: usize) -> impl Iterator<Item = &Path> {
        self.files
            .iter()
            .filter(move |f| f.depth <= max_depth)
            .map(|f| f.path.as_path())
    }

    /// Read each file `wants` accepts, at most once, in walk order, and hand
    /// its text to `visit`.
    ///
    /// The point of the two-callback shape: several consumers want overlapping
    /// but unequal subsets of the tree, and the expensive part is the read, not
    /// the predicate. `wants` is evaluated first for every file, and a file no
    /// consumer wants is never opened — so this performs exactly the union of
    /// its callers' former reads, never more.
    ///
    /// Unreadable files are skipped silently, matching every former call site
    /// (all of them used `if let Ok(content) = read_to_string(..)`).
    pub fn for_each_source(
        &self,
        wants: impl Fn(&VendorFile) -> bool,
        mut visit: impl FnMut(&VendorFile, &str),
    ) {
        for file in &self.files {
            if !wants(file) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&file.path) {
                visit(file, &text);
            }
        }
    }
}

#[cfg(test)]
mod tests;

/// Components of `path` below `base`, matching `WalkDir::max_depth` for a walk
/// rooted at `base`. `None` when `path` is not under `base`.
///
/// Lets a consumer whose former walk was rooted *inside* `vendor/` — the
/// service-provider scan's `vendor/laravel/framework/src/Illuminate` leg —
/// re-apply its depth budget against its own root rather than against
/// `vendor/`, so the budget does not silently shift with the package layout.
pub fn depth_below(path: &Path, base: &Path) -> Option<usize> {
    path.strip_prefix(base)
        .ok()
        .map(|rest| rest.components().count())
}
