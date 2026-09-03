//! Eager, project-wide index of Artisan commands — the resolution half of
//! issue #62. Where [`crate::command_signature`] reads a single `Command` class
//! and [`crate::command_call_locator`] finds the *references* to a command in
//! ordinary PHP, this module ties them together: it scans the whole project
//! (including `vendor/`) for `Command` subclasses, extracts each one's
//! `$signature`, and keys them by command name so goto-definition and hover can
//! resolve a call-site string (`Artisan::call('emails:send')`) to the class
//! that declares it.
//!
//! Built once at init and refreshed when a relevant `Command` file changes
//! (mirrors [`crate::migration_index`]). The walk is regex-gated — a file is
//! only fully parsed when it actually `extends ...Command`, so the cost of
//! scanning a large `vendor/` tree stays bounded to real command classes. The
//! built index is persisted by [`crate::command_disk_cache`] so a cold restart
//! can restore it instantly instead of re-walking the whole tree.
//!
//! ## Priority (AC: app overrides framework/package)
//!
//! Two classes can declare the same command name (a package ships
//! `queue:work`, an app overrides it). The index keeps the highest-priority
//! declaration. Higher wins, as everywhere else in the codebase, but this
//! index has its own three-tier scale derived from the file path — it is NOT
//! the four-tier service-provider scale (`0=framework, 1=package, 2=module,
//! 3=app`), which no command declaration carries:
//!
//! | Source | Priority | Detected by |
//! |--------|----------|-------------|
//! | App    | 2 | not under `vendor/` |
//! | Package| 1 | under `vendor/` (non-framework) |
//! | Framework | 0 | under `vendor/laravel/framework/` |
//!
//! On a tie (same name, same priority) the first one walked wins — stable and
//! good enough; ambiguity across two packages is vanishingly rare.
//!
//! ## Position convention
//!
//! 0-based throughout; `start_column`/`end_column` bracket the signature
//! string's *content* (inside the quotes), matching the rest of the stack — see
//! `CLAUDE.md`. Goto lands on the `$signature` declaration, which is where the
//! command is actually defined.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::command_disk_cache::{CommandScanCache, ScannedFile};
use crate::command_signature::{extends_console_command, extract_command_signature};
use crate::vendor_index::{has_pruned_ancestor, VendorFile, VendorIndex};

/// Source tier of a command declaration. Higher wins when two classes declare
/// the same command name (`App` overrides a `Package` which overrides the
/// `Framework`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CommandPriority {
    /// Shipped by `laravel/framework` itself (`vendor/laravel/framework/`).
    Framework = 0,
    /// Shipped by any other `vendor/` package.
    Package = 1,
    /// Defined in the project's own source (not under `vendor/`).
    App = 2,
}

/// One Artisan command discovered on a `Command` subclass, resolved to its
/// declaration site. `class_name` powers the hover summary; the position fields
/// point at the `$signature` string content so goto lands on the declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntry {
    /// The resolvable command name (`emails:send`).
    pub name: String,
    /// The declaring class's short name (`SendEmails`), for hover summaries.
    pub class_name: String,
    /// The full signature string content as written (arguments/options kept).
    pub raw_signature: String,
    /// File that declares the command.
    pub file: PathBuf,
    /// 0-based row of the `$signature` string content.
    pub line: u32,
    /// 0-based column of the first content character (after the opening quote).
    pub start_column: u32,
    /// 0-based column one past the last content character (before the quote).
    pub end_column: u32,
    /// Source tier — see [`CommandPriority`].
    pub priority: CommandPriority,
}

/// Resolved Artisan commands across the project and `vendor/`, keyed by command
/// name with app-over-package-over-framework priority already applied.
#[derive(Debug, Clone, Default)]
pub struct CommandIndex {
    commands: HashMap<String, CommandEntry>,
}

impl CommandIndex {
    /// The declaring class for `name`, if any command provides it.
    pub fn resolve(&self, name: &str) -> Option<&CommandEntry> {
        self.commands.get(name)
    }

    /// Number of distinct command names indexed.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Every resolved command declaration, in arbitrary order. Used by the
    /// on-disk cache to persist the index.
    pub fn entries(&self) -> impl Iterator<Item = &CommandEntry> {
        self.commands.values()
    }

