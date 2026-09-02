use super::*;

#[test]
fn locates_top_level_key() {
    let src = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
];
"#;
    let pos = locate_in_source(src, &["name"]).expect("expected a position");
    assert_eq!(pos.line, 2);
    // The string content `name` sits between the quotes; the column should
    // point at the `n`.
    assert_eq!(
        &src.lines().nth(2).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "name"
    );
}

#[test]
fn locates_nested_key() {
    let src = r#"<?php
return [
    'database' => [
        'connections' => [
            'mysql' => [
                'host' => '127.0.0.1',
            ],
        ],
    ],
];
"#;
    let pos = locate_in_source(src, &["database", "connections", "mysql", "host"]).expect("pos");
    assert_eq!(pos.line, 5);
    assert_eq!(
        &src.lines().nth(5).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "host"
    );
}

#[test]
fn returns_none_for_missing_top_level_key() {
    let src = r#"<?php
return [
    'name' => 'Laravel',
];
"#;
    assert!(locate_in_source(src, &["missing"]).is_none());
}

#[test]
fn returns_none_for_missing_nested_key() {
    let src = r#"<?php
return [
    'database' => [
        'connections' => [
            'mysql' => ['host' => '127.0.0.1'],
        ],
    ],
];
"#;
    assert!(locate_in_source(src, &["database", "connections", "pgsql", "host"]).is_none());
}

#[test]
fn returns_none_when_path_passes_through_non_array() {
    let src = r#"<?php
return [
    'name' => 'Laravel',
];
"#;
    // 'name' resolves to a string, not an array, so descending further fails.
    assert!(locate_in_source(src, &["name", "deeper"]).is_none());
}

#[test]
fn handles_double_quoted_key() {
    let src = r#"<?php
return [
    "name" => "Laravel",
];
"#;
    let pos = locate_in_source(src, &["name"]).expect("pos");
    assert_eq!(
        &src.lines().nth(2).unwrap()[pos.start_column as usize..pos.end_column as usize],
        "name"
    );
}

#[test]
fn empty_path_returns_none() {
    let src = r#"<?php
return ['x' => 1];
"#;
    assert!(locate_in_source(src, &[]).is_none());
}

#[test]
fn handles_file_with_use_statements_above_return() {
    let src = r#"<?php

use Illuminate\Support\Str;

return [
    'name' => 'Laravel',
    'env' => 'production',
];
"#;
    let pos = locate_in_source(src, &["env"]).expect("pos");
    // Slice the line the locator actually pointed at — robust to whatever
    // line numbering the raw string convention produces.
    let line_text = src.lines().nth(pos.line as usize).unwrap();
    assert_eq!(
        &line_text[pos.start_column as usize..pos.end_column as usize],
        "env"
    );
}

// ── enumerate_keys_in_source ──────────────────────────────────────────────

fn enum_keys(src: &str) -> Vec<String> {
    enumerate_keys_in_source(src)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

#[test]
fn enumerate_top_level_keys_with_positions() {
    let src = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
];
"#;
    let entries = enumerate_keys_in_source(src);
    assert_eq!(
        entries.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["name", "env"]
    );
    // Position is the key string content (matches locate_in_source).
    assert_eq!(entries[0].1, locate_in_source(src, &["name"]).unwrap());
}

#[test]
fn enumerate_emits_nested_paths_leaf_and_intermediate() {
    let src = r#"<?php
return [
    'default' => 'mysql',
    'connections' => [
        'mysql' => [
            'host' => '127.0.0.1',
            'port' => 3306,
        ],
    ],
];
"#;
    let keys = enum_keys(src);
    assert_eq!(
        keys,
        vec![
            "default",
            "connections",
            "connections.mysql",
            "connections.mysql.host",
            "connections.mysql.port",
        ]
    );
}

