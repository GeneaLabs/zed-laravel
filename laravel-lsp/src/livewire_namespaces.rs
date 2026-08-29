//! Statically extract imperative Livewire class-component namespace
//! registrations from service-provider source.
//!
//! Two shapes are recognised:
//!
//! 1. The Livewire 4 API itself, positional or PHP named arguments:
//!    `Livewire::addNamespace('prefix', 'Class\\Namespace', __DIR__.'/…')`
//!    `Livewire::addNamespace(namespace: 'prefix', classNamespace: …, classPath: …)`
//! 2. Wrapper conventions (`modules.livewireRegistrars`, default
//!    `loadLivewireComponentsFrom`):
//!    `$this->loadLivewireComponentsFrom(__DIR__.'/../Livewire', 'prefix')`
//!    — the modular-monolith shape where an abstract base provider forwards
//!    to `Livewire::addNamespace` and derives the class namespace from the
//!    concrete provider's own namespace
//!    (`Str::beforeLast(static::class, '\\Providers\\').'\\Livewire'`).
//!    That derivation is reproduced here from the file's own
//!    `namespace …;` declaration, so no cross-file analysis is needed.
//!
//! Calls whose arguments aren't static (bare variables, computed strings)
//! are skipped silently — the extractor under-reports rather than guesses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::parser::parse_php;
use crate::vendor_translations::{
    argument_value, is_this_receiver, resolve_path_arg, string_literal_text,
};

/// The three directories every registration in one provider file is resolved
/// and gated against: `provider_dir` is `__DIR__`, `root` backs the
/// `base_path()`/`lang_path()` helpers, and `gate_dir` is what containment is
/// judged by (see [`contained_class_path`]).
struct PathContext<'a> {
    provider_dir: &'a Path,
    root: &'a Path,
    gate_dir: &'a Path,
}

/// One registered Livewire class-component namespace: the PHP class
/// namespace the prefix maps to and the directory its classes live in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivewireClassNamespace {
    pub class_namespace: String,
    pub class_path: PathBuf,
}

/// Extract every statically resolvable Livewire namespace registration from
/// one provider file. Within a file the LAST registration wins on a prefix
/// conflict — PHP executes every statement, so the later `addNamespace`
/// overwrites the earlier one in Livewire's registry.
pub fn extract_livewire_namespaces(
    source: &str,
    provider_path: &Path,
    root: &Path,
    module_dir: Option<&Path>,
    registrar_methods: &[String],
) -> HashMap<String, LivewireClassNamespace> {
    let mut out = HashMap::new();
    if !source.contains("addNamespace")
        && !registrar_methods
            .iter()
            .any(|m| !m.is_empty() && source.contains(m.as_str()))
    {
        return out;
    }
    let Ok(tree) = parse_php(source) else {
        return out;
    };
    let bytes = source.as_bytes();
    let provider_dir = provider_path.parent().unwrap_or(provider_path);
    let file_namespace = php_file_namespace(tree.root_node(), bytes);
    // A module provider is contained by its OWN module, an app provider by
    // the project root — see `contained_class_path`.
    let paths = PathContext {
        provider_dir,
        root,
        gate_dir: module_dir.unwrap_or(root),
    };

    walk(
        tree.root_node(),
        bytes,
        &paths,
        registrar_methods,
        file_namespace.as_deref(),
        &mut out,
    );
    out
}

