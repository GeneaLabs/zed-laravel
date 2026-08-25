//! Resolve Laravel translation keys to their localized strings.
//!
//! Every shape resolves under `{lang_root}/`, where `{lang_root}` is `lang/`
//! (Laravel 9+) or `resources/lang/` (Laravel 8 and earlier) — both are always
//! searched, in that order. See [`project_lang_roots`].
//!
//! Laravel supports three translation shapes:
//!
//! - **Dotted keys** (`__('validation.required')`) — resolved through PHP files
//!   under `{lang_root}/{locale}/`. `validation.required` →
//!   `lang/de/validation.php` on a `de` project, key `required`.
//!
//! - **Namespaced dotted keys** (`__('filament-tables::table.label')`) — resolved
//!   through `{lang_root}/vendor/{namespace}/{locale}/{file}.php` (the published
//!   location for package translations) first, then — when the caller supplies
//!   the `vendor_map` built by [`crate::vendor_translations`] — the package's
//!   own unpublished lang directory under `vendor/{vendor}/{package}/...`.
//!   That directory comes from untrusted source, so every read against it is
//!   fenced by [`crate::path_containment`] (issue #248).
//!
//! - **Text keys** (`__('Welcome to our app')`) — resolved through the single
//!   JSON file `{lang_root}/{locale}.json`. The key IS the source string and
//!   the value is the translated string.
//!
//! No shape assumes a locale. [`available_locales`] answers "which locales
//! could define this key", and hover, go-to-definition and diagnostics all
//! resolve against that one set so they cannot disagree (issue #288).
//!
//! All three shapes route to the same PHP-array walker from [`config_lookup`]
//! since Laravel's `.php` translation files share their exact shape with
//! config files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config_lookup;

/// A resolved translation along with the file the value was read from.
/// The source file is rendered as a display-friendly path in hover output so
/// users can tell whether a key came from an app file, a published vendor
/// translation, or the JSON catalogue.
#[derive(Debug, Clone)]
pub struct ResolvedTranslation {
    pub value: String,
    pub source_file: PathBuf,
}

/// Resolve a translation key against a project root and locale. Returns both
/// the translated value and the file it was read from — see
/// [`ResolvedTranslation`].
///
/// For namespaced keys (`package::file.key`), the resolver tries the
/// published location first (`lang/vendor/<namespace>/...`) and falls back
/// to the unpublished vendor location when `vendor_map` is provided. See
/// [`crate::vendor_translations`] for how that map is built.
pub fn resolve_translation_detailed(
    root: &Path,
    key: &str,
    locale: &str,
    vendor_map: Option<&HashMap<String, PathBuf>>,
) -> Option<ResolvedTranslation> {
    if let Some((namespace, rest)) = split_namespace(key) {
        if let Some(r) = resolve_namespaced(root, namespace, rest, locale) {
            return Some(r);
        }
        // Published path missed — try the unpublished vendor directory.
        if let Some(map) = vendor_map {
            if let Some(dir) = map.get(namespace) {
                return resolve_namespaced_in_dir(root, dir, rest, locale);
            }
        }
        return None;
    }
    if is_dotted_key(key) {
        return resolve_dotted(root, key, locale);
    }
    resolve_text_key(root, key, locale)
}

/// Backwards-compatible wrapper that returns only the value, matching the
/// pre-source-file API. Used by tests that don't care about the source path.
pub fn resolve_translation(root: &Path, key: &str, locale: &str) -> Option<String> {
    resolve_translation_detailed(root, key, locale, None).map(|r| r.value)
}

/// Split a namespaced key (`package::file.key.path`) into its namespace and
/// the rest. Returns `None` for keys without a `::` separator.
fn split_namespace(key: &str) -> Option<(&str, &str)> {
    let idx = key.find("::")?;
    Some((&key[..idx], &key[idx + 2..]))
}

/// Distinguish a dotted PHP-file key (`validation.required`) from a text key
/// (`"Welcome to our app"`). Heuristic: dotted keys contain a `.` and no
/// whitespace.
fn is_dotted_key(key: &str) -> bool {
    key.contains('.') && !key.contains(' ')
}

/// The directories a project may keep translations in, in priority order:
/// `lang/` (Laravel 9+) and `resources/lang/` (Laravel 8 and earlier).
///
/// Both are checked everywhere, because the diagnostics path has always
/// checked both — resolving only `lang/` left hover unable to find any
/// translation on a Laravel-8-style project while diagnostics happily
/// resolved it, which is precisely the hover/diagnostics divergence issue
/// #288 exists to close.
pub fn project_lang_roots(root: &Path) -> [PathBuf; 2] {
    [root.join("lang"), root.join("resources").join("lang")]
}

/// Resolve a dotted key against `{lang_root}/{locale}/{file}.php`.
fn resolve_dotted(root: &Path, key: &str, locale: &str) -> Option<ResolvedTranslation> {
    let mut parts = key.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    project_lang_roots(root).iter().find_map(|lang| {
        read_php_value(&lang.join(locale).join(format!("{}.php", file)), &key_path)
    })
}

/// Resolve a published namespaced key against
/// `lang/vendor/{namespace}/{locale}/{file}.php`.
fn resolve_namespaced(
    root: &Path,
    namespace: &str,
    rest: &str,
    locale: &str,
) -> Option<ResolvedTranslation> {
    let mut parts = rest.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    project_lang_roots(root).iter().find_map(|lang| {
        read_php_value(
            &lang
                .join("vendor")
                .join(namespace)
                .join(locale)
                .join(format!("{}.php", file)),
            &key_path,
        )
    })
}

