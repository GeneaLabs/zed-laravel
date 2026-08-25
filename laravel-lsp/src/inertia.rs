//! Inertia.js page resolution (issue #10).
//!
//! Inertia "views" are not Blade templates — they resolve to JS/TS files under
//! `resources/js/Pages/`. A page name uses `/` for nesting, so `'Auth/Login'`
//! resolves to `resources/js/Pages/Auth/Login.{ext}`. This module is the
//! Inertia analogue of the Blade view-resolution logic on
//! [`crate::salsa_impl::LaravelConfigData`]: pure path math with a
//! project-root containment guard, plus startup discovery helpers
//! (dominant-extension detection and page listing) used by code actions and
//! completion.
//!
//! Only the default page directory (`resources/js/Pages/`) is supported for
//! now; detecting a custom directory from the Inertia Vite plugin
//! (`vite.config.js`) is deliberately deferred (see issue #10).

use std::path::{Path, PathBuf};

use crate::path_containment::path_within_root_lexical;

/// Supported Inertia page-file extensions, in resolution-priority order. When a
/// page name matches files with several of these extensions, the earlier one
/// wins (unless a dominant project extension overrides it — see
/// [`resolve_page_path`]). This order also drives the default extension for the
/// "create page" code action when the project has no pages yet.
pub const PAGE_EXTENSIONS: [&str; 4] = ["vue", "tsx", "jsx", "svelte"];

/// The default Inertia pages directory, relative to the project root.
pub const PAGES_DIR: &str = "resources/js/Pages";

/// Absolute path to the project's Inertia pages directory.
pub fn pages_dir(root: &Path) -> PathBuf {
    root.join(PAGES_DIR)
}

/// Whether a path is an Inertia page file: it sits under a `resources/js/Pages/`
/// segment and carries a supported page extension ([`PAGE_EXTENSIONS`]). A
/// lightweight, root-agnostic heuristic — mirroring the migration/command
/// substring checks in the file-watcher — used to recognise page
/// create/change/delete events so the file-existence cache can be invalidated
/// eagerly instead of waiting out its TTL (issue #10).
pub fn is_page_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    // Segment matching, not substring: the old `contains("{PAGES_DIR}/")`
    // only matched a forward-slash spelling, so no file was ever recognised
    // as an Inertia page on Windows (issue #292).
    PAGE_EXTENSIONS.contains(&ext) && crate::path_segments::contains_segments(path, PAGES_DIR)
}

/// Whether a captured page name is safe to turn into a filesystem path.
///
/// Defense-in-depth mirroring [`crate::salsa_impl::LaravelConfigData::resolve_component_path`]:
/// the name comes straight from a string literal in source, and the
/// page→path mapping below would otherwise let a crafted name escape the
/// pages directory. We reject a name that:
///   - is empty (`inertia('')`);
///   - starts with `/` — an absolute path, where `PathBuf::join` would
///     discard the receiver;
///   - starts with `.` — a leading-dot / `../` traversal;
///   - contains a traversing `/`-segment — any segment that is `.`, `..`,
///     empty, or an absolute Windows-style drive prefix.
///
/// A backstop root-containment filter still runs on the built candidates, but
/// rejecting up front keeps us from ever constructing an out-of-root path.
pub fn is_valid_page_name(page: &str) -> bool {
    if page.is_empty() || page.starts_with('/') || page.starts_with('.') {
        return false;
    }
    // Reject any traversal or absolute segment. Inertia page names are simple
    // `/`-nested identifiers; a `..` or empty segment never appears in a real
    // one and is how a literal smuggles a traversal past the join. A `:` in a
    // segment is a Windows drive prefix (`C:`), likewise absent from a real
    // page name — reject it so the code matches the documented invariant.
    !page
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == ".." || seg.contains(':'))
}

/// Build the candidate file paths for an Inertia page name, in resolution
/// order. The page name's `/` separators map to directories; each candidate
/// appends one of [`PAGE_EXTENSIONS`].
///
/// Returns an empty vec for an invalid (traversing) page name, and drops any
/// candidate that escapes `root` as a backstop.
pub fn resolve_page_candidates(root: &Path, page: &str) -> Vec<PathBuf> {
    if !is_valid_page_name(page) {
        return Vec::new();
    }

    let base = pages_dir(root).join(page);
    PAGE_EXTENSIONS
        .iter()
        .map(|ext| base.with_extension(ext))
        .filter(|path| path_within_root_lexical(path, root))
        .collect()
}

