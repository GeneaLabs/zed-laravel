//! Project-root containment guard — the single source of truth for the
//! "does this path resolve inside the project root?" invariant.
//!
//! The check used to live in three independent copies (`main.rs`,
//! `slot_navigation.rs`, and an inline `retain` in `salsa_impl.rs`), each with
//! its own fallback behaviour that could drift apart (issue #156). They are
//! consolidated here behind one canonical-first core ([`canonical_containment`])
//! and **five** public entry points that differ only in what they do when a
//! path can't be canonicalized:
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
//! - [`path_within_root_emit_safe`] is the middle ground for a *speculative
//!   candidate that will be emitted* — e.g. the "Expected at:" create-this-file
//!   hint baked into a "view not found" diagnostic, which a client can turn back
//!   into a `CreateFile` (issue #201). Like the lexical guard it admits a
//!   genuinely-absent in-root path — the diagnostic fires *because* the target
//!   doesn't exist, so the fail-closed guard would drop every normal
//!   not-yet-created in-root hint. Unlike the lexical guard it does **not** admit
//!   a *dangling* under-root symlink whose target is missing: `canonicalize`
//!   can't prove where that link resolves, and following it on create could
//!   write outside the tree (issues #134/#155), so it is refused while a path
//!   with nothing at all on disk is admitted. Used by `main.rs`'s
//!   `in_root_expected_path_hint`.
//! - [`path_within_root_walk_entry`] is the fail-closed guard for a path
//!   **discovered during a `follow_links(true)` directory walk** (issue #228).
//!   A `WalkDir` rooted at a containment-trusted in-root directory still follows
//!   a symlink encountered *inside* it whose target escapes the root, surfacing
//!   out-of-root files as entries; gating each discovered entry drops those
//!   before they become a read primitive or an emitted candidate. It mirrors
//!   [`path_within_root`] exactly — a discovered entry the walk just yielded
//!   normally exists and canonicalizes, an under-root symlink resolving inside
//!   the root passes, and anything that escapes (or can't be canonicalized) is
//!   refused. Used by `scan_dir` in `component_completion.rs` and the
//!   `controllers_dir` walk in `main.rs`.
//! - [`path_within_root_registration`] is the fail-closed guard for a path
//!   **minted from discovered provider source that will be READ downstream**
//!   (issue #354 item 1). It is the missing combination of the two axes above:
//!   it refuses an out-of-root candidate lexically, with no disk probe (#145),
//!   AND fail-closes on anything it cannot canonicalize (#134/#155) — a
//!   dangling under-root symlink, an unsearchable parent, a path with nothing
//!   on disk. [`path_within_root_lexical`] admits all three and
//!   [`path_within_root_emit_safe`] admits the last, which is correct for a
//!   speculative *create target* and wrong for a value that becomes a read
//!   primitive. Gate against the provider's OWNING MODULE rather than the
//!   project root and a symlinked composer path repository still passes: both
//!   sides canonicalize to the real target. Used by `contained_class_path` in
//!   `livewire_namespaces.rs`.

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

/// True if a `path` **discovered during a `follow_links(true)` directory walk**
/// resolves inside `root`. The containment gate for entries a `WalkDir` yields
/// while following symlinks: the walk root may itself be in-root and
/// containment-trusted, yet `follow_links(true)` still descends a symlink
/// encountered *inside* it whose target escapes the project root and surfaces the
/// files under that target as entries. Gating each discovered entry against the
/// root drops those escaping paths before they become a read primitive or an
/// emitted candidate — the discovered-path leg the resolver guard in #226
/// deferred (issue #228).
///
/// **Fail-closed**, delegating to [`path_within_root`]: the entry's real,
/// symlink-resolved path is compared against the real root, and an entry that
/// can't be canonicalized is refused, not admitted. A discovered entry the walk
/// just yielded normally exists on disk and canonicalizes; the fail-closed arm
/// only bites a path that vanishes mid-walk, which is correctly refused. An
/// under-root symlink whose target stays *inside* the root passes, so legitimate
/// in-root symlinked content is still discovered — the gate does not over-refuse.
///
/// Takes the entry's `&Path` (from `walkdir::DirEntry::path`) rather than the
/// `DirEntry`, so `path_containment` carries no `walkdir` coupling.
pub fn path_within_root_walk_entry(path: &Path, root: &Path) -> bool {
    path_within_root(path, root)
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
    if !lexically_in_root(path, root) {
        return false;
    }

    // Lexically in-root: canonicalize the candidate to reject symlink escapes; a
    // speculative not-yet-created candidate (canonicalize fails) is admitted,
    // already proven lexically contained above.
    canonical_containment(path, root).unwrap_or(true)
}

