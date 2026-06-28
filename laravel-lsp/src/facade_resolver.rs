//! Facade receiver resolution — every facade call form, to the real impl.
//!
//! laravel-lsp resolves a facade static call to the **real implementation** the
//! call forwards to, for all three forms:
//!
//! 1. **Imported / inline fully-qualified** — `use
//!    Illuminate\Support\Facades\Auth; Auth::check();` or
//!    `\Illuminate\Support\Facades\Auth::check()`. Intelephense also resolves
//!    this form, but only as well as the framework's committed `@method static`
//!    docblocks (e.g. `Auth` ships 59 `@method` tags, zero `@property`, no
//!    macros), and its goto lands on the *docblock*, not the implementation. We
//!    resolve to the concrete impl, accepting that Zed shows a 2-entry goto
//!    multibuffer (Intelephense's docblock + our real impl) for the documented
//!    methods (zed#24100, an accepted trade).
//! 2. **Root-namespace alias** — `\Auth::check()`, `\DB::beginTransaction()`.
//!    The global aliases registered by `Facade::defaultAliases()` make the
//!    leading-`\` token resolve at runtime, but there is no `use` import for
//!    Intelephense to follow, so the call resolves nowhere without us.
//! 3. **Facade-returning helper** — `auth()->check()`, `app('db')->…`. Handled
//!    by the container / helper receiver paths in `member_resolver`; this
//!    module supplies the accessor → binding bridge they share.
//!
//! ## Resolution model (walk the chain, not the `@see` shortcut)
//!
//! A facade is a thin static proxy: `getFacadeAccessor()` returns a container
//! binding key, and the bound concrete is the real implementation the calls
//! forward to. We resolve the same way Laravel does at runtime:
//!
//! ```text
//! token (Auth)
//!   → facade FQCN (Illuminate\Support\Facades\Auth)   [alias map]
//!   → accessor key ('auth')                            [getFacadeAccessor()]
//!   → concrete FQCN (Illuminate\Auth\AuthManager)      [binding registry]
//! ```
//!
//! This module owns the first two hops. The third — accessor key → concrete
//! class — is the existing
//! [`ClassFileResolver::binding_concrete`](crate::member_resolver::ClassFileResolver::binding_concrete)
//! seam, fed by the provider-binding registry (`extract_provider_bindings`),
//! which already sees vendor framework providers at priority 0. The
//! `@see` docblock tag is intentionally **not** the primary path: it is only a
//! last-resort fallback when both the parsed accessor and the binding lookup
//! come up empty.

use crate::query_chain::use_aliases::resolve_class_name;
use crate::query_chain::use_aliases::UseAliases;
use std::collections::HashMap;
use std::path::Path;

/// The `Illuminate\Support\Facades` namespace every built-in facade lives in.
/// Used both to seed the default alias map and as the marker that identifies a
/// facade FQCN: a receiver that resolves into this namespace (via a `use` import
/// or written inline fully-qualified) IS a facade, and we resolve it to the real
/// implementation.
pub const FACADE_NAMESPACE: &str = "Illuminate\\Support\\Facades";

/// Laravel's built-in facade tokens — the local aliases `Facade::defaultAliases()`
/// registers globally. Maps the bare token (`Auth`) to the facade short class
/// name; the FQCN is `FACADE_NAMESPACE\<short>`. This is the seed for the
/// root-namespace alias form, merged at the Salsa layer with any user aliases
/// from `config/app.php` / `bootstrap/app.php`.
///
/// The list mirrors `Illuminate\Foundation\Application::registerCoreContainerAliases`
/// /`Facade::defaultAliases()` for the facades that proxy a container binding.
const DEFAULT_FACADES: &[&str] = &[
    "App",
    "Artisan",
    "Auth",
    "Blade",
    "Broadcast",
    "Bus",
    "Cache",
    "Config",
    "Context",
    "Cookie",
    "Crypt",
    "Date",
    "DB",
    "Event",
    "File",
    "Gate",
    "Hash",
    "Http",
    "Lang",
    "Log",
    "Mail",
    "Notification",
    "Password",
    "Pipeline",
    "Process",
    "Queue",
    "RateLimiter",
    "Redirect",
    "Redis",
    "Request",
    "Response",
    "Route",
    "Schema",
    "Session",
    "Storage",
    "URL",
    "Validator",
    "View",
    "Vite",
];

