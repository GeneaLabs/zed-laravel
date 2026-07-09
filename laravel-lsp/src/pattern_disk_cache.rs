//! Persistent on-disk cache for parsed file patterns.
//!
//! Without this, every Zed startup re-parses the entire project — even
//! when not a single file has changed. On a 40k-file Laravel project
//! that's a ~7-second tax on every editor reopen. With it: the first
//! startup parses everything and writes the cache to disk; subsequent
//! startups stat every project file (fast — pure metadata, no reads),
//! restore unchanged entries from the cache, and only re-parse the
//! files whose mtime has actually changed.
//!
//! ## Format
//!
//! `bincode`-encoded `CacheFile { schema_version, entries }`. Binary
//! because the load runs on the critical path of LSP startup and JSON
//! decode of 40k entries is ~10× slower than bincode.
//!
//! ## Invalidation
//!
//! Per-file mtime. Each entry stores the mtime at parse time; on load
//! we stat the path and only restore the entry if the mtime is byte-
//! identical (both `secs` and `nanos`). Anything else — file edited,
//! file deleted, vendor reinstalled, schema version bumped — falls
//! through to a fresh parse, which is exactly what warming already
//! handles.
//!
//! ## Location
//!
//! Same XDG-compliant project-hash directory the existing `cache.json`
//! lives in, alongside it as `pattern_cache.bin`. One cache per project
//! root path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use dashmap::DashMap;
use directories::ProjectDirs;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::class_hierarchy_index::ClassNode;
use crate::salsa_impl::ParsedPatternsData;

/// Bump this when the cache format changes — old caches are discarded
/// on read instead of risking deserialization into a struct that no
/// longer matches.
///
/// History:
///   v1 — initial pattern cache.
///   v2 — @livewire('name') directive form added to `livewire_refs`
///        so directive-form references are classified and indexed the
///        same as `<livewire:name>` tag form. Old caches lacked these
///        entries, breaking goto/hover/rename on the directive form.
///   v3 — property-form member accesses (`$user->email`) now populated by
///        the warming path. Old caches deserialize them empty (serde
///        default), so the magic-member index (M4) would build empty;
///        bump to force a re-parse that captures them.
///   v4 — class-hierarchy nodes persisted per entry so the hierarchy index
///        (and the magic-member index built from it) survive a warm restart
///        instead of being empty until something re-parses.
///   v5 — Blade-embedded member accesses (`{{ $user->email }}`,
///        `{{ auth()->user()->email }}`) are now captured into
///        `member_access_refs` for `.blade.php` files. Caches written before
///        this restore Blade entries with empty refs (serde default) and, being
///        schema-valid, are NOT re-parsed — so the magic-member index skips all
///        Blade/Volt/auth usages and find-references finds only PHP `$this->`
///        self-references. Bump forces a re-parse that captures them.
///   v6 — Blade `@foreach` loop metadata (`blade_loops`) now captured so a
///        loop variable (`@foreach($users as $user) … {{ $user->email }}`) can
///        be typed from its iterable. Caches written before this deserialize it
///        empty (serde default), so loop-variable usages wouldn't resolve on
///        restored files; bump to force a re-parse that captures the loops.
///   v7 — member accesses inside `@foreach` iterables (`$this->entities`) are
///        now captured into `member_access_refs` for Blade files (directive
///        args the echo/PHP capture misses). Caches from before lack them on
///        restored files; bump to force a re-parse that captures them.
///   v8 — member accesses inside Blade attribute expressions — bound (`:icon=
///        "$post->is_published ? …"`) and directive (`@class(['x' => $p->y])`)
///        — now captured. Caches from before lack these usages on restored
///        files; bump to force a re-parse that captures them.
///   v9 — call-form member accesses (`->active()`, `User::whereEmail()`, #77)
///        now captured with an `AccessForm` field. Caches from before lack
///        every call-form site; bump to force a re-parse.
///   v10 — interpolated-string extraction changed (#84): fragments are
///        skipped, config keys constant-propagate to full dotted keys.
///        Cached patterns from older extractors still carry fragment keys
///        (`.export_connection`) on restored files; bump to force a
///        re-parse. The envelope SHAPE didn't change — only the extraction
///        OUTPUT — which is exactly why an output-affecting change must
///        bump this even when serde would happily decode the old bytes.
///   v11 — M1 single-parse capture: `ParsedPatternsData` grew a
///        `member_context` (per-site receiver recipes + view-render plans +
///        Volt surface), compiled at parse so the magic build stops re-reading
///        target files. bincode is non-self-describing, so a v10 entry lacks
///        those bytes entirely — stale slim entries must re-parse rather than
///        mis-decode. The bump also guarantees every restored non-vendor entry
///        carries context, so no "refs present but context missing" state can
///        exist on the resolve path.
const SCHEMA_VERSION: u32 = 11;

