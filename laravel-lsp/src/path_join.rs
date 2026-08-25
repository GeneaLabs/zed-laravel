//! Join a relative path fragment captured from PHP source onto a base
//! directory — the single source of truth for the "strip, then join" rule.
//!
//! PHP registrations concatenate a directory base with a string literal
//! (`__DIR__ . '/../resources/views'`, `resource_path('views/vendor/ns')`), and
//! the captured fragment keeps its leading separator. Rust's [`Path::join`]
//! treats a leading separator as rooted and **discards the receiver**, so
//! joining a captured fragment verbatim silently yields a root-relative path
//! that resolves to nothing — the namespace looks registered and never
//! resolves (issue #285).
//!
//! The rule previously lived in four copies across `vendor_translations` and
//! `salsa_impl`, two of which also stripped a leading `./` (issue #290).
//!
//! **On the `./` strip.** On Unix it is a no-op: `Path`'s component iterator
//! normalizes `.` away, so `/pkg/./views` and `/pkg/views` compare, normalize,
//! and resolve identically. It is *not* a no-op on Windows — `std::path`
//! preserves `.` as a real `CurDir` component inside a **verbatim** path
//! (`\\?\C:\...`), and Windows hands verbatim paths to the OS without
//! normalizing them, so a stray `.` simply fails to resolve. `canonicalize`
//! returns verbatim paths on Windows, so such a base is reachable here. The
//! strip therefore stays.
//!
//! Separator matching goes through [`std::path::is_separator`], which is `/`
//! on Unix and `/` or `\` on Windows — so a `\`-prefixed fragment, which is
//! rooted on Windows and an ordinary filename character on Unix, is handled
//! correctly on each without a `cfg`.

use std::path::{Path, PathBuf};

/// Join a relative fragment captured from PHP source onto `base`.
///
/// Strips leading separators and `./` segments so the fragment stays relative;
/// without that, `Path::join` discards `base` entirely. The strip **loops**
/// because one pass can expose a fresh leading separator — `".//views"`
/// becomes `"/views"` after a single `./` removal, which would discard `base`
/// again. On return the fragment begins with neither, so the join always
/// extends `base`.
///
/// A leading `..` is preserved: `__DIR__ . '/../x'` genuinely means the parent
/// directory, and callers collapse it with
/// [`crate::route_discovery::normalize_path`] or `canonicalize`.
pub fn join_relative(base: &Path, rel: &str) -> PathBuf {
    let mut rel = rel;
    loop {
        let stripped = rel.trim_start_matches(std::path::is_separator);
        // A `.` only counts as a current-directory segment when a separator
        // follows it — otherwise `..` would lose a dot.
        let stripped = match stripped.strip_prefix('.') {
            Some(rest) if rest.starts_with(std::path::is_separator) => rest,
            _ => stripped,
        };
        if stripped.len() == rel.len() {
            break;
        }
        rel = stripped;
    }
    base.join(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_slash_so_base_is_kept() {
        assert_eq!(
            join_relative(Path::new("/pkg/src/Providers"), "/../resources/views"),
            PathBuf::from("/pkg/src/Providers/../resources/views")
        );
    }

    #[test]
    fn preserves_parent_dir_segments() {
        assert_eq!(
            join_relative(Path::new("/pkg/src"), "../resources/views"),
            PathBuf::from("/pkg/src/../resources/views")
        );
    }

    #[test]
    fn plain_relative_fragment_is_unchanged() {
        assert_eq!(
            join_relative(Path::new("/pkg"), "resources/views"),
            PathBuf::from("/pkg/resources/views")
        );
    }

    /// The assertions that shipped with #285 as
    /// `load_views_relative_fragment_resolves_against_provider_dir`, retained
    /// verbatim against the promoted shared helper.
    #[test]
    fn load_views_relative_fragment_resolves_against_provider_dir() {
        let provider_dir = PathBuf::from("/pkg/src/Providers");
        assert_eq!(
            join_relative(&provider_dir, "/../../resources/views"),
            PathBuf::from("/pkg/src/Providers/../../resources/views")
        );
        assert_eq!(
            join_relative(&provider_dir, "/views"),
            PathBuf::from("/pkg/src/Providers/views")
        );
        assert_eq!(
            join_relative(&provider_dir, "./views"),
            PathBuf::from("/pkg/src/Providers/views")
        );
    }

    /// Pins the Windows-verbatim contract described in the module docs — and
    /// discriminates on every platform besides, since the `.` strip runs after
    /// the separator strip and so exposes a separator that only the loop
    /// re-trims.
    #[test]
    fn strips_leading_dot_slash() {
        assert_eq!(
            join_relative(Path::new("/pkg/src"), "./views"),
            PathBuf::from("/pkg/src/views")
        );
    }

    /// A single strip pass turns `.//views` into `/views`, which `Path::join`
    /// then treats as rooted and uses to discard `base` — a fail-open the
    /// `./` strip itself would otherwise manufacture, on every platform.
    #[test]
    fn repeated_strip_never_leaves_a_rooted_fragment() {
        for rel in [".//views", "//views", "././views", "/./views"] {
            let joined = join_relative(Path::new("/pkg/src"), rel);
            assert!(
                joined.starts_with("/pkg/src"),
                "fragment {rel:?} discarded the base, yielding {joined:?}"
            );
        }
    }

    /// `is_separator` is platform-defined: a `\\`-prefixed fragment is rooted on
    /// Windows and an ordinary filename on Unix. One assertion per platform.
    #[cfg(unix)]
    #[test]
    fn backslash_fragment_is_an_ordinary_filename_on_unix() {
        assert_eq!(
            join_relative(Path::new("/pkg"), "\\views"),
            PathBuf::from("/pkg/\\views")
        );
    }

    #[cfg(windows)]
    #[test]
    fn backslash_fragment_is_stripped_on_windows() {
        assert_eq!(
            join_relative(Path::new(r"C:\pkg"), r"\views"),
            PathBuf::from(r"C:\pkg\views")
        );
    }

    #[test]
    fn empty_fragment_yields_the_base() {
        assert_eq!(join_relative(Path::new("/pkg"), ""), PathBuf::from("/pkg"));
    }
}