#[test]
fn enumerate_indexes_numeric_list_entries() {
    let src = r#"<?php
return [
    'providers' => [
        App\Providers\AppServiceProvider::class,
        App\Providers\AuthServiceProvider::class,
    ],
    'aliases' => [
        'Route' => Illuminate\Support\Facades\Route::class,
    ],
];
"#;
    let keys = enum_keys(src);
    // `providers`' list items get PHP's own indices. `config('app.providers.0')`
    // resolves at runtime, so the tool reports it; they used to be skipped.
    assert_eq!(
        keys,
        vec![
            "providers",
            "providers.0",
            "providers.1",
            "aliases",
            "aliases.Route"
        ]
    );
}

#[test]
fn enumerate_empty_for_non_array_php() {
    assert!(enum_keys("<?php echo 'hi';").is_empty());
}

// ── enumerate_entries_in_source: value text + the #369 Part B regressions ──
//
// Each test below names the finding it pins. The retired
// `salsa_impl::parse_translation_keys` failed every one of them; it counted
// `[` and `]` per line, so any bracket inside a value or on a line of its own
// desynchronised its key stack.

fn enum_entries(src: &str) -> Vec<(String, String)> {
    enumerate_entries_in_source(src)
        .into_iter()
        .map(|(key, value, _)| (key, value))
        .collect()
}