/// True if `path` is contained within `root` **and is safe to emit** as a
/// speculative create target — the guard for a path baked into client-facing
/// text that may be turned back into a `CreateFile`, such as the "Expected at:"
/// hint of a "view not found" diagnostic (issue #201).
///
/// It shares [`path_within_root_lexical`]'s lexical gate (an out-of-root
/// candidate is refused with no disk probe, issue #145) and likewise admits a
/// *genuinely-absent* in-root candidate: the diagnostic fires precisely because
/// the target view doesn't exist, so the fail-closed [`path_within_root`] would
/// drop every normal not-yet-created in-root hint and break the "create view"
/// affordance. The one case it refuses that the lexical guard admits is a
/// **dangling under-root symlink** — a link whose path is lexically inside the
/// root but whose target is missing, so `canonicalize` can't prove where it
/// resolves. Emitting it could let the client's `CreateFile` follow the link out
/// of the project tree (issues #134/#155), so it is refused, while a path with
/// nothing on disk to follow is admitted.
///
/// The distinction lives in the `None` arm: `symlink_metadata` (lstat) succeeds
/// for the dangling-symlink node (the leaf exists, it just won't resolve) and
/// fails with `NotFound` for a path that truly doesn't exist. Only `NotFound`
/// admits — every other lstat error (`EACCES` from a non-searchable parent,
/// `ENOTDIR`, `ELOOP`) leaves the target unverifiable and is refused, so the
/// guard fails *closed* on anything it cannot prove absent. This closes the
/// residual the lexical guard's `unwrap_or(true)` leaves open for emitted paths
/// — a *leaf* dangling symlink; a path traversing a dangling symlink *directory*
/// is out of scope here (the conventional view-root surface never produces one).
pub fn path_within_root_emit_safe(path: &Path, root: &Path) -> bool {
    if !lexically_in_root(path, root) {
        return false;
    }

    match canonical_containment(path, root) {
        // The candidate exists on disk: trust its real, symlink-resolved target —
        // inside the root is safe to emit, an escape is refused.
        Some(contained) => contained,
        // The candidate can't be canonicalized. Admit ONLY a genuinely-absent
        // path — lstat fails with `NotFound`, a legitimate speculative create
        // target with nothing on disk to follow. Refuse every other case: a
        // dangling under-root symlink (lstat succeeds, leaf exists but won't
        // resolve), and any non-`NotFound` lstat error (`EACCES` from a
        // no-search-permission parent, `ENOTDIR`, `ELOOP`) — all leave the real
        // target unverifiable, so a `CreateFile` following the path could escape
        // the root (issues #134/#155). `is_err()` would admit those, failing
        // OPEN against this guard's contract; discriminate on `NotFound` to fail
        // closed on anything we cannot prove absent.
        None => path
            .symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
    }
}

