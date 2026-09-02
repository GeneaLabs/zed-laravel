//! Persistent on-disk cache for the Artisan command index.
//!
//! [`crate::command_index::build_command_index`] walks the *entire* project
//! and `vendor/` tree, reading every `*.php` file to find the handful that
//! `extends ...Command`. On a large Laravel install that's a real chunk of
//! I/O paid on every LSP cold start — even when nothing has changed since the
//! last run. This cache removes that tax: the first build writes the resolved
//! index to disk, and subsequent startups restore it instantly so
//! goto-definition and hover on command strings work without waiting for the
//! walk.
//!
//! It's a cold-start accelerator, not the source of truth. The full
//! [`crate::command_index::build_command_index`] still runs after the restore
//! (`rebuild_command_index` in `main.rs`) to pick up anything that changed
//! while the LSP was off — added/removed command classes included — and then
//! re-saves the refreshed index. The file watcher keeps the index (and this
//! cache) current while the server is running. This mirrors the design of
//! [`crate::pattern_disk_cache`].
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`, where each entry
//! is one command declaration plus the mtime of the file that declares it.
//! Binary because the load is on the startup critical path.
//!
//! ## Invalidation
//!
//! Per-file mtime, identical to [`crate::pattern_disk_cache`]. Each entry
//! stores the mtime observed when the command was indexed; on load we stat the
//! declaring file and only restore the entry if the mtime is byte-identical
//! (both `secs` and `nanos`). A changed, moved, or deleted command file drops
//! its entry — the full rebuild that follows the restore then re-discovers the
//! correct state. A schema-version bump discards the whole cache.
//!
//! ## Location
//!
//! The same XDG-compliant project-hash directory the pattern cache lives in,
//! alongside it as `command_cache.bin`. One cache per project root path.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::command_index::{CommandEntry, CommandIndex};

/// Bump this when the cached structures change — old caches are discarded on
/// read instead of risking deserialization into a struct that no longer
/// matches.
///
/// History:
///   v1 — initial command index cache (command declarations only).
///   v2 — records EVERY scanned `*.php` file with its mtime, not just the ones
///        that declared a command. A cache of declarations alone can say
///        "this command is unchanged", but it cannot say "this file declares
///        nothing and still declares nothing" — so it could never authorise
///        skipping the read. Recording the negative verdicts is what turns the
///        cache from a cold-start display accelerator into a real skip
///        (issue #371).
const SCHEMA_VERSION: u32 = 2;

const CACHE_FILENAME: &str = "command_cache.bin";

/// On-disk envelope. The version field is checked before we try to decode the
/// entries, so a stale cache from an older build is dropped instead of
/// crashing the LSP.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    /// Every `*.php` file the last scan looked at, **in walk order**. Order is
    /// load-bearing: `CommandIndex::insert_entry` keeps the first file walked
    /// on a same-tier duplicate, so replaying these in a different order could
    /// resolve a colliding command name to a different file.
    files: Vec<ScannedFile>,
}

/// One file the scan looked at, the mtime observed then, and the command it
/// declared — `None` when it declared none.
///
/// Both `secs` and `nanos` are stored independently for byte-exact comparison:
/// a `touch` that preserves the second but bumps the nanos must still count as
/// "changed".
///
/// The `None` case is the whole point of schema v2. A cache holding only
/// declarations can tell you a known command is unchanged, but it cannot tell
/// you that the other 16,000 files still declare nothing — so every one of them
/// had to be read and regex-scanned again on every startup.
#[derive(Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub path: PathBuf,
    pub entry: Option<CommandEntry>,
}

/// Where the cache file for `project_root` lives on disk. Returns `None` only
/// if the user's home directory can't be resolved — effectively infallible in
/// practice. Hashes the canonical root path so the location matches
/// [`crate::pattern_disk_cache`]'s per-project directory.
fn cache_file_path(project_root: &Path) -> Option<PathBuf> {
    let cache_base = crate::cache_root::cache_root()?;
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());
    Some(cache_base.join(project_hash).join(CACHE_FILENAME))
}

/// Decompose a `SystemTime` into `(secs, nanos)` relative to UNIX_EPOCH.
/// `None` if the time predates the epoch — shouldn't happen for real files,
/// but we drop the entry rather than panic.
fn split_mtime(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

/// Read the file's mtime as `(secs, nanos)`. `None` if the file doesn't exist
/// or isn't reachable — caller treats both as "no cache for this path."
fn read_mtime(path: &Path) -> Option<(u64, u32)> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(split_mtime)
}

/// A previous scan's per-file verdicts, keyed by path.
///
/// Consulted by [`crate::command_index::scan_commands`] to skip reading a file
/// whose mtime is unchanged — the same per-file staleness model
/// [`crate::pattern_disk_cache`] uses.
#[derive(Default)]
pub struct CommandScanCache {
    by_path: std::collections::HashMap<PathBuf, ScannedFile>,
}

impl CommandScanCache {
    /// The verdict recorded for `path`, **only if** the file's current mtime
    /// still matches what the scan observed.
    ///
    /// `Some(None)` means "known to declare nothing" — a real answer, and the
    /// one that saves nearly all the work. `None` means unknown or stale, so
    /// the caller must read the file.
    ///
    /// `current` is passed in rather than stat'd here so the caller can reuse
    /// the `metadata` its directory walk already produced.
    pub fn verdict(&self, path: &Path, current: (u64, u32)) -> Option<Option<&CommandEntry>> {
        let cached = self.by_path.get(path)?;
        (cached.mtime_secs == current.0 && cached.mtime_nanos == current.1)
            .then_some(cached.entry.as_ref())
    }

    /// How many files the cache holds verdicts for.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }
}

/// Load the previous scan's verdicts for `project_root`.
///
/// `None` when there is no cache, it is unreadable, or the schema does not
/// match — the caller then does a full scan, which is exactly the pre-cache
/// behaviour.
///
/// Entries are NOT mtime-validated here. Validation happens per file in
/// [`CommandScanCache::verdict`], against the mtime the caller's walk already
/// read, so this load performs no I/O beyond the single cache-file read.
pub fn load_scan(project_root: &Path) -> Option<CommandScanCache> {
    let path = cache_file_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;

    let cache: CacheFile =
        match bincode::serde::decode_from_slice(&bytes, bincode::config::standard()) {
            Ok((c, _)) => c,
            Err(e) => {
                tracing::debug!("command_disk_cache: decode failed, ignoring: {}", e);
                return None;
            }
        };

    if cache.schema_version != SCHEMA_VERSION {
        tracing::info!(
            "command_disk_cache: schema mismatch (disk={}, current={}), ignoring",
            cache.schema_version,
            SCHEMA_VERSION
        );
        return None;
    }

    let mut by_path = std::collections::HashMap::with_capacity(cache.files.len());
    for file in cache.files {
        by_path.insert(file.path.clone(), file);
    }
    Some(CommandScanCache { by_path })
}

/// Restore a command index from the cache for `project_root`, validating every
/// entry against its declaring file's current mtime.
///
/// The cold-start accelerator: goto-definition and hover on command strings
/// work off this while the full scan runs. Returns `None` when there is no
/// usable cache.
///
/// Files are replayed in the recorded walk order so the App > Package >
/// Framework merge — and its first-walked-wins tie-break on same-tier
/// duplicates — reproduces the scan that wrote the cache.
pub fn load_index(project_root: &Path) -> Option<CommandIndex> {
    let path = cache_file_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;

    let cache: CacheFile =
        match bincode::serde::decode_from_slice(&bytes, bincode::config::standard()) {
            Ok((c, _)) => c,
            Err(e) => {
                tracing::debug!("command_disk_cache: decode failed, ignoring: {}", e);
                return None;
            }
        };
    if cache.schema_version != SCHEMA_VERSION {
        return None;
    }

    let mut index = CommandIndex::default();
    for file in cache.files {
        let Some(entry) = file.entry else { continue };
        // Only restore a declaration whose file is still present and unchanged.
        match read_mtime(&entry.file) {
            Some((s, n)) if s == file.mtime_secs && n == file.mtime_nanos => {
                index.insert_entry(entry);
            }
            _ => {}
        }
    }
    Some(index)
}

/// Persist a completed scan. Called after every successful build/rebuild.
/// Safe to run on the blocking pool — it is sync I/O.
///
/// Returns the number of file verdicts written. Errors are advisory: failing
/// to persist does not affect the live in-memory index, it only costs the next
/// startup its shortcut.
///
/// Unlike the v1 `save_index` this replaces, the mtimes are NOT re-stat'd here.
/// They are the ones the scan observed when it read each file, which is the
/// only value that makes the verdict sound: re-stat'ing at save time would
/// stamp a file modified DURING the scan as clean, and the next run would trust
/// a verdict derived from the pre-edit contents.
pub fn save_scan(files: &[ScannedFile], project_root: &Path) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;

    let total = files.len();
    let cache = CacheFile {
        schema_version: SCHEMA_VERSION,
        files: files.to_vec(),
    };

    let encoded = bincode::serde::encode_to_vec(&cache, bincode::config::standard())
        .context("bincode encode failed")?;

    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).context("could not create cache directory")?;
    }
    // Write to a temp file then rename — atomic on POSIX, so a crash mid-save
    // leaves the previous cache intact rather than a truncated one.
    let tmp = cache_path.with_extension("bin.tmp");
    std::fs::write(&tmp, &encoded).context("write tmp cache file failed")?;
    std::fs::rename(&tmp, &cache_path).context("rename tmp cache file failed")?;

    Ok(total)
}

#[cfg(test)]
mod tests;
