//! Locate environment-variable key declarations across a project's
//! `.env*` files — column-accurate positions suitable for building a
//! rename `WorkspaceEdit`.
//!
//! `.env` files are line-oriented `KEY=value` text — no PHP AST walk
//! needed, just a small parser that:
//!   * skips blank lines and `#` comments
//!   * finds the first `=` on each remaining line
//!   * treats everything before the `=` (whitespace-trimmed) as the
//!     key
//!
//! `locate_keys_across_env_files` walks every `.env*` file at the
//! project root and returns one `EnvKeyLocation` per file that
//! declares the key. The same key in `.env`, `.env.example`,
//! `.env.testing`, `.env.local`, etc. all get rewritten together so
//! the project's env files stay in sync after a rename.

use std::path::{Path, PathBuf};

use crate::config_key_locator::KeyPosition;

/// One declaration of an env key, paired with the file it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvKeyLocation {
    pub file_path: PathBuf,
    pub position: KeyPosition,
}

/// Whether `name` is a file name Laravel reads as an environment file:
/// exactly `.env`, or a `.env.<suffix>` variant (`.env.local`,
/// `.env.example`, `.env.testing`, …).
///
/// Every env feature that has to *classify* a file asks this function —
/// directly, or through [`path_is_env_file`] when it holds a path. Startup
/// registration is the one env path that never asks, because it names `.env`,
/// `.env.local`, and `.env.example` outright and so classifies nothing.
///
/// It takes a **file name** — never a path. A substring test against a whole
/// path (`path.contains(".env")`) also matches every file under a directory
/// such as `.envs/`, which attaches env behaviour to shell scripts and `.php`
/// files that are not env files at all.
///
/// Note `.env` alone is matched exactly, and the variant arm requires the
/// dot. A bare `starts_with(".env")` admits `.envrc` — direnv's file, not
/// Laravel's, and common in Laravel repositories — along with `.environment`
/// and `.env-backup`. Salsa merges env sources by variable name, so admitting
/// one of those does not just add a file: it lets a foreign `DATABASE_URL`
/// answer for the project's own.
pub fn is_env_file_name(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

/// Whether the file at `path` is an environment file, judged by its file
/// name alone. The convenience form of [`is_env_file_name`] for callers that
/// hold a path (an LSP request carries a URI, not a name).
///
/// Reducing to the file name first is the whole point: a path that merely
/// *mentions* `.env` — `/srv/app/.envs/deploy.sh` — is not an env file.
pub fn path_is_env_file(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_env_file_name)
}

/// Find every `.env*` file at the project root that declares `key`.
/// Returns one entry per matching file. A key absent from a particular
/// file simply contributes nothing — that file is left alone.
///
/// The match strategy:
///   * Match any regular file whose name is exactly `.env` OR starts
///     with `.env.` — every Laravel-recognised variant is covered:
///     `.env`, `.env.local`, `.env.testing`, `.env.production`,
///     `.env.staging`, `.env.example`, plus any custom suffix a
///     project might invent (`.env.qa`, `.env.docker`, etc.).
///   * Skip directories named `.env` — we've seen this in misconfigured
///     projects.
///   * Read each match as UTF-8; non-UTF-8 files are silently skipped
///     (Laravel itself doesn't support non-UTF-8 env files).
pub fn locate_keys_across_env_files(root: &Path, key: &str) -> Vec<EnvKeyLocation> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // `.env` exact match OR `.env.<suffix>` prefix match. We
        // deliberately don't recurse into subdirectories — Laravel
        // doesn't read env files from anywhere but the project root.
        if !is_env_file_name(name) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(pos) = locate_in_source(&content, key) {
            out.push(EnvKeyLocation {
                file_path: path,
                position: pos,
            });
        }
    }
    out
}

/// Find `key` in `source` (a `.env`-format string) and return the
/// position of the key text only — no leading whitespace, no `=`, no
/// value. Returns `None` if the key isn't declared in this source.
///
/// First-match-wins: if a malformed env file declares `KEY=` on two
/// separate lines, we rewrite only the first. Real `.env` files don't
/// duplicate keys, so this matches Laravel's own behaviour (later
/// duplicates would override the earlier, but `phpdotenv`'s parser
/// warns on duplicates).
pub fn locate_in_source(source: &str, key: &str) -> Option<KeyPosition> {
    for (line_idx, line) in source.lines().enumerate() {
        let Some((found, start_col, end_col)) = parse_key_declaration(line) else {
            continue;
        };
        // Exact match only — `APP_NAM` must NOT match `APP_NAME`.
        if found != key {
            continue;
        }
        return Some(KeyPosition {
            line: line_idx as u32,
            start_column: start_col,
            end_column: end_col,
            is_literal_key: true,
        });
    }
    None
}

