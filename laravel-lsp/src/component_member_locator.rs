//! Locate a class member's declaration by name.
//!
//! Blade goto/hover fallbacks that resolve `$this->member` or a `wire:*`
//! attribute value against the class backing the template (a Filament
//! `$view`-property page, a Livewire component) need to know where `member`
//! is actually declared in that class's source. This module is the dumb,
//! reusable primitive for that: no type resolution, no member classification
//! (that's `member_resolver`'s job) — just "does a member with this NAME
//! exist in this source, and where".
//!
//! Three declaration shapes are recognised: a `method_declaration`, a
//! `property_element` inside a `property_declaration`, and a promoted
//! constructor property (`public function __construct(public Foo $bar) {}`).
//! Positions land on the NAME token — for properties/promoted params, the
//! leading `$` is skipped so the caret sits on the bare name.

use tree_sitter::Node;

use crate::parser::parse_php;

/// The declaration shape [`locate_member`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Property,
}

/// A located member declaration: 0-based line, and the start/end column of
/// the NAME token (the `$` sigil excluded for properties).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberLocation {
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
    pub kind: MemberKind,
}

/// Find `member`'s declaration in `source`. Checks every `method_declaration`,
/// property (`property_declaration` → `property_element`), and promoted
/// constructor property in the file, in document order; returns the first
/// match. A method and a property never share a name within one class, so in
/// practice there's nothing to disambiguate between the two kinds.
///
/// Returns `None` when `source` doesn't parse, or no member named `member`
/// is declared.
pub fn locate_member(source: &str, member: &str) -> Option<MemberLocation> {
    let tree = parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if name.utf8_text(bytes).ok() == Some(member) {
                        return Some(node_location(name, 0, MemberKind::Method));
                    }
                }
            }
            "property_declaration" => {
                let mut c = n.walk();
                for element in n.children(&mut c) {
                    if element.kind() != "property_element" {
                        continue;
                    }
                    if let Some(name) = element.child_by_field_name("name") {
                        if matches_dollar_name(name, bytes, member) {
                            return Some(node_location(name, 1, MemberKind::Property));
                        }
                    }
                }
            }
            "property_promotion_parameter" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if matches_dollar_name(name, bytes, member) {
                        return Some(node_location(name, 1, MemberKind::Property));
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
    None
}

/// The declaration header of `member` in `source`, for a hover card: a
/// method's signature up to (not including) its body — modifiers, name,
/// parameters, return type — or a property's declaration up to (not
/// including) its initializer. Whitespace runs are collapsed so a multiline
/// signature renders as one line. `None` when the member isn't declared.
pub fn member_declaration_summary(source: &str, member: &str) -> Option<String> {
    let tree = parse_php(source).ok()?;
    let bytes = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if name.utf8_text(bytes).ok() == Some(member) {
                        let end = n
                            .child_by_field_name("body")
                            .map(|b| b.start_byte())
                            .unwrap_or_else(|| n.end_byte());
                        return Some(collapse_ws(&source[n.start_byte()..end]));
                    }
                }
            }
            "property_declaration" => {
                let mut c = n.walk();
                for element in n.children(&mut c) {
                    if element.kind() != "property_element" {
                        continue;
                    }
                    if let Some(name) = element.child_by_field_name("name") {
                        if matches_dollar_name(name, bytes, member) {
                            // Header = the declaration up to the property
                            // NAME (modifiers + type), then the `$name`
                            // itself — the initializer stays out.
                            let head = collapse_ws(&source[n.start_byte()..name.start_byte()]);
                            return Some(format!("{} ${}", head.trim_end(), member));
                        }
                    }
                }
            }
            "property_promotion_parameter" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if matches_dollar_name(name, bytes, member) {
                        let head = collapse_ws(&source[n.start_byte()..name.start_byte()]);
                        return Some(format!("{} ${}", head.trim_end(), member));
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
    None
}