const CACHE_FILENAME: &str = "pattern_cache.bin";

/// On-disk envelope. The version field is checked before we try to
/// decode the entries map, so a stale cache from an older build just
/// gets dropped instead of crashing the LSP.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    entries: HashMap<PathBuf, CachedEntry>,
}

/// One file's worth of cached patterns plus the mtime we observed when
/// the patterns were parsed. Both `secs` and `nanos` are stored
/// independently because we need byte-exact comparison: APFS gives us
/// nanosecond precision and we don't want a `touch` (which preserves
/// the second but changes the nanos) to slip past as "unchanged."
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    mtime_secs: u64,
    mtime_nanos: u32,
    patterns: ParsedPatternsData,
    /// Class-hierarchy nodes declared in this file. Persisted so the
    /// hierarchy index is restored on a warm start rather than left empty
    /// until a fresh parse repopulates it. `#[serde(default)]` so a future
    /// field addition doesn't have to bump the version for this one.
    #[serde(default)]
    nodes: Vec<ClassNode>,
}

/// Outcome of [`load_into`]: how many entries were restored vs dropped, plus
/// the per-file class-hierarchy nodes of the restored entries (the caller
/// re-imports these so the hierarchy index isn't empty on a warm start).
#[derive(Default)]
pub struct LoadResult {
    pub restored: usize,
    pub dropped: usize,
    pub hierarchy: Vec<(PathBuf, Vec<ClassNode>)>,
}

/// Live counter the parallel freshness pass in [`load_into_reporting`]
/// publishes into, so an async caller can render a moving progress bar
/// during the (multi-second on a cold FS) 40k-entry load instead of one
/// static message. `total` is `0` until the cache is decoded, then the
/// entry count; `done` counts entries processed. Both are monotonic
/// within a single load. All fields are atomics so the rayon workers and
/// the async reporter can touch them concurrently without a lock.
#[derive(Default)]
pub struct LoadProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

/// Where the cache file for `project_root` lives on disk. Returns `None`
/// only if the user's home directory can't be resolved — every modern
/// OS we support has one, so this is effectively infallible in practice.
fn cache_file_path(project_root: &Path) -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("org", "mike-bronner", "laravel-lsp")?;
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());
    Some(
        proj_dirs
            .cache_dir()
            .join(project_hash)
            .join(CACHE_FILENAME),
    )
}

/// Decompose a `SystemTime` into `(secs, nanos)` relative to UNIX_EPOCH.
/// Returns `None` if the time predates the epoch — that shouldn't happen
/// for real files but we'd rather drop the entry than panic.
fn split_mtime(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

/// Read the file's mtime as `(secs, nanos)`. `None` if the file doesn't
/// exist or isn't reachable — caller treats both as "no cache for this
/// path."
fn read_mtime(path: &Path) -> Option<(u64, u32)> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(split_mtime)
}

