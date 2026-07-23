//! Reverse dependency index for the magic-member system: which files
//! resolved member-access receivers against which classes.
//!
//! This is what makes the save-time refresh *incremental* (#80). When a
//! saved file changes a class's surface, the old behavior re-resolved the
//! entire project; this index answers "which files actually reference that
//! class?" so only the genuine blast radius re-resolves.
//!
//! **Populated from attempts, not successes.** The resolvers record every
//! receiver FQCN they *try* to classify against — including lookups where
//! the member doesn't (yet) exist on the class. That asymmetry is the
//! point: if `$user->avatar` fails today because `User` has no `avatar`,
//! the file still depends on `User` — adding the member tomorrow must
//! re-resolve this file or the new reference stays invisible until a full
//! rebuild. Receiver FQCNs come from use-statements and type-hints, so
//! they're recordable even when the class isn't indexed at all.
//!
//! Mirrors `symbol_index` ownership: actor-owned, no internal locking,
//! all access serialized through the actor queue.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Key-space prefix for a *container-binding attempt* dependency: a site whose
/// receiver is a string-keyed container resolution (`app('key')`,
/// `resolve('key')`, or a mapped zero-arg helper like `view()`) records
/// `binding:<key>` — resolved or not. The colon keeps the space disjoint from
/// FQCNs (`:` can't appear in a PHP class name), and recording the *abstract*
/// key is what lets a brand-new binding ripple to its call sites on the
/// provider save (#255): those sites resolved to nothing before, so they hold
/// no concrete-FQCN dependency the registration diff could otherwise reach.
pub const BINDING_DEP_PREFIX: &str = "binding:";

/// Key-space prefix for a *facade-alias attempt* dependency: a site whose
/// receiver is a bare/root-qualified facade token resolved through the global
/// alias map (`Auth::check()`, `\Cache::get()`) records `alias:<token>` — the
/// alias token lower-cased, resolved or not. The colon keeps the space disjoint
/// from FQCNs, and the analogue to `BINDING_DEP_PREFIX` is deliberate: recording
/// the stable *token* (not the resolved concrete) is what lets an alias
/// **retarget** ripple the OLD target's dependent sites on the very first save of
/// a session, when the registration baseline is still empty and the diff only
/// sees the new target added (#267). The lower-casing mirrors the
/// case-insensitive facade-alias matching in [`crate::facade_resolver`], so both
/// the call site and the registration diff agree on the key regardless of
/// source casing.
pub const ALIAS_DEP_PREFIX: &str = "alias:";

/// Build the `alias:<token>` attempt key for a facade alias `token`, lower-cased
/// to match the case-insensitive facade matching in [`crate::facade_resolver`].
/// Both the call-site recorder ([`crate::member_resolver`]) and the registration
/// diff ([`crate::salsa_impl::registration_ripple_keys`]) MUST build the key
/// through this one function, or the two would drift and an alias retarget would
/// ripple keys no call site recorded (#267).
pub fn alias_dep_key(token: &str) -> String {
    format!("{ALIAS_DEP_PREFIX}{}", token.to_ascii_lowercase())
}

#[derive(Default, Debug)]
pub struct MagicDependencyIndex {
    /// fqcn → files that resolved a receiver against it.
    dependents: HashMap<String, HashSet<PathBuf>>,
    /// file → FQCNs it referenced (for eviction on re-index).
    by_file: HashMap<PathBuf, HashSet<String>>,
}

impl MagicDependencyIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `path`'s recorded dependencies with `fqcns`
    /// (evict-then-insert, same contract as the other actor indexes).
    pub fn replace_file(&mut self, path: &Path, fqcns: HashSet<String>) {
        self.remove_file(path);
        if fqcns.is_empty() {
            return;
        }
        for fqcn in &fqcns {
            self.dependents
                .entry(fqcn.clone())
                .or_default()
                .insert(path.to_path_buf());
        }
        self.by_file.insert(path.to_path_buf(), fqcns);
    }

    /// Drop `path`'s contribution entirely (file deleted or re-indexing).
    pub fn remove_file(&mut self, path: &Path) {
        let Some(fqcns) = self.by_file.remove(path) else {
            return;
        };
        for fqcn in fqcns {
            if let Some(files) = self.dependents.get_mut(&fqcn) {
                files.remove(path);
                if files.is_empty() {
                    self.dependents.remove(&fqcn);
                }
            }
        }
    }

    /// Union of files that reference any of `fqcns`. The save flow feeds
    /// this the surface-changed classes plus their transitive descendants
    /// and re-resolves exactly the returned set.
    pub fn dependents_of<'a, I>(&self, fqcns: I) -> HashSet<PathBuf>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut out = HashSet::new();
        for fqcn in fqcns {
            if let Some(files) = self.dependents.get(fqcn) {
                out.extend(files.iter().cloned());
            }
        }
        out
    }

    /// Drop everything — paired with the full-rebuild path, which
    /// repopulates from scratch.
    pub fn clear(&mut self) {
        self.dependents.clear();
        self.by_file.clear();
    }

    /// Snapshot every file's recorded dependencies — the persistence side
    /// of the incremental magic-cache re-save (#80).
    pub fn export(&self) -> Vec<(PathBuf, HashSet<String>)> {
        self.by_file
            .iter()
            .map(|(path, fqcns)| (path.clone(), fqcns.clone()))
            .collect()
    }

    /// Number of files with recorded dependencies. For logs.
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Number of distinct FQCNs referenced. For logs.
    pub fn class_count(&self) -> usize {
        self.dependents.len()
    }
}

#[cfg(test)]
mod tests;
