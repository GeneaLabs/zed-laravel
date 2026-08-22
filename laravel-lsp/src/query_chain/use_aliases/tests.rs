use super::*;
use crate::parser::parse_php;

fn aliases_for(src: &str) -> UseAliases {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    extract_use_aliases(&tree, &wrapped)
}

#[allow(dead_code)]
fn dump_tree(src: &str) {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    fn walk(n: tree_sitter::Node, src: &str, depth: usize) {
        let indent = "  ".repeat(depth);
        let text = if n.child_count() == 0 {
            format!(" :: {:?}", &src[n.start_byte()..n.end_byte()])
        } else {
            String::new()
        };
        eprintln!("{}{}{}", indent, n.kind(), text);
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            walk(ch, src, depth + 1);
        }
    }
    walk(tree.root_node(), &wrapped, 0);
}

#[test]
fn dump_simple_use() {
    // Print the tree for diagnostic purposes — useful when AST shape
    // changes break the alias parser.
    dump_tree("use Illuminate\\Support\\Facades\\DB as Database;");
}

#[test]
fn dump_grouped_use() {
    dump_tree("use App\\Models\\{User, Post as P};");
}

#[test]
fn dump_function_use() {
    dump_tree("use function foo\\bar\\baz;");
}

#[test]
fn no_use_statements_returns_empty() {
    let aliases = aliases_for("DB::table('users');");
    assert!(aliases.is_empty());
}

#[test]
fn flat_use_no_alias_uses_last_segment() {
    let aliases = aliases_for("use Illuminate\\Support\\Facades\\DB;");
    assert_eq!(
        aliases.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB"),
        "aliases: {:?}",
        aliases
    );
}