/// Load the on-disk cache, validate every entry against its current
/// file mtime, and insert the valid ones into the shared `pattern_cache`.
///
/// Returns `(restored, dropped)` — restored is what's now in the live
/// cache and won't be re-parsed during warming; dropped is what was on
/// disk but failed the mtime check (stale, missing, or schema mismatch).
///
/// Errors silently degrade to "no cache available": the calling warming
/// flow handles that fine — it just parses everything from scratch.
///
/// Thin wrapper over [`load_into_reporting`] with no progress plumbing —
/// the entry point for callers (and tests) that don't render a bar.
pub fn load_into(
    pattern_cache: &Arc<DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    project_root: &Path,
) -> LoadResult {
    load_into_reporting(pattern_cache, project_root, None)
}

/// Same as [`load_into`], but publishes live per-entry counts into
/// `progress` (when `Some`) so an async caller can render a moving bar
/// during the freshness pass.
///
/// The pass is parallelized with rayon: on a large project the cache
/// holds ~40k entries (source + vendor), and each one needs an
/// independent mtime stat (a filesystem `metadata` call — the dominant
/// cost on a cold FS cache) plus a position-index rebuild. Serially that
/// was a multi-second startup stall; `into_par_iter` fans it across the
/// rayon pool. The result is identical to a serial pass — DashMap inserts
/// are lock-free, the counters are atomics, and the surfaced hierarchy is
/// order-independent (the caller bulk-imports it as a set) — so the only
/// observable change is that it finishes sooner.
pub fn load_into_reporting(
    pattern_cache: &Arc<DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    project_root: &Path,
    progress: Option<&LoadProgress>,
) -> LoadResult {
    let Some(path) = cache_file_path(project_root) else {
        return LoadResult::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        // No cache file yet — first-ever startup for this project.
        return LoadResult::default();
    };

    let cache: CacheFile =
        match bincode::serde::decode_from_slice(&bytes, bincode::config::standard()) {
            Ok((c, _)) => c,
            Err(e) => {
                tracing::debug!("pattern_disk_cache: decode failed, ignoring: {}", e);
                return LoadResult::default();
            }
        };

    if cache.schema_version != SCHEMA_VERSION {
        tracing::info!(
            "pattern_disk_cache: schema mismatch (disk={}, current={}), ignoring",
            cache.schema_version,
            SCHEMA_VERSION
        );
        return LoadResult::default();
    }

    // Publish the denominator now that the cache is decoded, so the async
    // reporter can switch from an indeterminate message to "done of total"
    // the moment the (fast) decode finishes and the (slow) stat pass runs.
    if let Some(p) = progress {
        p.total.store(cache.entries.len(), Ordering::Relaxed);
    }

    // Freshness pass, parallelized. Each entry is validated, and fresh
    // ones are rebuilt + inserted, independently; the closure surfaces the
    // restored entry's hierarchy nodes (or `None`) and bumps the shared
    // counters. `collect` gathers the hierarchy into a Vec whose order
    // doesn't matter — it's imported as a set.
    let restored = AtomicUsize::new(0);
    let dropped = AtomicUsize::new(0);
    let hierarchy: Vec<(PathBuf, Vec<ClassNode>)> = cache
        .entries
        .into_par_iter()
        .filter_map(|(path, entry)| {
            // Stat the file. If it's gone, or its mtime differs from the
            // cached value, drop the entry — warming will re-parse it.
            let node_entry = match read_mtime(&path) {
                Some((s, n)) if s == entry.mtime_secs && n == entry.mtime_nanos => {
                    // Fresh: rebuild the position index (we skipped persisting
                    // it because it duplicates the Vec data) and insert.
                    let mut patterns = entry.patterns;
                    patterns.build_position_index();
                    // Surface the file's hierarchy nodes so the caller can
                    // re-import them — the index is otherwise empty on a warm
                    // start (no parse runs for disk-restored files).
                    let node_entry = (!entry.nodes.is_empty()).then(|| (path.clone(), entry.nodes));
                    pattern_cache.insert(path, (0, Arc::new(patterns)));
                    restored.fetch_add(1, Ordering::Relaxed);
                    node_entry
                }
                _ => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    None
                }
            };
            if let Some(p) = progress {
                p.done.fetch_add(1, Ordering::Relaxed);
            }
            node_entry
        })
        .collect();

    LoadResult {
        restored: restored.into_inner(),
        dropped: dropped.into_inner(),
        hierarchy,
    }
}

