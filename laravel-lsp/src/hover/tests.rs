use super::*;

// ============================================================================
// render — section presence, ordering, omission
// ============================================================================

#[test]
fn render_empty_content_returns_empty_string() {
    let out = render(&HoverContent::default());
    assert_eq!(out, "");
}

#[test]
fn render_header_only() {
    let out = render(&HoverContent {
        header: Some("App\\Models\\User"),
        ..Default::default()
    });
    assert_eq!(out, "**App\\Models\\User**");
}

#[test]
fn render_source_link_only() {
    let out = render(&HoverContent {
        source_link: Some("[app/Models/User.php](file:///abs/User.php)"),
        ..Default::default()
    });
    // No `at ` prefix — the link renders verbatim. No backticks around the
    // label either — those would give it inline-code styling.
    assert_eq!(out, "[app/Models/User.php](file:///abs/User.php)");
}

#[test]
fn render_php_code_block_prepends_php_opening_tag() {
    // The opening tag is required for Zed's tree-sitter-php grammar to
    // parse the snippet (the standard `php` grammar variant requires it).
    let out = render(&HoverContent {
        code: Some(CodeBlock {
            language: CodeLanguage::Php,
            content: "public string $email;",
        }),
        ..Default::default()
    });
    assert_eq!(out, "```php\n<?php\npublic string $email;\n```");
}

#[test]
fn render_plain_code_block_omits_language() {
    let out = render(&HoverContent {
        code: Some(CodeBlock {
            language: CodeLanguage::Plain,
            content: "Laravel",
        }),
        ..Default::default()
    });
    assert_eq!(out, "```\nLaravel\n```");
}

#[test]
fn render_full_section_set_in_order() {
    let tags = vec![
        "@param mixed $x".to_string(),
        "@return Response".to_string(),
    ];
    let out = render(&HoverContent {
        header: Some("App\\Foo::bar"),
        detail: Some("Some detail line"),
        description: Some("Description prose."),
        code: Some(CodeBlock {
            language: CodeLanguage::Php,
            content: "public function bar()",
        }),
        tags: &tags,
        source_link: Some("[app/Foo.php:10](file:///abs/Foo.php#L10)"),
        trailer: None,
    });
    let expected = "**App\\Foo::bar**\n\
                    \n\
                    Some detail line\n\
                    \n\
                    Description prose.\n\
                    \n\
                    ```php\n\
                    <?php\n\
                    public function bar()\n\
                    ```\n\
                    \n\
                    *@param mixed $x*\n\
                    \n\
                    *@return Response*\n\
                    \n\
                    [app/Foo.php:10](file:///abs/Foo.php#L10)";
    assert_eq!(out, expected);
}

#[test]
fn render_skips_absent_sections() {
    let out = render(&HoverContent {
        header: Some("App\\Foo"),
        // no detail, no description, no code, no tags
        source_link: Some("[app/Foo.php](file:///abs/Foo.php)"),
        ..Default::default()
    });
    let expected = "**App\\Foo**\n\n[app/Foo.php](file:///abs/Foo.php)";
    assert_eq!(out, expected);
}

#[test]
fn render_trailer_appears_last() {
    let out = render(&HoverContent {
        trailer: Some("*(not registered)*"),
        ..Default::default()
    });
    assert_eq!(out, "*(not registered)*");
}

#[test]
fn render_empty_tags_slice_omits_section() {
    let tags: Vec<String> = Vec::new();
    let out = render(&HoverContent {
        header: Some("App\\Foo"),
        tags: &tags,
        ..Default::default()
    });
    assert_eq!(out, "**App\\Foo**");
}

#[test]
fn render_multiple_tags_separated_by_blank_line() {
    let tags = vec![
        "@param mixed $a".to_string(),
        "@param mixed $b".to_string(),
        "@return Response".to_string(),
    ];
    let out = render(&HoverContent {
        tags: &tags,
        ..Default::default()
    });
    let expected = "*@param mixed $a*\n\n*@param mixed $b*\n\n*@return Response*";
    assert_eq!(out, expected);
}

// ============================================================================
// Utility predicates
// ============================================================================

#[test]
fn is_class_like_type_distinguishes_classes_from_primitives() {
    assert!(is_class_like_type("App\\Models\\User"));
    assert!(is_class_like_type("\\App\\Models\\User"));
    assert!(is_class_like_type("Carbon"));
    assert!(is_class_like_type("Collection"));
    assert!(is_class_like_type("?Carbon"));

    assert!(!is_class_like_type("mixed"));
    assert!(!is_class_like_type("string"));
    assert!(!is_class_like_type("int"));
    assert!(!is_class_like_type("?int"));
    assert!(!is_class_like_type("null"));
    assert!(!is_class_like_type("array"));
}

