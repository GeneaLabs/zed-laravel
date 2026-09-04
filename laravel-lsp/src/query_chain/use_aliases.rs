//! Parse `use` statements in PHP source into an alias → fully-qualified-name
//! map.
//!
//! PHP class names are case-insensitive but the alias *name* (the local
//! identifier) is matched as-typed by source. We store keys as-typed.
//! Resolution helpers do case-insensitive lookups so `db` and `DB` both
//! resolve when an import says `use Foo\DB;`.
//!
//! Scope: top-level `use` statements (the `namespace_use_declaration` node).
//! Grouped uses (`use Foo\{Bar, Baz as B};`) are also handled. Function and
//! constant uses (`use function foo;`, `use const FOO;`) are ignored —
//! chains never receive functions or constants as their static scope.

use std::collections::HashMap;
use tree_sitter::{Node, Tree};

/// Map from the local name in source → the fully-qualified class name.
///
/// Examples:
/// - `use Illuminate\Support\Facades\DB;` → `"DB"` → `"Illuminate\Support\Facades\DB"`
/// - `use Illuminate\Support\Facades\DB as Database;` → `"Database"` → `"Illuminate\Support\Facades\DB"`
/// - `use App\Models\{User, Post as P};` → `"User"` → `"App\Models\User"`, `"P"` → `"App\Models\Post"`
pub type UseAliases = HashMap<String, String>;

/// Extract every `use` import in the file. Returns an empty map if no
/// imports exist or the file has parse errors.
pub fn extract_use_aliases(tree: &Tree, source: &str) -> UseAliases {
    let bytes = source.as_bytes();
    let mut aliases: UseAliases = HashMap::new();

    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "namespace_use_declaration" {
            collect_from_declaration(node, bytes, &mut aliases);
            // Don't recurse into the declaration — we've handled its children.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    aliases
}

/// Resolve a class reference as it appears in source to its FQCN, by
/// looking up the leading segment in the alias map. Falls back to the
/// original string when no match — Laravel's global aliases (the
/// `config/app.php` `aliases` array, which makes `\DB` etc. available
/// everywhere) are NOT in PHP's use-statement scope so they end up here.
///
/// Examples:
/// - `Database::table` with `Database → Illuminate\Support\Facades\DB` → `Illuminate\Support\Facades\DB`
/// - `\DB::table` (leading `\`) → `DB` (unchanged after stripping leading `\`)
/// - `DB::table` with no import → `DB` (unchanged; relies on global alias)
/// - `App\Foo::method` (no segment in map) → `App\Foo` (unchanged)
pub fn resolve_class_name(class: &str, aliases: &UseAliases) -> String {
    let stripped = class.trim_start_matches('\\');
    if let Some((head, rest)) = split_first_segment(stripped) {
        // Case-insensitive lookup against the map keys.
        for (alias, fqcn) in aliases {
            if alias.eq_ignore_ascii_case(head) {
                return if rest.is_empty() {
                    fqcn.clone()
                } else {
                    format!("{fqcn}\\{rest}")
                };
            }
        }
    }
    stripped.to_string()
}

/// Walk a `namespace_use_declaration` and add every clause to `aliases`.
///
/// AST shapes (from tree-sitter-php):
///
/// Flat: `use Foo\Bar as Baz;`
/// ```text
/// namespace_use_declaration
///   use
///   namespace_use_clause
///     qualified_name    <- the FQCN
///     as
///     name              <- the alias (only present with `as`)
/// ```
///
/// Grouped: `use Foo\{Bar, Baz as B};`
/// ```text
/// namespace_use_declaration
///   use
///   namespace_name      <- the shared prefix
///   namespace_use_group
///     namespace_use_clause
///       name            <- "Bar" (no alias)
///     namespace_use_clause
///       name            <- "Baz"
///       as
///       name            <- "B"
/// ```
///
/// Function/const: `use function foo;` — the `function` marker is INSIDE
/// the clause, not at declaration level.
fn collect_from_declaration(decl: Node, bytes: &[u8], aliases: &mut UseAliases) {
    // Find the (optional) prefix and (optional) group, scanning direct
    // children of the declaration.
    let mut prefix: Option<String> = None;
    let mut group: Option<Node> = None;
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        match child.kind() {
            "namespace_name" => prefix = node_text(child, bytes).map(String::from),
            "namespace_use_group" => group = Some(child),
            _ => {}
        }
    }

    // Clauses live inside the group when present, otherwise directly under
    // the declaration.
    let clauses_parent = group.unwrap_or(decl);
    let mut cursor = clauses_parent.walk();
    for clause in clauses_parent.children(&mut cursor) {
        if clause.kind() == "namespace_use_clause" {
            insert_clause(clause, bytes, prefix.as_deref(), aliases);
        }
    }
}