/// Resolve a page name to an existing file, if any. Candidates are probed in
/// [`PAGE_EXTENSIONS`] priority order, except that — when `dominant` is set and
/// that file exists — the dominant project extension is preferred (AC: "if
/// multiple extensions match the same page, prefer the dominant one").
pub fn resolve_existing_page(root: &Path, page: &str, dominant: Option<&str>) -> Option<PathBuf> {
    let candidates = resolve_page_candidates(root, page);
    if candidates.is_empty() {
        return None;
    }

    // Prefer the dominant extension when it both exists and is one of the
    // candidates — overrides the static priority order.
    if let Some(dom) = dominant {
        if let Some(hit) = candidates
            .iter()
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some(dom) && p.exists())
        {
            return Some(hit.clone());
        }
    }

    candidates.into_iter().find(|p| p.exists())
}

/// The preferred path to *create* a missing page at: the dominant extension if
/// known, else the first supported extension (`.vue`). Used to build the
/// code-action target. Returns `None` for an invalid page name.
pub fn page_create_path(root: &Path, page: &str, dominant: Option<&str>) -> Option<PathBuf> {
    if !is_valid_page_name(page) {
        return None;
    }
    let ext = dominant
        .filter(|d| PAGE_EXTENSIONS.contains(d))
        .unwrap_or(PAGE_EXTENSIONS[0]);
    Some(pages_dir(root).join(page).with_extension(ext))
}

/// Detect the project's dominant page-file extension by counting files of each
/// supported extension under the pages directory. Ties break toward
/// [`PAGE_EXTENSIONS`] priority order. Returns `None` when the pages directory
/// is absent or holds no recognised page files (a non-Inertia project).
pub fn detect_dominant_extension(root: &Path) -> Option<String> {
    let counts = extension_counts(root);
    // Iterate in priority order so ties resolve deterministically toward the
    // higher-priority extension.
    PAGE_EXTENSIONS
        .iter()
        .map(|ext| (*ext, counts.get(*ext).copied().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(ext, n)| {
            // Higher count wins; on a tie, the earlier priority extension wins
            // (lower index → higher rank).
            let rank =
                PAGE_EXTENSIONS.len() - PAGE_EXTENSIONS.iter().position(|e| e == ext).unwrap();
            (*n, rank)
        })
        .map(|(ext, _)| ext.to_string())
}

/// Count page files per supported extension under the pages directory
/// (recursive). A private helper for [`detect_dominant_extension`].
fn extension_counts(root: &Path) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    let dir = pages_dir(root);
    walk_pages(&dir, &mut |path| {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if PAGE_EXTENSIONS.contains(&ext) {
                *counts.entry(ext.to_string()).or_insert(0) += 1;
            }
        }
    });
    counts
}

/// List every Inertia page under the pages directory as a `/`-nested page name
/// without extension (e.g. `Auth/Login`). Used to drive completion. Sorted and
/// de-duplicated (the same page may exist as both `.vue` and `.tsx`).
pub fn list_pages(root: &Path) -> Vec<String> {
    let dir = pages_dir(root);
    let mut names = std::collections::BTreeSet::new();
    walk_pages(&dir, &mut |path| {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return;
        };
        if !PAGE_EXTENSIONS.contains(&ext) {
            return;
        }
        if let Ok(rel) = path.strip_prefix(&dir) {
            // Drop the extension, normalise separators to '/'.
            let with_ext = rel.to_string_lossy();
            let without_ext = &with_ext[..with_ext.len() - (ext.len() + 1)];
            names.insert(without_ext.replace('\\', "/"));
        }
    });
    names.into_iter().collect()
}