#[test]
fn source_link_with_line_includes_fragment() {
    let link = source_link("app/Foo.php", "file:///abs/Foo.php", Some(42));
    // No backticks around the label — those would render as inline code.
    assert_eq!(link, "[app/Foo.php:42](file:///abs/Foo.php#L42)");
}

#[test]
fn source_link_without_line_omits_fragment() {
    let link = source_link("app/Foo.php", "file:///abs/Foo.php", None);
    assert_eq!(link, "[app/Foo.php](file:///abs/Foo.php)");
}

#[test]
fn truncate_for_display_clips_long_strings() {
    let long = "x".repeat(500);
    let out = truncate_for_display(&long, 200);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().filter(|c| *c == 'x').count(), 200);
}

#[test]
fn truncate_for_display_passes_short_strings_through() {
    let short = "short";
    let out = truncate_for_display(short, 200);
    assert_eq!(out, "short");
}

#[test]
fn truncate_for_display_handles_multibyte_chars_at_boundary() {
    // 200 multibyte chars — make sure we count chars not bytes
    let s: String = "日".repeat(300);
    let out = truncate_for_display(&s, 200);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().count(), 201); // 200 + ellipsis
}

// ============================================================================
// magic_member_card (M6) — classification hover for Eloquent magic members
// ============================================================================

use crate::salsa_impl::{Confidence, MagicMemberKind};

#[test]
fn magic_member_card_relationship_high_confidence() {
    let out = magic_member_card(
        MagicMemberKind::Relationship,
        "posts",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
        Some("[app/Models/User.php:12](file:///p/app/Models/User.php#L12)"),
    );
    assert_eq!(
        out,
        "**Eloquent relationship**\n\n`posts` on `App\\Models\\User`\n\n[app/Models/User.php:12](file:///p/app/Models/User.php#L12)"
    );
}

#[test]
fn magic_member_card_with_definition_renders_php_code_block() {
    let out = magic_member_card(
        MagicMemberKind::Relationship,
        "account",
        "App\\Models\\User",
        Confidence::High,
        Some("public function account()\n{\n    return $this->belongsTo(Account::class);\n}"),
        None,
        None,
        None,
    );
    // Definition renders as a php fence (render prepends the <?php opener) and
    // sits between the detail line and any source link.
    assert!(
        out.contains("```php\n<?php\npublic function account()"),
        "got: {out}"
    );
    assert!(out.contains("belongsTo(Account::class)"), "got: {out}");
}

#[test]
fn magic_member_card_labels_each_kind() {
    let label = |k| {
        magic_member_card(
            k,
            "x",
            "App\\Models\\User",
            Confidence::High,
            None,
            None,
            None,
            None,
        )
        .lines()
        .next()
        .unwrap()
        .to_string()
    };
    assert_eq!(label(MagicMemberKind::Scope), "**Eloquent scope**");
    assert_eq!(label(MagicMemberKind::Accessor), "**Eloquent accessor**");
    assert_eq!(label(MagicMemberKind::Column), "**Database column**");
    assert_eq!(label(MagicMemberKind::DynamicFinder), "**Dynamic finder**");
    assert_eq!(label(MagicMemberKind::Factory), "**Model factory**");
    assert_eq!(label(MagicMemberKind::FactoryMethod), "**Factory method**");
    assert_eq!(label(MagicMemberKind::Pivot), "**Pivot model**");
    assert_eq!(
        label(MagicMemberKind::BuilderMethod),
        "**Query builder method**"
    );
}

#[test]
fn magic_member_card_builder_method_renders_vendor_signature_and_summary() {
    // The builder-method fallback has no decl_file to slice — its
    // signature/summary come pre-extracted from `BuilderMethodIndex` and are
    // passed straight through the existing `definition`/`description` slots.
    let card = magic_member_card(
        MagicMemberKind::BuilderMethod,
        "orderByDesc",
        "Illuminate\\Database\\Query\\Builder",
        Confidence::High,
        Some("public function orderByDesc($column)"),
        None,
        Some("Add an \"order by\" clause in descending order to the query."),
        None,
    );
    assert!(card.contains("**Query builder method**"));
    assert!(card.contains("`orderByDesc` on `Illuminate\\Database\\Query\\Builder`"));
    assert!(card.contains("public function orderByDesc($column)"));
    assert!(card.contains("Add an \"order by\" clause in descending order to the query."));
}

#[test]
fn magic_member_card_plain_member_is_empty() {
    // Generic properties are Intelephense's job — no card, no duplication.
    let out = magic_member_card(
        MagicMemberKind::PlainMember,
        "name",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
        None,
    );
    assert_eq!(out, "");
}

