//! Reverse component-usage index — "which Blade files render component X",
//! answered by hash lookup instead of a sweep over every view file.
//!
//! The ancestor walk that resolves a member inside an anonymous Blade partial
//! (`Backend::blade_backing_class_resolution`, #339 item 1) asks this question
//! once per node it visits. A design-system primitive — `<x-icon>`,
//! `<x-button>` — is rendered by hundreds of other partials, none of them a
//! Livewire view, so the walk expands through all of them before it finds an
//! answer or gives up. Asked as a linear scan over `SalsaActor::view_files`,
//! that is one project-wide pass **per visited node**, on the actor thread
//! that serialises every LSP request, per keystroke. Asked here, each step is
//! one `HashMap` lookup.
//!
//! ## What it stores
//!
//! Three maps, kept in sync as files are indexed, edited, and deleted:
//!
//! - **by_component**: `<x-…>` tag name → the Blade files that render it.
//! - **by_livewire**: `<livewire:…>` name → the Blade files that render it.
//! - **by_file**: the names each file contributed, so re-indexing or deleting
//!   one yanks exactly its entries back out without scanning the other two.
//!
//! Plus `pending`, the files whose patterns still have to be folded in — the
//! same deferred-invalidation shape [`crate::symbol_index`] uses, and for the
//! same reason: an edit marks, the next query pays.
//!
//! ## Correctness invariants
//!
//! 1. Every path in a `by_component` / `by_livewire` value appears as a key in
//!    `by_file`, and the name it is filed under appears in that file's entry.
//! 2. `insert_file` is idempotent: folding the same patterns twice leaves the
//!    index identical, because it removes the file's previous entries first.
//! 3. A file is reachable through this index only after it has been queued and
//!    drained. **Every mutation of `SalsaActor::view_files` must queue the
//!    affected path** (`mark_dirty`) or drop it (`remove_file`) — a push that
//!    forgets to queue is a renderer this index cannot see.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::salsa_impl::ParsedPatternsData;

/// Reverse index from a component identity to the Blade files rendering it.
#[derive(Debug, Default)]
pub struct ComponentUsageIndex {
    by_component: HashMap<String, BTreeSet<PathBuf>>,
    by_livewire: HashMap<String, BTreeSet<PathBuf>>,
    by_file: HashMap<PathBuf, FileEntry>,
    pending: BTreeSet<PathBuf>,
}

/// The names one file contributed, kept so its entries can be withdrawn.
#[derive(Debug, Default)]
struct FileEntry {
    components: Vec<String>,
    livewire: Vec<String>,
}

impl ComponentUsageIndex {
    /// Queue `path` for (re-)indexing on the next [`Self::take_pending`].
    ///
    /// Non-Blade paths are ignored: only a `.blade.php` file can carry an
    /// `<x-…>` or `<livewire:…>` tag, and admitting the rest would make every
    /// PHP edit in the project pay for a drain it can never contribute to.
    pub fn mark_dirty(&mut self, path: &Path) {
        if path.to_string_lossy().ends_with(".blade.php") {
            self.pending.insert(path.to_path_buf());
        }
    }

    /// Drain the queue. The caller folds each path back in with
    /// [`Self::insert_file`], or drops it with [`Self::remove_file`] when its
    /// patterns are unreadable — leaving a path drained but neither indexed
    /// nor removed would strand its stale entries.
    pub fn take_pending(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Fold `path`'s component and Livewire tag usage into the index,
    /// replacing whatever it contributed before.
    pub fn insert_file(&mut self, path: &Path, patterns: &ParsedPatternsData) {
        self.remove_file(path);

        let mut entry = FileEntry::default();
        for component in &patterns.components {
            if !entry.components.contains(&component.name) {
                entry.components.push(component.name.clone());
            }
        }
        for livewire in &patterns.livewire_refs {
            if !entry.livewire.contains(&livewire.name) {
                entry.livewire.push(livewire.name.clone());
            }
        }

        for name in &entry.components {
            self.by_component
                .entry(name.clone())
                .or_default()
                .insert(path.to_path_buf());
        }
        for name in &entry.livewire {
            self.by_livewire
                .entry(name.clone())
                .or_default()
                .insert(path.to_path_buf());
        }

        // Filed even when it renders nothing, so invariant 1 holds in both
        // directions and a later `remove_file` has something to withdraw.
        self.by_file.insert(path.to_path_buf(), entry);
    }

    /// Withdraw every entry `path` contributed. Idempotent — a path that was
    /// never indexed removes nothing.
    pub fn remove_file(&mut self, path: &Path) {
        let Some(entry) = self.by_file.remove(path) else {
            return;
        };
        withdraw(&mut self.by_component, &entry.components, path);
        withdraw(&mut self.by_livewire, &entry.livewire, path);
    }

    /// Drop the whole index, including anything still queued.
    pub fn clear(&mut self) {
        self.by_component.clear();
        self.by_livewire.clear();
        self.by_file.clear();
        self.pending.clear();
    }

    /// The Blade files rendering any of `component_names` as an `<x-…>` tag or
    /// any of `livewire_names` as a `<livewire:…>` tag, sorted and deduplicated.
    ///
    /// Sorted because the walk that consumes this takes the FIRST answer of a
    /// level: an unordered result would let two equidistant parents swap places
    /// between calls, so goto would land somewhere different on each keystroke.
    pub fn find(&self, component_names: &[String], livewire_names: &[String]) -> Vec<PathBuf> {
        let mut files: BTreeSet<PathBuf> = BTreeSet::new();
        for name in component_names {
            if let Some(paths) = self.by_component.get(name) {
                files.extend(paths.iter().cloned());
            }
        }
        for name in livewire_names {
            if let Some(paths) = self.by_livewire.get(name) {
                files.extend(paths.iter().cloned());
            }
        }
        files.into_iter().collect()
    }

    /// How many files are folded in — for tests and diagnostics.
    pub fn indexed_file_count(&self) -> usize {
        self.by_file.len()
    }
}

/// Remove `path` from each named bucket, dropping buckets that empty out so
/// the map doesn't accumulate keys for components nothing renders any more.
fn withdraw(map: &mut HashMap<String, BTreeSet<PathBuf>>, names: &[String], path: &Path) {
    for name in names {
        if let Some(paths) = map.get_mut(name) {
            paths.remove(path);
            if paths.is_empty() {
                map.remove(name);
            }
        }
    }
}

#[cfg(test)]
mod tests;
