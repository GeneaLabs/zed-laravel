//! Match whole path segments, separator-agnostically.
//!
//! The gap `Path` leaves open. `starts_with`, `ends_with` and `strip_prefix`
//! all compare components and are correct on every platform, but there is no
//! built-in for "does this path contain this run of segments *anywhere*". That
//! question kept being answered with `to_string_lossy().contains("/config/")`,
//! which matches only a forward-slash spelling and so never fired on Windows —
//! config-file detection, Inertia page detection, migration and Artisan
//! command change detection all silently did nothing there (issue #292).
//!
//! Not to be confused with an extension check. `path.to_string_lossy()
//! .ends_with(".blade.php")` is correct and must stay a string comparison: a
//! filename suffix contains no separators, `Path::extension` returns only the
//! last extension (`php`, never `blade.php`), and `Path::ends_with` compares
//! whole components — so it is `false` for `".blade.php"` and would silently
//! break every such check if "modernized".

use std::path::Path;

/// Does `path` contain `needle` as a consecutive run of whole segments?
///
/// `needle` is spelled with forward slashes on every platform —
/// `contains_segments(p, "database/migrations")` — because it describes
/// segments, not a literal substring. Matching goes through
/// [`Path::components`], so it holds wherever the path itself came from.
///
/// Matching whole segments also fixes a bug the substring form had on *every*
/// platform: `contains("/config/")` matched a directory called
/// `configuration`; this does not.
pub fn contains_segments(path: &Path, needle: &str) -> bool {
    let needle: Vec<&str> = needle.split('/').filter(|s| !s.is_empty()).collect();
    if needle.is_empty() {
        return false;
    }
    let components: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    components
        .windows(needle.len())
        .any(|window| window.iter().zip(&needle).all(|(have, want)| have == want))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_a_single_segment() {
        assert!(contains_segments(
            &PathBuf::from("/proj/config/app.php"),
            "config"
        ));
    }

    #[test]
    fn matches_a_consecutive_run_of_segments() {
        assert!(contains_segments(
            &PathBuf::from("/proj/database/migrations/2026_01_01_create.php"),
            "database/migrations"
        ));
    }

    /// The run must be consecutive — segments that appear apart do not match.
    #[test]
    fn rejects_a_non_consecutive_run() {
        assert!(!contains_segments(
            &PathBuf::from("/proj/database/seeders/migrations/x.php"),
            "database/migrations"
        ));
    }

    /// Whole segments only. The substring form this replaces matched
    /// `configuration` for `"/config/"` on every platform, not just Windows.
    #[test]
    fn rejects_a_partial_segment() {
        assert!(!contains_segments(
            &PathBuf::from("/proj/configuration/app.php"),
            "config"
        ));
        assert!(!contains_segments(
            &PathBuf::from("/proj/myconfig/app.php"),
            "config"
        ));
    }

    #[test]
    fn rejects_an_absent_segment() {
        assert!(!contains_segments(
            &PathBuf::from("/proj/app/Models/User.php"),
            "config"
        ));
    }

    #[test]
    fn an_empty_needle_never_matches() {
        assert!(!contains_segments(&PathBuf::from("/proj/config"), ""));
        assert!(!contains_segments(&PathBuf::from("/proj/config"), "/"));
    }

    /// The point of the helper: a Windows path carries `\`, and `components`
    /// splits on it there. Only runnable on Windows, because `Path` parsing is
    /// platform-gated — on Unix a backslash is an ordinary filename character,
    /// so this exact string is one component. The Windows CI leg runs it.
    #[cfg(windows)]
    #[test]
    fn matches_segments_in_a_windows_path() {
        assert!(contains_segments(
            &PathBuf::from(r"C:\Users\dev\proj\config\app.php"),
            "config"
        ));
        assert!(contains_segments(
            &PathBuf::from(r"C:\Users\dev\proj\database\migrations\x.php"),
            "database/migrations"
        ));
        assert!(!contains_segments(
            &PathBuf::from(r"C:\Users\dev\proj\configuration\app.php"),
            "config"
        ));
    }
}
