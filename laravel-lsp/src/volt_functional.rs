//! Member declarations in a FUNCTIONAL Volt component.
//!
//! A functional Volt file declares no class at all — its state and actions are
//! top-level calls and assignments in the template's front matter:
//!
//! ```php
//! <?php
//! use function Livewire\Volt\{state, computed};
//! state(['count' => 0]);
//! $increment = fn () => $this->count++;
//! $double = computed(fn () => $this->count * 2);
//! ?>
//! ```
//!
//! [`crate::component_member_locator`] parses class bodies, so it finds
//! nothing here: there is no `property_declaration` and no
//! `method_declaration`. This module is the functional-Volt half of the same
//! primitive — "does a member with this NAME exist in this source, and where"
//! — reading the two shapes Volt actually uses:
//!
//!   - every `state([...])` array KEY is a public property;
//!   - every top-level `$name = <closure>` assignment is a member. A
//!     `computed(...)` wrapper makes it a computed PROPERTY (read as
//!     `$this->name` / `{{ $name }}`); a bare closure, `action(...)` or
//!     `protect(...)` makes it an action METHOD (`wire:click="name"`).
//!
//! Positions land on the bare name — inside the quotes for a `state` key,
//! after the `$` for an assignment — matching the class-based locator's
//! convention.

use tree_sitter::Node;

use crate::component_member_locator::{MemberKind, MemberLocation};
use crate::parser::parse_php;

/// Volt wrappers that turn an assigned closure into a component member, and
/// the kind each produces.
fn wrapper_kind(name: &str) -> Option<MemberKind> {
    match name {
        "computed" => Some(MemberKind::Property),
        "action" | "protect" => Some(MemberKind::Method),
        _ => None,
    }
}

/// Every member a functional Volt source declares, as `(name, location)`, in
/// no particular order. Empty when `source` carries no Volt functional
/// signature — the same gate
/// [`crate::livewire_resolver::source_contains_volt_signature`] applies
/// everywhere else, so a plain PHP file is never mistaken for a component.
pub fn members(source: &str) -> Vec<(String, MemberLocation)> {
    if !crate::livewire_resolver::source_contains_volt_signature(source) {
        return Vec::new();
    }
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_call_expression" => collect_state_keys(n, bytes, &mut out),
            "assignment_expression" if is_top_level(n) => {
                collect_closure_member(n, bytes, &mut out)
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

/// Locate `member` in a functional Volt source, honouring `want` when the
/// caller knows which kind the reference demands. Candidates are sorted by
/// `(line, column)` so the first declaration in document order wins.
pub fn locate_member(
    source: &str,
    member: &str,
    want: Option<MemberKind>,
) -> Option<MemberLocation> {
    let mut hits: Vec<MemberLocation> = members(source)
        .into_iter()
        .filter(|(name, loc)| name == member && want.is_none_or(|k| k == loc.kind))
        .map(|(_, loc)| loc)
        .collect();
    hits.sort_by_key(|l| (l.line, l.start_column));
    hits.into_iter().next()
}

/// The public properties a functional Volt source declares, as `(name, type)`
/// pairs for `$`-completion. Volt state carries no declared PHP type, so
/// every entry is `"mixed"`.
pub fn property_types(source: &str) -> Vec<(String, String)> {
    members(source)
        .into_iter()
        .filter(|(_, loc)| loc.kind == MemberKind::Property)
        .map(|(name, _)| (name, "mixed".to_string()))
        .collect()
}

/// The action names a functional Volt source declares, sorted, for
/// `wire:click`-style completion.
pub fn action_names(source: &str) -> Vec<String> {
    let mut out: Vec<String> = members(source)
        .into_iter()
        .filter(|(_, loc)| loc.kind == MemberKind::Method)
        .map(|(name, _)| name)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The declaration text for a hover card: the `state()` array entry, or the
/// assignment up to (not including) the closure's body. Whitespace runs are
/// collapsed so a multiline declaration renders as one line.
pub fn declaration_summary(source: &str, member: &str) -> Option<String> {
    declaration_summary_of_kind(source, member, None)
}

/// [`declaration_summary`], restricted to the declaration kind the reference
/// demands — so a hover card whose header was filtered on `want` cannot show
/// the body of the other kind's declaration of the same name.
pub fn declaration_summary_of_kind(
    source: &str,
    member: &str,
    want: Option<MemberKind>,
) -> Option<String> {
    if !crate::livewire_resolver::source_contains_volt_signature(source) {
        return None;
    }
    let tree = parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let mut hits: Vec<(u32, u32, String)> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_call_expression" => {
                let mut found = Vec::new();
                collect_state_keys(n, bytes, &mut found);
                for (name, loc) in found {
                    if name == member && want.is_none_or(|k| k == loc.kind) {
                        if let Some(entry) = state_entry_node(n, bytes, member) {
                            hits.push((
                                loc.line,
                                loc.start_column,
                                collapse_ws(&source[entry.start_byte()..entry.end_byte()]),
                            ));
                        }
                    }
                }
            }
            "assignment_expression" if is_top_level(n) => {
                let mut found = Vec::new();
                collect_closure_member(n, bytes, &mut found);
                for (name, loc) in found {
                    if name == member && want.is_none_or(|k| k == loc.kind) {
                        let end = closure_body_start(n).unwrap_or_else(|| n.end_byte());
                        hits.push((
                            loc.line,
                            loc.start_column,
                            collapse_ws(&source[n.start_byte()..end]),
                        ));
                    }
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    hits.sort_by_key(|(line, col, _)| (*line, *col));
    hits.into_iter().next().map(|(_, _, text)| text)
}

/// Push every `state(['a' => 1, 'b'])` key on `call` as a public property.
fn collect_state_keys(call: Node, bytes: &[u8], out: &mut Vec<(String, MemberLocation)>) {
    if call_name(call, bytes).as_deref() != Some("state") {
        return;
    }
    for entry in state_entries(call) {
        let Some(key) = entry_key_node(entry) else {
            continue;
        };
        let Some(name) = string_literal_name(key, bytes) else {
            continue;
        };
        out.push((name, string_name_location(key)));
    }
}

/// Push a top-level `$name = <closure>` assignment as a member, when the
/// right-hand side is a closure or a Volt wrapper around one.
fn collect_closure_member(assign: Node, bytes: &[u8], out: &mut Vec<(String, MemberLocation)>) {
    let Some(left) = assign.child_by_field_name("left") else {
        return;
    };
    if left.kind() != "variable_name" {
        return;
    }
    let Some(right) = assign.child_by_field_name("right") else {
        return;
    };
    let kind = match right.kind() {
        "arrow_function" | "anonymous_function" | "anonymous_function_creation_expression" => {
            MemberKind::Method
        }
        "function_call_expression" => {
            match call_name(right, bytes).as_deref().and_then(wrapper_kind) {
                Some(k) => k,
                None => return,
            }
        }
        _ => return,
    };
    let Ok(text) = left.utf8_text(bytes) else {
        return;
    };
    let name = text.trim_start_matches('$').to_string();
    if name.is_empty() {
        return;
    }
    let start = left.start_position();
    let end = left.end_position();
    out.push((
        name,
        MemberLocation {
            line: start.row as u32,
            // Skip the `$` so the caret sits on the bare name, exactly as
            // the class-based locator does for `public $count`.
            start_column: start.column as u32 + 1,
            end_column: end.column as u32,
            kind,
        },
    ));
}

/// The called function's bare name (`state`, `computed`), namespace prefix
/// stripped so `\Livewire\Volt\state(...)` matches too.
fn call_name(call: Node, bytes: &[u8]) -> Option<String> {
    let f = call.child_by_field_name("function")?;
    let text = f.utf8_text(bytes).ok()?;
    Some(text.rsplit('\\').next()?.to_string())
}

/// Every `array_element_initializer` in the call's FIRST array argument.
fn state_entries<'tree>(call: Node<'tree>) -> Vec<Node<'tree>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    let arg_nodes: Vec<Node> = args.children(&mut cursor).collect();
    let mut array = None;
    for arg in arg_nodes {
        if arg.kind() == "array_creation_expression" {
            array = Some(arg);
            break;
        }
        let mut c = arg.walk();
        let nested: Vec<Node> = arg.children(&mut c).collect();
        if let Some(found) = nested
            .into_iter()
            .find(|n| n.kind() == "array_creation_expression")
        {
            array = Some(found);
            break;
        }
    }
    let Some(array) = array else {
        return Vec::new();
    };
    let mut c = array.walk();
    array
        .children(&mut c)
        .filter(|n| n.kind() == "array_element_initializer")
        .collect()
}

/// The key node of an array entry: the left side of `=>`, or — for a bare
/// `['color']` entry declaring a prop with no default — the entry's only
/// value. Both are the entry's first meaningful child.
fn entry_key_node<'tree>(entry: Node<'tree>) -> Option<Node<'tree>> {
    let mut c = entry.walk();
    let children: Vec<Node> = entry.children(&mut c).collect();
    children.into_iter().find(|n| n.kind() != "comment")
}

