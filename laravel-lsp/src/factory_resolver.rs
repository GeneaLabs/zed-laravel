//! Model → factory resolution (issue #30, item 3).
//!
//! `User::factory()` is `HasFactory::factory()` at runtime — a vendor trait
//! method no PHP LSP can see through without ide-helper output. This module
//! answers "which factory class does this model's `factory()` return?" from
//! project structure alone, mirroring Laravel's own resolution order:
//!
//! 1. An explicit `newFactory()` override on the model names the factory
//!    directly (`UserFactory::new()`, `new UserFactory`, `UserFactory::class`).
//! 2. Convention (`Illuminate\Database\Eloquent\Factories\Factory::resolveFactoryName`):
//!    the model's FQCN relative to `App\Models\` (or `App\`) maps into
//!    `Database\Factories\…Factory` — `App\Models\Admin\User` →
//!    `Database\Factories\Admin\UserFactory`. Where that namespace lives on
//!    disk is composer's business: the conventional candidate only resolves
//!    when the [`ClassFileResolver`] (PSR-4 aware, `autoload-dev` included)
//!    actually locates its file, so a project that remaps the factories path
//!    in `composer.json` still resolves and a model with no factory yields
//!    `None` instead of a dead goto target.

use crate::laravel_introspector::chain::ClassView;
use crate::member_resolver::ClassFileResolver;
use crate::parser::parse_php;
use crate::query_chain::use_aliases::extract_use_aliases;

/// The factory FQCN `view`'s `factory()` call resolves to, or `None` when the
/// model has no override and no conventional factory file exists.
pub fn factory_fqcn_for_model(
    view: &ClassView,
    resolver: &impl ClassFileResolver,
) -> Option<String> {
    // 1. `newFactory()` override — declared anywhere in the hierarchy; parse
    //    the DECLARING file (a parent/trait may carry it) for the class it
    //    names. Only models that actually declare one pay the file read.
    if let Some(m) = view
        .all_methods
        .iter()
        .find(|m| m.value.name == "newFactory")
    {
        if let Some(file) = resolver.class_file(&m.source_class) {
            if let Some(fqcn) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|src| factory_from_new_factory(&src))
            {
                // The declared override is authoritative — no convention
                // fallback — but it honors the same gate as the convention
                // branch: a named class with no file on disk is a dead
                // target (goto AND hover), so it yields `None`.
                return resolver.class_file(&fqcn).map(|_| fqcn);
            }
        }
    }
    // 2. Convention, gated on the factory class actually existing.
    let candidate = conventional_factory_fqcn(&view.fqcn);
    resolver.class_file(&candidate).map(|_| candidate)
}

/// Laravel's conventional factory name for a model FQCN: the model path
/// relative to the application namespace (`App\Models\` first, bare `App\`
/// second, the basename as a last resort) prefixed with `Database\Factories\`
/// and suffixed `Factory`.
pub fn conventional_factory_fqcn(model_fqcn: &str) -> String {
    let relative = model_fqcn
        .strip_prefix("App\\Models\\")
        .or_else(|| model_fqcn.strip_prefix("App\\"))
        .unwrap_or_else(|| model_fqcn.rsplit('\\').next().unwrap_or(model_fqcn));
    format!("Database\\Factories\\{relative}Factory")
}

/// Extract the factory FQCN named inside a `newFactory()` method body:
/// `UserFactory::new()`, `UserFactory::class`, or `new UserFactory(...)`.
/// The short name resolves through the file's own `use` aliases + namespace.
fn factory_from_new_factory(src: &str) -> Option<String> {
    let tree = parse_php(src).ok()?;
    let bytes = src.as_bytes();
    let aliases = extract_use_aliases(&tree, src);
    let namespace = file_namespace(tree.root_node(), bytes);

    let method = find_method(tree.root_node(), bytes, "newFactory")?;
    let raw = first_class_token(method, bytes)?;
    Some(
        crate::laravel_introspector::model_metadata::resolve_to_fqcn(
            &raw,
            namespace.as_deref(),
            &aliases,
        ),
    )
}