/// The class an import clause names: its fully-qualified name (with `prefix`
/// applied for a grouped import) and the AST node holding the name as written.
///
/// Shared by the alias map and the positioned [`php_use_class_refs`] so the two
/// agree on which clauses name a class at all. Returns `None` for `function` /
/// `const` imports, which bind no class.
fn clause_import<'a>(
    clause: Node<'a>,
    bytes: &[u8],
    prefix: Option<&str>,
) -> Option<(String, Node<'a>)> {
    // Collect children once so we can scan for both the function/const
    // modifier and the class name without re-walking.
    let mut cursor = clause.walk();
    let children: Vec<Node> = clause.children(&mut cursor).collect();

    if children
        .iter()
        .any(|c| matches!(c.kind(), "function" | "const"))
    {
        return None;
    }

    // The class name is the first `qualified_name` | `namespace_name` |
    // `name` child. `name` covers the single-identifier case inside grouped
    // imports (`{User, Post}`).
    let name_node = children
        .iter()
        .find(|c| matches!(c.kind(), "qualified_name" | "namespace_name" | "name"))
        .copied()?;
    let name_clean = node_text(name_node, bytes)?.trim_start_matches('\\');

    let fqcn = match prefix {
        Some(p) => format!("{p}\\{name_clean}"),
        None => name_clean.to_string(),
    };
    Some((fqcn, name_node))
}

/// Insert one `namespace_use_clause` into the alias map. `prefix` is the
/// shared prefix from a grouped use, if any.
///
/// Skips `function` / `const` imports — those don't bind classes, so chains
/// would never reference them as static receivers.
fn insert_clause(clause: Node, bytes: &[u8], prefix: Option<&str>, aliases: &mut UseAliases) {
    let Some((fqcn, _)) = clause_import(clause, bytes, prefix) else {
        return;
    };
    let mut cursor = clause.walk();
    let children: Vec<Node> = clause.children(&mut cursor).collect();

    // The alias name (if present) is the `name` node that comes AFTER an
    // `as` token among the clause's direct children. Walk in source order
    // so we don't mistake the class-name's `name` (in the grouped case) for
    // an alias.
    let mut alias: Option<String> = None;
    let mut saw_as = false;
    for child in &children {
        if child.kind() == "as" {
            saw_as = true;
            continue;
        }
        if saw_as && child.kind() == "name" {
            alias = node_text(*child, bytes).map(String::from);
            break;
        }
    }

    // No `as` — alias is the last segment of the FQCN.
    let alias = alias.unwrap_or_else(|| fqcn.rsplit('\\').next().unwrap_or(&fqcn).to_string());

    aliases.insert(alias, fqcn);
}

fn split_first_segment(class: &str) -> Option<(&str, &str)> {
    match class.find('\\') {
        Some(i) => Some((&class[..i], &class[i + 1..])),
        None if class.is_empty() => None,
        None => Some((class, "")),
    }
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(bytes.get(start..end)?).ok()
}