/// True if `path` is contained within `root` and its real target is **proven**
/// so — the guard for a path that is minted from *discovered* source data and
/// then READ (issue #354 item 1).
///
/// Two invariants meet at such a call site, and the four guards above each
/// satisfy only one of them:
///
/// - The candidate comes from provider source, so canonicalizing it upfront
///   would leak an out-of-root existence oracle (#145). Hence the
///   [`lexically_in_root`] pre-gate: an out-of-root candidate is refused with
///   no `stat`/`realpath` probe of it.
/// - The value is consumed as a read primitive, so an unverifiable path must
///   be refused, not admitted (#134/#155). Hence `unwrap_or(false)`: a
///   dangling under-root symlink, an `EACCES`/`ELOOP` parent, or a path with
///   nothing on disk all yield `false`.
///
/// [`path_within_root_lexical`] fails the second (it admits every
/// unverifiable in-root path) and [`path_within_root_emit_safe`] fails it for
/// the genuinely-absent case — correct when the path is a speculative
/// *create target*, wrong when it becomes a directory that gets walked and
/// read. [`path_within_root`] satisfies the second but fails the first.
///
/// Gate against the provider's owning module rather than the project root and
/// a symlinked composer path repository still passes: `canonical_containment`
/// canonicalizes BOTH sides, so the module's real target contains its own
/// registrations.
pub fn path_within_root_registration(path: &Path, root: &Path) -> bool {
    lexically_in_root(path, root) && canonical_containment(path, root).unwrap_or(false)
}

