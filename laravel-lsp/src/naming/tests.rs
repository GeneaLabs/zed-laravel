use super::*;

#[test]
fn pascal_to_kebab_single_word() {
    assert_eq!(pascal_to_kebab("Counter"), "counter");
}

#[test]
fn snake_to_pascal_cases() {
    assert_eq!(snake_to_pascal("active"), "Active");
    assert_eq!(snake_to_pascal("full_name"), "FullName");
    assert_eq!(snake_to_pascal("created_at_date"), "CreatedAtDate");
    // Defensive: leading/empty segments don't produce empty chunks.
    assert_eq!(snake_to_pascal("_foo"), "Foo");
    assert_eq!(snake_to_pascal(""), "");
}

#[test]
fn pascal_to_kebab_two_words() {
    assert_eq!(pascal_to_kebab("UserProfile"), "user-profile");
}

#[test]
fn pascal_to_kebab_three_words() {
    assert_eq!(pascal_to_kebab("AdminUserList"), "admin-user-list");
}

#[test]
fn pascal_to_kebab_leading_uppercase_only() {
    // Edge: single capital letter should NOT get a leading dash.
    assert_eq!(pascal_to_kebab("A"), "a");
}

#[test]
fn pascal_to_kebab_empty() {
    assert_eq!(pascal_to_kebab(""), "");
}

#[test]
fn pascal_to_kebab_acronym_simple_form() {
    // Acronyms split per-character. Documented as the simple convention;
    // adequate for Laravel-style class names where acronyms are rare.
    assert_eq!(pascal_to_kebab("HTTPClient"), "h-t-t-p-client");
}

#[test]
fn kebab_to_pascal_roundtrip() {
    let kebab = "admin-user-list";
    assert_eq!(pascal_to_kebab(&kebab_to_pascal(kebab)), kebab);
}

#[test]
fn split_dotted_single() {
    assert_eq!(split_dotted("counter"), vec!["counter"]);
}

#[test]
fn split_dotted_multi() {
    assert_eq!(split_dotted("admin.user-list"), vec!["admin", "user-list"]);
}

#[test]
fn split_dotted_deep() {
    assert_eq!(
        split_dotted("admin.users.show-profile"),
        vec!["admin", "users", "show-profile"]
    );
}

#[test]
fn dotted_to_namespace_single() {
    assert_eq!(dotted_to_namespace("counter"), "Counter");
}

#[test]
fn dotted_to_namespace_kebab_segment() {
    assert_eq!(dotted_to_namespace("user-profile"), "UserProfile");
}

#[test]
fn dotted_to_namespace_nested() {
    assert_eq!(dotted_to_namespace("admin.user-list"), "Admin\\UserList");
}

#[test]
fn dotted_to_class_path_single() {
    assert_eq!(dotted_to_class_path("counter").as_deref(), Some("Counter"));
}

#[test]
fn dotted_to_class_path_nested() {
    assert_eq!(
        dotted_to_class_path("admin.user-list").as_deref(),
        Some("Admin/UserList")
    );
}

#[test]
fn dotted_to_class_path_deep() {
    assert_eq!(
        dotted_to_class_path("admin.users.show-profile").as_deref(),
        Some("Admin/Users/ShowProfile")
    );
}

#[test]
fn has_emoji_detects_prefix() {
    assert!(has_emoji("\u{26A1}create"));
}

#[test]
fn has_emoji_rejects_plain() {
    assert!(!has_emoji("create"));
}

#[test]
fn has_emoji_rejects_emoji_in_middle() {
    assert!(!has_emoji("create\u{26A1}"));
}

#[test]
fn strip_emoji_removes_bare_prefix() {
    assert_eq!(strip_emoji("\u{26A1}create"), "create");
}

#[test]
fn strip_emoji_removes_prefix_with_text_selector() {
    // U+FE0E forces text presentation. Livewire's PHP regex strips it.
    assert_eq!(strip_emoji("\u{26A1}\u{FE0E}create"), "create");
}

#[test]
fn strip_emoji_removes_prefix_with_emoji_selector() {
    // U+FE0F forces emoji presentation. Livewire's PHP regex also strips it.
    assert_eq!(strip_emoji("\u{26A1}\u{FE0F}create"), "create");
}

#[test]
fn strip_emoji_passes_plain_through() {
    assert_eq!(strip_emoji("create"), "create");
}

#[test]
fn strip_emoji_ignores_emoji_not_at_start() {
    assert_eq!(strip_emoji("create\u{26A1}"), "create\u{26A1}");
}

#[test]
fn with_emoji_adds_when_enabled() {
    assert_eq!(with_emoji("create", true), "\u{26A1}create");
}

#[test]
fn with_emoji_strips_when_disabled() {
    assert_eq!(with_emoji("\u{26A1}create", false), "create");
}

#[test]
fn with_emoji_no_double_prefix() {
    // Idempotent: applying with_emoji(_, true) twice yields the same result.
    let once = with_emoji("create", true);
    let twice = with_emoji(&once, true);
    assert_eq!(once, twice);
}

#[test]
fn with_emoji_disabled_on_plain_is_noop() {
    assert_eq!(with_emoji("create", false), "create");
}

// ---------- validate_dotted_name (shared validator) ----------

#[test]
fn validate_dotted_accepts_simple_and_nested_names() {
    assert_eq!(validate_dotted_name("welcome", false), Ok(()));
    assert_eq!(validate_dotted_name("admin.user-list", true), Ok(()));
    assert_eq!(validate_dotted_name("admin.user_list", false), Ok(()));
}