/// Extract the alias map a **Blade** file declares through `@use` directives.
///
/// A Blade template has no PHP `use` statements — its file-level imports are
/// written `@use('App\Support\Reader\VerseMarkerResolver')`, which the PHP
/// parser never sees. Without this, a short class name in a `@php` block or
/// `{{ }}` echo has no import to resolve against and falls back to a
/// basename guess.
///
/// Every documented form is supported, because each `@use` is rewritten into
/// the equivalent PHP statement and run through [`extract_use_aliases`] — one
/// parse for the whole file, and no second implementation of grouped-import or
/// `function`/`const` handling to drift out of step:
///
/// | Blade | Equivalent PHP |
/// |---|---|
/// | `@use('App\Models\Flight')` | `use App\Models\Flight;` |
/// | `@use('App\Models\Flight', 'FlightModel')` | `use App\Models\Flight as FlightModel;` |
/// | `@use('App\Models\{Flight, Airport}')` | `use App\Models\{Flight, Airport};` |
/// | `@use('function App\Helpers\fmt')` | `use function App\Helpers\fmt;` (skipped — not a class) |
///
/// Directives inside Blade or HTML comments are ignored.
pub fn blade_use_aliases(source: &str) -> UseAliases {
    let sites = blade_use_sites(source);
    if sites.is_empty() {
        return UseAliases::new();
    }

    let mut statements = String::from("<?php\n");
    for site in &sites {
        match site.alias.as_deref().filter(|a| !a.is_empty()) {
            Some(alias) => statements.push_str(&format!("use {} as {alias};\n", site.import)),
            None => statements.push_str(&format!("use {};\n", site.import)),
        }
    }
    match crate::parser::parse_php(&statements) {
        Ok(tree) => extract_use_aliases(&tree, &statements),
        Err(_) => UseAliases::new(),
    }
}

/// One Blade `@use` directive, located in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BladeUseSite {
    /// The first argument, unquoted and with `\\` collapsed. Still carries any
    /// `function ` / `const ` marker and group braces — callers decide what to
    /// do with those.
    pub import: String,
    /// The optional second argument (the explicit alias), unquoted.
    pub alias: Option<String>,
    /// Byte offset in `source` of the first argument's raw content — the first
    /// character INSIDE the opening quote.
    pub raw_start: usize,
    /// Byte offset one past the raw content's last character.
    pub raw_end: usize,
}

/// Every `@use` directive in Blade `source`, in source order.
///
/// The single scanner behind both the alias map and the class-rename span
/// finder, so the two can never disagree about which directives exist or what
/// they import. Directives inside Blade (`{{-- --}}`) or HTML comments are
/// skipped, as is a longer word that merely starts with `@use`.
pub fn blade_use_sites(source: &str) -> Vec<BladeUseSite> {
    // Cheap bail — most Blade files declare no imports at all.
    if !source.contains("@use") {
        return Vec::new();
    }
    let comments = crate::blade_directive_tokens::dead_region_spans(source);
    let mut out = Vec::new();

    for (at, _) in source.match_indices("@use") {
        if comments.iter().any(|&(s, e)| at >= s && at < e) {
            continue;
        }
        // `@@use('App\Models\Flight')` renders the literal text `@use(...)`
        // and compiles nothing, so it declares no alias.
        if crate::blade_directive_tokens::is_escaped_directive(source, at) {
            continue;
        }
        let args_from = at + "@use".len();
        let after = &source[args_from..];
        // `@used_by`-style word, or a bare `@use` with no argument list.
        let Some(open) = after.find('(').filter(|&i| after[..i].trim().is_empty()) else {
            continue;
        };
        // A class name can never contain `)`, so the first one closes the args.
        let Some(close) = after[open..].find(')') else {
            continue;
        };
        let args_start = args_from + open + 1;
        let args = &source[args_start..args_from + open + close];

        let parts = split_top_level_args(args);
        let Some(first) = parts.first() else {
            continue;
        };
        let import = unquote_blade_arg(first);
        if import.is_empty() {
            continue;
        }

        // Locate the raw content inside the quotes. `first` is the argument as
        // written (leading whitespace and quotes included), so the content
        // starts after whatever the unquoting stripped.
        let raw = first.as_str();
        let lead = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim();
        let quoted = matches!(trimmed.chars().next(), Some('\'' | '"'))
            && trimmed.len() >= 2
            && trimmed.ends_with(trimmed.chars().next().unwrap());
        let inner_start = args_start + lead + usize::from(quoted);
        let inner_end = inner_start + trimmed.len() - if quoted { 2 } else { 0 };

        out.push(BladeUseSite {
            import,
            alias: parts.get(1).map(|p| unquote_blade_arg(p)),
            raw_start: inner_start,
            raw_end: inner_end,
        });
    }

    out
}

/// One PHP `use` import, positioned on **the class name as written at that
/// site** — `App\Models\Flight` in `use App\Models\Flight;`, but just `Flight`
/// in `use App\Models\{Flight, Airport};` where that is all the clause spells.
///
/// Deliberately not the basename segment: this span is what find-references
/// highlights, and highlighting the text actually present reads correctly in
/// both shapes. The narrower span a rename rewrites is computed separately, at
/// rename time, by `class_rename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpUseImport {
    /// Fully-qualified name, group prefix applied, no leading `\`.
    pub fqcn: String,
    /// 0-based line of the name.
    pub line: u32,
    /// 0-based byte column of the name's first character.
    pub column: u32,
    /// 0-based byte column one past the name's last character.
    pub end_column: u32,
}