    /// Insert one resolved command declaration, applying the priority merge:
    /// the new entry replaces an existing one for the same command name only
    /// when it ranks strictly higher (App > Package > Framework). A same-tier
    /// duplicate leaves the first-inserted winner in place. This is the single
    /// place the override rule lives — both the project walk and the disk-cache
    /// restore funnel through here.
    pub fn insert_entry(&mut self, entry: CommandEntry) {
        match self.commands.get(&entry.name) {
            Some(existing) if existing.priority >= entry.priority => {}
            _ => {
                self.commands.insert(entry.name.clone(), entry);
            }
        }
    }
}

/// Classify a command file's source tier from its path.
pub fn classify_priority(path: &Path) -> CommandPriority {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.contains("/vendor/laravel/framework/") || s.contains("vendor/laravel/framework/") {
        CommandPriority::Framework
    } else if s.contains("/vendor/") || s.starts_with("vendor/") {
        CommandPriority::Package
    } else {
        CommandPriority::App
    }
}

/// The declaring class's short name, e.g. `SendEmails` from
/// `class SendEmails extends Command`. Returns `None` when no class declaration
/// is found (the caller then skips the file — no class, no goto target).
pub fn class_name_from_content(content: &str) -> Option<String> {
    lazy_static! {
        static ref CLASS_RE: Regex = Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    CLASS_RE
        .captures(content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Directory names never worth descending into when hunting for command
/// classes — build output and VCS metadata, none of which hold PHP source.
pub const SKIP_DIRS: &[&str] = &["node_modules", ".git", "storage", "public"];

/// True when the command index wants this vendor file's text.
///
/// The former walk pruned [`SKIP_DIRS`] with `WalkDir::filter_entry`; the
/// shared vendor walk prunes nothing (other consumers descend into those
/// directories), so the same exclusion is re-applied here per file.
pub fn vendor_command_needs_source(vendor_root: &Path, file: &VendorFile) -> bool {
    !has_pruned_ancestor(&file.path, vendor_root, SKIP_DIRS)
}

/// Build the index by walking every `*.php` under `<root>` (project + vendor),
/// keeping the highest-priority declaration per command name. Non-PHP files,
/// build/VCS directories, and files that don't `extends ...Command` are skipped
/// cheaply so a large `vendor/` tree doesn't dominate the walk.
/// The uncached whole-project scan.
///
/// Production drives [`scan_commands`] directly so it can pass the disk cache
/// and keep the resulting verdicts; this wrapper is the plain
/// "index everything, trust nothing" form the tests compare against.
pub fn build_command_index(root: &Path) -> CommandIndex {
    build_command_index_with_vendor(root, &VendorIndex::build(root))
}

/// [`build_command_index`] driven by an already-built shared vendor walk
/// (issue #371), so warm start reads each vendor file once for this *and* the
/// route index instead of twice.
///
/// Output is identical to the single walk this replaces. The former traversal
/// visited project and vendor files interleaved in `root` order, and
/// `insert_entry` keeps the first file walked on a *same-tier* duplicate — but
/// [`classify_priority`] derives the tier from the path, and `App` means
/// exactly "not under `vendor/`". So a same-tier collision can never straddle
/// the project/vendor split, and preserving order *within* each leg (the
/// project walk below, and [`VendorIndex`]'s retained walk order) preserves
/// every winner.
pub fn build_command_index_with_vendor(root: &Path, vendor: &VendorIndex) -> CommandIndex {
    scan_commands(root, vendor, None).index
}

/// A completed command scan: the resolved index, plus the per-file verdict for
/// every `*.php` file looked at, in walk order.
///
/// The verdicts are what [`crate::command_disk_cache`] persists so the next
/// scan can skip unchanged files. Walk order is preserved because
/// [`CommandIndex::insert_entry`] keeps the first file walked on a same-tier
/// duplicate.
#[derive(Default)]
pub struct CommandScan {
    pub index: CommandIndex,
    pub files: Vec<ScannedFile>,
}

/// Scan the project and `vendor/` for Artisan commands, reusing `cache` for
/// any file whose mtime is unchanged (issue #371).
///
/// Without a cache this reads and regex-scans every `*.php` file in the
/// project — 16,202 of them on this repo's `test-project/` — on every startup
/// AND on every watched change under a `Commands/` directory. The disk cache
/// existed but only accelerated the cold-start *display*: the full walk ran
/// afterwards unconditionally, because a cache of declarations alone cannot
/// say "these 16,000 other files still declare nothing".
///
/// With a cache, an unchanged file costs one `metadata` call (which the walk
/// already needed) and a hash lookup. Only new or modified files are read.
///
/// A file the cache has no fresh verdict for is read, exactly as before — so a
/// missing, stale or schema-mismatched cache degrades to the old behaviour
/// rather than to a wrong index.
pub fn scan_commands(
    root: &Path,
    vendor: &VendorIndex,
    cache: Option<&CommandScanCache>,
) -> CommandScan {
    let mut scan = CommandScan::default();
    consider_project_commands(root, cache, &mut scan);

    let vendor_root = vendor.vendor_root().to_path_buf();
    for file in vendor.files() {
        if vendor_command_needs_source(&vendor_root, file) {
            consider_file(&mut scan, cache, &file.path);
        }
    }

    scan
}

/// Run the non-vendor leg of the scan into `scan`, honouring `cache`.
pub fn consider_project_commands(
    root: &Path,
    cache: Option<&CommandScanCache>,
    scan: &mut CommandScan,
) {
    for path in project_command_paths(root) {
        consider_file(scan, cache, &path);
    }
}

/// Index one file, reusing the cached verdict when its mtime is unchanged.
fn consider_file(scan: &mut CommandScan, cache: Option<&CommandScanCache>, path: &Path) {
    let CacheVerdict::NeedsSource { mtime } = try_cached(scan, cache, path) else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    record_source(scan, path, mtime, &content);
}

/// A file's mtime as `(secs, nanos)`, matching the disk cache's representation.
fn file_mtime(path: &Path) -> Option<(u64, u32)> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = modified
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    Some((d.as_secs(), d.subsec_nanos()))
}

/// Every `*.php` path in the project leg, in walk order — the non-vendor half
/// of the scan. Skips the build/VCS directories and the top-level `vendor/`
/// that the shared index already covers.
///
/// Only `<root>/vendor` is skipped, not every directory named `vendor`. A
/// nested `app/vendor/` was walked by the former single pass and is not part of
/// the shared index, so pruning it by name would silently drop its commands.
fn project_command_paths(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            if !e.file_type().is_dir() {
                return true;
            }
            let Some(name) = e.file_name().to_str() else {
                return true;
            };
            if e.depth() == 1 && name == "vendor" {
                return false;
            }
            !SKIP_DIRS.contains(&name)
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Index a single PHP file's command declaration, if it has one. Exposed for
/// unit tests; [`build_command_index`] is the production entry.
///
/// The new entry replaces an existing one for the same command name only when
/// it ranks strictly higher (App > Package > Framework) — so an app command
/// wins over a package/framework command of the same name, and a same-tier
/// duplicate leaves the first-walked winner in place.
pub fn index_command_file(index: &mut CommandIndex, path: &Path, content: &str) {
    if let Some(entry) = command_entry_for(path, content) {
        index.insert_entry(entry);
    }
}

/// The command declaration `content` holds, if any. The verdict the scan cache
/// persists — `None` is a real answer, not an absence of one.
pub fn command_entry_for(path: &Path, content: &str) -> Option<CommandEntry> {
    // Cheap gate: skip anything that isn't a Command subclass before the
    // heavier signature/class-name extraction runs.
    if !extends_console_command(content) {
        return None;
    }
    let sig = extract_command_signature(content)?;
    let class_name = class_name_from_content(content)?;

    Some(CommandEntry {
        name: sig.name,
        class_name,
        raw_signature: sig.raw_signature,
        file: path.to_path_buf(),
        line: sig.line,
        start_column: sig.start_column,
        end_column: sig.end_column,
        priority: classify_priority(path),
    })
}

/// What [`try_cached`] decided about a file, and the mtime it already read.
///
/// The mtime is carried out rather than discarded because the caller's next
/// step, [`record_source`], needs exactly that value. Returning a bare `bool`
/// meant `record_source` stat'd the same file a second time — 225 ms of the
/// 962 ms shared vendor pass on a Windows runner, and about a fifth of it on
/// Linux and macOS too (issue #373).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheVerdict {
    /// Nothing more to do: either the scan cache answered for this file, or it
    /// could not be stat'd and so is deliberately left unread.
    Settled,
    /// The file still has to be read. `mtime` is the value already on hand, to
    /// be handed straight to [`record_source`].
    NeedsSource { mtime: (u64, u32) },
}

/// What the scan cache and one `metadata` call say about a file, as **owned**
/// data that no longer borrows the cache.
///
/// The pure half of [`try_cached`], split out so the shared vendor pass in
/// [`crate::vendor_scan`] can run the stat and the lookup for every file across
/// a worker pool and fold the answers afterwards (issue #373). Owned rather
/// than borrowed because a verdict outlives the parallel map that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepassVerdict {
    /// The file could not be stat'd, so it is deliberately left unread and
    /// **nothing at all is recorded** for it. A verdict stored without a usable
    /// mtime could never be validated against a later scan, so it would be
    /// trusted forever.
    Unstattable,
    /// The cache answered. Fold with [`record_verdict`]; no read is needed.
    Settled {
        mtime: (u64, u32),
        entry: Option<CommandEntry>,
    },
    /// The file still has to be read. `mtime` is the value already on hand, to
    /// be handed straight to [`record_verdict`] once the content is classified.
    NeedsSource { mtime: (u64, u32) },
}

/// Stat `path` and ask `cache` about it, without touching a scan.
///
/// Does no I/O beyond the single `metadata` call, and takes no `&mut`, so it is
/// safe to call concurrently for many files.
pub fn cached_verdict(cache: Option<&CommandScanCache>, path: &Path) -> PrepassVerdict {
    let Some(mtime) = file_mtime(path) else {
        return PrepassVerdict::Unstattable;
    };
    match cache.and_then(|c| c.verdict(path, mtime)) {
        Some(entry) => PrepassVerdict::Settled {
            mtime,
            entry: entry.cloned(),
        },
        None => PrepassVerdict::NeedsSource { mtime },
    }
}

/// Try to satisfy `path` from the scan cache, recording its verdict when the
/// mtime is unchanged.
///
/// Public so the combined warm-start pass in [`crate::vendor_scan`] can decide
/// whether a vendor file still has to be read for the command index before it
/// reads it for the route index.
pub fn try_cached(
    scan: &mut CommandScan,
    cache: Option<&CommandScanCache>,
    path: &Path,
) -> CacheVerdict {
    match cached_verdict(cache, path) {
        PrepassVerdict::Unstattable => CacheVerdict::Settled,
        PrepassVerdict::NeedsSource { mtime } => CacheVerdict::NeedsSource { mtime },
        PrepassVerdict::Settled { mtime, entry } => {
            record_verdict(scan, path, mtime, entry);
            CacheVerdict::Settled
        }
    }
}

/// Fold one already-classified verdict into `scan`.
///
/// **The single place the scan's order-dependence lives.**
/// [`CommandIndex::insert_entry`] keeps the first file inserted on a same-tier
/// duplicate, and `scan.files` is persisted in insertion order, so every
/// producer — the cache hit, the fresh read, and the parallel vendor pass —
/// must funnel through here *in walk order*.
pub fn record_verdict(
    scan: &mut CommandScan,
    path: &Path,
    mtime: (u64, u32),
    entry: Option<CommandEntry>,
) {
    if let Some(entry) = entry.clone() {
        scan.index.insert_entry(entry);
    }
    scan.files.push(ScannedFile {
        mtime_secs: mtime.0,
        mtime_nanos: mtime.1,
        path: path.to_path_buf(),
        entry,
    });
}

/// Record a freshly-read file's verdict into `scan`, stamped with the `mtime`
/// [`try_cached`] already read for it.
///
/// **Taking the mtime rather than reading one is also the more careful
/// choice**, not only the cheaper one. The stored mtime is what the next scan
/// validates the verdict against, and `content` was read *after* this mtime
/// was taken. Stamping the entry with a *later* mtime — which is what a fresh
/// stat here would produce — would mean a file rewritten between the read and
/// the stat gets its stale verdict stored under the new mtime, and the next
/// scan trusts it. Stamping the earlier mtime can only cause a needless
/// re-read, never a stale hit.
pub fn record_source(scan: &mut CommandScan, path: &Path, mtime: (u64, u32), content: &str) {
    record_verdict(scan, path, mtime, command_entry_for(path, content));
}

#[cfg(test)]
mod tests;