#[test]
fn magic_member_card_medium_confidence_adds_inferred_trailer() {
    let out = magic_member_card(
        MagicMemberKind::Scope,
        "active",
        "App\\Models\\User",
        Confidence::Medium,
        None,
        None,
        None,
        None,
    );
    assert!(out.ends_with("*receiver type inferred*"), "got: {out}");
}

#[test]
fn magic_member_card_high_confidence_has_no_trailer() {
    let out = magic_member_card(
        MagicMemberKind::Scope,
        "active",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
        None,
    );
    assert!(!out.contains("inferred"), "got: {out}");
}

#[test]
fn candidate_method_names_by_kind() {
    // Relationship / finder: accessed verbatim.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Relationship, "account"),
        vec!["account".to_string()]
    );
    // Scope: scope{Pascal}.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Scope, "active"),
        vec!["scopeActive".to_string()]
    );
    // Accessor: old-style get{Pascal}Attribute + new-style camelCase.
    assert_eq!(
        candidate_method_names(MagicMemberKind::Accessor, "full_name"),
        vec!["getFullNameAttribute".to_string(), "fullName".to_string()]
    );
}

#[test]
fn extract_member_snippet_dedents_and_slices() {
    let src = "<?php\nclass User {\n    public function account()\n    {\n        return $this->belongsTo(Account::class);\n    }\n}\n";
    // Method spans lines 2..=5 (0-based): signature, brace, body, close.
    let snippet = extract_member_snippet(src, 2, 5);
    assert_eq!(
        snippet,
        "public function account()\n{\n    return $this->belongsTo(Account::class);\n}"
    );
}

#[test]
fn extract_member_snippet_out_of_bounds_is_empty() {
    assert_eq!(extract_member_snippet("a\nb\n", 9, 12), "");
}

#[test]
fn extract_member_snippet_caps_long_bodies() {
    let body: String = (0..40).map(|i| format!("    line{i}\n")).collect();
    let src = format!("<?php\nclass X {{\n{body}}}\n");
    let snippet = extract_member_snippet(&src, 2, 41);
    assert!(
        snippet.lines().count() <= 21,
        "should cap at MAX_LINES + marker"
    );
    assert!(snippet.ends_with("// …"));
}

// ─── docblock mining: leading-block extraction, summary, @return, fold ──────

#[test]
fn extract_leading_docblock_returns_the_block_above_a_declaration() {
    // Method at line 6 (0-based), with a 3-line `/** … */` block immediately
    // above at lines 2..=5. The extractor returns the raw block, no dedent.
    let src = "<?php\nclass Guard {\n    /**\n     * Determine if the user is authenticated.\n     * @return bool\n     */\n    public function check()\n    { return true; }\n}\n";
    let block = extract_leading_docblock(src, 6).expect("docblock present");
    assert!(block.contains("/**"), "opening missing: {block}");
    assert!(
        block.contains("Determine if the user is authenticated."),
        "summary line missing: {block}"
    );
    assert!(block.contains("@return bool"), "@return missing: {block}");
}

#[test]
fn extract_leading_docblock_skips_attributes() {
    // A `#[Foo]` attribute line sits between the docblock and the declaration —
    // the upward scan must skip it and still find the docblock.
    let src = "<?php\nclass X {\n    /** Summary. */\n    #[Override]\n    public function run(): void\n    {}\n}\n";
    let block = extract_leading_docblock(src, 4).expect("docblock present");
    assert!(block.contains("Summary."), "docblock missing: {block}");
}

#[test]
fn extract_leading_docblock_is_none_without_phpdoc() {
    // No docblock above the declaration → None.
    let src = "<?php\nclass User {\n    public function account()\n    {\n        return $this->belongsTo(Account::class);\n    }\n}\n";
    assert_eq!(extract_leading_docblock(src, 2), None);
}

#[test]
fn docblock_summary_collapses_multiline_prose_before_first_tag() {
    let block =
        "/**\n * Determine if the current user\n * is authenticated.\n *\n * @return bool\n */";
    assert_eq!(
        docblock_summary(block).as_deref(),
        Some("Determine if the current user is authenticated.")
    );
}

#[test]
fn docblock_summary_handles_inline_block() {
    assert_eq!(
        docblock_summary("/** Get the guard instance. */").as_deref(),
        Some("Get the guard instance.")
    );
}

#[test]
fn docblock_summary_is_none_when_only_tags() {
    // No prose before the first `@tag` → no summary.
    assert_eq!(docblock_summary("/**\n * @return bool\n */"), None);
    assert_eq!(docblock_summary("/**\n */"), None);
}