#[test]
fn flat_use_with_as_alias() {
    let aliases = aliases_for("use Illuminate\\Support\\Facades\\DB as Database;");
    assert_eq!(
        aliases.get("Database").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
    assert!(
        !aliases.contains_key("DB"),
        "aliased import shouldn't also bind the bare name"
    );
}

#[test]
fn multiple_independent_imports() {
    let src = r#"use Illuminate\Support\Facades\DB;
use App\Models\User as MyUser;
use App\Models\Post;"#;
    let aliases = aliases_for(src);
    assert_eq!(
        aliases.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
    assert_eq!(
        aliases.get("MyUser").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert_eq!(
        aliases.get("Post").map(String::as_str),
        Some("App\\Models\\Post")
    );
}

#[test]
fn grouped_use_distributes_prefix() {
    let src = r#"use App\Models\{User, Post as P, Comment};"#;
    let aliases = aliases_for(src);
    assert_eq!(
        aliases.get("User").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert_eq!(
        aliases.get("P").map(String::as_str),
        Some("App\\Models\\Post")
    );
    assert_eq!(
        aliases.get("Comment").map(String::as_str),
        Some("App\\Models\\Comment")
    );
}

#[test]
fn function_use_is_ignored() {
    // `use function foo;` doesn't bind a class — chains never receive
    // functions, so we don't track them.
    let src = "use function foo\\bar\\baz;";
    let aliases = aliases_for(src);
    assert!(aliases.is_empty(), "got {:?}", aliases);
}

#[test]
fn const_use_is_ignored() {
    let src = "use const FOO\\BAR;";
    let aliases = aliases_for(src);
    assert!(aliases.is_empty(), "got {:?}", aliases);
}

// ---- resolve_class_name -------------------------------------------------

#[test]
fn resolve_with_no_aliases_returns_input_unchanged() {
    let aliases = UseAliases::new();
    assert_eq!(resolve_class_name("DB", &aliases), "DB");
    assert_eq!(resolve_class_name("\\DB", &aliases), "DB");
    assert_eq!(resolve_class_name("App\\Foo", &aliases), "App\\Foo");
}

#[test]
fn resolve_replaces_leading_segment_with_aliased_fqcn() {
    let mut aliases = UseAliases::new();
    aliases.insert(
        "Database".to_string(),
        "Illuminate\\Support\\Facades\\DB".to_string(),
    );
    assert_eq!(
        resolve_class_name("Database", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}

#[test]
fn resolve_handles_namespaced_alias_use() {
    // `use App\Models as M; M\User::query()` — the `M` segment resolves to
    // `App\Models` and the rest of the path tacks on. (Rare in practice but
    // legal PHP.)
    let mut aliases = UseAliases::new();
    aliases.insert("M".to_string(), "App\\Models".to_string());
    assert_eq!(resolve_class_name("M\\User", &aliases), "App\\Models\\User");
}

#[test]
fn resolve_is_case_insensitive_on_alias_head() {
    // PHP class names are case-insensitive. `db::table()` should resolve
    // via the `DB` alias.
    let mut aliases = UseAliases::new();
    aliases.insert(
        "DB".to_string(),
        "Illuminate\\Support\\Facades\\DB".to_string(),
    );
    assert_eq!(
        resolve_class_name("db", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}

#[test]
fn resolve_strips_leading_backslash() {
    let aliases = UseAliases::new();
    assert_eq!(
        resolve_class_name("\\Illuminate\\Support\\Facades\\DB", &aliases),
        "Illuminate\\Support\\Facades\\DB"
    );
}

// ---------------------------------------------------------------------------
// Blade `@use` directives — a Blade file's file-level imports.
// ---------------------------------------------------------------------------

fn blade(src: &str) -> UseAliases {
    blade_use_aliases(src)
}

/// The reported case: the short name in the template must resolve to the FQCN
/// the `@use` imported.
#[test]
fn blade_use_binds_the_short_class_name() {
    let map = blade("@use(\"App\\Support\\Reader\\VerseMarkerResolver\")\n<div></div>");

    assert_eq!(
        map.get("VerseMarkerResolver").map(String::as_str),
        Some("App\\Support\\Reader\\VerseMarkerResolver")
    );
}

#[test]
fn blade_use_accepts_single_quotes() {
    let map = blade("@use('App\\Models\\Flight')");

    assert_eq!(
        map.get("Flight").map(String::as_str),
        Some("App\\Models\\Flight")
    );
}

#[test]
fn blade_use_honours_an_explicit_alias() {
    let map = blade("@use('App\\Models\\Flight', 'FlightModel')");

    assert_eq!(
        map.get("FlightModel").map(String::as_str),
        Some("App\\Models\\Flight")
    );
    assert!(
        !map.contains_key("Flight"),
        "the aliased import must not also bind the short name"
    );
}

#[test]
fn blade_use_expands_a_group_import() {
    let map = blade("@use('App\\Models\\{Flight, Airport as Field}')");

    assert_eq!(
        map.get("Flight").map(String::as_str),
        Some("App\\Models\\Flight")
    );
    assert_eq!(
        map.get("Field").map(String::as_str),
        Some("App\\Models\\Airport")
    );
}

#[test]
fn blade_use_collapses_double_escaped_separators() {
    let map = blade("@use('App\\\\Models\\\\Flight')");

    assert_eq!(
        map.get("Flight").map(String::as_str),
        Some("App\\Models\\Flight"),
        "`App\\\\Models\\\\Flight` names the same class as `App\\Models\\Flight`"
    );
}

/// Function and const imports bind no class — same exclusion the PHP path makes.
#[test]
fn blade_use_skips_function_and_const_imports() {
    assert!(blade("@use('function App\\Helpers\\fmt')").is_empty());
    assert!(blade("@use('const App\\Constants\\MAX')").is_empty());
}

#[test]
fn blade_use_collects_every_directive_in_the_file() {
    let map = blade("@use('App\\Models\\Flight')\n@use('App\\Models\\Airport')\n<div></div>");

    assert_eq!(map.len(), 2, "got {map:?}");
}

// --- guards -----------------------------------------------------------------

#[test]
fn blade_use_ignores_directives_inside_comments() {
    assert!(
        blade("{{-- @use('App\\Models\\Flight') --}}").is_empty(),
        "a Blade comment is not a declaration"
    );
    assert!(
        blade("<!-- @use('App\\Models\\Flight') -->").is_empty(),
        "an HTML comment is not a declaration"
    );
}

/// `@use` must not match a longer word that merely starts with it.
#[test]
fn blade_use_ignores_a_longer_directive_name() {
    assert!(blade("@usesomething('App\\Models\\Flight')").is_empty());
}

#[test]
fn blade_use_ignores_an_empty_argument_list() {
    assert!(blade("@use()").is_empty());
    assert!(blade("@use('')").is_empty());
}

#[test]
fn file_without_use_directives_yields_no_aliases() {
    assert!(blade("<div>{{ $x }}</div>").is_empty());
}

// ---------------------------------------------------------------------------
// Positioned PHP `use` imports — the class-reference index's PHP source.
// ---------------------------------------------------------------------------

fn php_imports(src: &str) -> Vec<PhpUseImport> {
    let wrapped = format!("<?php\n{src}");
    let tree = parse_php(&wrapped).expect("parse");
    php_use_class_refs(&tree, &wrapped)
}

/// The source text the reported span covers — the class name as written at
/// that site.
fn slice(src: &str, i: &PhpUseImport) -> String {
    let wrapped = format!("<?php\n{src}");
    let line = wrapped.lines().nth(i.line as usize).expect("line in range");
    line[i.column as usize..i.end_column as usize].to_string()
}

#[test]
fn php_use_is_positioned_on_the_name_as_written() {
    let src = "use App\\Models\\Flight;";
    let found = php_imports(src);

    assert_eq!(found.len(), 1, "got {found:?}");
    assert_eq!(found[0].fqcn, r"App\Models\Flight");
    assert_eq!(found[0].line, 1, "line 0 is `<?php`");
    assert_eq!(
        slice(src, &found[0]),
        r"App\Models\Flight",
        "a flat import spells the whole FQCN, so the span covers it"
    );
}

#[test]
fn aliased_php_use_is_positioned_on_the_class_not_the_alias() {
    let src = "use App\\Models\\Flight as F;";
    let found = php_imports(src);

    assert_eq!(found.len(), 1, "got {found:?}");
    assert_eq!(found[0].fqcn, r"App\Models\Flight");
    assert_eq!(
        slice(src, &found[0]),
        r"App\Models\Flight",
        "the span stops before `as F` — the alias is a local binding, not a \
         reference to the class name"
    );
}

#[test]
fn grouped_php_use_yields_one_entry_per_clause() {
    let src = "use App\\Models\\{Flight, Airport as Field};";
    let found = php_imports(src);

    assert_eq!(
        found.iter().map(|i| i.fqcn.as_str()).collect::<Vec<_>>(),
        [r"App\Models\Flight", r"App\Models\Airport"],
        "prefix applied to each clause"
    );
    assert_eq!(
        slice(src, &found[0]),
        "Flight",
        "a grouped clause spells only its own leaf, so that is the span"
    );
    assert_eq!(slice(src, &found[1]), "Airport");
}

#[test]
fn php_use_entries_come_back_in_source_order() {
    let src = "use App\\Models\\Airport;\nuse App\\Models\\Flight;";
    let found = php_imports(src);

    assert_eq!(
        found.iter().map(|i| i.fqcn.as_str()).collect::<Vec<_>>(),
        [r"App\Models\Airport", r"App\Models\Flight"]
    );
    assert_eq!(found[0].line, 1);
    assert_eq!(found[1].line, 2);
}

#[test]
fn php_function_and_const_imports_are_not_class_refs() {
    assert!(php_imports("use function App\\Helpers\\fmt;").is_empty());
    assert!(php_imports("use const App\\Constants\\MAX;").is_empty());
}

/// A leading `\` is not part of the name — the FQCN must match the key the
/// index and the alias map already use.
#[test]
fn leading_separator_is_stripped_from_the_fqcn() {
    let found = php_imports("use \\App\\Models\\Flight;");

    assert_eq!(found.len(), 1, "got {found:?}");
    assert_eq!(found[0].fqcn, r"App\Models\Flight");
}

#[test]
fn file_without_imports_yields_nothing() {
    assert!(php_imports("class Foo {}").is_empty());
}

/// Trait `use` inside a class body binds a trait, not an import — it is not a
/// `namespace_use_declaration` and must not be collected.
#[test]
fn trait_use_inside_a_class_is_not_an_import() {
    let found = php_imports("class Foo {\n    use SomeTrait;\n}");

    assert!(found.is_empty(), "got {found:?}");
}

// ---------------------------------------------------------------------------
// Blade `@use` site locations — the class rename rewrites inside these spans.
// ---------------------------------------------------------------------------

#[test]
fn blade_use_site_span_covers_the_content_inside_the_quotes() {
    let src = "@use('App\\Models\\Flight')\n<div></div>";
    let sites = blade_use_sites(src);

    assert_eq!(sites.len(), 1, "got {sites:?}");
    assert_eq!(
        &src[sites[0].raw_start..sites[0].raw_end],
        r"App\Models\Flight",
        "span excludes the quotes and the parentheses"
    );
}

#[test]
fn blade_use_site_span_handles_double_quotes_and_whitespace() {
    let src = "@use( \"App\\Models\\Flight\" )";
    let sites = blade_use_sites(src);

    assert_eq!(sites.len(), 1, "got {sites:?}");
    assert_eq!(
        &src[sites[0].raw_start..sites[0].raw_end],
        r"App\Models\Flight"
    );
}

#[test]
fn blade_use_site_span_survives_an_alias_second_argument() {
    let src = "@use('App\\Models\\Flight', 'FlightModel')";
    let sites = blade_use_sites(src);

    assert_eq!(sites.len(), 1, "got {sites:?}");
    assert_eq!(
        &src[sites[0].raw_start..sites[0].raw_end],
        r"App\Models\Flight",
        "the span tracks the first argument only"
    );
    assert_eq!(sites[0].alias.as_deref(), Some("FlightModel"));
}

#[test]
fn blade_use_site_spans_are_correct_for_several_directives() {
    let src = "@use('App\\Models\\Airport')\n@use('App\\Models\\Flight')\n";
    let sites = blade_use_sites(src);

    let spans: Vec<&str> = sites.iter().map(|s| &src[s.raw_start..s.raw_end]).collect();
    assert_eq!(spans, [r"App\Models\Airport", r"App\Models\Flight"]);
}

/// The raw span is the source text, so a double-escaped import keeps its
/// escaping in the span while `import` is normalised.
#[test]
fn blade_use_site_keeps_raw_text_while_import_is_normalised() {
    let src = "@use('App\\\\Models\\\\Flight')";
    let sites = blade_use_sites(src);

    assert_eq!(sites.len(), 1, "got {sites:?}");
    assert_eq!(sites[0].import, r"App\Models\Flight", "normalised");
    assert_eq!(
        &src[sites[0].raw_start..sites[0].raw_end],
        r"App\\Models\\Flight",
        "raw span is verbatim source"
    );
}
