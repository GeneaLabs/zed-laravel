//! Tests for facade resolution: token → facade FQCN (imported, inline
//! fully-qualified, and global-alias forms, plus the bare-namespaced `None`
//! case) and facade FQCN → `getFacadeAccessor()` binding key.

use super::*;
use crate::query_chain::use_aliases::{extract_use_aliases, UseAliases};
use std::collections::HashMap;

/// Parse a PHP file's `use` imports into the alias map the resolver consumes.
fn aliases(source: &str) -> UseAliases {
    let tree = crate::parser::parse_php(source).expect("parse");
    extract_use_aliases(&tree, source)
}

// ---- alias map seed ----

#[test]
fn default_aliases_map_token_to_facade_fqcn() {
    let map = default_facade_aliases();
    assert_eq!(
        map.get("Auth").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
    assert_eq!(
        map.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
    assert_eq!(
        map.get("Storage").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Storage")
    );
}

// ---- root-namespace alias form (we respond) ----

#[test]
fn root_namespace_alias_resolves() {
    // A leading-`\` token hits the global alias regardless of the file's
    // namespace — pass `is_namespaced = true` to prove `\` wins over the
    // current-namespace rule.
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn("\\Auth", &no_imports, &map, true).as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
    assert_eq!(
        resolve_facade_fqcn("\\DB", &no_imports, &map, true).as_deref(),
        Some("Illuminate\\Support\\Facades\\DB")
    );
}

#[test]
fn bare_token_in_non_namespaced_file_resolves() {
    // `Auth::check()` in a file with NO `namespace` declaration and no `use`
    // import: PHP resolves the bare class name in the global namespace, where
    // the alias lives — so the global facade alias applies and we respond.
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn("Auth", &no_imports, &map, false).as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn bare_token_in_namespaced_file_is_none() {
    // `Auth::check()` inside `namespace App\Http;` with no `use` import: PHP's
    // class-name rule resolves this against the CURRENT namespace
    // (`App\Http\Auth`), NOT the global `\Auth` alias. We must stay silent — a
    // goto to the facade would be wrong, and Intelephense's "undefined
    // App\Http\Auth" squiggle correctly stands.
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(resolve_facade_fqcn("Auth", &no_imports, &map, true), None);
}

#[test]
fn alias_match_is_case_insensitive() {
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    // The alias machinery is case-insensitive; `\db` should still resolve.
    assert_eq!(
        resolve_facade_fqcn("\\db", &no_imports, &map, true).as_deref(),
        Some("Illuminate\\Support\\Facades\\DB")
    );
}

#[test]
fn user_alias_merged_over_defaults() {
    // A package facade aliased in config/app.php: `'Flux' => Flux\Flux::class`.
    let mut map = default_facade_aliases();
    map.insert("Flux".to_string(), "Flux\\Flux".to_string());
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn("\\Flux", &no_imports, &map, true).as_deref(),
        Some("Flux\\Flux")
    );
}

// ---- imported / fully-qualified form (we resolve to the real impl) ----

#[test]
fn imported_facade_resolves() {
    // `use Illuminate\Support\Facades\Auth; Auth::check();` — the import resolves
    // into the Facades namespace, so this IS the facade FQCN. We own this form
    // and resolve to the real impl; Zed shows a 2-entry goto multibuffer
    // (Intelephense's @method docblock + our impl) for documented methods,
    // accepted (zed#24100).
    let imports = aliases("<?php\nuse Illuminate\\Support\\Facades\\Auth;\n");
    let map = default_facade_aliases();
    assert_eq!(
        resolve_facade_fqcn("Auth", &imports, &map, false).as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn imported_facade_in_namespaced_file_resolves() {
    // The import wins over the bare-namespaced rule: `use …\Facades\Auth;` inside
    // a namespaced file still resolves to the facade FQCN, because the Facades-
    // namespace check runs before the bare-namespaced `None` rule.
    let imports = aliases("<?php\nnamespace App\\Http;\nuse Illuminate\\Support\\Facades\\Auth;\n");
    let map = default_facade_aliases();
    assert_eq!(
        resolve_facade_fqcn("Auth", &imports, &map, true).as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn imported_aliased_facade_resolves() {
    // `use Illuminate\Support\Facades\Auth as Authentication;` — the alias still
    // resolves into the Facades namespace, so it IS the facade FQCN.
    let imports = aliases("<?php\nuse Illuminate\\Support\\Facades\\Auth as Authentication;\n");
    let map = default_facade_aliases();
    assert_eq!(
        resolve_facade_fqcn("Authentication", &imports, &map, false).as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn fully_qualified_facade_resolves() {
    // `\Illuminate\Support\Facades\Auth::check()` written inline — resolves into
    // the Facades namespace with no import needed, so it IS the facade FQCN.
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn(
            "\\Illuminate\\Support\\Facades\\Auth",
            &no_imports,
            &map,
            false
        )
        .as_deref(),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

// ---- non-facades ----

#[test]
fn unknown_token_is_none() {
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn("\\SomeRandomClass", &no_imports, &map, false),
        None
    );
}

#[test]
fn namespaced_class_reference_is_none() {
    // `App\Services\Auth::run()` — a real class that happens to end in `Auth`
    // is not a root-namespace facade alias.
    let map = default_facade_aliases();
    let no_imports = HashMap::new();
    assert_eq!(
        resolve_facade_fqcn("App\\Services\\Auth", &no_imports, &map, false),
        None
    );
}

// ---- getFacadeAccessor parsing ----

#[test]
fn parses_block_body_accessor() {
    let src = r#"<?php
namespace Illuminate\Support\Facades;
class Auth extends Facade {
    protected static function getFacadeAccessor()
    {
        return 'auth';
    }
}
"#;
    assert_eq!(parse_facade_accessor(src).as_deref(), Some("auth"));
}

#[test]
fn parses_typed_return_accessor() {
    let src = r#"<?php
namespace Illuminate\Support\Facades;
class DB extends Facade {
    protected static function getFacadeAccessor(): string
    {
        return 'db';
    }
}
"#;
    assert_eq!(parse_facade_accessor(src).as_deref(), Some("db"));
}

#[test]
fn non_string_accessor_is_none() {
    // A facade whose accessor returns a `::class` constant isn't a string
    // binding key — the binding lookup keys on strings.
    let src = r#"<?php
namespace Illuminate\Support\Facades;
class Date extends Facade {
    protected static function getFacadeAccessor()
    {
        return DateFactory::class;
    }
}
"#;
    assert_eq!(parse_facade_accessor(src), None);
}

#[test]
fn missing_accessor_method_is_none() {
    let src = r#"<?php
namespace Illuminate\Support\Facades;
class Weird extends Facade {}
"#;
    assert_eq!(parse_facade_accessor(src), None);
}
