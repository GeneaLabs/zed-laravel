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
//! - [`path_within_root_lexical`] gates on a `normalize_path`-collapsed
//!   `starts_with` check that **never canonicalizes the candidate**, so an
//!   out-of-root candidate is refused without ever being probed on disk (issue
//!   #145). The check tests the candidate against the root as given and, only if
//!   that fails, against the root's canonicalized form — canonicalizing the
//!   *root* (a trusted in-root path) is not an oracle and tolerates the macOS
//!   `/var`→`/private/var` symlinked-root case. A candidate that *is* lexically
//!   in-root is then canonicalized to reject symlink escapes, falling back to the
//!   already-proven lexical result for *speculative* candidates that don't exist
//!   on disk yet (so they can't canonicalize) — fail-closing those would drop
//!   every not-yet-created candidate. Used by `salsa_impl.rs`'s
//!   component-candidate filter, which must admit such paths while still refusing
//!   interior-`..` escapes (`normalize_path` collapses `..`/`.` before the prefix
//!   test, because `Path::starts_with` is component-wise and does NOT resolve
//!   `..`).

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

/// True if `path` is contained within `root`, gated by a **lexical** check that
/// never `canonicalize()`s the candidate, so an out-of-root candidate is rejected
/// *before* any `stat`/`realpath` probe of it — closing the out-of-root existence
/// oracle that an upfront `canonicalize()` of the candidate would leak (issue
/// #145).
///
/// The candidate is `normalize_path`-collapsed (interior `..`/`.` resolved) and
/// tested with `starts_with` against the root — first as given, and only if that
/// fails, against the root's canonicalized form. Canonicalizing the *root* (a
/// trusted, in-root path) is not an oracle; it just tolerates the macOS
/// `/var`→`/private/var` symlinked-root case, where an in-root candidate carries
/// the resolved prefix that the raw `root` lacks. A candidate under neither form
/// is out-of-root and is refused without ever being probed.
///
/// A candidate that *is* lexically inside the root is then verified by
/// canonicalizing it — rejecting symlink escapes (a real target resolving outside
/// the root) and preserving the #55/#134 symlink behaviour. A speculative
/// candidate that doesn't exist on disk yet can't be canonicalized — already
/// proven lexically contained, it is admitted, which the candidate filter in
/// `salsa_impl.rs` relies on. Not a security guard for paths that will be read or
/// emitted: those must use [`path_within_root`], which is fail-closed.
pub fn path_within_root_lexical(path: &Path, root: &Path) -> bool {
    let normalized = normalize_path(path);

    // Lexical gate — never canonicalizes the candidate. A path under neither the
    // root as given nor its symlink-resolved form is out-of-root and refused with
    // no probe of it (the existence oracle issue #145 closes). Canonicalizing the
    // root only tolerates the macOS `/var`→`/private/var` symlinked-root case.
    let lexically_in_root = normalized.starts_with(root)
        || root
            .canonicalize()
            .map(|real_root| normalized.starts_with(&real_root))
            .unwrap_or(false);
    if !lexically_in_root {
        return false;
    }

    // Lexically in-root: canonicalize the candidate to reject symlink escapes; a
    // speculative not-yet-created candidate (canonicalize fails) is admitted,
    // already proven lexically contained above.
    canonical_containment(path, root).unwrap_or(true)
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
    fn lexical_reject_precedes_canonicalize_for_out_of_root_candidate() {
        // Prove the lexical-reject branch fires BEFORE any `canonicalize()`
        // syscall (issue #145 — close the out-of-root existence oracle). The
        // candidate is an out-of-root symlink that, *if* canonicalized, would
        // resolve back INSIDE the root. So the guard can only reject it by
        // running the lexical check first: a `false` result proves no disk probe
        // of the out-of-root path decided the outcome. This is distinct from
        // merely asserting `None` — under the old canonicalize-first order this
        // exact path would be admitted (`true`).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        let views = root.join("resources").join("views");
        std::fs::create_dir_all(&views).unwrap();
        let real = views.join("card.blade.php");
        std::fs::write(&real, "{{ $x }}").unwrap();

        // A symlink OUTSIDE the root pointing at the real in-root file.
        let outside_link = tmp.path().join("outside-link.blade.php");
        std::os::unix::fs::symlink(&real, &outside_link).unwrap();

        // Preconditions: the link is lexically out-of-root, but canonicalizes to
        // a path inside it — so a canonicalize-first guard would *admit* it.
        assert!(
            !outside_link.starts_with(&root),
            "the link path is lexically outside the root"
        );
        assert_eq!(
            outside_link.canonicalize().unwrap(),
            real.canonicalize().unwrap(),
            "the link resolves back inside the root"
        );

        // Lexical reject wins → the out-of-root candidate is refused without the
        // canonicalize() syscall ever deciding it.
        assert!(
            !path_within_root_lexical(&outside_link, &root),
            "an out-of-root candidate must be rejected lexically, before any disk probe"
        );
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
