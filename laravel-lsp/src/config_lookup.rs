//! Resolve dotted Laravel config keys to their source-text values.
//!
//! `resolve_value(root, "app.name")` reads `config/app.php`, finds the
//! `'name' => ...` entry in the returned array, and returns the source text
//! of the value (e.g. `"env('APP_NAME', 'Laravel')"`).
//!
//! Nested keys recurse into nested array literals — `"database.connections.mysql.host"`
//! walks through three levels of nesting. The resolver is deliberately
//! conservative: when the path leads through something other than an array
//! literal (a function-call result, a constant, an object), it returns `None`
//! and the caller falls back to a less-specific hover.
//!
//! Pure parsing — no I/O outside the initial `read_to_string`. Easy to unit-test
//! with synthetic PHP source.
//!
//! The walk itself lives in [`crate::config_key_locator`], which reads the same
//! `return [...]` array with tree-sitter. Until #369 this module carried its own
//! byte scanner — a second implementation over identical data, and a strictly
//! worse one: it stopped at the first entry it could not parse, so a key
//! declared after a list entry resolved to nothing, and a list index never
//! resolved at all.

use std::path::{Path, PathBuf};

/// Resolve a dotted Laravel config key (`"app.name"`) against a project root.
/// Returns the source text of the resolved value, trimmed of surrounding
/// whitespace. `None` when the file or key is missing.
pub fn resolve_value(root: &Path, dotted_key: &str) -> Option<String> {
    resolve_value_with_source(root, &[], dotted_key).map(|(value, _)| value)
}

/// Like [`resolve_value`], but additionally searches every module config
/// file contributing to the key's group and reports which file produced
/// the value. [`crate::config::config_group_files`] already hands the files
/// over in descending merge precedence — `array_replace_recursive`
/// semantics: the last-merged module wins, the project `config/` file is
/// the fallback — so the first file resolving the key is the winner.
pub fn resolve_value_with_source(
    root: &Path,
    module_dirs: &[PathBuf],
    dotted_key: &str,
) -> Option<(String, PathBuf)> {
    let mut parts = dotted_key.split('.');
    let file = parts.next()?;
    let key_path: Vec<&str> = parts.collect();

    for config_path in crate::config::config_group_files(root, module_dirs, file) {
        let Ok(content) = std::fs::read_to_string(&config_path) else {
            continue;
        };
        if let Some(value) = resolve_in_source(&content, &key_path) {
            return Some((value, config_path));
        }
    }
    None
}

/// Source-only variant for unit tests — operates on a string rather than
/// reading from disk.
pub fn resolve_in_source(source: &str, key_path: &[&str]) -> Option<String> {
    // Delegates to the tree-sitter walk that already backs goto, completion
    // and code lenses (#369). The byte scanner this replaced aborted at the
    // first entry it could not parse, so a key declared after a list entry
    // resolved to nothing, and a list index (`providers.0`) never resolved at
    // all — while completion offered both and goto navigated to them. Hover
    // said "value not found" and `__()` raised a false "not found"
    // diagnostic for keys that resolve perfectly well at runtime.
    crate::config_key_locator::resolve_value_source(source, key_path)
}

#[cfg(test)]
mod tests;
