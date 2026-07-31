//! The per-user cache root, plus the one-shot migration off the pre-rebrand
//! directory name.
//!
//! Every on-disk cache in this crate — patterns, magic methods, commands,
//! vendor aliases, and the [`crate::cache_manager`] blob — hangs off a single
//! per-user root resolved through [`directories::ProjectDirs`]. Routing them
//! all through [`cache_root`] means they cannot drift apart, and the rebrand
//! migration runs exactly once per process no matter which cache happens to
//! touch disk first.
//!
//! Locations (application name `laravel-ce-lsp`):
//! - Linux: `~/.cache/laravel-ce-lsp/`
//! - macOS: `~/Library/Caches/org.mike-bronner.laravel-ce-lsp/`
//! - Windows: `%LOCALAPPDATA%\mike-bronner\laravel-ce-lsp\cache\`

use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{info, warn};

const QUALIFIER: &str = "org";
const ORGANIZATION: &str = "mike-bronner";

/// Application name for the cache directory.
const APPLICATION: &str = "laravel-ce-lsp";

/// The pre-rebrand application name. Every cache written by a release before
/// the "Laravel CE" rename lives under this name; [`cache_root`] moves the
/// whole directory across once, so upgrading users keep their warm index
/// instead of paying for a cold re-scan.
///
/// Unrelated to the `laravel-lsp` language-server id Zed keys user settings
/// off — that id is deliberately unchanged.
const LEGACY_APPLICATION: &str = "laravel-lsp";

/// What [`migrate_legacy_cache`] did, so callers (and tests) can tell the
/// four outcomes apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Migration {
    /// The legacy directory was moved to the new location.
    Moved,
    /// There was no legacy directory to move — a fresh install, or a machine
    /// that already migrated and had the legacy directory cleaned up.
    NoLegacyCache,
    /// The new location already exists, so the legacy directory is left
    /// exactly where it is. Merging two live cache trees risks mixing stale
    /// entries into a good cache; preferring the new one is the safe read.
    AlreadyPresent,
    /// The move was attempted and failed (permissions, a cross-device
    /// boundary, a hostile parent path). The new location is used anyway —
    /// the cost is one cold re-index, never a server that won't start.
    Failed,
}

/// The per-user cache root, with any legacy cache migrated in on first call.
///
/// Returns `None` only when the user's home directory can't be resolved.
/// Every OS this server supports has one, so that is effectively unreachable
/// in practice; callers already treat it as "no disk cache available" and
/// fall back to computing from source.
pub fn cache_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

    ROOT.get_or_init(|| {
        let root = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)?
            .cache_dir()
            .to_path_buf();

        if let Some(legacy) = ProjectDirs::from(QUALIFIER, ORGANIZATION, LEGACY_APPLICATION) {
            migrate_legacy_cache(legacy.cache_dir(), &root);
        }

        Some(root)
    })
    .clone()
}

/// Move `legacy` to `new`, but only when `new` doesn't exist yet.
///
/// Never destructive: an existing `new` short-circuits before anything is
/// touched, and the move itself is a plain rename, so a failure leaves the
/// legacy cache intact for a later attempt. A failure is reported, not
/// propagated — a cache is an optimisation, and refusing to start the
/// language server because an old directory wouldn't move would be a far
/// worse outcome than re-indexing once.
pub fn migrate_legacy_cache(legacy: &Path, new: &Path) -> Migration {
    if new.exists() {
        return Migration::AlreadyPresent;
    }
    if !legacy.exists() {
        return Migration::NoLegacyCache;
    }

    // `fs::rename` will not create the destination's parent. On macOS and
    // Linux that parent is the shared cache root and always exists, but on
    // Windows the new location nests under its own `laravel-ce-lsp\` folder,
    // which won't exist until something creates it.
    //
    // A failure here deliberately gets no branch of its own: the rename below
    // is the real gate, and it fails in turn on exactly the same conditions
    // and reports them. An early return would be a second path to the
    // identical outcome that no test could tell apart.
    if let Some(parent) = new.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match fs::rename(legacy, new) {
        Ok(()) => {
            info!(
                "Migrated cache from {} to {}",
                legacy.display(),
                new.display()
            );
            Migration::Moved
        }
        Err(error) => {
            warn!(
                "Could not migrate the cache from {} to {} ({error}); \
                 continuing with an empty cache",
                legacy.display(),
                new.display()
            );
            Migration::Failed
        }
    }
}

#[cfg(test)]
mod tests;