/// Default `getFacadeAccessor()` keys for the built-in facades whose accessor is
/// a bare string binding key — the fast path that avoids parsing the facade
/// source, and the fallback when the source can't be read (vendor absent). Only
/// facades that proxy a *string-keyed* container binding are listed; facades
/// whose accessor returns a `::class` constant (e.g. `Date`, `Pipeline`) are
/// omitted and fall through to source parsing.
const DEFAULT_ACCESSORS: &[(&str, &str)] = &[
    ("App", "app"),
    ("Artisan", "artisan"),
    ("Auth", "auth"),
    ("Blade", "blade.compiler"),
    ("Broadcast", "Illuminate\\Contracts\\Broadcasting\\Factory"),
    ("Cache", "cache"),
    ("Config", "config"),
    ("Cookie", "cookie"),
    ("Crypt", "encrypter"),
    ("DB", "db"),
    ("Event", "events"),
    ("File", "files"),
    ("Gate", "Illuminate\\Contracts\\Auth\\Access\\Gate"),
    ("Hash", "hash"),
    ("Lang", "translator"),
    ("Log", "log"),
    ("Mail", "mailer"),
    ("Password", "auth.password"),
    ("Queue", "queue"),
    ("Redirect", "redirect"),
    ("Redis", "redis"),
    ("Request", "request"),
    ("Route", "router"),
    ("Schema", "db.schema"),
    ("Session", "session"),
    ("Storage", "filesystem"),
    ("URL", "url"),
    ("View", "view"),
];

/// Seed the default facade alias map: token → facade FQCN. The Salsa layer
/// merges user-defined aliases (`config/app.php` `aliases`, `bootstrap/app.php`)
/// on top — a user alias for an existing token overrides the default, and a new
/// token is added.
pub fn default_facade_aliases() -> HashMap<String, String> {
    DEFAULT_FACADES
        .iter()
        .map(|short| ((*short).to_string(), format!("{FACADE_NAMESPACE}\\{short}")))
        .collect()
}

/// Resolve a static-call receiver token to its facade FQCN, covering all three
/// facade forms and honoring PHP's namespace-resolution rule for class names.
///
/// `receiver` is the scope text exactly as written (`\Auth`, `Auth`,
/// `\Illuminate\Support\Facades\Auth`). `aliases` is the file's `use`-import
/// map; `facade_aliases` is the seeded-plus-merged token → facade-FQCN map.
/// `is_namespaced` is whether the *calling file* has a `namespace` declaration —
/// determined at the call site from a `namespace_definition` node.
///
/// ## Resolution order
///
/// 1. **Facades-namespace form first** — if the receiver resolves into
///    `Illuminate\Support\Facades\*` (through a `use` import, or written inline
///    fully-qualified `\Illuminate\Support\Facades\Auth`), that IS the facade
///    FQCN — return it. This is checked *before* the bare-namespaced rule so an
///    imported bare `Auth` resolves even inside a namespaced file (the import
///    wins). We own this form now, resolving to the real impl rather than staying
///    silent — see the module docs for the Intelephense rationale.
/// 2. **Global alias form** — a leading-`\` token (`\Auth`), or a bare token
///    (`Auth`) in a **non-namespaced** file: PHP resolves these in the global
///    namespace where `Facade::defaultAliases()` lives, so the facade alias
///    applies — match it against `facade_aliases` and return its FQCN.
///
/// ## PHP namespace-resolution rule (why `is_namespaced` matters)
///
/// For a **class** name, an unqualified token with no matching `use` import
/// resolves against the *current* namespace — it does **not** fall back to the
/// global namespace (that fallback exists for functions and constants only). So
/// bare `Auth::check()` inside `namespace App\Http;` means `App\Http\Auth`, not
/// the global `\Auth` facade alias.
///
/// Returns `None` when:
/// - it's a **bare token in a namespaced file** with no facade import (PHP
///   resolves it against the current namespace, not the global alias — we must
///   not emit a wrong goto; Intelephense's "undefined `CurrentNs\Auth`" squiggle
///   correctly stands), or
/// - the token contains a `\` separator but is not in the Facades namespace
///   (`App\Services\Auth` — a real class reference, not a facade alias), or
/// - the token isn't a known facade at all.
pub fn resolve_facade_fqcn(
    receiver: &str,
    aliases: &UseAliases,
    facade_aliases: &HashMap<String, String>,
    is_namespaced: bool,
) -> Option<String> {
    // Distinguish a root-`\` receiver from a bare one BEFORE `resolve_class_name`
    // strips the leading `\` and collapses the two cases. A leading `\` forces
    // global resolution regardless of the file's namespace.
    let is_root_qualified = receiver.starts_with('\\');

    // A receiver that resolves into `Illuminate\Support\Facades\*` IS a facade —
    // whether through a `use` import (`use …\Facades\Auth; Auth::…`) or written
    // inline fully-qualified (`\Illuminate\Support\Facades\Auth::…`). Return it as
    // the facade FQCN so it flows on to accessor → binding → real impl. We now own
    // this form too: Intelephense only resolves it via the framework's committed
    // `@method` docblocks (and goto lands on the docblock, not the impl), so we
    // resolve to the real implementation and accept Zed's 2-entry goto multibuffer
    // for documented methods (zed#24100). `resolve_class_name` expands the file's
    // `use` aliases and strips a leading `\`, so an imported `Auth` becomes the
    // facade FQCN here. This check is FIRST so an imported bare token resolves even
    // in a namespaced file (the import wins over the bare-namespaced rule below).
    let via_use = resolve_class_name(receiver, aliases);
    if via_use.starts_with(FACADE_NAMESPACE) {
        return Some(via_use);
    }

    // Otherwise it's a bare / root-`\` token relying on the global alias. Match
    // it (case-insensitively, like the rest of the alias machinery) against our
    // facade map — the leading `\` is already stripped by `resolve_class_name`,
    // and `via_use` is the unresolved token when no import matched.
    let token = via_use.as_str();
    // A token that itself contains a namespace separator is a real class
    // reference, not a facade alias (`App\Foo::bar()`), so it can't be a
    // root-namespace facade alias — bail.
    if token.contains('\\') {
        return None;
    }

    // PHP class-name rule: a bare (non-`\`-qualified) token in a namespaced file
    // with no matching `use` import resolves against the current namespace, NOT
    // the global alias. Only honor the global alias for a leading-`\` token or a
    // file with no namespace declaration.
    if !is_root_qualified && is_namespaced {
        return None;
    }

    facade_aliases
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(token))
        .map(|(_, fqcn)| fqcn.clone())
}

