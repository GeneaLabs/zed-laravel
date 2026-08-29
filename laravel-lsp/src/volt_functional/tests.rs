//! Functional-Volt member declarations — `state()` keys and top-level
//! closure assignments (issue #339, item 3).

use super::*;

const FUNCTIONAL: &str = r#"<?php
use function Livewire\Volt\{state, computed, action};

state(['count' => 0, 'name']);

$increment = fn () => $this->count++;
$double = computed(fn () => $this->count * 2);
$reset = action(fn () => $this->count = 0);
?>
<div wire:click="increment">{{ $count }}</div>
"#;

#[test]
fn state_keys_are_properties_at_the_key_position() {
    let loc = locate_member(FUNCTIONAL, "count", None).expect("state declares count");
    assert_eq!(loc.kind, MemberKind::Property);
    assert_eq!(loc.line, 3, "0-based line of the `state([...])` call");
    // `state(['count' ...` — the caret sits on `count`, inside the quotes.
    assert_eq!(loc.start_column, 8);
    assert_eq!(loc.end_column, 13);
}

#[test]
fn bare_state_entry_without_a_default_is_a_property() {
    let loc = locate_member(FUNCTIONAL, "name", None).expect("`'name'` declares a property");
    assert_eq!(loc.kind, MemberKind::Property);
    assert_eq!(loc.line, 3);
}

#[test]
fn closure_assignment_is_an_action_method_at_the_variable() {
    let loc = locate_member(FUNCTIONAL, "increment", None).expect("$increment is an action");
    assert_eq!(loc.kind, MemberKind::Method);
    assert_eq!(loc.line, 5, "0-based line of `$increment = fn () => …`");
    assert_eq!(loc.start_column, 1, "the `$` is skipped");
    assert_eq!(loc.end_column, 10);
}

#[test]
fn computed_wrapper_is_a_property_and_action_wrapper_is_a_method() {
    let double = locate_member(FUNCTIONAL, "double", None).expect("computed() declares a member");
    assert_eq!(double.kind, MemberKind::Property);
    assert_eq!(double.line, 6);

    let reset = locate_member(FUNCTIONAL, "reset", None).expect("action() declares a member");
    assert_eq!(reset.kind, MemberKind::Method);
    assert_eq!(reset.line, 7);
}

#[test]
fn kind_filter_rejects_the_wrong_declaration_kind() {
    assert!(
        locate_member(FUNCTIONAL, "increment", Some(MemberKind::Property)).is_none(),
        "an action must not answer a property-kind reference"
    );
    assert!(
        locate_member(FUNCTIONAL, "count", Some(MemberKind::Method)).is_none(),
        "state is a property, never a method"
    );
}

#[test]
fn locals_inside_a_closure_are_not_members() {
    let source = r#"<?php
state(['count' => 0]);
$increment = fn () => $tmp = 1;
?>"#;
    assert!(
        locate_member(source, "tmp", None).is_none(),
        "an assignment nested in a closure is a local, not a component member"
    );
}

#[test]
fn a_plain_php_file_declares_no_volt_members() {
    let source = "<?php\n$increment = fn () => 1;\n";
    assert!(
        members(source).is_empty(),
        "no Volt signature — the file is not a functional component"
    );
}

#[test]
fn completion_surfaces_split_by_kind() {
    let props: Vec<String> = property_types(FUNCTIONAL)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(props.contains(&"count".to_string()));
    assert!(props.contains(&"double".to_string()));
    assert!(!props.contains(&"increment".to_string()));

    assert_eq!(action_names(FUNCTIONAL), vec!["increment", "reset"]);
}

#[test]
fn declaration_summary_names_the_shape() {
    assert_eq!(
        declaration_summary(FUNCTIONAL, "count").as_deref(),
        Some("'count' => 0")
    );
    assert_eq!(
        declaration_summary(FUNCTIONAL, "increment").as_deref(),
        Some("$increment = fn () =>")
    );
}