/// Recursively walk the pages directory, invoking `visit` for each file. A
/// small bounded-depth walker (no external crate); silently does nothing when
/// the directory is absent. Symlinked directories are not followed, matching
/// the conservative posture of the view walkers.
fn walk_pages(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk_pages(&path, visit),
            Ok(ft) if ft.is_file() => visit(&path),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "<template></template>").unwrap();
    }

    #[test]
    fn candidates_follow_extension_priority() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let cands = resolve_page_candidates(root, "Auth/Login");
        let exts: Vec<&str> = cands
            .iter()
            .map(|p| p.extension().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(exts, vec!["vue", "tsx", "jsx", "svelte"]);
        // Nesting maps '/' to directories.
        assert!(cands[0].ends_with("resources/js/Pages/Auth/Login.vue"));
    }

    #[test]
    fn resolves_each_supported_extension() {
        for ext in PAGE_EXTENSIONS {
            let dir = tempdir().unwrap();
            let root = dir.path();
            let page_file = pages_dir(root).join("Dashboard").with_extension(ext);
            write(&page_file);
            let resolved = resolve_existing_page(root, "Dashboard", None);
            assert_eq!(resolved.as_deref(), Some(page_file.as_path()), "ext {ext}");
        }
    }

    #[test]
    fn missing_page_resolves_to_none() {
        let dir = tempdir().unwrap();
        assert!(resolve_existing_page(dir.path(), "Does/NotExist", None).is_none());
    }

    #[test]
    fn priority_prefers_vue_over_tsx_when_both_exist() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&pages_dir(root).join("Profile").with_extension("vue"));
        write(&pages_dir(root).join("Profile").with_extension("tsx"));
        let resolved = resolve_existing_page(root, "Profile", None).unwrap();
        assert_eq!(resolved.extension().unwrap(), "vue");
    }

    #[test]
    fn dominant_extension_overrides_priority_when_ambiguous() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&pages_dir(root).join("Profile").with_extension("vue"));
        write(&pages_dir(root).join("Profile").with_extension("tsx"));
        // tsx is dominant → prefer it even though vue has higher static priority.
        let resolved = resolve_existing_page(root, "Profile", Some("tsx")).unwrap();
        assert_eq!(resolved.extension().unwrap(), "tsx");
    }

    #[test]
    fn detects_dominant_extension_by_count() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&pages_dir(root).join("A").with_extension("tsx"));
        write(&pages_dir(root).join("B").with_extension("tsx"));
        write(&pages_dir(root).join("C").with_extension("vue"));
        assert_eq!(detect_dominant_extension(root).as_deref(), Some("tsx"));
    }

    #[test]
    fn dominant_extension_is_none_without_pages() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_dominant_extension(dir.path()), None);
    }

    #[test]
    fn lists_pages_without_extension_and_nested() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(&pages_dir(root).join("Dashboard").with_extension("vue"));
        write(&pages_dir(root).join("Auth/Login").with_extension("tsx"));
        // Same page in two extensions de-duplicates to one entry.
        write(&pages_dir(root).join("Profile").with_extension("vue"));
        write(&pages_dir(root).join("Profile").with_extension("tsx"));
        let pages = list_pages(root);
        assert_eq!(pages, vec!["Auth/Login", "Dashboard", "Profile"]);
    }

    #[test]
    fn create_path_uses_dominant_then_falls_back_to_vue() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert!(page_create_path(root, "New/Page", Some("tsx"))
            .unwrap()
            .ends_with("resources/js/Pages/New/Page.tsx"));
        assert!(page_create_path(root, "New/Page", None)
            .unwrap()
            .ends_with("resources/js/Pages/New/Page.vue"));
    }

    #[test]
    fn recognises_page_files_by_dir_and_extension() {
        // Under the pages dir with a supported extension → a page file.
        for ext in PAGE_EXTENSIONS {
            let p = PathBuf::from(format!("/app/resources/js/Pages/Auth/Login.{ext}"));
            assert!(is_page_file(&p), "should recognise .{ext} page");
        }
        // Wrong directory, or unsupported extension → not a page file.
        assert!(!is_page_file(Path::new(
            "/app/resources/js/Pages/readme.md"
        )));
        assert!(!is_page_file(Path::new(
            "/app/resources/views/home.blade.php"
        )));
        assert!(!is_page_file(Path::new(
            "/app/resources/js/components/Btn.vue"
        )));
        // No extension at all.
        assert!(!is_page_file(Path::new("/app/resources/js/Pages/Login")));
    }

    #[test]
    fn rejects_traversing_page_names() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for bad in [
            "",
            "/etc/passwd",
            "../secret",
            "..",
            "a/../../b",
            "./x",
            "C:",
            "C:/Windows",
            "Auth/C:",
        ] {
            assert!(!is_valid_page_name(bad), "should reject {bad:?}");
            assert!(
                resolve_page_candidates(root, bad).is_empty(),
                "no candidates for {bad:?}"
            );
        }
        assert!(is_valid_page_name("Auth/Login"));
        assert!(is_valid_page_name("Dashboard"));
    }
}