fn walk(
    node: tree_sitter::Node,
    bytes: &[u8],
    paths: &PathContext,
    registrar_methods: &[String],
    file_namespace: Option<&str>,
    out: &mut HashMap<String, LivewireClassNamespace>,
) {
    match node.kind() {
        "scoped_call_expression" => {
            if let Some((prefix, reg)) = classify_add_namespace(node, bytes, paths) {
                out.insert(prefix, reg);
            }
        }
        "member_call_expression" => {
            if let Some((prefix, reg)) =
                classify_registrar_call(node, bytes, paths, registrar_methods, file_namespace)
            {
                out.insert(prefix, reg);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, bytes, paths, registrar_methods, file_namespace, out);
    }
}

/// Classify one `Livewire::addNamespace(...)` call. Arguments may be
/// positional (`namespace, classNamespace, classPath` — Livewire's
/// signature order) or named in any order.
fn classify_add_namespace(
    node: tree_sitter::Node,
    bytes: &[u8],
    paths: &PathContext,
) -> Option<(String, LivewireClassNamespace)> {
    let scope = node.child_by_field_name("scope")?.utf8_text(bytes).ok()?;
    if scope != "Livewire" && !scope.ends_with("\\Livewire") {
        return None;
    }
    if node.child_by_field_name("name")?.utf8_text(bytes).ok()? != "addNamespace" {
        return None;
    }

    let args = collect_arguments(node, bytes, &["namespace", "classNamespace", "classPath"])?;
    let prefix = string_literal_text(*args.first()?.as_ref()?, bytes)?;
    // The class namespace literal contains escaped backslashes, which
    // tree-sitter-php splits into escape_sequence nodes — take the raw
    // quoted text instead of the (truncated) first string_content chunk.
    let class_namespace =
        unescape_php_namespace(&string_literal_raw(*args.get(1)?.as_ref()?, bytes)?);
    let class_path = contained_class_path(*args.get(2)?.as_ref()?, bytes, paths)?;

    Some((
        prefix,
        LivewireClassNamespace {
            class_namespace,
            class_path,
        },
    ))
}

/// Classify one `$this->{registrar}(<path>, '<prefix>')` wrapper call. The
/// class namespace is derived from the file's `namespace X\Providers;`
/// declaration as `X\Livewire`, reproducing the abstract-base-class rule.
fn classify_registrar_call(
    node: tree_sitter::Node,
    bytes: &[u8],
    paths: &PathContext,
    registrar_methods: &[String],
    file_namespace: Option<&str>,
) -> Option<(String, LivewireClassNamespace)> {
    if !is_this_receiver(node.child_by_field_name("object")?, bytes) {
        return None;
    }
    let method = node.child_by_field_name("name")?.utf8_text(bytes).ok()?;
    if !registrar_methods.iter().any(|m| m == method) {
        return None;
    }

    let args = collect_arguments(node, bytes, &["path", "prefix"])?;
    let class_path = contained_class_path(*args.first()?.as_ref()?, bytes, paths)?;
    let prefix = string_literal_text(*args.get(1)?.as_ref()?, bytes)?;
    if prefix.is_empty() {
        return None;
    }

    let class_namespace = format!("{}\\Livewire", module_root_namespace(file_namespace?));

    Some((
        prefix,
        LivewireClassNamespace {
            class_namespace,
            class_path,
        },
    ))
}

/// Resolve and CONTAIN a registration's class path, then canonicalize it.
///
/// The path argument is provider-source-derived — discovered data — and the
/// resolved value is consumed without further gating by the component
/// completion walk and by `try_namespaced_class`. Gating here, at the single
/// point where a registration is minted, covers both: a
/// `__DIR__.'/../../../..'` never enters the config, so nothing downstream
/// can walk or probe outside the project.
///
/// [`PathContext::gate_dir`] is the provider's OWNING MODULE directory
/// ([`crate::config::owning_module`]) and falls back to the project root for
/// an app-level provider. Two things follow, and both are the point:
///
/// - A module symlinked in from a composer path repository canonicalizes
///   OUTSIDE the project root, so gating the canonical path against the root
///   dropped its every Livewire registration — silently, with no diagnostic —
///   while [`crate::config::expand_module_dirs`] deliberately admits exactly
///   that layout. Gating LEXICALLY against the owning module, before
///   canonicalizing, is what `resolve_provider_class_file` already does for
///   the PSR-4 provider branch, and it keeps the symlinked module working.
/// - The gate gets STRICTER for a module provider, not looser: a
///   registration reaching into a sibling module or into bare `app/` is
///   inside the root but outside its own module, and is dropped.
///
/// Fail-closed via [`crate::path_containment::path_within_root_lexical`]: a
/// path that cannot be proven inside `gate_dir` yields no registration.
/// Canonicalization happens only after the gate passes, so the value handed
/// downstream still resolves symlinks as it always did.
fn contained_class_path(
    arg: tree_sitter::Node,
    bytes: &[u8],
    paths: &PathContext,
) -> Option<PathBuf> {
    let resolved = resolve_path_arg(arg, bytes, paths.provider_dir, paths.root)?;
    crate::path_containment::path_within_root_lexical(&resolved, paths.gate_dir)
        .then(|| resolved.canonicalize().unwrap_or(resolved))
}

/// Order a call's arguments by the given parameter names: positional
/// arguments fill slots left to right, named arguments land in their slot
/// regardless of position. An argument we don't understand — a named
/// argument for a parameter we don't track (`lazy: true`, or any parameter
/// a future Livewire adds), or a positional argument past the tracked
/// slots — skips THAT argument, never the whole call: one unknown flag must
/// not silently disable the namespace registration.
fn collect_arguments<'t>(
    call: tree_sitter::Node<'t>,
    bytes: &[u8],
    parameter_names: &[&str],
) -> Option<Vec<Option<tree_sitter::Node<'t>>>> {
    let args = call.child_by_field_name("arguments")?;
    let mut slots: Vec<Option<tree_sitter::Node>> = vec![None; parameter_names.len()];
    let mut next_positional = 0usize;

    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let value = argument_value(arg);
        match arg.child_by_field_name("name") {
            Some(label) => {
                let Some(label_text) = label.utf8_text(bytes).ok() else {
                    continue;
                };
                let Some(slot) = parameter_names.iter().position(|p| *p == label_text) else {
                    continue;
                };
                slots[slot] = value;
            }
            None => {
                // A positional argument consumes its slot even when its
                // value is unusable ($variable, an expression): skipping the
                // slot too would shift every LATER positional one place
                // left, silently pairing values with the wrong parameters.
                if next_positional < slots.len() {
                    slots[next_positional] = value;
                }
                next_positional += 1;
            }
        }
    }
    Some(slots)
}