#[test]
fn validate_dotted_trims_before_checking() {
    assert_eq!(validate_dotted_name("  users.profile  ", false), Ok(()));
}

#[test]
fn validate_dotted_rejects_empty() {
    assert_eq!(validate_dotted_name("", false), Err(DottedNameError::Empty));
    assert_eq!(
        validate_dotted_name("   ", true),
        Err(DottedNameError::Empty)
    );
}

#[test]
fn validate_dotted_rejects_namespaced_only_when_opted_in() {
    // reject_namespaced = true → the `::` is caught before the segment scan.
    assert_eq!(
        validate_dotted_name("billing::invoice", true),
        Err(DottedNameError::Namespaced)
    );
    // reject_namespaced = false → the `:` falls through as an invalid character
    // (the view locator relies on this).
    assert_eq!(
        validate_dotted_name("billing::invoice", false),
        Err(DottedNameError::InvalidCharacter(':'))
    );
}

#[test]
fn validate_dotted_rejects_slashes() {
    assert_eq!(
        validate_dotted_name("users/profile", false),
        Err(DottedNameError::ContainsSlash)
    );
    assert_eq!(
        validate_dotted_name("users\\profile", true),
        Err(DottedNameError::ContainsSlash)
    );
}

#[test]
fn validate_dotted_rejects_extensions() {
    for name in ["a.blade.php", "a.blade", "a.php"] {
        assert_eq!(
            validate_dotted_name(name, false),
            Err(DottedNameError::HasExtension),
            "expected HasExtension for {name:?}"
        );
    }
}

#[test]
fn validate_dotted_rejects_empty_segments() {
    // Leading, trailing, and double-dot all collapse to EmptySegment.
    assert_eq!(
        validate_dotted_name(".users", false),
        Err(DottedNameError::EmptySegment)
    );
    assert_eq!(
        validate_dotted_name("users.", false),
        Err(DottedNameError::EmptySegment)
    );
    assert_eq!(
        validate_dotted_name("users..profile", true),
        Err(DottedNameError::EmptySegment)
    );
}

#[test]
fn validate_dotted_rejects_invalid_characters() {
    assert_eq!(
        validate_dotted_name("users profile", false),
        Err(DottedNameError::InvalidCharacter(' '))
    );
    assert_eq!(
        validate_dotted_name("users@profile", true),
        Err(DottedNameError::InvalidCharacter('@'))
    );
}

#[test]
fn dotted_to_class_path_refuses_an_absolute_name() {
    // `Path::join` REPLACES the base when the right-hand side is absolute, so
    // an absolute name made `class_path.join(..)` resolve to the name itself,
    // escaping the registered directory entirely.
    assert_eq!(dotted_to_class_path("/etc/passwd"), None);
    assert_eq!(dotted_to_class_path("/tmp/Secret"), None);
}

#[test]
fn dotted_to_class_path_refuses_separators_inside_a_segment() {
    // A component name is dotted segments of kebab identifiers. Anything
    // carrying its own separator is not a name, and must not become a path.
    assert_eq!(dotted_to_class_path("admin/users"), None);
    assert_eq!(dotted_to_class_path("admin.users/show"), None);
    assert_eq!(dotted_to_class_path("admin\\users"), None);
    assert_eq!(dotted_to_class_path("C:windows"), None);
}

#[test]
fn dotted_to_class_path_refuses_an_empty_segment() {
    // Splitting on `.` turns `..` into empty segments, so this is also what
    // stops classic dot-dot traversal from surviving the conversion.
    assert_eq!(dotted_to_class_path("admin..users"), None);
    assert_eq!(dotted_to_class_path(".users"), None);
    assert_eq!(dotted_to_class_path("users."), None);
    assert_eq!(dotted_to_class_path(""), None);
}

#[test]
fn dotted_to_class_path_still_accepts_ordinary_names() {
    // The guard must not cost a legitimate name — including digits and the
    // deep nesting Laravel conventions produce.
    assert_eq!(dotted_to_class_path("counter").as_deref(), Some("Counter"));
    assert_eq!(
        dotted_to_class_path("admin.users.show-profile").as_deref(),
        Some("Admin/Users/ShowProfile")
    );
    assert_eq!(
        dotted_to_class_path("v2-widget").as_deref(),
        Some("V2Widget")
    );
}

#[test]
fn dotted_to_class_path_refuses_a_segment_that_collapses_to_nothing() {
    // `kebab_to_pascal` splits on `-` and maps every empty part to `""`, so an
    // all-dashes segment survives a check of the RAW segments (non-empty, no
    // separator) and then vanishes from the join. An empty leading segment
    // makes the result absolute, and `Path::join` discards its base — the very
    // escape this guard exists to stop. `"-.foo"` minted `"/Foo"`, which the
    // rename path turned into a class file written to `/Foo.php`.
    assert_eq!(dotted_to_class_path("-"), None);
    assert_eq!(dotted_to_class_path("-.foo"), None);
    assert_eq!(dotted_to_class_path("--.foo"), None);
    assert_eq!(dotted_to_class_path("-.etc.passwd"), None);
    // A collapsing segment in the middle is refused too: it would double the
    // separator rather than re-root, but it is still not a component name.
    assert_eq!(dotted_to_class_path("foo.-.bar"), None);
}
