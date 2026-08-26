//! Split a translation key into the catalogue file and nested path a rename
//! must edit.
//!
//! Translation keys live in `lang/<locale>/<file>.php` as nested PHP array
//! keys, just like config keys. The structural walk is identical (it delegates
//! to [`crate::config_key_locator::locate_in_source`]); the difference is that
//! translations exist in *every* registered locale, so a single rename must
//! update the corresponding key in `lang/en/auth.php`, `lang/es/auth.php`,
//! `lang/fr/auth.php`, etc. simultaneously.
//!
//! JSON-format translation files (`lang/en.json`) are out of scope for now —
//! the AST walker is PHP-specific. JSON support follows once we add a small
//! JSON walker.
//!
//! # This module does no I/O
//!
//! Like [`crate::translation_lookup`], it is pure: it names the file stem and
//! key path, and [`crate::salsa_impl::TranslationCache::locate_key_across_locales`]
//! does the reading, the memoizing and the containment check (issues #248,
//! #293).

/// Split `auth.throttle.message` into the catalogue stem (`auth`) and the
/// nested key path inside it (`["throttle", "message"]`).
///
/// `None` when the key names no leaf — a bare `auth`, or a trailing dot —
/// because a file name alone is not a declaration a rename can edit.
pub fn split_dotted_key(dotted_key: &str) -> Option<(&str, Vec<String>)> {
    let mut parts = dotted_key.split('.');
    let file_stem = parts.next()?;
    let key_path: Vec<String> = parts.map(str::to_string).collect();
    (!key_path.is_empty()).then_some((file_stem, key_path))
}

#[cfg(test)]
mod tests;
