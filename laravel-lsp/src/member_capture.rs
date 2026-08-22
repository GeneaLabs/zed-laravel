//! M1 single-parse capture — orchestration.
//!
//! [`capture_member_context`] runs at PARSE time (in `pattern_indexer` and the
//! actor's `handle_get_patterns`) and compiles everything the whole-project
//! magic-member resolve passes need from a file's OWN source into an owned
//! [`MemberContextData`]: per-site receiver recipes (`member_resolver`), the
//! controller view-render plans + Volt front-matter surface + component member
//! names (`view_var_index`). The resolve passes then finish the cross-file half
//! against actor snapshots + memos, never re-reading or re-parsing the file.
//!
//! Returns `None` when there is nothing to resolve (icon Blade templates, plain
//! files) — a single tag byte on `ParsedPatternsData`, honoring the
//! zero-added-cost budget for pattern-free files. Only called for NON-vendor
//! files: the three build passes all skip `vendor/`, so capturing there would
//! be wasted memory (the incremental save path keeps a tree-based fallback for
//! the rare vendor save).

use std::path::Path;
use std::sync::Arc;

use tree_sitter::Tree;

use crate::query_chain::use_aliases::extract_use_aliases;
use crate::salsa_impl::{MemberAccessReferenceData, MemberContextData};

/// Compile the file's own-source resolution context. `php_tree` is the
/// full-file PHP parse for `.php` files (reused — no second tree-sitter pass);
/// `None` for Blade (parsing a `.blade.php` as PHP is pathologically slow, so
/// its receivers compile from per-site `<?php {receiver};` snippets instead).
/// `is_volt` is the already-captured `source_contains_volt_signature`.
pub fn capture_member_context(
    path: &Path,
    text: &str,
    php_tree: Option<&Tree>,
    refs: &[Arc<MemberAccessReferenceData>],
    is_volt: bool,
) -> Option<MemberContextData> {
    let is_blade = path.to_string_lossy().ends_with(".blade.php");

    let (aliases, sites, view_renders) = if let Some(tree) = php_tree {
        // Full-file PHP: aliases + per-site recipes + view-render plans all off
        // the one shared parse.
        let aliases = extract_use_aliases(tree, text);
        let sites = crate::member_resolver::capture_php_sites(text, tree, refs, &aliases);
        let view_renders = crate::view_var_index::capture_render_plans(text, tree, &aliases);
        (aliases, sites, view_renders)
    } else {
        // Blade: no full-file PHP tree. Each site's chain receiver compiles from
        // its own `<?php {receiver};` snippet (bare `$var` / `$this->prop`
        // receivers short-circuit to `Unresolvable` — they type from the
        // view-var / Volt-prop maps by text, never the recipe). A Blade file has
        // no controller view() renders.
        //
        // It DOES have file-level `use` aliases, just not as PHP statements —
        // they're written `@use('App\Support\Foo')`, which the PHP parser never
        // sees. Reading them here is what lets a short class name in a `@php`
        // block or `{{ }}` echo resolve to its FQCN instead of falling back to
        // a basename guess.
        let aliases = crate::query_chain::use_aliases::blade_use_aliases(text);
        let sites = refs
            .iter()
            .map(|m| crate::member_resolver::compile_blade_site(m.receiver.trim()))
            .collect();
        (aliases, sites, Vec::new())
    };

    // Volt front-matter surface — only for a Volt single-file component.
    let volt_surface = if is_blade && is_volt {
        crate::view_var_index::capture_volt_surface(text)
    } else {
        None
    };

    // Component `$this->member` identity — captured only when the file actually
    // reads `$this->…`. This gate is what keeps the ~58k published icon Blade
    // templates (no member refs) from each paying a `mfc_sibling` filesystem
    // stat: resolve_component_member_accesses yields entries ONLY for `$this`
    // receivers, so with none present there is nothing to capture.
    let has_this = refs.iter().any(|m| m.receiver.trim() == "$this");
    let component = if has_this {
        crate::view_var_index::capture_component(path, text, is_volt)
    } else {
        None
    };

    // Deliberately NOT gated on `aliases`: an import only ever resolves a
    // member access or chain receiver, and both live in `sites`. A Blade file
    // carrying `@use` but no member refs has nothing for those aliases to
    // resolve, so allocating a context for it would be pure cost — exactly what
    // this gate exists to avoid on large published template sets.
    if sites.is_empty() && view_renders.is_empty() && volt_surface.is_none() && component.is_none()
    {
        return None;
    }
    Some(MemberContextData {
        aliases,
        sites,
        view_renders,
        volt_surface,
        component,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_php;
    use std::path::Path;

    #[test]
    fn pattern_free_php_captures_nothing() {
        // No member access, no view() render, not a component → `None`, so the
        // field costs a single tag byte.
        let src = "<?php\nnamespace App;\nclass Widget { public function noop() {} }\n";
        let tree = parse_php(src).unwrap();
        let ctx = capture_member_context(
            Path::new("/proj/app/Widget.php"),
            src,
            Some(&tree),
            &[],
            false,
        );
        assert!(ctx.is_none(), "a pattern-free .php must capture no context");
    }

    #[test]
    fn icon_blade_template_captures_nothing() {
        // The ~58k published icon templates: no member refs, not Volt, no
        // sibling `.php`. This is the zero-cost budget's key case.
        let src = "<svg viewBox=\"0 0 24 24\"><path d=\"M0 0h24v24H0z\"/></svg>\n";
        let ctx = capture_member_context(
            Path::new("/proj/resources/views/icons/star.blade.php"),
            src,
            None,
            &[],
            false,
        );
        assert!(
            ctx.is_none(),
            "an icon Blade template must capture no context"
        );
    }

    #[test]
    fn controller_with_render_captures_context() {
        // A controller that only PASSES a view var (no `->member` of its own)
        // still captures — its render plan is what Blade typing needs.
        let src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C { public function show(User $u) { return view('users.show', ['user' => $u]); } }
"#;
        let tree = parse_php(src).unwrap();
        let ctx =
            capture_member_context(Path::new("/proj/app/C.php"), src, Some(&tree), &[], false)
                .expect("controller with a render must capture context");
        assert_eq!(ctx.view_renders.len(), 1);
        assert_eq!(ctx.view_renders[0].view_name, "users.show");
    }
}