/// Collapse every whitespace run (including newlines) to a single space.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every PUBLIC property of the class in `source`, as `(name, declared type
/// text)` — `"mixed"` when untyped. Includes promoted public constructor
/// properties. Unlike the render-index surface (which keeps only
/// class-typed properties, since scalars have no members to resolve), this
/// keeps scalar and untyped publics too: a Livewire/Filament template reads
/// them as bare `$vars`, so `$`-completion must offer all of them.
pub fn public_property_types(source: &str) -> Vec<(String, String)> {
    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "property_declaration" | "property_promotion_parameter" => {
                let is_public = {
                    let mut c = n.walk();
                    // A property_declaration without an explicit modifier is
                    // public by PHP default; a promoted parameter without one
                    // isn't a property at all (plain constructor arg). A
                    // `static` property isn't part of the component's
                    // template surface — Livewire only serializes instance
                    // state — so it's excluded like a non-public one.
                    // Separate flags, exactly as `public_action_method_names`
                    // below keeps them: PHP accepts either modifier order,
                    // and a shared flag let `static public string $x` slip
                    // through — the `static_modifier` child is visited
                    // first, and the `visibility_modifier` then reset the
                    // flag back to true.
                    let mut public = n.kind() == "property_declaration";
                    let mut is_static = false;
                    for ch in n.children(&mut c) {
                        match ch.kind() {
                            "visibility_modifier" => {
                                public = ch.utf8_text(bytes).ok() == Some("public");
                            }
                            "static_modifier" => is_static = true,
                            _ => {}
                        }
                    }
                    public && !is_static
                };
                if is_public {
                    let type_text = n
                        .child_by_field_name("type")
                        .and_then(|t| t.utf8_text(bytes).ok())
                        .map(|t| t.trim_start_matches('?').to_string())
                        .unwrap_or_else(|| "mixed".to_string());
                    if n.kind() == "property_promotion_parameter" {
                        if let Some(name) = n.child_by_field_name("name") {
                            if let Ok(t) = name.utf8_text(bytes) {
                                out.push((t.trim_start_matches('$').to_string(), type_text));
                            }
                        }
                    } else {
                        let mut c = n.walk();
                        for element in n.children(&mut c) {
                            if element.kind() != "property_element" {
                                continue;
                            }
                            if let Some(name) = element.child_by_field_name("name") {
                                if let Ok(t) = name.utf8_text(bytes) {
                                    out.push((
                                        t.trim_start_matches('$').to_string(),
                                        type_text.clone(),
                                    ));
                                }
                            }
                        }
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
    out
}

/// Every PUBLIC, non-static method of the class in `source` that can be a
/// Livewire action target, sorted by name. Magic methods (`__*`) and the
/// Livewire/Filament lifecycle surface (`mount`, `render`, `boot`, `booted`,
/// `rendering`, `rendered`, `exception`, and the `updated*` / `updating*` /
/// `hydrate*` / `dehydrate*` hook families) are excluded — they exist on
/// every component and are never what a `wire:click` wants to call.
pub fn public_action_method_names(source: &str) -> Vec<String> {
    const LIFECYCLE: &[&str] = &[
        "mount",
        "render",
        "boot",
        "booted",
        "rendering",
        "rendered",
        "exception",
    ];
    const LIFECYCLE_PREFIXES: &[&str] = &["updated", "updating", "hydrate", "dehydrate"];

    let Ok(tree) = parse_php(source) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "method_declaration" {
            let mut public = true; // PHP methods default to public
            let mut is_static = false;
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                match ch.kind() {
                    "visibility_modifier" => {
                        public = ch.utf8_text(bytes).ok() == Some("public");
                    }
                    "static_modifier" => is_static = true,
                    _ => {}
                }
            }
            if public && !is_static {
                if let Some(name) = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(bytes).ok())
                {
                    // A prefix only counts when the hook's target follows in
                    // camelCase (`updatedFooBar`) or the name IS the bare
                    // hook — a method that merely starts with the same
                    // letters (`updates`, `hydrateX` vs `hydraulics`) is a
                    // legitimate action.
                    let is_lifecycle_hook = |name: &str| {
                        LIFECYCLE_PREFIXES.iter().any(|p| {
                            name.strip_prefix(p).is_some_and(|rest| {
                                rest.is_empty() || rest.starts_with(char::is_uppercase)
                            })
                        })
                    };
                    let excluded = name.starts_with("__")
                        || LIFECYCLE.contains(&name)
                        || is_lifecycle_hook(name);
                    if !excluded {
                        out.push(name.to_string());
                    }
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out.sort();
    out
}

/// Whether `name` node's text, with a leading `$` stripped, equals `member`.
fn matches_dollar_name(name: Node, bytes: &[u8], member: &str) -> bool {
    name.utf8_text(bytes)
        .map(|t| t.trim_start_matches('$') == member)
        .unwrap_or(false)
}

/// Build a [`MemberLocation`] from a name node, skipping `dollar_skip`
/// columns off the start (1 for a `$name` property/param, 0 for a bare
/// method name).
fn node_location(node: Node, dollar_skip: u32, kind: MemberKind) -> MemberLocation {
    let start = node.start_position();
    let end = node.end_position();
    MemberLocation {
        line: start.row as u32,
        start_column: start.column as u32 + dollar_skip,
        end_column: end.column as u32,
        kind,
    }
}

#[cfg(test)]
mod tests;