#[test]
fn docblock_return_type_extracts_first_token() {
    assert_eq!(
        docblock_return_type("/**\n * @return bool\n */").as_deref(),
        Some("bool")
    );
    // Description after the type is dropped; the type token is kept as-written.
    assert_eq!(
        docblock_return_type("/**\n * @return Collection<int, User> the users\n */").as_deref(),
        Some("Collection<int,")
    );
    // Union / nullable kept verbatim (no normalization).
    assert_eq!(
        docblock_return_type("/**\n * @return ?User\n */").as_deref(),
        Some("?User")
    );
}

#[test]
fn docblock_return_type_is_none_without_tag() {
    assert_eq!(docblock_return_type("/**\n * Just a summary.\n */"), None);
    // `@return` with no type token → None.
    assert_eq!(docblock_return_type("/**\n * @return\n */"), None);
}

#[test]
fn fold_return_type_appends_when_signature_is_bare() {
    // Body brace on the signature line → `: bool` inserted before the `{`.
    let snippet = "public function check()\n{\n    return true;\n}";
    assert_eq!(
        fold_return_type(snippet, Some("bool")),
        "public function check(): bool\n{\n    return true;\n}"
    );
}

#[test]
fn fold_return_type_appends_when_brace_on_next_line() {
    // No body opener on the signature line → append the type to the line.
    let snippet = "public function check()";
    assert_eq!(
        fold_return_type(snippet, Some("bool")),
        "public function check(): bool"
    );
}

#[test]
fn fold_return_type_folds_into_abstract_signature() {
    // Interface / abstract method: `;` is the "body opener".
    let snippet = "public function check();";
    assert_eq!(
        fold_return_type(snippet, Some("bool")),
        "public function check(): bool;"
    );
}

#[test]
fn fold_return_type_leaves_native_return_type_untouched() {
    // Signature already declares `: bool` in source → no double-append.
    let snippet = "public function check(): bool\n{\n    return true;\n}";
    assert_eq!(fold_return_type(snippet, Some("bool")), snippet);
}

#[test]
fn fold_return_type_without_type_is_identity() {
    let snippet = "public function check()\n{\n}";
    assert_eq!(fold_return_type(snippet, None), snippet);
}

#[test]
fn magic_member_card_without_link_omits_source_section() {
    let out = magic_member_card(
        MagicMemberKind::Column,
        "email",
        "App\\Models\\User",
        Confidence::High,
        None,
        None,
        None,
        None,
    );
    assert_eq!(out, "**Database column**\n\n`email` on `App\\Models\\User`");

    // With a resolved type (M6.2), a "Type" line appears under the detail.
    let typed = magic_member_card(
        MagicMemberKind::Column,
        "email",
        "App\\Models\\User",
        Confidence::High,
        None,
        Some("string"),
        None,
        None,
    );
    assert_eq!(
        typed,
        "**Database column**\n\n`email` on `App\\Models\\User`\n\nType `string`"
    );
}

#[test]
fn facade_method_card_promotes_summary_to_description_and_keeps_docblock_out_of_code() {
    // The rich FacadeMethod card: header + detail line + a DESCRIPTION line
    // carrying the chased declaration's docblock summary, then a docblock-free
    // PHP code block whose signature already carries the folded return type.
    // (`main.rs` does the docblock mining; the card just renders the pieces.)
    let definition = "public function check(): bool\n{\n    return ! is_null($this->user());\n}";
    let out = magic_member_card(
        MagicMemberKind::FacadeMethod,
        "check",
        "Illuminate\\Auth\\AuthManager",
        Confidence::High,
        Some(definition),
        None,
        Some("Determine if the current user is authenticated."),
        None,
    );
    assert!(
        out.starts_with("**Facade method**"),
        "header missing: {out}"
    );
    assert!(
        out.contains("`check` on `Illuminate\\Auth\\AuthManager`"),
        "detail line missing: {out}"
    );
    // Summary is the DESCRIPTION line — between the detail and the code block.
    assert!(
        out.contains(
            "`check` on `Illuminate\\Auth\\AuthManager`\n\nDetermine if the current user is authenticated.\n\n```php"
        ),
        "summary not promoted to description line: {out}"
    );
    // The code block is docblock-free with a typed signature and the body.
    assert!(out.contains("```php"), "code block missing: {out}");
    assert!(!out.contains("/**"), "docblock leaked into code: {out}");
    assert!(!out.contains("@return"), "@return leaked into code: {out}");
    assert!(
        out.contains("public function check(): bool"),
        "typed signature missing: {out}"
    );
    assert!(
        out.contains("return ! is_null($this->user());"),
        "body missing: {out}"
    );
}

