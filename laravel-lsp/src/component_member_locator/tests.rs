use super::*;

#[test]
fn locates_a_method_declaration() {
    let source = r#"<?php
class ContractViewPage extends Page
{
    public function checkPrefillStatus(): void
    {
    }
}
"#;
    let loc = locate_member(source, "checkPrefillStatus").expect("method found");
    assert_eq!(loc.kind, MemberKind::Method);
    // Line 3 (0-based) is `    public function checkPrefillStatus(): void`.
    assert_eq!(loc.line, 3);
    let name = &source.lines().nth(3).unwrap()[loc.start_column as usize..loc.end_column as usize];
    assert_eq!(name, "checkPrefillStatus");
}

#[test]
fn locates_a_property_declaration_and_skips_the_dollar() {
    let source = r#"<?php
class ContractViewPage extends Page
{
    public ?string $prefillStatus = null;
}
"#;
    let loc = locate_member(source, "prefillStatus").expect("property found");
    assert_eq!(loc.kind, MemberKind::Property);
    assert_eq!(loc.line, 3);
    let line = source.lines().nth(3).unwrap();
    let name = &line[loc.start_column as usize..loc.end_column as usize];
    assert_eq!(name, "prefillStatus");
    // The `$` sigil sits immediately before the located range.
    assert_eq!(
        &line[loc.start_column as usize - 1..loc.start_column as usize],
        "$"
    );
}

#[test]
fn locates_a_promoted_constructor_property() {
    let source = r#"<?php
class ContractService
{
    public function __construct(public readonly ContractRepository $repository)
    {
    }
}
"#;
    let loc = locate_member(source, "repository").expect("promoted property found");
    assert_eq!(loc.kind, MemberKind::Property);
    let line = source.lines().nth(3).unwrap();
    let name = &line[loc.start_column as usize..loc.end_column as usize];
    assert_eq!(name, "repository");
}

#[test]
fn returns_none_when_no_member_matches() {
    let source = r#"<?php
class ContractViewPage extends Page
{
    public ?string $prefillStatus = null;

    public function checkPrefillStatus(): void
    {
    }
}
"#;
    assert!(locate_member(source, "doesNotExist").is_none());
}

#[test]
fn returns_none_for_unparseable_source() {
    // Not actually invalid PHP as far as tree-sitter's concerned, but there's
    // no member declaration matching — same shape as `returns_none_when_no_member_matches`,
    // just confirms an empty/degenerate file doesn't panic.
    assert!(locate_member("", "anything").is_none());
}

#[test]
fn public_action_method_names_skips_lifecycle_and_non_public() {
    let source = r#"<?php
class ContractViewPage {
    public function mount(?string $id = null): void {}
    public function render() {}
    public function updatedContractData(): void {}
    public function checkPrefillStatus(): void {}
    public function enterEditMode(): void {}
    public static function staticHelper(): void {}
    protected function internal(): void {}
    public function __get($name) {}
}
"#;
    assert_eq!(
        public_action_method_names(source),
        vec![
            "checkPrefillStatus".to_string(),
            "enterEditMode".to_string()
        ]
    );
}

#[test]
fn public_property_types_includes_scalars_and_untyped() {
    let source = r#"<?php
class Page {
    public string $prefillStatus = 'none';
    public ?string $contractId = null;
    public $legacy;
    protected string $hidden = '';
    public function __construct(public int $count) {}
}
"#;
    let props = public_property_types(source);
    let names: Vec<&str> = props.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"prefillStatus"));
    assert!(names.contains(&"contractId"));
    assert!(names.contains(&"legacy"));
    assert!(names.contains(&"count"));
    assert!(!names.contains(&"hidden"));
    assert_eq!(
        props.iter().find(|(n, _)| n == "contractId").unwrap().1,
        "string"
    );
    assert_eq!(
        props.iter().find(|(n, _)| n == "legacy").unwrap().1,
        "mixed"
    );
}

#[test]
fn public_property_types_excludes_static_properties() {
    // A `static` property isn't part of the component's template surface —
    // Livewire only serializes instance state.
    let source = r#"<?php
class Page {
    public static string $registry = 'none';
    protected static $view = 'pages.x';
    public string $title = '';
}
"#;
    let names: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, vec!["title".to_string()]);
}

#[test]
fn action_names_lifecycle_prefix_requires_camel_case_boundary() {
    // `updatedFoo` is Livewire's per-property hook; `updates` and
    // `hydraulics` merely share the prefix letters and ARE actions.
    let source = r#"<?php
class Page {
    public function updatedTitle(): void {}
    public function updating(): void {}
    public function updates(): void {}
    public function hydraulics(): void {}
}
"#;
    assert_eq!(
        public_action_method_names(source),
        vec!["hydraulics".to_string(), "updates".to_string()]
    );
}

