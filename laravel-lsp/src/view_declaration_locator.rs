//! Phase 3a — locate the `.blade.php` file backing a `view(...)` reference
//! and validate the user-supplied new name for a view rename.
//!
//! A view's "declaration" isn't a source position the way a route name or
//! config key is — it's the file itself. So this module returns paths, not
//! line/column ranges. Renaming a view is a [`crate::rename::FileRename`]
//! plus the text-edit rewrites at every `view('xxx')` call site (collected
//! independently via the Salsa references lookup).
//!
//! Validation is strict by default. The user can always cancel the rename
//! and retype, so refusing a malformed new name with a specific message
//! is friendlier than silently performing a half-broken rename.

use std::path::{Path, PathBuf};

use crate::naming::{self, DottedNameError};
use crate::salsa_impl::LaravelConfigData;

/// Reasons a new view name was rejected. Each variant carries the data
/// needed to produce a user-facing error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewNameError {
    Empty,
    ContainsSlash,
    EmptySegment,
    HasExtension,
    InvalidCharacter(char),
}

impl ViewNameError {
    /// User-facing message body. Combined with the LSP error framing
    /// ("Error: Rename via laravel-lsp failed: ...") this is what the user
    /// sees as a toast in Zed.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "view name cannot be empty".to_string(),
            Self::ContainsSlash => "use dots instead of slashes (e.g., users.profile)".to_string(),
            Self::EmptySegment => "view name cannot have empty segments (no leading, trailing, \
                 or double dots)"
                .to_string(),
            Self::HasExtension => "omit the file extension (write 'users.profile', not \
                 'users.profile.blade.php')"
                .to_string(),
            Self::InvalidCharacter(c) => {
                format!(
                    "invalid character '{}' — view names may contain only \
                     letters, digits, hyphens, and underscores",
                    c
                )
            }
        }
    }
}

impl From<DottedNameError> for ViewNameError {
    fn from(error: DottedNameError) -> Self {
        match error {
            DottedNameError::Empty => Self::Empty,
            DottedNameError::ContainsSlash => Self::ContainsSlash,
            DottedNameError::EmptySegment => Self::EmptySegment,
            DottedNameError::HasExtension => Self::HasExtension,
            DottedNameError::InvalidCharacter(c) => Self::InvalidCharacter(c),
            // View names don't support namespaced renames, so `validate_view_name`
            // never opts into the `::` check (it passes `reject_namespaced = false`)
            // and this arm is unreachable in practice. Map it defensively to the
            // invalid-character reason a bare `:` would otherwise produce.
            DottedNameError::Namespaced => Self::InvalidCharacter(':'),
        }
    }
}

/// Validate a user-typed new view name. Returns `Ok(())` only when the name
/// is a well-formed dotted segment list — what Laravel's `view()` helper
/// actually resolves at runtime.
///
/// **Limitation:** namespaced views (`package::view`) are not supported — the
/// `:` namespace separator trips the [`ViewNameError::InvalidCharacter`] check.
/// This is intentional for now: rename only handles application views, and the
/// cross-file binding rename (issue #55) skips a namespaced source `view(...)`
/// rather than mis-resolving it. Supporting `package::view` would mean parsing
/// the namespace segment against the registered view hints — a separate change.
pub fn validate_view_name(name: &str) -> Result<(), ViewNameError> {
    // View renames don't support namespaced (`package::view`) targets, so the
    // shared validator runs without the `::` check — a `:` reads as an invalid
    // character, matching this locator's historical behaviour.
    naming::validate_dotted_name(name, false).map_err(ViewNameError::from)
}

/// Find the on-disk `.blade.php` file backing a dotted view name. Walks
/// every candidate path the config produces and returns the first one that
/// exists. Returns `None` if the view name resolves to no extant file —
/// rare in practice (the LSP only routes here when the call site was
/// already classified), but possible if the file was deleted out from
/// under the editor.
pub fn locate_view_file(name: &str, config: &LaravelConfigData) -> Option<PathBuf> {
    config
        .resolve_view_path(name)
        .into_iter()
        .find(|p| p.is_file())
}

/// Compute the target file path for a view rename, preserving the
/// `view_path` base directory the original file lives under.
///
/// If `view_paths` has multiple entries (project + package paths), we find
/// the entry containing `current_path` and emit the new path under the
/// SAME entry. Renames never move a view across `view_path` roots — that
/// would change which package owns the file, which the rename doesn't
/// intend.
///
/// Returns `None` when `current_path` doesn't match any candidate path the
/// config would produce for `old_name`. That happens when the file lives
/// at a non-standard location (e.g., a published vendor view) — refuse
/// rather than guess.
pub fn compute_target_path(
    old_name: &str,
    new_name: &str,
    current_path: &Path,
    config: &LaravelConfigData,
) -> Option<PathBuf> {
    let current_candidates = config.resolve_view_path(old_name);
    let new_candidates = config.resolve_view_path(new_name);
    let idx = current_candidates.iter().position(|p| p == current_path)?;
    new_candidates.get(idx).cloned()
}

/// True if `path` lives anywhere under `{root}/vendor`. Used to refuse
/// renames that would attempt to move a Composer-installed file.
pub fn is_under_vendor(path: &Path, root: &Path) -> bool {
    path.starts_with(root.join("vendor"))
}

/// Reverse of [`locate_view_file`]: given a `.blade.php` path that lives
/// under a configured `view_paths` entry, derive the dotted view name
/// Laravel would resolve to it.
///
/// Returns `None` when:
///   - `path` doesn't end with `.blade.php`
///   - `path` lives under `view_paths/components/` (that's a Blade
///     component, not a view — different rename pipeline)
///   - `path` doesn't sit under any configured `view_paths` entry
///   - the resulting name fails validation (slashes, empty segments, etc.)
///
/// Used by the file-rename handler to compute the symbol name from the
/// path Zed is about to rename.
pub fn view_name_for_path(path: &Path, config: &LaravelConfigData) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if !file_name.ends_with(".blade.php") {
        return None;
    }

    for base in &config.view_paths {
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel_str = rel.to_str()?;
        // Anything under `components/` belongs to the Blade-component
        // rewriter, not the view rewriter — refuse the match so the
        // caller routes correctly.
        if rel_str.starts_with("components/") || rel_str.starts_with("components\\") {
            return None;
        }
        let stripped = rel_str.strip_suffix(".blade.php")?;
        // Convert path separators to dots. Use both `/` and `\` because
        // PathBuf on Windows uses backslashes.
        let dotted = stripped.replace(['/', '\\'], ".");
        if validate_view_name(&dotted).is_ok() {
            return Some(dotted);
        }
    }
    None
}

#[cfg(test)]
mod tests;