/// The container binding key a facade proxies — its `getFacadeAccessor()`
/// return value.
///
/// Fast path: the [`DEFAULT_ACCESSORS`] table for the built-in facades.
/// Otherwise parse the facade source (located via `class_locator`) and read the
/// string returned by `getFacadeAccessor()`. Returns `None` when the facade
/// isn't built-in and its source can't be read or its accessor isn't a plain
/// string (e.g. it returns a `::class` constant — the caller's binding lookup
/// keys on the string form only).
pub fn facade_accessor(facade_fqcn: &str, root: &Path) -> Option<String> {
    let short = facade_fqcn.rsplit('\\').next().unwrap_or(facade_fqcn);
    if let Some((_, accessor)) = DEFAULT_ACCESSORS
        .iter()
        .find(|(token, _)| token.eq_ignore_ascii_case(short))
    {
        return Some((*accessor).to_string());
    }

    let file = crate::class_locator::find_php_class_file_in_app_or_vendor(facade_fqcn, root)?;
    let source = std::fs::read_to_string(&file).ok()?;
    parse_facade_accessor(&source)
}

/// Read the string a facade's `getFacadeAccessor()` returns from source.
///
/// Handles the canonical forms:
/// - `protected static function getFacadeAccessor() { return 'auth'; }`
/// - `protected static function getFacadeAccessor(): string { return 'db'; }`
/// - an arrow-style single-return body.
///
/// Returns `None` when the method is absent, returns a non-string (a `::class`
/// constant — the binding lookup keys on string accessors), or returns a
/// computed expression we can't statically read.
pub fn parse_facade_accessor(source: &str) -> Option<String> {
    let tree = crate::parser::parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let method = find_facade_accessor_method(tree.root_node(), bytes)?;
    let body = method.child_by_field_name("body")?;
    let ret = single_return_expr(body)?;
    string_literal_value(ret, bytes)
}

/// Locate the `getFacadeAccessor` `method_declaration` node anywhere in the
/// subtree (it's nested under `class_declaration` → `declaration_list`).
fn find_facade_accessor_method<'t>(
    root: tree_sitter::Node<'t>,
    bytes: &[u8],
) -> Option<tree_sitter::Node<'t>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "method_declaration" {
            let is_accessor = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                == Some("getFacadeAccessor");
            if is_accessor {
                return Some(node);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// The expression of a body's sole `return <expr>;`, or `None` for zero /
/// multiple returns. Mirrors `salsa_impl::single_return_expr`; kept local so
/// this module stays self-contained.
fn single_return_expr(body: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut found: Option<tree_sitter::Node> = None;
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == "return_statement" {
            if found.is_some() {
                return None;
            }
            found = node.named_child(0);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // A nested closure's returns belong to it, not this body.
            if matches!(child.kind(), "arrow_function" | "anonymous_function") {
                continue;
            }
            stack.push(child);
        }
    }
    found
}

/// The content of a single/double-quoted string literal, descending to the
/// `string_content` child (matching the rest of the LSP); `None` for a
/// non-string node.
fn string_literal_value(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
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
            .trim_matches(['\'', '"'])
            .to_string(),
    )
}

#[cfg(test)]
mod tests;