#[test]
fn member_declaration_summary_renders_a_method_signature() {
    let source = r#"<?php
class Page {
    public function save(
        string $status,
        ?int $retries = null,
    ): bool { return true; }
}
"#;
    assert_eq!(
        member_declaration_summary(source, "save").as_deref(),
        Some("public function save( string $status, ?int $retries = null, ): bool")
    );
}

#[test]
fn member_declaration_summary_renders_a_property_header_without_initializer() {
    let source = r#"<?php
class Page {
    protected static string $view = 'pages.report';
    public ?\App\Models\User $user = null;
}
"#;
    assert_eq!(
        member_declaration_summary(source, "user").as_deref(),
        Some("public ?\\App\\Models\\User $user")
    );
    assert_eq!(
        member_declaration_summary(source, "view").as_deref(),
        Some("protected static string $view")
    );
    assert!(member_declaration_summary(source, "missing").is_none());
}

#[test]
fn locates_member_inside_an_inline_sfc_blade_source() {
    // Livewire v4 single-file component: the class lives in the template's
    // own front matter, so the .blade.php content IS the source to parse —
    // positions land inside the blade file.
    let source = r#"<?php

use Livewire\Component;

new class extends Component {
    public int $count = 0;

    public function increment(): void
    {
        $this->count++;
    }
};
?>

<div>
    <span>{{ $count }}</span>
    <button wire:click="increment">+</button>
</div>
"#;
    let count = locate_member(source, "count").expect("property in front matter");
    assert_eq!(count.kind, MemberKind::Property);
    assert_eq!(count.line, 5, "0-based line of `public int $count = 0;`");
    let increment = locate_member(source, "increment").expect("method in front matter");
    assert_eq!(increment.kind, MemberKind::Method);
    assert_eq!(increment.line, 7);
}

#[test]
fn locates_member_inside_a_volt_class_blade_source() {
    let source = r#"<?php

use Livewire\Volt\Component;

new class extends Component {
    public string $search = '';
}; ?>

<div><input wire:model="search"></div>
"#;
    let loc = locate_member(source, "search").expect("Volt front-matter property");
    assert_eq!(loc.kind, MemberKind::Property);
    assert_eq!(loc.line, 5);
}

// ---- true document order + declaration scoping (issue #339, item 6) -------

#[test]
fn locate_member_returns_the_first_declaration_in_document_order() {
    let source = "<?php\nclass A { public $dup = 1; }\n\nclass B { public $dup = 2; }\n";
    let loc = locate_member(source, "dup").expect("both classes declare $dup");
    assert_eq!(
        loc.line, 1,
        "the FIRST declaration wins, not the last-visited one"
    );
}

#[test]
fn a_deeply_nested_earlier_candidate_beats_a_shallow_later_one() {
    // The trait's `$dup` sits one level deeper in the tree than the class's,
    // and comes first in the document. A breadth-first or reverse-DFS walk
    // returns the shallow/later one; true `(line, column)` order returns the
    // trait's.
    let source = "<?php\ntrait T {\n    public $dup = 1;\n}\nclass C { public $dup = 2; }\n";
    let loc = locate_member(source, "dup").expect("both declare $dup");
    assert_eq!(
        loc.line, 2,
        "the trait's declaration is earlier in the file"
    );
}

#[test]
fn member_summary_describes_the_same_declaration_goto_lands_on() {
    let source = "<?php\nclass A { public int $dup = 1; }\n\nclass B { public string $dup = 2; }\n";
    assert_eq!(
        member_declaration_summary(source, "dup").as_deref(),
        Some("public int $dup"),
        "the summary follows the same document-order pick as locate_member"
    );
}

#[test]
fn kind_filter_keeps_a_property_reference_off_a_same_named_method() {
    let source = "<?php\nclass C {\n    public $save = 1;\n    public function save() {}\n}\n";
    let as_property = locate_member_of_kind(source, "save", Some(MemberKind::Property))
        .expect("the property is declared");
    assert_eq!(as_property.line, 2);
    let as_method = locate_member_of_kind(source, "save", Some(MemberKind::Method))
        .expect("the method is declared");
    assert_eq!(as_method.line, 3);
}

#[test]
fn kind_filter_returns_nothing_when_only_the_other_kind_exists() {
    let source = "<?php\nclass C {\n    public function save() {}\n}\n";
    assert!(
        locate_member_of_kind(source, "save", Some(MemberKind::Property)).is_none(),
        "a wire:model binding must not resolve to a method"
    );
}

#[test]
fn completion_excludes_members_of_every_other_top_level_declaration() {
    let source = "<?php\nclass Component1 {\n    public $owned = 1;\n    public function act() {}\n}\n\nclass Unrelated {\n    public $foreign = 2;\n    public function stranger() {}\n}\n";
    let props: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(props, vec!["owned"], "only the component's own properties");
    assert_eq!(
        public_action_method_names(source),
        vec!["act"],
        "only the component's own actions"
    );
}

