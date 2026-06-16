//! Project-root containment guard — the single source of truth for the
//! "does this path resolve inside the project root?" invariant.
//!
//! The check used to live in three independent copies (`main.rs`,
//! `slot_navigation.rs`, and an inline `retain` in `salsa_impl.rs`), each with
//! its own fallback behaviour that could drift apart (issue #156). They are
//! consolidated here behind one canonical-first core ([`canonical_containment`])
//! and **two** public entry points that differ only in what they do when a path
//! can't be canonicalized:
//!
//! - [`path_within_root`] is **fail-closed**: an unverifiable path is refused.
//!   It is the security guard — a `WorkspaceEdit` must never leak a path whose
//!   real target escapes the project root, including a *dangling* under-root
//!   symlink whose link path is textually inside the root but whose target is
//!   missing (issues #55, #130, #134, #155). Used by `main.rs` and
//!   `slot_navigation.rs`.
//! - [`path_within_root_lexical`] falls back to a `normalize_path`-based lexical
//!   prefix check. It is for *speculative* candidate paths that don't exist on
//!   disk yet (so they can't canonicalize) — fail-closing them would drop every
//!   not-yet-created candidate. Used by `salsa_impl.rs`'s component-candidate
//!   filter, which must admit such paths while still refusing interior-`..`
//!   escapes (`normalize_path` collapses `..`/`.` before the prefix test,
//!   because `Path::starts_with` is component-wise and does NOT resolve `..`).

use std::path::Path;

use crate::route_discovery::normalize_path;

/// Canonical-first containment: when both `path` and `root` canonicalize,
/// compare their real (symlink-resolved) forms with `starts_with` and return
/// `Some(result)`. Returns `None` when either side can't be canonicalized,
/// leaving the fallback policy (refuse vs. lexical check) to the caller.
///
/// Canonicalizing both sides is what makes the guard real: `locate_view_file`
/// builds candidate paths by joining and `Path::starts_with` is purely textual,
/// so without canonicalization a symlink under the project could resolve outside
/// the root and slip past the prefix test.
fn canonical_containment(path: &Path, root: &Path) -> Option<bool> {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(real_path), Ok(real_root)) => Some(real_path.starts_with(&real_root)),
        _ => None,
    }
}

/// True if `path` resolves to a location inside `root`. **Fail-closed**: when
/// either side can't be canonicalized (a missing file, or a dangling under-root
/// symlink whose target no longer exists) this returns `false` rather than
/// falling back to a textual prefix check that would admit it (issue #134).
/// Every caller uses this as a security guard, so an unprovable path is refused,
/// not admitted.
pub fn path_within_root(path: &Path, root: &Path) -> bool {
    canonical_containment(path, root).unwrap_or(false)
}

/// True if `path` is contained within `root`, with a **lexical** fallback for
/// paths that can't be canonicalized. When canonicalization fails, the path is
/// `normalize_path`-collapsed (interior `..`/`.` resolved) and tested against
/// `root` with `starts_with`. This admits speculative candidates that don't yet
/// exist on disk — the candidate filter in `salsa_impl.rs` relies on that — while
/// still refusing a candidate whose normalized form escapes the root. Not a
/// security guard: callers that read or emit the path must use
/// [`path_within_root`] instead.
pub fn path_within_root_lexical(path: &Path, root: &Path) -> bool {
    canonical_containment(path, root).unwrap_or_else(|| normalize_path(path).starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn in_root_path_is_contained() {
        // A real file under the root: both sides canonicalize and the real path
        // starts with the real root.
        let root = TempDir::new().unwrap();
        let child = root.path().join("resources").join("views");
        std::fs::create_dir_all(&child).unwrap();
        let file = child.join("home.blade.php");
        std::fs::write(&file, "{{ $x }}").unwrap();

        assert!(path_within_root(&file, root.path()));
        assert!(path_within_root_lexical(&file, root.path()));
    }

    #[test]
    fn sibling_root_path_is_refused() {
        // A real file under a *sibling* root must not be reported as contained.
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        let sibling = parent.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let escapee = sibling.join("secret.blade.php");
        std::fs::write(&escapee, "{{ $x }}").unwrap();

        assert!(!path_within_root(&escapee, &root));
        assert!(!path_within_root_lexical(&escapee, &root));
    }

    #[test]
    fn lexical_refuses_interior_dotdot_escape() {
        // A speculative candidate that doesn't exist on disk (so it can't
        // canonicalize) whose interior `..` escapes the root must be refused.
        // `normalize_path` collapses `root/sub/../../escape` to `<parent>/escape`
        // before the prefix test — a raw `starts_with` would be fooled by the
        // textual `root/` prefix.
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let escaping = root
            .join("sub")
            .join("..")
            .join("..")
            .join("escape.blade.php");

        assert!(
            escaping.canonicalize().is_err(),
            "candidate must not exist on disk"
        );
        assert!(
            !path_within_root_lexical(&escaping, &root),
            "an interior-`..` escape must be refused after normalization"
        );
    }

    #[test]
    fn lexical_admits_speculative_in_root_candidate() {
        // A not-yet-created candidate that stays lexically inside the root must
        // be admitted — this is the speculative-candidate case the security
        // guard would wrongly fail-close.
        let root = TempDir::new().unwrap();
        let candidate = root
            .path()
            .join("resources")
            .join("views")
            .join("ghost.blade.php");

        assert!(
            candidate.canonicalize().is_err(),
            "candidate must not exist on disk"
        );
        assert!(path_within_root_lexical(&candidate, root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn guard_is_fail_closed_on_dangling_under_root_symlink() {
        // A symlink at `<root>/dangling` whose target was never created:
        // `canonicalize` fails, so the fail-closed guard must refuse it even
        // though its link path is lexically inside the root (issues #134, #155).
        let root = TempDir::new().unwrap();
        let missing_target = root.path().join("never-created.blade.php");
        let dangling = root.path().join("dangling");
        std::os::unix::fs::symlink(&missing_target, &dangling).unwrap();

        // Precondition: the link exists but can't be canonicalized, and it is
        // lexically inside the root — exactly the path a textual fallback admits.
        assert!(
            dangling.canonicalize().is_err(),
            "a dangling symlink must fail to canonicalize"
        );
        assert!(
            dangling.starts_with(root.path()),
            "the link path is lexically inside the root"
        );

        assert!(
            !path_within_root(&dangling, root.path()),
            "a dangling under-root symlink must be refused — the guard is fail-closed"
        );
    }
}