/// Every class-importing `use` statement in a PHP file, positioned.
///
/// `tree` must be a PHP parse of `source`; callers pass the full-file parse they
/// already hold. Grouped imports yield one entry per clause, each carrying the
/// prefix-applied FQCN and pointing at its own name inside the braces.
/// `function` / `const` imports bind no class and are skipped, matching
/// [`extract_use_aliases`].
///
/// An aliased import (`use App\Models\Flight as F;`) is positioned on `Flight`,
/// never on the alias — the alias is a local binding, not a reference to the
/// class's name.
pub fn php_use_class_refs(tree: &Tree, source: &str) -> Vec<PhpUseImport> {
    if !source.contains("use ") {
        return Vec::new();
    }
    let bytes = source.as_bytes();
    let mut out = Vec::new();

    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "namespace_use_declaration" {
            collect_positions_from_declaration(node, bytes, &mut out);
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    out.sort_by_key(|i| (i.line, i.column));
    out
}

/// Positioned counterpart to [`collect_from_declaration`] — same prefix/group
/// resolution, emitting spans instead of alias bindings.
fn collect_positions_from_declaration(decl: Node, bytes: &[u8], out: &mut Vec<PhpUseImport>) {
    let mut prefix: Option<String> = None;
    let mut group: Option<Node> = None;
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        match child.kind() {
            "namespace_name" => prefix = node_text(child, bytes).map(String::from),
            "namespace_use_group" => group = Some(child),
            _ => {}
        }
    }

    let clauses_parent = group.unwrap_or(decl);
    let mut cursor = clauses_parent.walk();
    for clause in clauses_parent.children(&mut cursor) {
        if clause.kind() != "namespace_use_clause" {
            continue;
        }
        let Some((fqcn, name_node)) = clause_import(clause, bytes, prefix.as_deref()) else {
            continue;
        };
        // A PHP qualified name can't contain whitespace, so the name node never
        // spans lines and its start/end columns describe one line.
        let start = name_node.start_position();
        let end = name_node.end_position();
        out.push(PhpUseImport {
            fqcn,
            line: start.row as u32,
            column: start.column as u32,
            end_column: end.column as u32,
        });
    }
}

/// One class named by a Blade `@use` directive, located exactly in the source.
///
/// A group import names several classes in one directive, so a single `@use`
/// can produce several of these — each pointing at its own member inside the
/// braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BladeUseImport {
    /// Fully-qualified name, group prefix applied, `\\` collapsed.
    pub fqcn: String,
    /// Byte span of the class name **as written at this site**: the whole
    /// `App\Models\Flight` for a flat import, or just `Flight` for a member of
    /// `App\Models\{Flight, Airport}`.
    pub name: (usize, usize),
    /// Byte span of the basename segment within [`Self::name`] — the part a
    /// class rename rewrites.
    pub basename: (usize, usize),
}

/// Every class a Blade file's `@use` directives import, with exact source spans.
///
/// Computing offsets from the source rather than anchoring to the end of the
/// import string is what lets group imports and padded imports participate:
/// `@use('App\Models\{Flight, Airport}')` yields one entry per member, and
/// `@use(' App\Models\Flight ')` locates the name inside the padding instead of
/// being skipped as untrustworthy.
///
/// `function` / `const` imports are skipped — they bind no class.
pub fn blade_use_imports(source: &str) -> Vec<BladeUseImport> {
    let mut out = Vec::new();
    for site in blade_use_sites(source) {
        let Some(raw) = source.get(site.raw_start..site.raw_end) else {
            continue;
        };
        let lead = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let base = site.raw_start + lead;

        let lowered = trimmed.to_ascii_lowercase();
        if lowered.starts_with("function ") || lowered.starts_with("const ") {
            continue;
        }

        match group_body(trimmed) {
            Some((prefix_len, body_start, body)) => {
                let prefix = normalize_separators(trimmed[..prefix_len].trim_end_matches('\\'));
                for (offset, member) in comma_segments(body) {
                    // `Foo as Bar` — only the class part is a reference.
                    let class_part = strip_alias(member);
                    let inner_lead = class_part.len() - class_part.trim_start().len();
                    let name = class_part.trim();
                    if name.is_empty() {
                        continue;
                    }
                    let name_start = base + body_start + offset + inner_lead;
                    push_import(&mut out, &prefix, name, name_start, source);
                }
            }
            None => push_import(&mut out, "", trimmed, base, source),
        }
    }
    out.sort_by_key(|i| i.name.0);
    out
}