#[test]
fn completion_excludes_a_trait_declared_beside_the_component() {
    let source = "<?php\nclass C {\n    public $owned = 1;\n}\n\ntrait Helper {\n    public $fromTrait = 2;\n    public function helperAction() {}\n}\n";
    let props: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(props, vec!["owned"]);
    assert!(
        public_action_method_names(source).is_empty(),
        "trait-provided members stay a documented limitation"
    );
}

#[test]
fn an_inline_anonymous_component_class_owns_the_members() {
    let source = "<?php\ntrait Helper {\n    public $fromTrait = 1;\n}\n\nnew class extends Component {\n    public $owned = 2;\n};\n";
    let props: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        props,
        vec!["owned"],
        "the anonymous class is the component even with a trait above it"
    );
}

/// A class declaring BOTH kinds under one name: the summary must describe the
/// kind asked for, not whichever declaration the parser reaches first. The
/// method is declared first, so an unfiltered call answers with it — which is
/// what makes these two assertions discriminate.
const SUMMARY_KIND_CLASH: &str = r#"<?php
class Counter
{
    public function save(): void
    {
    }

    public string $save = '';
}
"#;

#[test]
fn declaration_summary_of_kind_describes_the_requested_kind() {
    assert_eq!(
        member_declaration_summary_of_kind(SUMMARY_KIND_CLASH, "save", Some(MemberKind::Property))
            .as_deref(),
        Some("public string $save"),
    );
    assert_eq!(
        member_declaration_summary_of_kind(SUMMARY_KIND_CLASH, "save", Some(MemberKind::Method))
            .as_deref(),
        Some("public function save(): void"),
    );
}

#[test]
fn an_unfiltered_summary_still_answers_in_document_order() {
    assert_eq!(
        member_declaration_summary(SUMMARY_KIND_CLASH, "save").as_deref(),
        Some("public function save(): void"),
        "no filter means first-in-document-order, which is the method here",
    );
}

#[test]
fn declaration_summary_of_a_kind_the_class_lacks_is_none() {
    let source = "<?php\nclass Counter\n{\n    public function save(): void {}\n}\n";
    assert!(
        member_declaration_summary_of_kind(source, "save", Some(MemberKind::Property)).is_none()
    );
}

/// Item 6 scoped the member surface to the component's own declaration. A
/// trait the component actually `use`s is on `$this` at runtime, so scoping it
/// away traded a false positive for a false negative.
const USED_TRAIT: &str = r#"<?php
trait HasCounter
{
    public $fromTrait = 1;

    public function bumpFromTrait(): void
    {
    }
}

trait Unrelated
{
    public $fromUnrelated = 1;

    public function bumpFromUnrelated(): void
    {
    }
}

class Counter extends Component
{
    use HasCounter;

    public $own = 1;
}
"#;

#[test]
fn a_used_same_file_trait_contributes_its_members() {
    let properties: Vec<String> = public_property_types(USED_TRAIT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(properties.contains(&"own".to_string()));
    assert!(
        properties.contains(&"fromTrait".to_string()),
        "`use HasCounter` puts $fromTrait on $this: {properties:?}"
    );

    let actions = public_action_method_names(USED_TRAIT);
    assert!(
        actions.contains(&"bumpFromTrait".to_string()),
        "`use HasCounter` puts bumpFromTrait() on $this: {actions:?}"
    );
}

#[test]
fn an_unused_same_file_trait_contributes_nothing() {
    let properties: Vec<String> = public_property_types(USED_TRAIT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        !properties.contains(&"fromUnrelated".to_string()),
        "a trait the class never `use`s is not on $this: {properties:?}"
    );
    assert!(
        !public_action_method_names(USED_TRAIT).contains(&"bumpFromUnrelated".to_string()),
        "a trait the class never `use`s is not on $this"
    );
}

#[test]
fn a_trait_used_by_a_used_trait_contributes_its_members() {
    let source = r#"<?php
trait Inner
{
    public $deep = 1;
}

trait Outer
{
    use Inner;

    public $middle = 1;
}

class Counter extends Component
{
    use Outer;
}
"#;
    let properties: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(properties.contains(&"middle".to_string()), "{properties:?}");
    assert!(
        properties.contains(&"deep".to_string()),
        "a trait's own `use` clauses count too: {properties:?}"
    );
}

/// A `use` naming a trait declared in another file resolves to nothing — the
/// documented cross-file limitation, unchanged by the same-file fix.
#[test]
fn a_use_of_an_absent_trait_is_ignored() {
    let source = "<?php\nclass Counter extends Component\n{\n    use \\App\\Traits\\Absent;\n\n    public $own = 1;\n}\n";
    let properties: Vec<String> = public_property_types(source)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(properties, vec!["own".to_string()]);
}
