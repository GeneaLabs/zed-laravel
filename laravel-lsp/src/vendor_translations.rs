//! Discover translation-namespace registrations across service providers.
//!
//! Laravel packages and apps register their translations in a
//! `ServiceProvider::boot()` method via:
//!
//! ```php
//! $this->loadTranslationsFrom(__DIR__.'/../resources/lang', 'package-namespace');
//! ```
//!
//! The published location for those translations is `lang/vendor/<namespace>/`
//! in the host project, which [`crate::translation_lookup`] already handles.
//! This module fills the gap for translations that **haven't been published**,
//! and for **app-level** registrations that never go through `lang/vendor/`:
//! it walks `vendor/` (and `app/Providers/`) for service providers that call
//! `loadTranslationsFrom`, extracts each `(namespace, directory)` pair, and
//! returns a map the resolver can fall back to when the published path doesn't
//! exist.
//!
//! The first-argument expression is parsed from the PHP AST (tree-sitter)
//! rather than a single literal regex, so the common path-helper forms all
//! resolve:
//!
//! | Form                              | Resolves to                                  |
//! | --------------------------------- | -------------------------------------------- |
//! | `__DIR__.'/rel'`                  | provider dir + `rel`                          |
//! | `dirname(__DIR__).'/rel'`         | provider dir's parent + `rel` (nests)         |
//! | `lang_path('app')`               | `<root>/lang/app`                             |
//! | `base_path('lang/custom')`       | `<root>/lang/custom`                          |
//!
//! The fluent package-builder convention (`->name('pkg')->hasTranslations()`)
//! is still matched by regex — its real `loadTranslationsFrom($computedDir,
//! $name)` call runs in a base class with runtime-computed arguments that no
//! AST walk of the provider file can see.
//!
//! No on-disk cache yet — the scan runs once at LSP startup and the result
//! lives in memory. A composer.lock-keyed cache (like
//! [`crate::config::scan_vendor_for_component_aliases`]) is a worthwhile
//! follow-up once the scan time becomes a noticeable cost on first hover.

use crate::parser::parse_php;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

lazy_static! {
    /// Matches a fluent package-builder name declaration: `->name('package')`.
    /// Builder-convention providers (e.g. Filament via laravel-package-tools)
    /// never call `loadTranslationsFrom` with literal arguments — the real call
    /// runs in a base class as
    /// `$this->loadTranslationsFrom($computedDir, $this->package->shortName())`.
    /// This pair of patterns reconstructs that registration form, the same way
    /// the view-namespace discovery in [`crate::salsa_impl`] does for
    /// `->hasViews()`.
    static ref BUILDER_NAME_RE: Regex = Regex::new(
        r#"->name\s*\(\s*['"]([^'"]+)['"]\s*\)"#
    ).unwrap();

    /// Matches the builder translation capability: `->hasTranslations()`.
    /// Unlike `->hasViews('ns')` there is no explicit-namespace argument —
    /// the namespace is always the package short-name.
    static ref BUILDER_HAS_TRANSLATIONS_RE: Regex = Regex::new(
        r#"->hasTranslations\s*\(\s*\)"#
    ).unwrap();
}

/// Walk `vendor/` for service providers that register translation namespaces.
/// Returns a map of `namespace → absolute lang directory`.
///
/// The scan applies two cheap gates before parsing any file:
/// - **Filename**: must contain `ServiceProvider`
/// - **Content substring**: must contain `loadTranslationsFrom` or
///   `hasTranslations`
///
/// Roughly the same shape as
/// [`crate::config::scan_vendor_for_component_aliases`] — these two scans
/// could share a single vendor-walk pass once we add the persistent cache.
pub fn scan_vendor_translation_namespaces(root: &Path) -> HashMap<String, PathBuf> {
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return HashMap::new();
    }

    let mut namespaces: HashMap<String, PathBuf> = HashMap::new();

    for entry in walkdir::WalkDir::new(&vendor)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("php") {
            continue;
        }
        let filename_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("ServiceProvider"))
            .unwrap_or(false);
        if !filename_matches {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        if !source.contains("loadTranslationsFrom") && !source.contains("hasTranslations") {
            continue;
        }

        process_provider_file(&source, path, root, &mut namespaces);
    }

    namespaces
}