/// Record one import. `prefix` is the group's shared namespace (empty for a
/// flat import); `name` is the class name as written, starting at `name_start`.
fn push_import(
    out: &mut Vec<BladeUseImport>,
    prefix: &str,
    name: &str,
    name_start: usize,
    source: &str,
) {
    let normalized = normalize_separators(name);
    if normalized.is_empty() {
        return;
    }
    let fqcn = if prefix.is_empty() {
        normalized
    } else {
        format!("{prefix}\\{normalized}")
    };

    // The basename starts after the last separator *in the written text*, so a
    // `\\`-escaped import lands on the same characters as a `\` one.
    let basename_offset = name.rfind('\\').map(|i| i + 1).unwrap_or(0);
    let name_end = name_start + name.len();
    let basename_start = name_start + basename_offset;

    // Defensive: a span that doesn't slice cleanly out of the source would
    // corrupt an edit. Drop the entry rather than emit it.
    if source.get(name_start..name_end).is_none() || basename_start > name_end {
        return;
    }
    out.push(BladeUseImport {
        fqcn,
        name: (name_start, name_end),
        basename: (basename_start, name_end),
    });
}

/// If `trimmed` is a group import, return `(prefix_len, body_start, body)` —
/// the length of the leading namespace, the offset of the brace body within
/// `trimmed`, and the body text between the braces.
fn group_body(trimmed: &str) -> Option<(usize, usize, &str)> {
    let open = trimmed.find('{')?;
    let close = trimmed.rfind('}')?;
    if close <= open {
        return None;
    }
    Some((open, open + 1, &trimmed[open + 1..close]))
}

/// Split `body` on commas, yielding each segment with its byte offset in `body`.
fn comma_segments(body: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, ch) in body.char_indices() {
        if ch == ',' {
            out.push((start, &body[start..i]));
            start = i + 1;
        }
    }
    out.push((start, &body[start..]));
    out
}

/// Drop a trailing ` as Alias` from a group member, leaving the class part.
fn strip_alias(member: &str) -> &str {
    let lowered = member.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(rel) = lowered[search..].find(" as ") {
        let at = search + rel;
        // Must be a real separator, not part of a longer identifier.
        let before_ok = member[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
        if before_ok {
            return &member[..at];
        }
        search = at + 1;
    }
    member
}

/// Collapse escaped namespace separators: `App\\Models\\X` and `App\Models\X`
/// name the same class.
fn normalize_separators(name: &str) -> String {
    name.replace("\\\\", "\\").trim().to_string()
}

/// Split a directive argument list on commas that sit outside quotes and
/// outside a `{…}` group import. Returns the raw, still-quoted arguments.
fn split_top_level_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut escaped = false;

    for ch in args.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote.is_some() => {
                current.push(ch);
                escaped = true;
            }
            '\'' | '"' => {
                match quote {
                    Some(q) if q == ch => quote = None,
                    None => quote = Some(ch),
                    _ => {}
                }
                current.push(ch);
            }
            '{' if quote.is_none() => {
                depth += 1;
                current.push(ch);
            }
            '}' if quote.is_none() => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if quote.is_none() && depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    out.push(current);
    out
}

/// Strip the surrounding quotes from one `@use` argument and collapse the
/// escaped namespace separators PHP would collapse (`'App\\Models\\X'` and
/// `'App\Models\X'` name the same class).
fn unquote_blade_arg(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = match trimmed.chars().next() {
        Some(q @ ('\'' | '"')) if trimmed.len() >= 2 && trimmed.ends_with(q) => {
            &trimmed[1..trimmed.len() - 1]
        }
        // Unquoted — Blade tolerates `@use(App\Models\Flight)`.
        _ => trimmed,
    };
    inner.replace("\\\\", "\\").trim().to_string()
}

#[cfg(test)]
mod tests;