/// The `array_element_initializer` in `call` whose key is `member`.
fn state_entry_node<'a>(call: Node<'a>, bytes: &[u8], member: &str) -> Option<Node<'a>> {
    if call_name(call, bytes).as_deref() != Some("state") {
        return None;
    }
    state_entries(call).into_iter().find(|entry| {
        entry_key_node(*entry)
            .and_then(|k| string_literal_name(k, bytes))
            .as_deref()
            == Some(member)
    })
}

/// A quoted string literal's inner text, when it is a bare identifier.
fn string_literal_name(node: Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let text = node.utf8_text(bytes).ok()?;
    let inner = text
        .strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .or_else(|| text.strip_prefix('"').and_then(|r| r.strip_suffix('"')))?;
    let mut chars = inner.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() || first == '_')
        .then(|| inner.to_string())
        .filter(|_| inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// The location of a string literal's CONTENT — the quotes excluded, so the
/// caret lands on the bare name.
fn string_name_location(node: Node) -> MemberLocation {
    let start = node.start_position();
    let end = node.end_position();
    MemberLocation {
        line: start.row as u32,
        start_column: start.column as u32 + 1,
        end_column: (end.column as u32).saturating_sub(1),
        kind: MemberKind::Property,
    }
}

/// The byte offset where the assigned closure's body starts, so a hover
/// summary can stop at the signature.
fn closure_body_start(assign: Node) -> Option<usize> {
    let right = assign.child_by_field_name("right")?;
    right.child_by_field_name("body").map(|b| b.start_byte())
}

/// True when `n` sits directly in the file's top-level statement list — not
/// inside a class, function, method, or closure. Volt's functional API only
/// declares members at the top level; an assignment nested in a closure is a
/// local, not a component member.
fn is_top_level(n: Node) -> bool {
    let mut cur = n.parent();
    while let Some(p) = cur {
        if matches!(
            p.kind(),
            "class_declaration"
                | "trait_declaration"
                | "enum_declaration"
                | "interface_declaration"
                | "method_declaration"
                | "function_definition"
                | "arrow_function"
                | "anonymous_function"
                | "anonymous_function_creation_expression"
                | "anonymous_class"
        ) {
            return false;
        }
        cur = p.parent();
    }
    true
}

/// Collapse every whitespace run (including newlines) to a single space.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