/// Walk `app/Providers/` for app service providers that register translation
/// namespaces. Returns a map of `namespace → absolute lang directory`.
///
/// App providers (e.g. `AppServiceProvider`) commonly register translations
/// with `loadTranslationsFrom(lang_path('app'), 'app')` — a path the vendor
/// scan never sees because it lives outside `vendor/` and never publishes to
/// `lang/vendor/`. The directory itself is the gate here (everything under
/// `app/Providers/` is a provider), so only the content substring is checked.
pub fn scan_app_translation_namespaces(root: &Path) -> HashMap<String, PathBuf> {
    let providers = root.join("app").join("Providers");
    if !providers.is_dir() {
        return HashMap::new();
    }

    let mut namespaces: HashMap<String, PathBuf> = HashMap::new();

    for entry in walkdir::WalkDir::new(&providers)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("php") {
            continue;
        }

        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        if !source.contains("loadTranslationsFrom") && !source.contains("hasTranslations") {
            continue;
        }

        process_provider_file(&source, path, root, &mut namespaces);
    }

    namespaces
}

/// Run both extraction passes over a single provider file: the AST-based
/// `loadTranslationsFrom(...)` walk and the regex-based builder convention.
fn process_provider_file(
    source: &str,
    provider_path: &Path,
    root: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    extract_load_translations_calls(source, provider_path, root, namespaces);
    extract_builder_translations_from(source, provider_path, namespaces);
}

/// Walk the PHP AST for `$this->loadTranslationsFrom(<path>, '<namespace>')`
/// calls and contribute a `namespace → absolute_lang_dir` entry for each one
/// whose path argument resolves to a directory.
///
/// First-match-wins on namespace conflict — service-provider boot order is
/// non-deterministic and we have no good way to rank packages without a full
/// composer dependency graph.
fn extract_load_translations_calls(
    source: &str,
    provider_path: &Path,
    root: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    let Some(provider_dir) = provider_path.parent() else {
        return;
    };
    let Ok(tree) = parse_php(source) else {
        return;
    };
    let bytes = source.as_bytes();
    walk_load_translations(tree.root_node(), bytes, provider_dir, root, namespaces);
}

/// Recursive AST descent collecting every `loadTranslationsFrom` registration.
fn walk_load_translations(
    node: tree_sitter::Node,
    bytes: &[u8],
    provider_dir: &Path,
    root: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    if node.kind() == "member_call_expression" {
        if let Some((ns, dir)) = classify_load_translations_call(node, bytes, provider_dir, root) {
            namespaces.entry(ns).or_insert(dir);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_load_translations(child, bytes, provider_dir, root, namespaces);
    }
}

/// Classify one `$this->loadTranslationsFrom(<path>, '<namespace>')` call.
/// Returns `None` for any call that isn't such a registration, or whose path
/// argument can't be resolved to a directory (e.g. a bare variable).
fn classify_load_translations_call(
    node: tree_sitter::Node,
    bytes: &[u8],
    provider_dir: &Path,
    root: &Path,
) -> Option<(String, PathBuf)> {
    if !is_this_receiver(node.child_by_field_name("object")?, bytes) {
        return None;
    }
    if node.child_by_field_name("name")?.utf8_text(bytes).ok()? != "loadTranslationsFrom" {
        return None;
    }

    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut arg_exprs = args.named_children(&mut cursor).map(argument_value);

    let path_arg = arg_exprs.next()??;
    let ns_arg = arg_exprs.next()??;
    let namespace = string_literal_text(ns_arg, bytes)?;

    let lang_dir = resolve_path_arg(path_arg, bytes, provider_dir, root)?;
    let resolved = lang_dir.canonicalize().unwrap_or(lang_dir);
    Some((namespace, resolved))
}

/// Resolve a `loadTranslationsFrom` path argument to an absolute directory.
///
/// Handles the four common forms (see the module docs): `__DIR__.'/rel'`,
/// `dirname(__DIR__).'/rel'`, `lang_path('…')`, and `base_path('…')`, plus a
/// bare `__DIR__`. Anything else (a variable, an unrecognized helper) yields
/// `None` and the registration is skipped.
fn resolve_path_arg(
    node: tree_sitter::Node,
    bytes: &[u8],
    provider_dir: &Path,
    root: &Path,
) -> Option<PathBuf> {
    match node.kind() {
        // `__DIR__.'/rel'` or `dirname(__DIR__).'/rel'`: a `.` concatenation
        // of a directory base and a string literal.
        "binary_expression" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let rel = string_literal_text(right, bytes)?;
            let base = resolve_dir_base(left, bytes, provider_dir)?;
            Some(join_relative(&base, &rel))
        }
        // `lang_path('app')` / `base_path('lang/custom')` / a bare
        // `dirname(__DIR__)` used directly without concatenation.
        "function_call_expression" => {
            let fname = node
                .child_by_field_name("function")?
                .utf8_text(bytes)
                .ok()?;
            match fname {
                "lang_path" => {
                    let base = root.join("lang");
                    Some(match first_string_arg(node, bytes) {
                        Some(arg) => join_relative(&base, &arg),
                        None => base,
                    })
                }
                "base_path" => Some(match first_string_arg(node, bytes) {
                    Some(arg) => join_relative(root, &arg),
                    None => root.to_path_buf(),
                }),
                "dirname" => resolve_dir_base(node, bytes, provider_dir),
                _ => None,
            }
        }
        // Bare `__DIR__` with no relative suffix.
        "name" if node.utf8_text(bytes).ok()? == "__DIR__" => Some(provider_dir.to_path_buf()),
        _ => None,
    }
}