/// Persist every entry currently in `pattern_cache` to disk, stamped
/// with each file's CURRENT mtime. Called at the end of warming; safe
/// to run on the tokio blocking pool because the work is sync I/O.
///
/// Returns the number of entries written, or an error if we couldn't
/// touch the cache directory or write the file. Errors are advisory —
/// failing to persist doesn't break the in-memory cache.
pub fn save_from(
    pattern_cache: &Arc<DashMap<PathBuf, (i32, Arc<ParsedPatternsData>)>>,
    hierarchy_by_file: &HashMap<PathBuf, Vec<ClassNode>>,
    project_root: &Path,
) -> Result<usize> {
    let cache_path =
        cache_file_path(project_root).context("could not resolve cache directory for project")?;

    let mut entries: HashMap<PathBuf, CachedEntry> = HashMap::with_capacity(pattern_cache.len());

    // Walk the live DashMap and copy out the data we need. We stat each
    // path at save time (rather than trust an mtime we computed earlier)
    // so the cache reflects what's on disk RIGHT NOW. A file that's been
    // modified since parsing won't get a stale mtime stamped against
    // potentially-stale parsed data — the next load will see the actual
    // mtime mismatch and re-parse.
    for entry in pattern_cache.iter() {
        let path = entry.key();
        let (_, ref patterns) = *entry.value();
        let Some((secs, nanos)) = read_mtime(path) else {
            // File vanished between parse and save — skip it. Saving a
            // dangling entry would just waste space; load_into would
            // drop it anyway.
            continue;
        };
        entries.insert(
            path.clone(),
            CachedEntry {
                mtime_secs: secs,
                mtime_nanos: nanos,
                // ParsedPatternsData: Clone is cheap (Arc bumps).
                patterns: (**patterns).clone(),
                nodes: hierarchy_by_file.get(path).cloned().unwrap_or_default(),
            },
        );
    }

    let total = entries.len();
    let cache = CacheFile {
        schema_version: SCHEMA_VERSION,
        entries,
    };

    let encoded = bincode::serde::encode_to_vec(&cache, bincode::config::standard())
        .context("bincode encode failed")?;

    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).context("could not create cache directory")?;
    }
    // Write to a PER-CALL unique temp file, then atomically rename it onto
    // the cache path. Atomic-rename-on-POSIX gives two guarantees at once:
    //   1. Crash safety — a crash mid-write leaves the previous cache
    //      intact rather than a truncated file we'd fail to decode.
    //   2. Concurrent-save safety — the save now runs spawned/background
    //      (see the warming path), so a reindex can start a second save
    //      while a prior one is still writing. A SHARED tmp path would let
    //      the two interleave their bytes into one file and corrupt it; a
    //      unique suffix per call keeps them disjoint, and the two renames
    //      simply race to last-writer-wins on a complete, valid file.
    // The suffix combines the process id with a monotonic counter so it's
    // unique across concurrent saves within this process.
    static SAVE_SEQ: AtomicUsize = AtomicUsize::new(0);
    let unique = format!(
        "bin.tmp.{}.{}",
        std::process::id(),
        SAVE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = cache_path.with_extension(unique);
    std::fs::write(&tmp, &encoded).context("write tmp cache file failed")?;
    std::fs::rename(&tmp, &cache_path).context("rename tmp cache file failed")?;

    Ok(total)
}

#[cfg(test)]
mod tests;