/// Enumerate every key declaration in a `.env`-format source, in file order,
/// with column-accurate positions. First declaration wins per key (matching
/// [`locate_in_source`]'s first-match-wins), so a malformed file with a
/// duplicated key yields a single entry. Used to build env-var code lenses for
/// an open `.env*` file.
pub fn enumerate_keys_in_source(source: &str) -> Vec<(String, KeyPosition)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (line_idx, line) in source.lines().enumerate() {
        let Some((key, start_col, end_col)) = parse_key_declaration(line) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        out.push((
            key.to_string(),
            KeyPosition {
                line: line_idx as u32,
                start_column: start_col,
                end_column: end_col,
                is_literal_key: true,
            },
        ));
    }
    out
}

/// The declaration a `#` comment hides, or `None` when `line` is not a
/// whole-line comment.
///
/// The single definition of "commented" for `.env` text: a line is commented
/// when its first non-blank character is `#`, the whole `#` run is the marker
/// (so `## KEY=value` is a commented declaration), and the body begins after
/// the whitespace that follows it. Every reader of `.env` lines classifies
/// through this one function — Salsa's `parse_env_source`, the declaration
/// parser that must reject exactly these lines, the commented-key enumeration
/// that parses what they hide, and the inline-comment tokenizer — so the
/// buffer-local and Salsa views of "commented" cannot drift apart.
///
/// The body is always a suffix of `line`, so `line.len() - body.len()` is the
/// column it starts at.
pub fn commented_declaration_body(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix('#')
        .map(|rest| rest.trim_start_matches('#').trim_start())
}

/// Enumerate every *commented-out* key declaration (`# KEY=value`) in a
/// `.env`-format source, in file order, with column-accurate positions
/// relative to the whole line. First declaration wins per key, matching
/// [`enumerate_keys_in_source`].
///
/// The companion to [`enumerate_keys_in_source`], which classifies a `#` line
/// as "not a declaration" and so can never see these. Both classify through
/// [`commented_declaration_body`] and parse through [`parse_key_declaration`],
/// so a commented key is parsed exactly as its active twin would be — only the
/// `#` run and the whitespace after it are removed first, and the resulting
/// columns shifted back by that offset.
///
/// The two enumerations are kept separate rather than merged behind a flag:
/// merging would let a commented `# KEY=` earlier in the file win the
/// first-match-wins race against an active `KEY=` below it, which would change
/// what the env code lens anchors on.
pub fn enumerate_commented_keys_in_source(source: &str) -> Vec<(String, KeyPosition)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (line_idx, line) in source.lines().enumerate() {
        let Some(body) = commented_declaration_body(line) else {
            continue;
        };
        let offset = (line.len() - body.len()) as u32;
        let Some((key, start_col, end_col)) = parse_key_declaration(body) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        out.push((
            key.to_string(),
            KeyPosition {
                line: line_idx as u32,
                start_column: start_col + offset,
                end_column: end_col + offset,
                is_literal_key: true,
            },
        ));
    }
    out
}

/// Parse one `.env` line into `(key, start_column, end_column)` — the key text
/// (everything before the first `=`, whitespace-trimmed) and its column span.
/// Returns `None` for blank lines, `#` comments, lines without `=`, and lines
/// whose key is empty. Shared by [`locate_in_source`] and
/// [`enumerate_keys_in_source`] so both classify lines identically, and
/// rejecting the comments [`commented_declaration_body`] admits — the two
/// sides of one rule, read from one place.
fn parse_key_declaration(line: &str) -> Option<(&str, u32, u32)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || commented_declaration_body(line).is_some() {
        return None;
    }
    let eq_pos = line.find('=')?;
    let key_part = &line[..eq_pos];
    let key = key_part.trim();
    if key.is_empty() {
        return None;
    }
    let leading_ws = key_part.len() - key_part.trim_start().len();
    let start_col = leading_ws as u32;
    let end_col = (leading_ws + key.len()) as u32;
    Some((key, start_col, end_col))
}

#[cfg(test)]
mod tests;
