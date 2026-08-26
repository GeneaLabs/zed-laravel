//! Work out *which files could define* a Laravel translation key.
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
//!
//! - **Text keys** (`__('Welcome to our app')`) — resolved through the single
//!   JSON file `{lang_root}/{locale}.json`. The key IS the source string and
//!   the value is the translated string.
//!
//! No shape assumes a locale. [`locale_candidate_dirs`] answers "which
//! directories could reveal a locale for this key", and hover, go-to-definition
//! and diagnostics all resolve against that one set so they cannot disagree
//! (issue #288).
//!
//! # This module does no I/O
//!
//! It is **pure path arithmetic**: it names candidate files and directories and
//! stops there. Reading them, parsing them and caching the result is
//! [`crate::salsa_impl::TranslationCache`]'s job, so every lang-file read goes
//! through Salsa and is invalidated rather than repeated (issue #293). Splitting
//! it this way keeps one definition of "where could this key live" shared by the
//! resolver, locale discovery and their tests.
//!
//! Every path built here joins segments taken verbatim from a translation key
//! in parsed PHP/Blade source — the `vendor::` namespace, the dotted file
//! segment — or from a `loadTranslationsFrom` argument. **All of it is
//! untrusted and can carry `../` traversal or an absolute path.** Because
//! nothing here reads, the fail-closed containment guard that used to sit at
//! each read site now sits at the single choke point that does:
//! `TranslationCache::ensure_file` / `ensure_dir`, which refuse any candidate
//! they cannot prove inside the project root (issue #248). A candidate emitted
//! here is a *proposal*, never a permission.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The fallback locale when a project exposes none — Laravel's own default.
pub const DEFAULT_LOCALE: &str = "en";

/// One file that could define a translation key, and how to look it up in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationCandidate {
    /// A PHP array catalogue and the nested key path to walk inside it.
    Php {
        /// The `.php` catalogue to read.
        path: PathBuf,
        /// The key segments after the file name (`['required']` for
        /// `validation.required`). Never empty.
        key_path: Vec<String>,
    },
    /// A `{lang_root}/{locale}.json` catalogue, where the key is the source
    /// string itself.
    Json {
        /// The `.json` catalogue to read.
        path: PathBuf,
        /// The source string, used verbatim as the lookup key.
        key: String,
    },
}

impl TranslationCandidate {
    /// The file this candidate would read.
    pub fn path(&self) -> &Path {
        match self {
            Self::Php { path, .. } | Self::Json { path, .. } => path,
        }
    }
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

/// Split a namespaced key (`package::file.key.path`) into its namespace and
/// the rest. Returns `None` for keys without a `::` separator.
pub fn split_namespace(key: &str) -> Option<(&str, &str)> {
    let idx = key.find("::")?;
    Some((&key[..idx], &key[idx + 2..]))
}

/// Distinguish a dotted PHP-file key (`validation.required`) from a text key
/// (`"Welcome to our app"`). Heuristic: dotted keys contain a `.` and no
/// whitespace.
pub fn is_dotted_key(key: &str) -> bool {
    key.contains('.') && !key.contains(' ')
}

/// Split a dotted key into its file segment and the nested key path after it.
/// `None` when there is no key path left (`'validation'`, `'validation.'`) —
/// a bare file name names no key, which is not a resolution.
fn split_file_and_key_path(key: &str) -> Option<(&str, Vec<String>)> {
    let mut parts = key.split('.');
    let file = parts.next()?;
    let key_path: Vec<String> = parts.map(str::to_string).collect();
    (!key_path.is_empty()).then_some((file, key_path))
}

/// Every file that could define `key` in `locale`, in the order the resolver
/// must try them — first hit wins.
///
/// For namespaced keys the published override
/// (`{lang_root}/vendor/{namespace}/{locale}/{file}.php`) is proposed before
/// the package's own unpublished directory from `vendor_map`, matching
/// Laravel's own precedence.
///
/// Returns an empty vec for a key that names no lookup at all (a bare
/// `'validation'` with no nested path).
pub fn translation_candidates(
    root: &Path,
    key: &str,
    locale: &str,
    vendor_map: Option<&HashMap<String, PathBuf>>,
) -> Vec<TranslationCandidate> {
    let php = |path: PathBuf, key_path: &[String]| TranslationCandidate::Php {
        path,
        key_path: key_path.to_vec(),
    };

    if let Some((namespace, rest)) = split_namespace(key) {
        let Some((file, key_path)) = split_file_and_key_path(rest) else {
            return Vec::new();
        };
        let mut out: Vec<TranslationCandidate> = project_lang_roots(root)
            .iter()
            .map(|lang| {
                php(
                    lang.join("vendor")
                        .join(namespace)
                        .join(locale)
                        .join(format!("{file}.php")),
                    &key_path,
                )
            })
            .collect();
        // Published path missed — the package's own unpublished lang dir.
        if let Some(dir) = vendor_map.and_then(|m| m.get(namespace)) {
            out.push(php(dir.join(locale).join(format!("{file}.php")), &key_path));
        }
        return out;
    }

    if is_dotted_key(key) {
        let Some((file, key_path)) = split_file_and_key_path(key) else {
            return Vec::new();
        };
        return project_lang_roots(root)
            .iter()
            .map(|lang| php(lang.join(locale).join(format!("{file}.php")), &key_path))
            .collect();
    }

    project_lang_roots(root)
        .iter()
        .map(|lang| TranslationCandidate::Json {
            path: lang.join(format!("{locale}.json")),
            key: key.to_string(),
        })
        .collect()
}

/// Every directory whose listing could reveal a locale for `key`.
///
/// For a namespaced key that is the published vendor override directory plus
/// the registered namespace directory; for every other shape it is the project
/// lang roots. Locale *subdirectories* and `{locale}.json` catalogues both
/// count as evidence of a locale — see
/// [`crate::salsa_impl::locales_in_dir`], which reads the listing.
///
/// This is the single source of truth for "which locales matter for this key" —
/// hover, go-to-definition and diagnostics all resolve against the same set, so
/// a key defined only in `de` renders, navigates and validates consistently.
pub fn locale_candidate_dirs(
    root: &Path,
    key: &str,
    vendor_map: Option<&HashMap<String, PathBuf>>,
) -> Vec<PathBuf> {
    let Some((namespace, _)) = split_namespace(key) else {
        return project_lang_roots(root).to_vec();
    };
    let mut dirs: Vec<PathBuf> = project_lang_roots(root)
        .iter()
        .map(|lang| lang.join("vendor").join(namespace))
        .collect();
    if let Some(dir) = vendor_map.and_then(|m| m.get(namespace)) {
        dirs.push(dir.clone());
    }
    dirs
}

/// Is `path` a Laravel translation catalogue — a `.php` array file or a
/// `.json` text catalogue under one of this project's lang roots?
///
/// Used to route editor edits and watched-file events at the correct Salsa
/// input: a lang file must invalidate the translation cache, which no other
/// `.php` file does (issue #293). Deliberately **lexical** — this classifies,
/// it does not authorize, and the fail-closed containment guard still runs at
/// the read site (issue #248).
///
/// `lang/vendor/**` is included: published package translations are catalogues
/// like any other, and an edit to one must invalidate just the same.
pub fn is_lang_file(root: &Path, path: &Path) -> bool {
    let is_catalogue = path
        .extension()
        .is_some_and(|ext| ext == "php" || ext == "json");
    is_catalogue
        && project_lang_roots(root)
            .iter()
            .any(|lang| path.starts_with(lang))
}

#[cfg(test)]
mod tests;
