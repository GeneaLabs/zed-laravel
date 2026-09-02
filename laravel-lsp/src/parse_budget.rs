//! The per-file limits that keep tree-sitter away from files it cannot parse
//! affordably, in one place.
//!
//! Two exclusions, both learned the hard way during warm-up tuning:
//!
//! * **`*.json.php`** — the Laravel/PHP convention for pre-baked JSON data
//!   wrapped as PHP. Pure data, never user-facing Laravel patterns, and
//!   tree-sitter-php chokes on the deeply-nested array literals: 0.4–2.2 s
//!   *per file*. This repo's `test-project/` has 1,735 of them under
//!   `aws-sdk-php`.
//! * **A 256 KB size cap** — dropping it from 4 MB cut warming from 70 s to
//!   under 10 s on a real Laravel project with vendor, Flux icons and IDE
//!   helpers checked in. The biggest real application PHP file in the test
//!   project is 55 KB; IDE helpers top out at 186 KB. Everything above the cap
//!   is auto-generated metadata.
//!
//! The rules lived as three independent copies of `const MAX_FILE_SIZE_BYTES:
//! u64 = 256 * 1024` plus one hand-written `.json.php` test, and the watched-
//! file magic rebuild had neither (issue #371). Enumerations maintained by hand
//! in several places drift; this module exists so there is one to change.

use std::path::Path;

/// Largest file worth handing to tree-sitter.
pub const MAX_PARSED_FILE_SIZE_BYTES: u64 = 256 * 1024;

/// True when `path` names a pre-baked JSON data file wrapped as PHP.
///
/// A filename-suffix test, so it needs no path normalization — a file
/// extension contains no separators, unlike the directory tests elsewhere in
/// this crate that had to learn about Windows `\` the hard way (issue #292).
pub fn is_json_php(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".json.php"))
}

/// Why a file is excluded from parsing, if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Pre-baked JSON data wrapped as PHP.
    JsonPhp,
    /// Larger than [`MAX_PARSED_FILE_SIZE_BYTES`]; carries the actual size.
    TooLarge(u64),
}

impl SkipReason {
    /// A short phrase for logs.
    pub fn describe(&self) -> String {
        match self {
            SkipReason::JsonPhp => "pre-baked JSON data file".to_string(),
            SkipReason::TooLarge(size) => {
                format!("{size} bytes > {MAX_PARSED_FILE_SIZE_BYTES} cap")
            }
        }
    }
}

/// Whether `path` of `size` bytes should be excluded from parsing.
///
/// The name test comes first: it is free, and a `.json.php` file under the size
/// cap must still be excluded.
pub fn skip_reason(path: &Path, size: u64) -> Option<SkipReason> {
    if is_json_php(path) {
        return Some(SkipReason::JsonPhp);
    }
    (size > MAX_PARSED_FILE_SIZE_BYTES).then_some(SkipReason::TooLarge(size))
}

/// [`skip_reason`] for a file on disk, sizing it with one `metadata` call.
///
/// A path that cannot be stat'd yields `None` — "nothing to exclude" — leaving
/// the caller's own existence handling to decide what a missing file means.
/// Deciding that here would let a transient stat failure masquerade as an
/// exclusion and silently withdraw a live file from an index.
pub fn skip_reason_on_disk(path: &Path) -> Option<SkipReason> {
    if is_json_php(path) {
        return Some(SkipReason::JsonPhp);
    }
    let size = std::fs::metadata(path).ok()?.len();
    (size > MAX_PARSED_FILE_SIZE_BYTES).then_some(SkipReason::TooLarge(size))
}

#[cfg(test)]
mod tests;