/// Resolve a namespaced key against an explicit lang directory — the
/// fallback used when the published path missed and the namespace was
/// discovered via [`crate::vendor_translations`]. `root` is the project root,
/// used to fence the read inside the tree.
fn resolve_namespaced_in_dir(
    root: &Path,
    lang_dir: &Path,
    rest: &str,
    locale: &str,
) -> Option<ResolvedTranslation> {
    let mut parts = rest.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();
    if key_path.is_empty() {
        return None;
    }
    let path = lang_dir.join(locale).join(format!("{}.php", file));
    // Defense-in-depth: `lang_dir` is derived from a `loadTranslationsFrom`
    // argument in project/vendor source — untrusted input. A traversal like
    // `base_path('../../../../.ssh')` or `__DIR__.'/../../../../etc'` could seed
    // an out-of-root directory; fail-closed before the read so a namespaced key
    // can never turn the LSP into an arbitrary-file-read primitive. Mirrors the
    // guard every other read site in this codebase applies (issue #248).
    if !crate::path_containment::path_within_root(&path, root) {
        return None;
    }
    read_php_value(&path, &key_path)
}

/// Resolve a text key against `lang/{locale}.json`.
fn resolve_text_key(root: &Path, key: &str, locale: &str) -> Option<ResolvedTranslation> {
    project_lang_roots(root).iter().find_map(|lang| {
        let path = lang.join(format!("{}.json", locale));
        let content = std::fs::read_to_string(&path).ok()?;
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&content).ok()?;
        let value = map.get(key)?.as_str()?;
        Some(ResolvedTranslation {
            value: format!("'{}'", value),
            source_file: path,
        })
    })
}

/// The fallback locale when a project exposes none — Laravel's own default.
const DEFAULT_LOCALE: &str = "en";

/// Every locale that could define `key`, ordered with the project's configured
/// `APP_LOCALE` first and the rest alphabetically.
///
/// Discovery looks at the lang directories the key could live in — the
/// published vendor override plus the registered namespace directory for a
/// namespaced key, the project lang roots otherwise — and treats both locale
/// *subdirectories* and `{locale}.json` catalogues as evidence of a locale.
/// The `vendor` subdirectory is excluded: it holds published package
/// translations, not a locale. A registered namespace directory that resolves
/// outside the project root is dropped before it is read (issue #248).
///
/// Never returns empty. A project with no discoverable locales (no lang
/// directory at all, or one containing nothing) falls back to
/// `["en"]`, so callers always have something to resolve against.
///
/// This is the single source of truth for "which locales matter for this key" —
/// hover, go-to-definition and diagnostics all resolve against the same set, so
/// a key defined only in `de` renders, navigates and validates consistently.
pub fn available_locales(
    root: &Path,
    key: &str,
    vendor_map: Option<&HashMap<String, PathBuf>>,
) -> Vec<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some((namespace, _)) = split_namespace(key) {
        for lang in project_lang_roots(root) {
            dirs.push(lang.join("vendor").join(namespace));
        }
        // The unpublished vendor dir comes from a `loadTranslationsFrom`
        // argument in project/vendor source — untrusted input that can point
        // anywhere (issue #248). `resolve_namespaced_in_dir` already fences its
        // read; this enumeration is a read site too, so it takes the same
        // fail-closed guard rather than `read_dir`-ing an out-of-root directory
        // and rendering whatever it finds there as this key's locales.
        if let Some(dir) = vendor_map.and_then(|m| m.get(namespace)) {
            if crate::path_containment::path_within_root(dir, root) {
                dirs.push(dir.clone());
            }
        }
    } else {
        dirs.extend(project_lang_roots(root));
    }

    let mut locales: Vec<String> = Vec::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let locale = if path.is_dir() {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            } else if path.extension().is_some_and(|e| e == "json") {
                path.file_stem()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            } else {
                None
            };
            // `vendor` is a namespace container, not a locale. Dedupe across
            // dirs so a locale present in both the published and unpublished
            // vendor directory is listed once.
            if let Some(locale) = locale {
                if locale != "vendor" && !locales.contains(&locale) {
                    locales.push(locale);
                }
            }
        }
    }

    if locales.is_empty() {
        return vec![DEFAULT_LOCALE.to_string()];
    }

    locales.sort();
    // The project's own locale leads; everything else stays alphabetical. An
    // APP_LOCALE that no directory defines simply doesn't appear, leaving the
    // alphabetical order untouched.
    if let Some(app_locale) = crate::config::read_env_value(root, "APP_LOCALE") {
        if let Some(idx) = locales.iter().position(|l| *l == app_locale) {
            let leading = locales.remove(idx);
            locales.insert(0, leading);
        }
    }
    locales
}

/// Shared PHP-file read + walk. Returns the bundled value + source path on hit.
fn read_php_value(path: &Path, key_path: &[&str]) -> Option<ResolvedTranslation> {
    let content = std::fs::read_to_string(path).ok()?;
    let value = config_lookup::resolve_in_source(&content, key_path)?;
    Some(ResolvedTranslation {
        value,
        source_file: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests;