/// Resolve the directory-base side of a concatenation: `__DIR__` (the provider
/// dir) or `dirname(...)` (one level up, nesting for `dirname(dirname(...))`).
fn resolve_dir_base(node: tree_sitter::Node, bytes: &[u8], provider_dir: &Path) -> Option<PathBuf> {
    match node.kind() {
        "name" if node.utf8_text(bytes).ok()? == "__DIR__" => Some(provider_dir.to_path_buf()),
        "function_call_expression" => {
            if node
                .child_by_field_name("function")?
                .utf8_text(bytes)
                .ok()?
                != "dirname"
            {
                return None;
            }
            let inner = first_argument(node)?;
            let base = resolve_dir_base(inner, bytes, provider_dir)?;
            base.parent().map(|p| p.to_path_buf())
        }
        _ => None,
    }
}

/// Join a captured relative fragment onto a base directory. PHP source like
/// `__DIR__.'/../resources/lang'` yields a fragment starting with `/`; Rust's
/// `Path::join` treats a leading `/` as absolute and discards the receiver, so
/// strip leading `/` and `./` before joining.
fn join_relative(base: &Path, rel: &str) -> PathBuf {
    let rel = rel.trim_start_matches('/').trim_start_matches("./");
    base.join(rel)
}

/// The first string-literal argument of a function call, or `None`.
fn first_string_arg(call: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    string_literal_text(first_argument(call)?, bytes)
}

/// The value expression of a function call's first argument.
fn first_argument(call: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let first = args.named_children(&mut cursor).next();
    first.and_then(argument_value)
}

/// The value expression of a call argument. tree-sitter-php wraps each argument
/// in an `argument` node; for a named argument the parameter label is the
/// `name` field, so the value is the other child.
fn argument_value(arg: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if arg.kind() != "argument" {
        return Some(arg);
    }
    let label = arg.child_by_field_name("name");
    (0..arg.named_child_count() as u32)
        .filter_map(|i| arg.named_child(i))
        .find(|&ch| Some(ch) != label)
}

/// Whether `object` is the `$this` receiver of a `$this->method(...)` call.
fn is_this_receiver(object: tree_sitter::Node, bytes: &[u8]) -> bool {
    object.utf8_text(bytes).ok() == Some("$this")
}

/// The content of a single/double-quoted string literal node, or `None`.
/// Descends to the `string_content` child, matching the rest of the LSP; an
/// empty literal has no such child, so fall back to stripping a surrounding
/// quote pair.
fn string_literal_text(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return Some(child.utf8_text(bytes).ok()?.to_string());
        }
    }
    Some(
        node.utf8_text(bytes)
            .ok()?
            .trim_start_matches(['\'', '"'])
            .trim_end_matches(['\'', '"'])
            .to_string(),
    )
}

/// Reconstruct the fluent package-builder translation registration:
/// `$package->name('filament-tables')->hasTranslations()`. The builder's base
/// class registers `loadTranslationsFrom(<pkg>/resources/lang, shortName())`
/// at runtime — both arguments computed, invisible to the AST walk above.
/// The namespace is the package short-name (leading `laravel-` stripped) and
/// the directory follows the builder's `basePath('/../resources/lang')`
/// convention: one level up from the provider's `src/` dir.
fn extract_builder_translations_from(
    source: &str,
    provider_path: &Path,
    namespaces: &mut HashMap<String, PathBuf>,
) {
    if !BUILDER_HAS_TRANSLATIONS_RE.is_match(source) {
        return;
    }
    let Some(name_cap) = BUILDER_NAME_RE.captures(source) else {
        return;
    };
    let Some(package_name) = name_cap.get(1) else {
        return;
    };
    let namespace = crate::salsa_impl::builder_short_name(package_name.as_str());
    if namespace.is_empty() {
        return;
    }

    let Some(provider_dir) = provider_path.parent() else {
        return;
    };
    let lang_dir = provider_dir.join("../resources/lang");
    let resolved = lang_dir.canonicalize().unwrap_or(lang_dir);
    namespaces.entry(namespace).or_insert(resolved);
}

#[cfg(test)]
mod tests;