#[test]
fn facade_method_card_without_summary_omits_description_line() {
    // No docblock summary → no description line; just header, detail, code.
    let out = magic_member_card(
        MagicMemberKind::FacadeMethod,
        "check",
        "Illuminate\\Auth\\AuthManager",
        Confidence::High,
        Some("public function check(): bool\n{\n}"),
        None,
        None,
        None,
    );
    assert_eq!(
        out,
        "**Facade method**\n\n`check` on `Illuminate\\Auth\\AuthManager`\n\n```php\n<?php\npublic function check(): bool\n{\n}\n```"
    );
}

#[test]
fn facade_method_card_keeps_inferred_trailer_for_medium_confidence() {
    // A helper-chain receiver type is inferred → Medium confidence keeps the
    // `*receiver type inferred*` trailer alongside the rich code block.
    let out = magic_member_card(
        MagicMemberKind::FacadeMethod,
        "make",
        "Illuminate\\View\\Factory",
        Confidence::Medium,
        Some("public function make($view)\n{\n}"),
        None,
        None,
        None,
    );
    assert!(out.contains("```php"), "code block missing: {out}");
    assert!(
        out.contains("*receiver type inferred*"),
        "Medium-confidence trailer missing: {out}"
    );
}

// ============================================================================
// helper-function hover (#58) — curated allow-list + card rendering
// ============================================================================

#[test]
fn helper_card_covers_exactly_the_seven_curated_helpers() {
    for name in ["route", "view", "config", "auth", "app", "session", "cache"] {
        assert!(
            helper_card(name).is_some(),
            "`{name}` should be in the curated allow-list"
        );
    }
    assert_eq!(HELPER_CARDS.len(), 7, "exactly seven curated helpers");
    // Non-curated helpers are Intelephense's job — never carded.
    for name in ["bcrypt", "abort", "collect", "str", "dd", "tap"] {
        assert!(
            helper_card(name).is_none(),
            "`{name}` must not be in the curated allow-list"
        );
    }
}

#[test]
fn helper_card_synopsis_and_links_are_populated() {
    for (name, card) in HELPER_CARDS {
        assert!(!card.synopsis.is_empty(), "{name} needs a synopsis");
        assert!(
            card.docs_url.starts_with("https://laravel.com/docs/"),
            "{name} docs_url should be a canonical laravel.com anchor"
        );
        assert!(
            card.vendor_path.ends_with("helpers.php"),
            "{name} vendor_path should point at a framework helpers.php"
        );
    }
}

#[test]
fn helper_identifier_card_renders_header_detail_and_link() {
    let link = "[Laravel documentation](https://laravel.com/docs/helpers#method-route)";
    let out = helper_identifier_card("route", Some(link)).expect("curated helper");
    assert_eq!(
        out,
        format!("**route**\n\nGenerate a URL for a named route.\n\n{link}")
    );
}

#[test]
fn helper_identifier_card_omits_link_section_when_absent() {
    let out = helper_identifier_card("config", None).expect("curated helper");
    assert_eq!(
        out,
        "**config**\n\nGet / set the value of a configuration variable."
    );
}

#[test]
fn helper_identifier_card_is_none_for_non_curated_helper() {
    assert!(
        helper_identifier_card("bcrypt", None).is_none(),
        "non-curated helpers render no card"
    );
}

// ============================================================================
// translation_card — leaf key + locale detail, quoted value, not-found trailer
// ============================================================================

#[test]
fn translation_card_shows_leaf_key_locale_quoted_value_and_link() {
    let link = "[lang/app/en/notification.php](file:///p/lang/app/en/notification.php)";
    let out = translation_card(
        "app::notification.task_group_status_change.title",
        "en",
        Some("Status changed"),
        Some(link),
    );
    // Only the leaf (`title`) — not the full namespaced key — and the value in
    // typographic quotes on its own line.
    assert_eq!(out, format!("`title` · en\n\n“Status changed”\n\n{link}"));
}

#[test]
fn translation_card_leaf_strips_namespace_and_parent_segments() {
    // A non-namespaced dotted key reduces to its last segment too.
    let out = translation_card("messages.nav.home", "fr", Some("Accueil"), None);
    assert_eq!(out, "`home` · fr\n\n“Accueil”");
}

#[test]
fn translation_card_without_value_shows_not_found_trailer() {
    let out = translation_card("app::missing.key", "en", None, None);
    assert_eq!(
        out,
        format!("`key` · en\n\n{TRANSLATION_NOT_FOUND_TRAILER}")
    );
}