/// The file's `namespace X;` declaration, if any.
fn file_namespace(root: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "namespace_definition" {
            return child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(str::to_string);
        }
    }
    None
}

/// Depth-first search for a `method_declaration` named `name`.
fn find_method<'t>(
    node: tree_sitter::Node<'t>,
    bytes: &[u8],
    name: &str,
) -> Option<tree_sitter::Node<'t>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "method_declaration"
            && n.child_by_field_name("name")
                .and_then(|c| c.utf8_text(bytes).ok())
                == Some(name)
        {
            return Some(n);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// The first class name referenced inside `node` — in document order — as
/// `X::…` (scoped call / `::class` constant) or `new X(…)`.
fn first_class_token(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let class_node = match n.kind() {
            "scoped_call_expression" | "class_constant_access_expression" => {
                n.child_by_field_name("scope").or_else(|| n.child(0))
            }
            "object_creation_expression" => n
                .named_children(&mut n.walk())
                .find(|c| matches!(c.kind(), "name" | "qualified_name")),
            _ => None,
        };
        if let Some(c) = class_node {
            if matches!(c.kind(), "name" | "qualified_name") {
                if let Ok(text) = c.utf8_text(bytes) {
                    return Some(text.to_string());
                }
            }
        }
        // Push children REVERSED so the LIFO stack pops siblings
        // left-to-right — a plain forward push would visit them in
        // reverse and return the LAST reference for multi-branch bodies.
        let mut cursor = n.walk();
        let children: Vec<_> = n.children(&mut cursor).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_name_strips_app_models() {
        assert_eq!(
            conventional_factory_fqcn("App\\Models\\User"),
            "Database\\Factories\\UserFactory"
        );
    }

    #[test]
    fn conventional_name_keeps_model_subdirectories() {
        assert_eq!(
            conventional_factory_fqcn("App\\Models\\Admin\\User"),
            "Database\\Factories\\Admin\\UserFactory"
        );
    }

    #[test]
    fn conventional_name_handles_bare_app_namespace() {
        assert_eq!(
            conventional_factory_fqcn("App\\User"),
            "Database\\Factories\\UserFactory"
        );
    }

    #[test]
    fn conventional_name_falls_back_to_basename() {
        assert_eq!(
            conventional_factory_fqcn("Modules\\Blog\\Models\\Post"),
            "Database\\Factories\\PostFactory"
        );
    }

    #[test]
    fn new_factory_static_new_resolves_through_aliases() {
        let src = r#"<?php
namespace App\Models;
use Database\Factories\Custom\UserFactory;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected static function newFactory() { return UserFactory::new(); }
}
"#;
        assert_eq!(
            factory_from_new_factory(src).as_deref(),
            Some("Database\\Factories\\Custom\\UserFactory")
        );
    }

    #[test]
    fn new_factory_object_creation_resolves() {
        let src = r#"<?php
namespace App\Models;
use Database\Factories\UserFactory;
class User {
    protected static function newFactory() { return new UserFactory(); }
}
"#;
        assert_eq!(
            factory_from_new_factory(src).as_deref(),
            Some("Database\\Factories\\UserFactory")
        );
    }

    #[test]
    fn no_new_factory_method_yields_none() {
        let src = "<?php namespace App\\Models; class User {}";
        assert_eq!(factory_from_new_factory(src), None);
    }

    #[test]
    fn new_factory_multi_reference_body_yields_first_in_document_order() {
        // A conditional override references two factories; the FIRST in
        // source order wins (regression: the DFS used to pop siblings
        // right-to-left and returned the last).
        let src = r#"<?php
namespace App\Models;
use Database\Factories\FirstFactory;
use Database\Factories\SecondFactory;
class User {
    protected static function newFactory() {
        if (app()->environment('testing')) {
            return FirstFactory::new();
        }
        return SecondFactory::new();
    }
}
"#;
        assert_eq!(
            factory_from_new_factory(src).as_deref(),
            Some("Database\\Factories\\FirstFactory")
        );
    }
}
