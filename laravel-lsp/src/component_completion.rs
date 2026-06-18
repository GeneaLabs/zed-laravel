//! Candidate collection for `<x-...>` Blade component autocomplete.
//!
//! A component tag can be backed by very different things — an anonymous
//! `.blade.php` file, a PHP class in `app/View/Components`, a package's
//! published views, or a class/anonymous *namespace* registered in a service
//! provider. This module turns each of those sources into a flat list of
//! `<x-...>` tag candidates and dedupes them by tag name (a class wins over an
//! anonymous view, mirroring Laravel's class-first component resolution).
//!
//! The filesystem walking lives here (not in the LSP handler) so it can be
//! unit-tested against tempdirs; the handler only gathers config and renders
//! the result into `CompletionItem`s.

use std::collections::HashMap;
use std::path::Path;

use walkdir::WalkDir;

/// What backs a completion candidate — drives the LSP item kind and the
/// dedup precedence when two sources produce the same tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// An anonymous `.blade.php`-backed component.
    AnonymousView,
    /// A PHP class-backed component (`app/View/Components` or a
    /// `Blade::componentNamespace` registration).
    Class,
}

/// One `<x-...>` completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentCandidate {
    /// The tag text inserted after `<x-`, e.g. `alert` or `test::backstage`.
    pub name: String,
    /// Detail shown alongside the completion item (usually a relative path).
    pub detail: String,
    pub kind: CandidateKind,
}

/// kebab-case a PascalCase / dotted identifier, preserving `.` separators so
/// nested component names stay dotted. `AlertDialog` → `alert-dialog`,
/// `Forms.InputText` → `forms.input-text`.
pub fn to_kebab_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c == '.' {
            result.push('.');
        } else if c.is_uppercase() {
            if i > 0 && !result.ends_with('.') && !result.ends_with('-') {
                result.push('-');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Turn a path relative to a scan root into the dotted, kebab-cased tag body,
/// or `None` if it isn't a PHP file. `Forms/InputText.php` → `forms.input-text`,
/// `button.blade.php` → `button`.
pub fn relative_path_to_tag_body(relative: &Path) -> Option<String> {
    let s = relative.to_string_lossy();
    let stem = s
        .strip_suffix(".blade.php")
        .or_else(|| s.strip_suffix(".php"))?;
    if stem.is_empty() {
        return None;
    }
    let dotted = stem.replace(['/', '\\'], ".");
    Some(to_kebab_case(&dotted))
}

/// Walk `dir` for anonymous `.blade.php` components, emitting candidates named
/// `{tag_prefix}{body}` (`tag_prefix` is `""` for the root component path or
/// `"ns::"` for a namespaced one). `display_root` is the project root used to
/// shorten the detail path.
pub fn scan_anonymous_dir(
    dir: &Path,
    tag_prefix: &str,
    display_root: &Path,
) -> Vec<ComponentCandidate> {
    scan_dir(dir, tag_prefix, display_root, CandidateKind::AnonymousView)
}

/// Walk `dir` for `.php` class-backed components (skipping `.blade.php` view
/// files), emitting `{tag_prefix}{body}` candidates with PascalCase filenames
/// kebab-cased.
pub fn scan_class_dir(
    dir: &Path,
    tag_prefix: &str,
    display_root: &Path,
) -> Vec<ComponentCandidate> {
    scan_dir(dir, tag_prefix, display_root, CandidateKind::Class)
}

fn scan_dir(
    dir: &Path,
    tag_prefix: &str,
    display_root: &Path,
    kind: CandidateKind,
) -> Vec<ComponentCandidate> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }

    let want_blade = kind == CandidateKind::AnonymousView;

    // Containment scope (issue #226): the `dir` reached via the PSR-4
    // namespace→directory path (`ComposerAutoload::resolve_namespace_dirs` →
    // `scan_class_dir`) is now containment-gated by the fail-closed
    // `path_within_root` guard at its source, so the *root* of this walk can no
    // longer be an out-of-root directory. What is NOT yet gated here is each
    // *discovered* path under `follow_links(true)`: a symlink encountered
    // *inside* an in-root `dir` whose target escapes the project root would be
    // followed and its files emitted as candidates. That symlink-escape leg is
    // deferred scope — `scan_dir` is shared by callers (e.g.
    // `collect_flux_components`) whose `dir`s are constructed in-root by
    // different means, so gating discovered paths uniformly belongs in a
    // dedicated follow-up rather than this resolver-scoped change. Low practical
    // risk under the LSP threat model: the scanned tree is the developer's own
    // project, and the probe is a read.
    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_blade = path.to_str().is_some_and(|s| s.ends_with(".blade.php"));

        // Anonymous scan wants only blade files; class scan wants only
        // non-blade `.php` files (a blade view paired with a class is found
        // by the anonymous scan, not duplicated here).
        if want_blade != is_blade {
            continue;
        }
        // Class scan: require a plain `.php` extension.
        if !is_blade && path.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }

        let Ok(relative) = path.strip_prefix(dir) else {
            continue;
        };
        let Some(body) = relative_path_to_tag_body(relative) else {
            continue;
        };

        let detail = path
            .strip_prefix(display_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        out.push(ComponentCandidate {
            name: format!("{tag_prefix}{body}"),
            detail,
            kind,
        });
    }
    out
}

/// Collect Flux component tag bodies for `<flux:...>` autocomplete. Scans the
/// app-published `resources/views/flux/` plus the Flux and Flux Pro package
/// sources under `vendor/`, returning bare dotted names (`button`,
/// `icon.arrow-right`) deduped and sorted.
pub fn collect_flux_components(root: &Path) -> Vec<ComponentCandidate> {
    let dirs = [
        root.join("resources/views/flux"),
        root.join("vendor/livewire/flux/stubs/resources/views/flux"),
        root.join("vendor/livewire/flux-pro/stubs/resources/views/flux"),
    ];
    let mut out = Vec::new();
    for dir in &dirs {
        out.extend(scan_anonymous_dir(dir, "", root));
    }
    dedup_and_sort(out)
}

/// Dedup candidates by tag name and sort by name. When two candidates share a
/// name, a `Class` beats an `AnonymousView` (Laravel resolves the class first);
/// otherwise the first occurrence wins.
pub fn dedup_and_sort(candidates: Vec<ComponentCandidate>) -> Vec<ComponentCandidate> {
    let mut by_name: HashMap<String, ComponentCandidate> = HashMap::new();
    for candidate in candidates {
        match by_name.get(&candidate.name) {
            // A class already holds this name — nothing outranks it.
            Some(existing) if existing.kind == CandidateKind::Class => {}
            // A class displaces a previously-stored view of the same name.
            Some(_) if candidate.kind == CandidateKind::Class => {
                by_name.insert(candidate.name.clone(), candidate);
            }
            // Same name, no class involved — keep what's already there.
            Some(_) => {}
            None => {
                by_name.insert(candidate.name.clone(), candidate);
            }
        }
    }
    let mut out: Vec<ComponentCandidate> = by_name.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests;