/// The lexical containment gate shared by [`path_within_root_lexical`],
/// [`path_within_root_emit_safe`] and [`path_within_root_registration`]. True
/// when `path`, with interior `..`/`.` collapsed by `normalize_path`, is a
/// `starts_with` prefix-match of `root` —
/// first against the root as given, then (only if that fails) against the root's
/// canonicalized form. **Never canonicalizes the candidate**, so an out-of-root
/// candidate is refused before any `stat`/`realpath` probe of it (the existence
/// oracle issue #145 closes). Canonicalizing the *root* (a trusted in-root path)
/// is not an oracle; it only tolerates the macOS `/var`→`/private/var`
/// symlinked-root case, where an in-root candidate carries the resolved prefix
/// the raw `root` lacks. `normalize_path` collapses `..`/`.` first because
/// `Path::starts_with` is component-wise and does NOT resolve `..`.
fn lexically_in_root(path: &Path, root: &Path) -> bool {
    let normalized = normalize_path(path);
    normalized.starts_with(root)
        || root
            .canonicalize()
            .map(|real_root| normalized.starts_with(&real_root))
            .unwrap_or(false)
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

    #[cfg(unix)]
    #[test]
    fn lexical_admits_in_root_candidate_under_symlinked_root() {
        // The macOS `/var`→`/private/var` symlinked-root tolerance (AC #2). When
        // the root is given as a symlink but the candidate carries the *resolved*
        // prefix, the only thing that admits it is the gate's `root.canonicalize()`
        // leg (`:91-95`) — canonicalizing the *trusted root* is not an oracle. If
        // that leg is ever dropped or inverted, in-root component goto-definition
        // silently breaks on macOS while the rest of the suite stays green.
        let tmp = TempDir::new().unwrap();
        let real_root = tmp.path().join("real-project");
        std::fs::create_dir_all(&real_root).unwrap();

        // Build the candidate under the root's *canonical* (symlink-resolved) form,
        // mirroring how macOS hands the LSP a `/private/var/...` candidate while the
        // workspace root is the `/var/...` symlink.
        let real_canonical = real_root.canonicalize().unwrap();
        let views = real_canonical.join("resources").join("views");
        std::fs::create_dir_all(&views).unwrap();
        let candidate = views.join("card.blade.php");
        std::fs::write(&candidate, "{{ $x }}").unwrap();

        // Root passed as a *symlink* pointing at the real root.
        let symlink_root = tmp.path().join("link-project");
        std::os::unix::fs::symlink(&real_root, &symlink_root).unwrap();

        // Preconditions: the candidate is NOT lexically under the root as given —
        // so the first `starts_with(root)` leg fails — yet the symlink root
        // canonicalizes to the candidate's prefix, so only the
        // `root.canonicalize()` leg can admit it.
        assert!(
            !candidate.starts_with(&symlink_root),
            "candidate must not be lexically under the symlink root as given"
        );
        assert_eq!(
            symlink_root.canonicalize().unwrap(),
            real_canonical,
            "the symlink root must canonicalize to the real root"
        );

        assert!(
            path_within_root_lexical(&candidate, &symlink_root),
            "an in-root candidate under a symlinked root must be admitted via the root-canonicalize leg"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lexical_refuses_in_root_symlink_escaping_the_root() {
        // AC #2's "no containment downgrade" (#55/#134): a symlink whose *link
        // path* is lexically inside the root but whose target resolves OUTSIDE it
        // must still be refused by the lexical guard. The lexical gate admits the
        // link path, but the candidate-canonicalize step
        // (`canonical_containment`, `:103`) catches the escape — this asserts that
        // directly on `path_within_root_lexical`, not just on the fail-closed
        // `path_within_root`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();

        // A real file OUTSIDE the root.
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let secret = outside_dir.join("secret.blade.php");
        std::fs::write(&secret, "{{ $x }}").unwrap();

        // An under-root symlink whose target escapes the root.
        let escaping_link = root.join("escape.blade.php");
        std::os::unix::fs::symlink(&secret, &escaping_link).unwrap();

        // Preconditions: the link path is lexically inside the root (it passes the
        // lexical gate), but it canonicalizes to a path outside the root.
        assert!(
            escaping_link.starts_with(&root),
            "the link path is lexically inside the root"
        );
        let link_target = escaping_link.canonicalize().unwrap();
        let real_root = root.canonicalize().unwrap();
        assert!(
            !link_target.starts_with(&real_root),
            "the link's real target escapes the root"
        );

        assert!(
            !path_within_root_lexical(&escaping_link, &root),
            "an in-root symlink whose target escapes the root must be refused — no containment downgrade"
        );
    }

    #[test]
    fn emit_safe_admits_speculative_in_root_candidate() {
        // The everyday "Expected at:" case: the hint points at a view the
        // developer hasn't created yet. Nothing exists on disk, so the path can't
        // canonicalize — but it is lexically in-root with no symlink to follow,
        // so it is safe to emit as a create target and must be admitted (the
        // fail-closed guard would wrongly drop it).
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
        assert!(path_within_root_emit_safe(&candidate, root.path()));
    }

    #[test]
    fn emit_safe_admits_in_root_existing_file() {
        // A real in-root file canonicalizes inside the root → safe to emit.
        let root = TempDir::new().unwrap();
        let file = root
            .path()
            .join("resources")
            .join("views")
            .join("home.blade.php");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{{ $x }}").unwrap();

        assert!(path_within_root_emit_safe(&file, root.path()));
    }

    #[test]
    fn emit_safe_refuses_out_of_root_candidate() {
        // The lexical gate refuses an out-of-root candidate before any probe,
        // exactly as the other two guards do. The escapee is written to disk so
        // the intent is unambiguous: even a *real, existing* out-of-root file is
        // refused — the lexical gate fires before `canonical_containment` can run
        // its `Some(false)` arm, so no `stat`-probe ever touches the file.
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        let sibling = parent.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let escapee = sibling.join("secret.blade.php");
        std::fs::write(&escapee, "{{ $x }}").unwrap();

        // Precondition: the out-of-root candidate exists on disk, so a `false`
        // result can only come from the lexical containment gate, never absence.
        assert!(
            escapee.exists(),
            "precondition: the out-of-root candidate exists on disk"
        );
        assert!(
            !path_within_root_emit_safe(&escapee, &root),
            "an existing out-of-root candidate must be refused on root grounds \
             alone — the lexical gate rejects it before any disk probe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn emit_safe_refuses_dangling_under_root_symlink() {
        // The issue #201 residual: a symlink at `<root>/resources/views/ghost`
        // whose target was never created. Its link path is lexically inside the
        // root, but `canonicalize` fails (dangling), so the lexical guard's
        // `unwrap_or(true)` ADMITS it — and would echo it into the "Expected at:"
        // hint, where a client `CreateFile` could follow the link out of tree.
        // The emit-safe guard must refuse it: the leaf node exists (lstat
        // succeeds) but won't resolve. This is the one case where the two guards
        // diverge, so assert both halves of the contrast.
        let root = TempDir::new().unwrap();
        let views = root.path().join("resources").join("views");
        std::fs::create_dir_all(&views).unwrap();
        let missing_target = root.path().join("..").join("never-created.blade.php");
        let dangling = views.join("ghost.blade.php");
        std::os::unix::fs::symlink(&missing_target, &dangling).unwrap();

        // Preconditions: the link exists but can't be canonicalized, and its link
        // path is lexically inside the root.
        assert!(
            dangling.canonicalize().is_err(),
            "a dangling symlink must fail to canonicalize"
        );
        assert!(
            dangling.symlink_metadata().is_ok(),
            "the dangling symlink node itself exists (lstat succeeds)"
        );

        assert!(
            path_within_root_lexical(&dangling, root.path()),
            "the lexical guard admits the dangling under-root symlink — the residual"
        );
        assert!(
            !path_within_root_emit_safe(&dangling, root.path()),
            "the emit-safe guard must refuse a dangling under-root symlink — its \
             target is unknown and a CreateFile following it could escape the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn emit_safe_refuses_in_root_symlink_escaping_the_root() {
        // A symlink whose link path is in-root but whose target resolves OUTSIDE
        // the root: `canonical_containment` returns `Some(false)`, so the
        // emit-safe guard refuses it — no containment downgrade (#55/#134).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let secret = outside_dir.join("secret.blade.php");
        std::fs::write(&secret, "{{ $x }}").unwrap();

        let escaping_link = root.join("escape.blade.php");
        std::os::unix::fs::symlink(&secret, &escaping_link).unwrap();

        assert!(
            !path_within_root_emit_safe(&escaping_link, &root),
            "an in-root symlink whose target escapes the root must be refused"
        );
    }

    #[cfg(unix)]
    #[test]
    fn emit_safe_refuses_unverifiable_candidate_behind_a_file_component() {
        // The corrected `None`-arm contract (PR #202 review): only a genuinely
        // *absent* path (lstat `NotFound`) may be admitted — any OTHER lstat
        // error means the candidate is unverifiable, not provably absent, and
        // must fail CLOSED. Here a regular *file* sits where a directory is
        // expected in the path, so lstat on `<file>/ghost.blade.php` returns
        // `ENOTDIR`. The old `.is_err()` arm admitted any error (failing open);
        // the `NotFound`-only arm refuses it. Deterministic on every unix uid
        // (no permission dependence), so it pins the contract even under a CI
        // runner that bypasses permission checks.
        let root = TempDir::new().unwrap();
        let views = root.path().join("resources").join("views");
        std::fs::create_dir_all(&views).unwrap();
        let not_a_dir = views.join("home.blade.php");
        std::fs::write(&not_a_dir, "{{ $x }}").unwrap();
        let candidate = not_a_dir.join("ghost.blade.php");

        // Precondition: lstat fails with a NON-`NotFound` error (`ENOTDIR`).
        let err_kind = candidate.symlink_metadata().err().map(|e| e.kind());
        assert!(
            err_kind.is_some() && err_kind != Some(std::io::ErrorKind::NotFound),
            "precondition: lstat must fail with a non-NotFound error, got {err_kind:?}"
        );

        // The lexical guard admits it (canonicalize fails ⇒ `unwrap_or(true)`);
        // the emit-safe guard must refuse it — both halves of the contrast.
        assert!(
            path_within_root_lexical(&candidate, root.path()),
            "the lexical guard admits an unverifiable in-root candidate"
        );
        assert!(
            !path_within_root_emit_safe(&candidate, root.path()),
            "a non-NotFound lstat error must fail closed — the candidate is \
             unverifiable, not provably absent, so a CreateFile following it \
             could escape the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn emit_safe_refuses_unverifiable_candidate_behind_a_symlink_loop() {
        // The third non-`NotFound` lstat error named in the `None`-arm contract
        // (`path_containment.rs` doc): `ELOOP`. Two under-root symlinks point at
        // each other (`a -> b`, `b -> a`); a candidate routed through one of them
        // makes lstat traverse the cycle and fail with `ELOOP` — the candidate is
        // unverifiable, not provably absent, so it must fail CLOSED. Like the
        // `ENOTDIR` sibling this is deterministic on every unix uid (no
        // permission dependence), so it pins the contract even under a CI runner
        // that bypasses permission checks.
        let root = TempDir::new().unwrap();
        let views = root.path().join("resources").join("views");
        std::fs::create_dir_all(&views).unwrap();
        let loop_a = views.join("loop_a");
        let loop_b = views.join("loop_b");
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        // A candidate whose path traverses the cycle as an intermediate
        // component: lstat must resolve `loop_a` and loops → `ELOOP`.
        let candidate = loop_a.join("ghost.blade.php");

        // Precondition: lstat fails with a NON-`NotFound` error (`ELOOP`).
        let err_kind = candidate.symlink_metadata().err().map(|e| e.kind());
        assert!(
            err_kind.is_some() && err_kind != Some(std::io::ErrorKind::NotFound),
            "precondition: lstat must fail with a non-NotFound error, got {err_kind:?}"
        );

        // The lexical guard admits it (canonicalize fails ⇒ `unwrap_or(true)`);
        // the emit-safe guard must refuse it — both halves of the contrast.
        assert!(
            path_within_root_lexical(&candidate, root.path()),
            "the lexical guard admits an unverifiable in-root candidate"
        );
        assert!(
            !path_within_root_emit_safe(&candidate, root.path()),
            "an `ELOOP` lstat error must fail closed — the candidate is \
             unverifiable, not provably absent, so a CreateFile following it \
             could escape the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn emit_safe_refuses_dangling_symlink_behind_unsearchable_parent() {
        use std::os::unix::fs::PermissionsExt;

        // The specific dangerous case Holmes named (PR #202 review): a dangling
        // under-root symlink hidden behind a parent directory with no search
        // permission. lstat on the leaf fails with `EACCES` (→ `PermissionDenied`),
        // NOT `NotFound`, so the candidate is unverifiable-but-not-provably-absent.
        // A `.is_err()` test would ADMIT it (failing open) and echo it into the
        // "Expected at:" hint, where a client `CreateFile` could follow the link
        // out of tree; the corrected `NotFound`-only contract must REFUSE it.
        let root = TempDir::new().unwrap();
        let locked = root.path().join("resources").join("views").join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let missing_target = root.path().join("..").join("never-created.blade.php");
        let ghost = locked.join("ghost.blade.php");
        std::os::unix::fs::symlink(&missing_target, &ghost).unwrap();

        // Drop search permission on the parent so lstat on `ghost` returns EACCES.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Only meaningful when lstat is actually denied; a uid that bypasses
        // permission checks (e.g. root in some CI containers) would see through.
        // Restore perms (so the TempDir can be cleaned up) and skip in that case —
        // the ENOTDIR sibling test pins the contract deterministically regardless.
        let denied = ghost.symlink_metadata().err().map(|e| e.kind())
            == Some(std::io::ErrorKind::PermissionDenied);
        if !denied {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let refused = !path_within_root_emit_safe(&ghost, root.path());

        // Restore perms before the assert (and TempDir drop) so cleanup always
        // succeeds even if the assertion fails.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            refused,
            "a dangling under-root symlink behind a no-search-permission parent \
             (lstat EACCES, not NotFound) must be refused — the guard fails closed"
        );
    }

    // ─── walk-entry gate (issue #228) ───────────────────────────────────────

    #[test]
    fn walk_entry_admits_in_root_file() {
        // The everyday case: a real file the walk yields from inside the root
        // canonicalizes under the root and must be admitted (the gate does not
        // over-refuse ordinary discovered entries).
        let root = TempDir::new().unwrap();
        let file = root.path().join("app").join("View").join("Alert.php");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "<?php\n").unwrap();

        assert!(path_within_root_walk_entry(&file, root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn walk_entry_refuses_path_through_under_root_symlink_escaping_root() {
        // The #228 escape: a symlink *inside* an in-root walk root points OUTSIDE
        // the root, so `follow_links(true)` would descend it and surface
        // `<root>/components/escape/secret.php` as an entry — whose real path is
        // `<tmp>/outside/secret.php`. The walk-entry gate must refuse it.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(root.join("components")).unwrap();

        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.php");
        std::fs::write(&secret, "<?php\n").unwrap();

        let escape_link = root.join("components").join("escape");
        std::os::unix::fs::symlink(&outside, &escape_link).unwrap();

        // Precondition: the entry exists through the symlink and resolves OUTSIDE
        // the root, so `false` can only be the gate refusing it, not absence.
        let entry = root.join("components").join("escape").join("secret.php");
        assert_eq!(
            entry.canonicalize().unwrap(),
            secret.canonicalize().unwrap(),
            "precondition: the discovered entry resolves through the symlink to the \
             out-of-root file"
        );
        assert!(
            !entry
                .canonicalize()
                .unwrap()
                .starts_with(root.canonicalize().unwrap()),
            "precondition: the resolved entry escapes the project root"
        );

        assert!(
            !path_within_root_walk_entry(&entry, &root),
            "a discovered entry whose path crosses an under-root symlink resolving \
             outside the root must be refused by the fail-closed walk-entry gate"
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_entry_admits_path_through_in_root_symlink() {
        // The positive control for the symlink case: a symlink inside the walk
        // root whose target stays *inside* the root must still admit the entries
        // reached through it — the gate refuses escapes, not all symlinks.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        let real = root.join("real-components");
        std::fs::create_dir_all(&real).unwrap();
        let file = real.join("button.php");
        std::fs::write(&file, "<?php\n").unwrap();

        let link = root.join("linked-components");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // The entry reached through the in-root symlink.
        let entry = link.join("button.php");
        assert!(
            entry
                .canonicalize()
                .unwrap()
                .starts_with(root.canonicalize().unwrap()),
            "precondition: the entry resolves through an in-root symlink, staying \
             inside the root"
        );
        assert!(
            path_within_root_walk_entry(&entry, &root),
            "an entry reached through an in-root symlink (target inside the root) \
             must be admitted — the gate does not over-refuse"
        );
    }

    #[test]
    fn walk_entry_refuses_sibling_root_entry() {
        // A real entry under a *sibling* root must not be reported as contained.
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        let sibling = parent.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let escapee = sibling.join("secret.php");
        std::fs::write(&escapee, "<?php\n").unwrap();

        assert!(!path_within_root_walk_entry(&escapee, &root));
    }

    #[test]
    fn registration_admits_in_root_existing_dir() {
        // The ordinary case: a real directory under the gate dir. Both sides
        // canonicalize and the real path starts with the real gate dir.
        let root = TempDir::new().unwrap();
        let classes = root.path().join("src").join("Livewire");
        std::fs::create_dir_all(&classes).unwrap();

        assert!(path_within_root_registration(&classes, root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn registration_admits_classes_under_a_symlinked_gate_dir() {
        // #354 item 1: a composer path-repo module symlinked into the project
        // canonicalizes OUTSIDE the project root, which is why the gate dir is
        // the OWNING MODULE and not the root. `canonical_containment`
        // canonicalizes both sides, so the module's real target contains its
        // own registrations and the registration survives.
        let tmp = TempDir::new().unwrap();
        let real_module = tmp.path().join("packages").join("ui-kit");
        let classes = real_module.join("src").join("Livewire");
        std::fs::create_dir_all(&classes).unwrap();

        let root = tmp.path().join("project");
        std::fs::create_dir_all(root.join("app")).unwrap();
        let module_link = root.join("app").join("UiKit");
        std::os::unix::fs::symlink(&real_module, &module_link).unwrap();

        // Precondition: the module's real target escapes the project root, so
        // gating against the ROOT is what used to drop this registration.
        assert!(
            !module_link
                .canonicalize()
                .unwrap()
                .starts_with(root.canonicalize().unwrap()),
            "the symlinked module resolves outside the project root"
        );

        assert!(
            path_within_root_registration(&module_link.join("src").join("Livewire"), &module_link),
            "a symlinked path-repo module contains its own registrations"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_dangling_under_root_symlink() {
        // The #134/#155 case this guard exists for: the link path is lexically
        // inside the gate dir, but the target is missing so `canonicalize`
        // cannot prove where it resolves. `path_within_root_lexical` ADMITS
        // this (it falls back to the lexical result) — which is why that guard
        // is wrong for a value that becomes a read primitive.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();

        let dangling = root.join("Livewire");
        std::os::unix::fs::symlink(root.join("NEVER_CREATED"), &dangling).unwrap();

        assert!(
            dangling.starts_with(&root),
            "the link path is lexically inside the gate dir"
        );
        assert!(
            dangling.canonicalize().is_err(),
            "the dangling link cannot be canonicalized"
        );

        assert!(
            path_within_root_lexical(&dangling, &root),
            "precondition: the lexical guard admits it — the behaviour this guard corrects"
        );
        assert!(
            !path_within_root_registration(&dangling, &root),
            "a dangling under-root symlink is unverifiable and must be refused"
        );
    }

    #[test]
    fn registration_refuses_genuinely_absent_candidate() {
        // Where this guard deliberately parts company with
        // `path_within_root_emit_safe`: nothing on disk is a legitimate
        // *create target*, but not a legitimate directory to walk and read.
        // Fail closed, matching the pre-#354 behaviour of this call site.
        let root = TempDir::new().unwrap();
        let absent = root.path().join("src").join("Livewire");

        assert!(
            path_within_root_emit_safe(&absent, root.path()),
            "precondition: emit_safe admits a genuinely-absent path"
        );
        assert!(
            !path_within_root_registration(&absent, root.path()),
            "an absent path proves nothing about where it will resolve — refuse it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_out_of_root_candidate_without_probing() {
        // Issue #145: the candidate is provider-source-derived, so an upfront
        // `canonicalize()` of it would leak an out-of-root existence oracle.
        // The candidate here is an out-of-root symlink that WOULD resolve back
        // inside the gate dir — so only a lexical-first check can refuse it,
        // and `false` proves no disk probe of the out-of-root path decided it.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        let classes = root.join("src").join("Livewire");
        std::fs::create_dir_all(&classes).unwrap();

        let outside_link = tmp.path().join("outside-link");
        std::os::unix::fs::symlink(&classes, &outside_link).unwrap();

        assert!(
            !outside_link.starts_with(&root),
            "the link path is lexically outside the gate dir"
        );
        assert_eq!(
            outside_link.canonicalize().unwrap(),
            classes.canonicalize().unwrap(),
            "the link resolves back inside the gate dir"
        );

        assert!(
            !path_within_root_registration(&outside_link, &root),
            "an out-of-root candidate must be refused lexically, before any disk probe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_in_root_symlink_escaping_the_gate_dir() {
        // No containment downgrade (#55/#134): a link path lexically inside
        // the gate dir whose real target escapes it is refused by the
        // canonicalize step. This is the sibling-module reach in #354 item 1.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let escaping = root.join("Livewire");
        std::os::unix::fs::symlink(&outside, &escaping).unwrap();

        assert!(escaping.starts_with(&root));
        assert!(
            !escaping
                .canonicalize()
                .unwrap()
                .starts_with(root.canonicalize().unwrap()),
            "the link's real target escapes the gate dir"
        );

        assert!(
            !path_within_root_registration(&escaping, &root),
            "an in-root symlink whose target escapes the gate dir must be refused"
        );
    }
}