fn value_of(src: &str, key: &str) -> Option<String> {
    enum_entries(src)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[test]
fn entry_value_is_the_string_content_without_quotes() {
    let src = r#"<?php
return [
    'welcome' => 'Hello, :name!',
    'empty' => '',
];
"#;
    assert_eq!(value_of(src, "welcome").as_deref(), Some("Hello, :name!"));
    // An empty literal has no `string_content` child at all.
    assert_eq!(value_of(src, "empty").as_deref(), Some(""));
}

#[test]
fn entry_value_for_a_non_string_scalar_is_its_raw_source_text() {
    let src = r#"<?php
return [
    'count' => 42,
    'flag' => true,
    'from_env' => env('APP_NAME', 'Laravel'),
];
"#;
    assert_eq!(value_of(src, "count").as_deref(), Some("42"));
    assert_eq!(value_of(src, "flag").as_deref(), Some("true"));
    // The whole call, NOT the `'APP_NAME'` string nested inside it — the
    // wrapper unwrap must not descend into an arbitrary expression.
    assert_eq!(
        value_of(src, "from_env").as_deref(),
        Some("env('APP_NAME', 'Laravel')")
    );
}

#[test]
fn entry_value_for_an_array_valued_key_is_empty() {
    let src = r#"<?php
return [
    'form' => [
        'title' => 'Title',
    ],
];
"#;
    // The group key is still enumerated — it is a real, referenceable key —
    // but it has no scalar of its own to display.
    assert_eq!(value_of(src, "form").as_deref(), Some(""));
    assert_eq!(value_of(src, "form.title").as_deref(), Some("Title"));
}

#[test]
fn b1_a_bare_bracket_list_entry_neither_pushes_nor_pops_a_key_level() {
    let src = r#"<?php
return [
    'form' => [
        'list' => [
            [
                'x' => 'val1',
            ],
        ],
        'manufacturer' => 'Manufacturer',
    ],
];
"#;
    // The bare `[` line pushed no name but its `]` popped one, so the scanner
    // desynchronised here. On this exact shape it also double-pushed `list`
    // (the bug #350 removes), which masked the pop on `manufacturer` — it
    // reported `page.form.list.list.x` and a correct `form.manufacturer`.
    // Pinning the whole set catches both halves; asserting `manufacturer`
    // alone would pass against the retired scanner.
    //
    // `x` sits under a list entry, so its real Laravel key is `form.list.0.x`
    // — which is what the walker reports. The scanner offered
    // `form.list.list.x`, a path that resolves to nothing.
    assert_eq!(
        enum_entries(src),
        vec![
            ("form".to_string(), String::new()),
            ("form.list".to_string(), String::new()),
            ("form.list.0".to_string(), String::new()),
            ("form.list.0.x".to_string(), "val1".to_string()),
            ("form.manufacturer".to_string(), "Manufacturer".to_string()),
        ]
    );
}

#[test]
fn b2_a_key_split_across_lines_nests_correctly_and_emits_no_empty_leaf() {
    let src = r#"<?php
return [
    'form' =>
    [
        'x' => 'y',
    ],
];
"#;
    assert_eq!(
        enum_entries(src),
        vec![
            ("form".to_string(), String::new()),
            ("form.x".to_string(), "y".to_string()),
        ]
    );
}

#[test]
fn b3_a_bracket_or_arrow_inside_a_value_does_not_corrupt_nesting() {
    let src = r#"<?php
return [
    'form' => [
        'a' => 'see [docs]',
        'b' => 'maps => [ like this',
        'c' => 'Kept',
    ],
];
"#;
    assert_eq!(
        enum_entries(src),
        vec![
            ("form".to_string(), String::new()),
            ("form.a".to_string(), "see [docs]".to_string()),
            ("form.b".to_string(), "maps => [ like this".to_string()),
            ("form.c".to_string(), "Kept".to_string()),
        ]
    );
}

#[test]
fn b4_an_escaped_quote_in_a_value_keeps_the_whole_value() {
    let src = "<?php\nreturn [\n    'a' => 'It\\'s here',\n    'b' => 'plain',\n];\n";
    // The value must survive the escape. Building it from the string node's
    // `string_content` children would yield `It`, because the escape splits
    // the run in two.
    assert_eq!(
        enum_entries(src),
        vec![
            ("a".to_string(), "It's here".to_string()),
            ("b".to_string(), "plain".to_string()),
        ]
    );
}

#[test]
fn b4_an_escaped_quote_in_a_key_yields_the_key_php_reads() {
    let src = "<?php\nreturn [\n    'it\\'s' => 'value',\n    'next' => 'also here',\n];\n";
    let keys = enum_entries(src)
        .into_iter()
        .map(|(k, _)| k)
        .collect::<Vec<_>>();
    // The retired regex captured `s`, the tail after the escape. Taking the
    // first `string_content` run instead would give `it`. Only unescaping the
    // whole literal body gives `it's` — the key PHP actually stores, and the
    // one `__('page.it\'s')` looks up.
    assert_eq!(keys, vec!["it's".to_string(), "next".to_string()]);
}

#[test]
fn b5_hyphenated_numeric_and_dotted_keys_all_enumerate() {
    let src = r#"<?php
return [
    'variant-color' => 'Color',
    '404' => 'Not found',
    'a.b' => 'Dotted',
];
"#;
    // The old identifier pattern `[a-zA-Z_][a-zA-Z0-9_]*` matched none of
    // these, so completion offered none of them while goto resolved all three.
    assert_eq!(
        enum_entries(src),
        vec![
            ("variant-color".to_string(), "Color".to_string()),
            ("404".to_string(), "Not found".to_string()),
            ("a.b".to_string(), "Dotted".to_string()),
        ]
    );
}

// ── every catalogue shape PHP legitimately allows ─────────────────────────

#[test]
fn list_entries_take_phps_own_indices_and_are_navigable() {
    let src = r#"<?php
return [
    'sizes' => ['sm', 'md'],
];
"#;
    assert_eq!(
        enum_entries(src),
        vec![
            ("sizes".to_string(), String::new()),
            ("sizes.0".to_string(), "sm".to_string()),
            ("sizes.1".to_string(), "md".to_string()),
        ]
    );
    // Completion must never offer a key goto cannot follow — that divergence
    // is the whole reason #369 exists.
    assert!(locate_in_source(src, &["sizes", "1"]).is_some());
}

#[test]
fn an_explicit_integer_key_advances_the_index_counter_like_php() {
    // PHP: `['a', 5 => 'b', 'c']` is 0, 5, 6.
    let src = r#"<?php
return [
    'list' => ['a', 5 => 'b', 'c'],
];
"#;
    let keys = enum_keys(src);
    assert_eq!(
        keys,
        vec!["list", "list.0", "list.5", "list.6"],
        "got {keys:?}"
    );
}

#[test]
fn an_unquoted_integer_key_is_enumerated_and_navigable() {
    let src = r#"<?php
return [
    404 => 'Not found',
];
"#;
    assert_eq!(
        enum_entries(src),
        vec![("404".to_string(), "Not found".to_string())]
    );
    assert!(locate_in_source(src, &["404"]).is_some());
}

#[test]
fn a_spread_stops_index_synthesis_rather_than_guessing() {
    // `...$other` contributes an unknown number of elements, so no later
    // position can be named. Keyed entries after it are still fine.
    let src = r#"<?php
return [
    'list' => ['a', ...$other, 'c'],
    'kept' => 'yes',
];
"#;
    let keys = enum_keys(src);
    assert!(keys.contains(&"list.0".to_string()), "got {keys:?}");
    assert!(!keys.contains(&"list.1".to_string()), "got {keys:?}");
    assert!(keys.contains(&"kept".to_string()), "got {keys:?}");
}

#[test]
fn a_list_entry_no_longer_hides_the_keys_declared_after_it() {
    // `locate_at_path` used to `?` on the first unparseable entry, so one
    // list item aborted the whole lookup.
    let src = r#"<?php
return [
    'providers' => [
        App\Providers\AppServiceProvider::class,
    ],
    'later' => 'reachable',
];
"#;
    assert!(locate_in_source(src, &["later"]).is_some());
}

#[test]
fn each_key_kind_reports_how_it_is_written() {
    let src = r#"<?php
return [
    'quoted' => 'a',
    404 => 'b',
    'list' => ['c'],
];
"#;
    let kind = |path: &[&str]| locate_in_source(src, path).unwrap().kind;
    assert_eq!(kind(&["quoted"]), KeyKind::Quoted);
    assert_eq!(kind(&["404"]), KeyKind::BareInteger);
    assert_eq!(kind(&["list", "0"]), KeyKind::SynthesizedIndex);
    // Every kind resolves — the distinction is only about the key's own text.
    assert!(locate_in_source(src, &["list", "0"]).is_some());
}

#[test]
fn concatenated_literal_values_are_folded() {
    let src = r#"<?php
return [
    'joined' => 'Hello, ' . 'world',
    'dynamic' => 'Hello, ' . $name,
];
"#;
    assert_eq!(value_of(src, "joined").as_deref(), Some("Hello, world"));
    // Not statically knowable — show the source rather than a wrong answer.
    assert_eq!(
        value_of(src, "dynamic").as_deref(),
        Some("'Hello, ' . $name")
    );
}

#[test]
fn heredoc_and_nowdoc_values_drop_their_markers() {
    let src = r#"<?php
return [
    'here' => <<<EOT
line one
EOT,
    'now' => <<<'EOT'
raw \n text
EOT,
];
"#;
    assert_eq!(value_of(src, "here").as_deref(), Some("line one"));
    // A nowdoc resolves nothing, so the backslash and the `n` stay two
    // separate characters.
    assert_eq!(value_of(src, "now").as_deref(), Some("raw \\n text"));
}

#[test]
fn escape_resolution_follows_the_quote_style() {
    let src = r#"<?php
return [
    'single' => 'a\nb',
    'double' => "a\nb",
    'quote' => "He said \"hi\"",
    'apostrophe' => 'It\'s here',
];
"#;
    // A single-quoted literal resolves only \' and \\, so `\n` is a
    // backslash followed by an `n` — two characters, not a newline.
    assert_eq!(value_of(src, "single").as_deref(), Some("a\\nb"));
    assert_eq!(value_of(src, "double").as_deref(), Some("a\nb"));
    assert_eq!(value_of(src, "quote").as_deref(), Some("He said \"hi\""));
    assert_eq!(value_of(src, "apostrophe").as_deref(), Some("It's here"));
}

#[test]
fn an_interpolated_value_is_not_folded() {
    let src = "<?php\nreturn [\n    'greet' => \"Hi $name\",\n];\n";
    // No static answer; the source text is the honest display.
    assert_eq!(value_of(src, "greet").as_deref(), Some("\"Hi $name\""));
}