/// The module's root namespace for a provider living in `X\Providers` (or a
/// sub-namespace of it): everything before the last `\Providers` segment.
/// A provider outside a `Providers` namespace keeps its full namespace.
fn module_root_namespace(file_namespace: &str) -> &str {
    if let Some(prefix) = file_namespace.strip_suffix("\\Providers") {
        return prefix;
    }
    if let Some(idx) = file_namespace.rfind("\\Providers\\") {
        return &file_namespace[..idx];
    }
    file_namespace
}

/// The file's `namespace …;` declaration, if any.
fn php_file_namespace(root_node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = root_node.walk();
    for child in root_node.named_children(&mut cursor) {
        if child.kind() == "namespace_definition" {
            if let Some(name) = child.child_by_field_name("name") {
                return name.utf8_text(bytes).ok().map(|s| s.to_string());
            }
        }
    }
    None
}

/// The raw inner text of a string literal node, quotes stripped but escape
/// sequences untouched. Unlike `string_literal_text` (which reads only the
/// first `string_content` chunk) this survives literals whose escape
/// sequences split the content into several nodes.
fn string_literal_raw(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let text = node.utf8_text(bytes).ok()?;
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| text.strip_prefix('"').and_then(|s| s.strip_suffix('"')))?;
    Some(inner.to_string())
}

/// PHP string literals escape backslashes (`'App\\Common\\UI'`); collapse
/// doubled backslashes to real namespace separators.
fn unescape_php_namespace(raw: &str) -> String {
    raw.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests;
