//! Helpers for locating the PHP source backing a Blade view's Livewire component.
//!
//! Livewire ships four discoverable component shapes the rename machinery
//! needs to distinguish:
//!
//!   - **V4 SFC** (single-file) — `⚡{leaf}.blade.php` containing an inline
//!     `new class extends Component`. The `⚡` filename prefix is the on-disk
//!     marker that disambiguates from Volt.
//!   - **V4 MFC** (multi-file) — `⚡{leaf}/` directory containing
//!     `{leaf}.php`, `{leaf}.blade.php`, and optional `.js` / `.css` /
//!     `.global.css` siblings. Livewire's discovery requires child basenames
//!     to match the emoji-stripped directory name; renaming the directory
//!     forces renaming every child.
//!   - **V3 Class-based** — a class file under `class_path` paired with a
//!     view under `view_path`. The v3 carry-over shape, still supported in
//!     v4 (`'make_command.type' => 'class'`).
//!   - **Volt** — a plain `{leaf}.blade.php` (no emoji) whose front-matter
//!     PHP block uses Volt's functional API (`state()`, `action()`,
//!     `computed()`, ...) or extends `Livewire\Volt\Component`.
//!
//! [`resolve_component`] picks the right shape for a given component name by
//! walking the configured locations / namespaces and returning the first
//! match. The lower-level helpers ([`mfc_sibling`], [`blade_contains_inline_class`])
//! are kept for hover/goto callers that don't need the full resolver.
//!
//! Pure path-based, side-effect-free filesystem checks — testable from a
//! tempdir without spinning up a Backend.

use std::path::{Path, PathBuf};

use crate::livewire_config::LivewireConfig;
use crate::livewire_version::LivewireVersion;
use crate::naming;

/// If the blade file has a sibling `.php` with the same stem and that sibling
/// contains an inline Livewire component class signature, return the sibling
/// path. Returns `None` if there is no sibling, the sibling is unreadable, or
/// the sibling exists but doesn't carry the signature.
pub fn mfc_sibling(blade_path: &Path) -> Option<PathBuf> {
    let name = blade_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".blade.php")?;
    let sibling = blade_path.with_file_name(format!("{}.php", stem));
    if !sibling.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&sibling).ok()?;
    if crate::php_class::detect_inline_livewire_class(&content) {
        Some(sibling)
    } else {
        None
    }
}

/// True when the blade file at `blade_path` contains an inline Livewire
/// `new class extends Component` declaration (single-file component pattern).
pub fn blade_contains_inline_class(blade_path: &Path) -> bool {
    std::fs::read_to_string(blade_path)
        .ok()
        .map(|content| crate::php_class::detect_inline_livewire_class(&content))
        .unwrap_or(false)
}

/// Given a line of text and a 0-based column position, identify what Blade
/// variable (and optional property access) the cursor is on. Returns:
///   - `("form", None)` for cursor anywhere on `$form`
///   - `("form", Some("name"))` for cursor on `name` in `$form->name`
///   - `("", None)` for cursor right after a bare `$` (used by `$` trigger completion)
///   - `None` if the cursor isn't on any `$variable` token
///
/// Used by the hover handler, the goto-definition fallback, and the `$`
/// trigger completion path.
pub fn extract_blade_variable_at_cursor(
    line: &str,
    cursor_col: u32,
) -> Option<(String, Option<String>)> {
    let cursor = cursor_col as usize;
    if cursor > line.len() {
        return None;
    }

    let bytes = line.as_bytes();

    // Walk back to find the start of the current identifier.
    let mut ident_start = cursor;
    while ident_start > 0 {
        let c = bytes[ident_start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_start -= 1;
        } else {
            break;
        }
    }

    // Walk forward to find the end of the current identifier.
    let mut ident_end = cursor;
    while ident_end < bytes.len() {
        let c = bytes[ident_end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            ident_end += 1;
        } else {
            break;
        }
    }

    if ident_start >= ident_end {
        // Cursor not on any identifier; handle the bare-`$` trigger case
        // (cursor sits immediately after a `$` with no identifier yet).
        if ident_start > 0 && bytes[ident_start - 1] == b'$' {
            return Some((String::new(), None));
        }
        return None;
    }

    let ident = &line[ident_start..ident_end];

    // Case A: cursor on the variable itself (preceded by `$`).
    if ident_start > 0 && bytes[ident_start - 1] == b'$' {
        return Some((ident.to_string(), None));
    }

    // Case B: cursor on a property name preceded by `->`. Walk back from
    // `ident_start` past `->` and look for the originating `$variable`.
    if ident_start >= 2 && &line[ident_start - 2..ident_start] == "->" {
        let mut probe = ident_start - 2;
        while probe > 0 {
            let c = bytes[probe - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                probe -= 1;
            } else {
                break;
            }
        }
        if probe < ident_start - 2 && probe > 0 && bytes[probe - 1] == b'$' {
            let var_name = &line[probe..ident_start - 2];
            return Some((var_name.to_string(), Some(ident.to_string())));
        }
    }

    None
}

/// The Livewire binding target of a `wire:` attribute, extracted from its
/// quoted value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireTarget {
    /// A method-call binding (`wire:click`, `wire:submit.prevent`,
    /// `wire:poll.2000ms`, ...) — the bare identifier before any `(`.
    Method(String),
    /// `wire:model[.modifiers]` binds a PROPERTY, not a method call — the
    /// target is the first dot-segment of the value (`contractData.title`
    /// → `contractData`).
    Property(String),
    /// `wire:target` names EITHER an action method (`wire:target="save"`)
    /// or, paired with `wire:model`'s loading states, a property. Livewire
    /// accepts both spellings for the same attribute, so the kind cannot be
    /// decided from the attribute alone and the member lookup must accept
    /// either declaration.
    Member(String),
}

/// If the cursor sits inside a `wire:*="value"` attribute's quoted value on
/// `line`, extract its goto/hover target.
///
/// Returns `None` when the cursor isn't inside such a value, or the value
/// isn't a resolvable PHP identifier (`wire:click="$wire.foo++"` has no
/// member target — a Blade/JS expression, not a bound method).
///
/// `cursor_col` is treated as a byte offset into `line`, matching
/// [`extract_blade_variable_at_cursor`]'s convention.
pub fn wire_attribute_target_at(line: &str, cursor_col: u32) -> Option<WireTarget> {
    let cursor = cursor_col as usize;
    // Same cursor guard as every other cursor site (alpine.rs,
    // method_name_completion.rs): the LSP position is not a trustworthy
    // byte index — past-the-end and mid-UTF-8-codepoint offsets both
    // arrive in practice, and slicing on them panics.
    if cursor > line.len() || !line.is_char_boundary(cursor) {
        return None;
    }
    let bytes = line.as_bytes();
    let mut search_from = 0usize;

    while let Some(rel) = line[search_from..].find("wire:") {
        let attr_start = search_from + rel;
        let mut i = attr_start + "wire:".len();
        // Attribute name, modifiers included (`wire:poll.2000ms`, `wire:model.live`).
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric()
                || bytes[i] == b'.'
                || bytes[i] == b'-'
                || bytes[i] == b'_')
        {
            i += 1;
        }
        let attr_name = &line[attr_start..i];
        search_from = i;

        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let Some(&quote) = bytes.get(j).filter(|b| **b == b'"' || **b == b'\'') else {
            continue;
        };
        let value_start = j + 1;
        let Some(rel_end) = line[value_start..].find(quote as char) else {
            continue;
        };
        let value_end = value_start + rel_end;
        search_from = (value_end + 1).min(line.len());

        if cursor < value_start || cursor > value_end {
            continue;
        }
        let value = &line[value_start..value_end];
        let base = attr_name.strip_prefix("wire:")?.split('.').next()?;
        return match wire_value_kind(base) {
            Some(WireValueKind::Property) => {
                let prop = value.split('.').next().unwrap_or("").trim();
                is_php_identifier(prop).then(|| WireTarget::Property(prop.to_string()))
            }
            Some(WireValueKind::Method) => {
                let ident = value.split('(').next().unwrap_or("").trim();
                is_php_identifier(ident).then(|| WireTarget::Method(ident.to_string()))
            }
            Some(WireValueKind::Member) => {
                let ident = comma_segment_at(value, cursor - value_start)?.trim();
                is_php_identifier(ident).then(|| WireTarget::Member(ident.to_string()))
            }
            None => None,
        };
    }

    None
}

/// What kind of component member a `wire:{base}` attribute's value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireValueKind {
    /// The value is an action — a public method (`wire:click`, `wire:poll`,
    /// `wire:submit`, any DOM-event binding).
    Method,
    /// The value binds a public property (`wire:model`, `wire:show`,
    /// `wire:text`).
    Property,
    /// The value names a member of EITHER kind — `wire:target`, which takes
    /// an action name or a `wire:model` property depending on the loading
    /// state it scopes.
    Member,
}

/// Classify a `wire:` attribute's base name (modifiers stripped) by what its
/// value names. `None` for attributes whose value is not a component member
/// at all (`wire:key`, `wire:ignore`, ...). Everything not explicitly listed
/// is treated as a DOM-event action binding, since Livewire accepts
/// `wire:{any-dom-event}`.
///
/// `target` is [`WireValueKind::Member`], not `None`: its value names the
/// action(s) or property a loading state is scoped to, and both are
/// navigable members of the component.
fn wire_value_kind(base: &str) -> Option<WireValueKind> {
    match base {
        "model" | "show" | "text" => Some(WireValueKind::Property),
        "target" => Some(WireValueKind::Member),
        "key" | "id" | "ignore" | "loading" | "dirty" | "offline" | "stream" | "replace"
        | "transition" | "navigate" | "cloak" | "current" | "confirm" => None,
        _ => Some(WireValueKind::Method),
    }
}

/// The comma-separated segment of `value` that contains byte offset `at`.
///
/// `wire:target="save, delete"` is a legal Livewire list, so the cursor
/// decides which entry goto resolves. A cursor on the comma itself belongs
/// to the segment on its left. `None` when `at` is past the value.
fn comma_segment_at(value: &str, at: usize) -> Option<&str> {
    if at > value.len() {
        return None;
    }
    let mut start = 0usize;
    for (i, b) in value.bytes().enumerate() {
        if b == b',' {
            if at <= i {
                return Some(&value[start..i]);
            }
            start = i + 1;
        }
    }
    Some(&value[start..])
}

/// If the cursor sits inside a `wire:*="…"` quoted value on `line`, return
/// the completion context: what member kind the attribute binds and the
/// (possibly empty) identifier prefix typed before the cursor.
///
/// Unlike [`wire_attribute_target_at`] this accepts empty and partial
/// values — that's the completion moment. `None` when the typed text is
/// already something other than a plain identifier (a JS expression, a
/// nested `wire:model` path past its first segment), so richer expressions
/// fall through to other completion providers.
pub fn wire_attribute_completion_context(
    line: &str,
    cursor_col: u32,
) -> Option<(WireValueKind, String)> {
    let cursor = cursor_col as usize;
    // See wire_attribute_target_at — identical cursor guard, identical
    // reason.
    if cursor > line.len() || !line.is_char_boundary(cursor) {
        return None;
    }
    let bytes = line.as_bytes();
    let mut search_from = 0usize;

    while let Some(rel) = line[search_from..].find("wire:") {
        let attr_start = search_from + rel;
        let mut i = attr_start + "wire:".len();
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric()
                || bytes[i] == b'.'
                || bytes[i] == b'-'
                || bytes[i] == b'_')
        {
            i += 1;
        }
        let attr_name = &line[attr_start..i];
        search_from = i;

        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let Some(&quote) = bytes.get(j).filter(|b| **b == b'"' || **b == b'\'') else {
            continue;
        };
        let value_start = j + 1;
        // The closing quote may not be typed yet — treat end-of-line as the
        // value end in that case.
        let value_end = line[value_start..]
            .find(quote as char)
            .map(|rel_end| value_start + rel_end)
            .unwrap_or(line.len());
        // An unclosed value ends AT line.len(); the resume point must not
        // step past it — `line[search_from..]` on len()+1 panics.
        search_from = (value_end + 1).min(line.len());

        if cursor < value_start || cursor > value_end {
            continue;
        }
        let kind = wire_value_kind(attr_name.strip_prefix("wire:")?.split('.').next()?)?;
        // A `wire:target` list completes per entry, so only the segment the
        // cursor sits in is the typed prefix — `save, del|` offers `delete`,
        // not nothing.
        let typed = match kind {
            WireValueKind::Member => line[value_start..cursor]
                .rsplit(',')
                .next()
                .unwrap_or("")
                .trim(),
            _ => line[value_start..cursor].trim(),
        };
        let acceptable = match kind {
            // Actions are a single identifier; a `wire:target` entry is too.
            WireValueKind::Method | WireValueKind::Member => {
                typed.is_empty() || is_php_identifier(typed)
            }
            // Bindings may be a dotted path into a nested object
            // (`wire:model="contractData.title"`): every completed segment
            // must be an identifier, the segment under the cursor may be
            // empty (right after a dot) or partial.
            WireValueKind::Property => is_property_path_prefix(typed),
        };
        return if acceptable {
            Some((kind, typed.to_string()))
        } else {
            None
        };
    }

    None
}

/// A (possibly unfinished) dotted property path: `a`, `a.b`, `a.` or an
/// empty string. Every segment before the last must be an identifier; the
/// last may be empty (cursor right after a dot) or a partial identifier.
fn is_property_path_prefix(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let segments: Vec<&str> = s.split('.').collect();
    let (last, completed) = segments.split_last().unwrap();
    completed.iter().all(|seg| is_php_identifier(seg))
        && (last.is_empty() || is_php_identifier(last))
}

/// A bare PHP identifier: `[A-Za-z_][A-Za-z0-9_]*`, nothing else. Used to
/// reject `wire:` values that are JS/Blade expressions rather than a plain
/// method or property name (`$wire.foo++`, `save() && close()`).
fn is_php_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ============================================================================
// Component resolution (Phase 3)
// ============================================================================

/// The on-disk shape of a discovered Livewire component. Phase 3 rename
/// dispatches on this — each kind drives a different rewriter (SFC moves one
/// file, MFC moves a directory plus N children, V3 moves a class + view,
/// Volt moves one view file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivewireComponentKind {
    V4Sfc,
    V4Mfc,
    V3Class,
    Volt,
}

/// A resolved Livewire component — the kind plus every file that belongs to
/// it. `paths` is what rename consumes: every entry is either a candidate for
/// a `RenameFile` op or (for V3Class) a class file whose `class X extends
/// Component` declaration also needs an in-file `TextEdit`.
///
/// For V4 MFC the first entry is the directory itself; child files follow.
/// Rename emits a `RenameFile` for each in order (directory first so the
/// child paths in subsequent ops are relative to the new dir name on
/// clients that apply operations sequentially).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivewireComponent {
    pub kind: LivewireComponentKind,
    pub paths: Vec<PathBuf>,
}

/// Resolve a Livewire component tag name (e.g. `admin.user-list` or
/// `pages::dashboard`) to the concrete on-disk component, if any.
///
/// Returns `None` when the name doesn't match anything Livewire would
/// actually discover at runtime. The caller (Phase 3c rename, Phase 3d
/// file-rename, hover, goto-definition) then gives up gracefully.
///
/// Resolution order, mirroring Livewire 4's discovery preference:
///   1. V4 SFC — `⚡{leaf}.blade.php` under each candidate base
///   2. V4 MFC — `⚡{leaf}/` directory with the required `{leaf}.php` child
///   3. Volt (signature) — `{leaf}.blade.php` carrying a Volt front-matter
///      signature, under any component location
///   4. V3 Class — `{class_path}/{Pascal}/{Pascal}.php` (skipped when the
///      name is namespaced — class lookups don't honor `<livewire:pages::...>`)
///   5. Volt (anonymous) — a signature-less `{leaf}.blade.php` directly under
///      the Volt mount root (`view_path`) with no backing class. Checked last
///      so a class-based component's companion view isn't mistaken for a
///      standalone Volt component.
///
/// V3 projects (per `version`) skip the V4 SFC/MFC checks but still try Volt
/// (which ships on Livewire 3 too) and class-based resolution. Unknown-version
/// projects try everything — better to over-discover than to miss a component.
pub fn resolve_component(
    name: &str,
    config: &LivewireConfig,
    version: LivewireVersion,
) -> Option<LivewireComponent> {
    let (namespace, bare) = split_namespace(name);
    let segments: Vec<&str> = bare.split('.').collect();
    let leaf = *segments.last()?;
    if leaf.is_empty() {
        return None;
    }
    // Every segment becomes a path component: the parents through
    // `parents_to_path` (which uses `PathBuf::push`, and an ABSOLUTE segment
    // replaces the whole path), the leaf as a file stem. A name is discovered
    // data — it comes from a `<livewire:…>` tag or an `@livewire('…')`
    // literal — so one carrying its own path syntax must not become a path.
    // This gate covers every branch below; the class branch is additionally
    // gated by `dotted_to_class_path`, which checks the CONVERTED segments.
    if !segments
        .iter()
        .all(|seg| crate::naming::is_safe_path_segment(seg))
    {
        return None;
    }
    let parents = &segments[..segments.len() - 1];
    let sub = parents_to_path(parents);

    let base_dirs: Vec<&PathBuf> = match namespace {
        Some(ns) => {
            // A namespace may map to a view directory (config
            // `component_namespaces`), a registered class namespace
            // (`Livewire::addNamespace` — checked in the class fallback
            // below), or both. Unknown namespaces resolve to nothing.
            let view_dir = config.component_namespaces.get(ns);
            if view_dir.is_none() && !config.class_namespaces.contains_key(ns) {
                return None;
            }
            view_dir.into_iter().collect()
        }
        None => config.component_locations.iter().collect(),
    };

    let try_v4 = matches!(version, LivewireVersion::V4 | LivewireVersion::Unknown);

    for base in &base_dirs {
        let parent_dir = if sub.as_os_str().is_empty() {
            (*base).clone()
        } else {
            base.join(&sub)
        };

        // SFC and MFC are the v4-only `⚡`-prefixed shapes.
        if try_v4 {
            if let Some(c) = try_v4_sfc(&parent_dir, leaf) {
                return Some(c);
            }
            if let Some(c) = try_v4_mfc(&parent_dir, leaf) {
                return Some(c);
            }
        }

        // Volt ships on both Livewire 3 and 4, so it isn't gated on `version`.
        // A signature-bearing `.blade.php` is an unambiguous standalone Volt
        // component anywhere a component location points.
        if let Some(c) = try_volt(&parent_dir, leaf, true) {
            return Some(c);
        }
    }

    // Class-based fallback. Un-namespaced names use the global class_path;
    // a namespaced name resolves through a class namespace registered via
    // `Livewire::addNamespace(...)` when one exists (see
    // [`crate::livewire_namespaces`]).
    match namespace {
        None => {
            if let Some(c) = try_v3_class(bare, config) {
                return Some(c);
            }
        }
        Some(ns) => {
            if let Some(reg) = config.class_namespaces.get(ns) {
                if let Some(c) = try_namespaced_class(bare, reg) {
                    return Some(c);
                }
            }
        }
    }

    // Anonymous Volt fallback (issue #250). Volt auto-mounts every `.blade.php`
    // under `view_path` (default `resources/views/livewire`) as a component,
    // including anonymous ones with no class and no functional-API signature.
    // This runs *after* the v3-class check so that a signature-less blade which
    // is actually a class-based component's companion view stays attached to its
    // class rather than being mistaken for a standalone Volt component. Only
    // un-namespaced names apply — Volt mounts `view_path`, which the namespace
    // map doesn't cover.
    if namespace.is_none() {
        let parent_dir = if sub.as_os_str().is_empty() {
            config.view_path.clone()
        } else {
            config.view_path.join(&sub)
        };
        if let Some(c) = try_volt(&parent_dir, leaf, false) {
            return Some(c);
        }
    }

    None
}

/// Reverse of [`resolve_component`]: given a component file path, return the
/// Livewire component name it backs (`counter`, `admin.user-list`,
/// `pages::dashboard`), or `None` if the path isn't a Livewire component.
///
/// Works by *guess and verify*: derive candidate names from the path under the
/// configured roots (class path, component locations, namespace dirs), then
/// confirm each by running [`resolve_component`] forward and checking it points
/// back at this file. Every shape/convention nuance (v3 class, v4 SFC/MFC,
/// Volt, `⚡` prefixes, kebab-casing) stays in the forward resolver — a wrong
/// guess simply fails verification, so this never returns a bogus name (at
/// worst it returns `None` and the caller shows no lens).
pub fn livewire_name_for_path(
    path: &Path,
    config: &LivewireConfig,
    version: LivewireVersion,
) -> Option<String> {
    let target = crate::route_discovery::normalize_path(path);
    for name in candidate_livewire_names(path, config) {
        if let Some(component) = resolve_component(&name, config, version) {
            if component
                .paths
                .iter()
                .any(|p| crate::route_discovery::normalize_path(p) == target)
            {
                return Some(name);
            }
        }
    }
    None
}

/// Candidate component names for `path`, one per configured root it falls
/// under. Over-generation is safe — [`livewire_name_for_path`] verifies each.
fn candidate_livewire_names(path: &Path, config: &LivewireConfig) -> Vec<String> {
    let mut out = Vec::new();
    let is_blade = path.to_string_lossy().ends_with(".blade.php");
    let is_php = !is_blade && path.extension().and_then(|e| e.to_str()) == Some("php");

    // V3 class: a non-blade `.php` under the class path → kebab-dotted class
    // path relative to the root.
    if is_php {
        if let Ok(rel) = path.strip_prefix(&config.class_path) {
            if let Some(stem) = rel.to_str().and_then(|s| s.strip_suffix(".php")) {
                if let Some(name) = kebab_dotted(stem.split(['/', '\\']), "") {
                    out.push(name);
                }
            }
        }
        // Registered class namespaces (`Livewire::addNamespace`) — same
        // shape, prefixed with the namespace.
        for (ns, reg) in &config.class_namespaces {
            if let Ok(rel) = path.strip_prefix(&reg.class_path) {
                if let Some(stem) = rel.to_str().and_then(|s| s.strip_suffix(".php")) {
                    if let Some(name) = kebab_dotted(stem.split(['/', '\\']), "") {
                        out.push(format!("{ns}::{name}"));
                    }
                }
            }
        }
    }

    // V4 SFC / MFC / Volt under a component location (+ namespaced variants).
    for loc in &config.component_locations {
        if let Ok(rel) = path.strip_prefix(loc) {
            if let Some(name) = name_from_component_rel(rel, is_blade) {
                out.push(name);
            }
        }
    }
    for (ns, dir) in &config.component_namespaces {
        if let Ok(rel) = path.strip_prefix(dir) {
            if let Some(name) = name_from_component_rel(rel, is_blade) {
                out.push(format!("{ns}::{name}"));
            }
        }
    }
    out
}

/// Derive a component name from a path relative to a component location.
///   - file inside a `⚡leaf/` dir (MFC, `.php` or `.blade.php`) → the `⚡leaf`
///     dir supplies the leaf, the trailing file is dropped.
///   - `[⚡]leaf.blade.php` (SFC or Volt) → dir segments + emoji-stripped leaf.
fn name_from_component_rel(rel: &Path, is_blade: bool) -> Option<String> {
    let s = rel.to_str()?;
    let segs: Vec<&str> = s.split(['/', '\\']).collect();
    if segs.is_empty() {
        return None;
    }
    // MFC: the file lives inside a `⚡leaf/` directory.
    if segs.len() >= 2 && naming::has_emoji(segs[segs.len() - 2]) {
        let leaf_dir = segs[segs.len() - 2];
        return kebab_dotted(segs[..segs.len() - 2].iter().copied(), leaf_dir);
    }
    // SFC / Volt: a `.blade.php` file directly under the location tree.
    if is_blade {
        let (last, parents) = segs.split_last()?;
        let leaf = last.strip_suffix(".blade.php").unwrap_or(last);
        return kebab_dotted(parents.iter().copied(), leaf);
    }
    None
}

/// Kebab-case each segment (PascalCase or emoji-prefixed) and dot-join. When
/// `leaf` is empty the last `parents` segment is treated as the leaf (used for
/// the class-path form where the whole relative path is segments).
fn kebab_dotted<'a>(parents: impl Iterator<Item = &'a str>, leaf: &str) -> Option<String> {
    let mut parts: Vec<String> = parents
        .map(|p| naming::pascal_to_kebab(naming::strip_emoji(p)))
        .collect();
    if !leaf.is_empty() {
        parts.push(naming::pascal_to_kebab(naming::strip_emoji(leaf)));
    }
    parts.retain(|p| !p.is_empty());
    (!parts.is_empty()).then(|| parts.join("."))
}

// ---------- format-specific lookups ----------

fn try_v4_sfc(parent_dir: &Path, leaf: &str) -> Option<LivewireComponent> {
    let candidate = parent_dir.join(format!("{}{}.blade.php", naming::LIVEWIRE_EMOJI, leaf));
    if candidate.is_file() {
        return Some(LivewireComponent {
            kind: LivewireComponentKind::V4Sfc,
            paths: vec![candidate],
        });
    }
    None
}

fn try_v4_mfc(parent_dir: &Path, leaf: &str) -> Option<LivewireComponent> {
    let dir = parent_dir.join(format!("{}{}", naming::LIVEWIRE_EMOJI, leaf));
    if !dir.is_dir() {
        return None;
    }
    let class_file = dir.join(format!("{}.php", leaf));
    if !class_file.is_file() {
        // Bare directory without the required class file — not an MFC.
        return None;
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::V4Mfc,
        paths: mfc_paths(&dir, leaf),
    })
}

/// Resolve a Volt component blade file (`{leaf}.blade.php`) under `parent_dir`.
///
/// When `require_signature` is true the file must carry a Volt front-matter
/// signature (a functional-API call or `Volt\Component`). That guard applies to
/// general component locations, where a signature-less `.blade.php` is an
/// anonymous *Blade* component — not Volt. Under the Volt mount root
/// (`view_path`) callers pass `false`: Volt auto-discovers every `.blade.php`
/// there as a component, including anonymous ones with no class and no
/// functional-API call (issue #250).
fn try_volt(parent_dir: &Path, leaf: &str, require_signature: bool) -> Option<LivewireComponent> {
    let candidate = parent_dir.join(format!("{}.blade.php", leaf));
    if !candidate.is_file() {
        return None;
    }
    if require_signature && !blade_contains_volt_signature(&candidate) {
        return None;
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::Volt,
        paths: vec![candidate],
    })
}

/// Class lookup for a namespaced name registered via
/// `Livewire::addNamespace` — `{class_path}/{Pascal}.php`, dotted parents
/// mapping to subdirectories exactly like the global class path.
fn try_namespaced_class(
    bare: &str,
    reg: &crate::livewire_namespaces::LivewireClassNamespace,
) -> Option<LivewireComponent> {
    let class_file = reg
        .class_path
        .join(naming::dotted_to_class_path(bare)?)
        .with_extension("php");
    if !class_file.is_file() {
        return None;
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::V3Class,
        paths: vec![class_file],
    })
}

fn try_v3_class(bare: &str, config: &LivewireConfig) -> Option<LivewireComponent> {
    let class_path = config
        .class_path
        .join(naming::dotted_to_class_path(bare)?)
        .with_extension("php");
    if !class_path.is_file() {
        return None;
    }
    let mut paths = vec![class_path];
    // Companion view file — kebab path under view_path. Optional: a class-
    // based component can return its own view via render(), in which case
    // there's no canonical view file. We include the conventional one when
    // it exists so rename catches it.
    let view_file = config
        .view_path
        .join(bare.replace('.', "/"))
        .with_extension("blade.php");
    if view_file.is_file() {
        paths.push(view_file);
    }
    Some(LivewireComponent {
        kind: LivewireComponentKind::V3Class,
        paths,
    })
}

// ---------- helpers ----------

fn split_namespace(name: &str) -> (Option<&str>, &str) {
    if let Some(pos) = name.find("::") {
        (Some(&name[..pos]), &name[pos + 2..])
    } else {
        (None, name)
    }
}

fn parents_to_path(parents: &[&str]) -> PathBuf {
    let mut p = PathBuf::new();
    for seg in parents {
        p.push(seg);
    }
    p
}

/// Enumerate the files inside an MFC directory in the order rename should
/// emit them: the directory itself first, then each child basename that
/// exists. Mirrors Livewire's `MultiFileParser::parse` expectations — class,
/// view, optional js, optional css, optional global.css.
fn mfc_paths(dir: &Path, leaf: &str) -> Vec<PathBuf> {
    let mut paths = vec![dir.to_path_buf()];
    for ext in MFC_CHILD_EXTENSIONS {
        let child = dir.join(format!("{}.{}", leaf, ext));
        if child.is_file() {
            paths.push(child);
        }
    }
    paths
}

const MFC_CHILD_EXTENSIONS: &[&str] = &["php", "blade.php", "js", "css", "global.css", "test.php"];

/// True when the Blade file's front-matter PHP block carries a Volt
/// signature — either an explicit `Livewire\Volt\Component` import/extends,
/// or a bare functional-API call (`state()`, `action()`, `computed()`,
/// `mount()`, `usesPagination()`, ...). Permissive by design — false
/// positives are harmless (we'd treat a Volt-like file as Volt) while
/// false negatives would silently drop the file from rename coverage.
pub fn blade_contains_volt_signature(blade_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(blade_path) else {
        return false;
    };
    source_contains_volt_signature(&content)
}

/// Same Volt-signature check as [`blade_contains_volt_signature`] but on
/// already-read source — lets callers that already hold the file contents avoid
/// a second read.
pub fn source_contains_volt_signature(content: &str) -> bool {
    let window = front_matter_window(content);
    if window.contains("Volt\\Component") || window.contains("volt\\component") {
        return true;
    }
    VOLT_FUNCTIONAL_CALLS
        .iter()
        .any(|needle| window.contains(needle))
}

/// Volt files put their PHP in a front-matter block — usually the first few
/// dozen lines. Scanning only that window keeps the check cheap and avoids
/// matching the same call words inside the Blade body.
fn front_matter_window(content: &str) -> &str {
    const WINDOW_BYTES: usize = 4096;
    let end = WINDOW_BYTES.min(content.len());
    // Snap `end` back to a UTF-8 char boundary so we never slice mid-codepoint.
    let mut adjusted = end;
    while adjusted > 0 && !content.is_char_boundary(adjusted) {
        adjusted -= 1;
    }
    &content[..adjusted]
}

/// True when `var` (name without the `$`) is bound LOCALLY in the template
/// at 0-based `line`, so a bare-`$var` reference there must not navigate to a
/// backing-class property — the local binding shadows it.
///
/// Local binding sources, per Blade's own scoping:
/// - an ENCLOSING `@foreach` / `@forelse` loop variable (`as $item`,
///   `as $key => $item`) — out of scope again after the matching
///   `@endforeach` / `@endforelse`;
/// - Blade's `$loop`, inside any `@foreach` / `@forelse`;
/// - an ENCLOSING `@for` init variable (`@for ($i = 0; …)`);
/// - a `@php` assignment (block or inline `@php($x = …)`) — PHP locals
///   persist in the compiled template's scope, so the binding holds from
///   that line to the end of the file;
/// - a `@props([...])` / `@aware([...])` component-prop declaration —
///   file-wide.
///
/// Line-granular by design: a binding and a use on the same line count as
/// in scope (`@foreach ($users as $user)` — cursor on `$user`).
pub fn is_template_local_binding(content: &str, line: u32, var: &str) -> bool {
    // Blade (`{{-- --}}`) and HTML (`<!-- -->`) comments never execute, so a
    // directive inside one binds nothing — a commented-out `@foreach` is the
    // routine debugging move, and without this it would shadow its loop
    // variable for the rest of the file (a single-line comment leaves no
    // `@endforeach` to pop). Blanked rather than removed so line numbers
    // stay stable. A directive in ordinary prose is deliberately still
    // honored: Blade genuinely compiles those (that is what `@@` escaping
    // is for), so #351's third shape is out of scope here by design.
    let content = crate::blade_directive_tokens::blank_dead_regions(content);
    let mut loop_stack: Vec<Vec<String>> = Vec::new();
    let mut for_stack: Vec<Vec<String>> = Vec::new();
    let mut persistent: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_php_block = false;

    let mut line_start = 0usize;
    for (idx, text) in content.lines().enumerate() {
        // Byte offset of this line inside `content`. `lines()` strips the
        // terminator, so step over whichever of `\n` / `\r\n` follows.
        // `@props`/`@aware` need it: their array literal may run past the end
        // of this line, and the directive's arguments are read from `content`
        // rather than from `text` alone.
        let this_line_start = line_start;
        line_start += text.len();
        if content[line_start..].starts_with("\r\n") {
            line_start += 2;
        } else if content[line_start..].starts_with('\n') {
            line_start += 1;
        }
        if idx > line as usize {
            break;
        }
        let mut scan = text;

        // `@php($x = …)` inline form first — it must not toggle block state.
        if let Some(rest) = find_directive(scan, "@php") {
            if rest.trim_start().starts_with('(') {
                collect_assignments(rest, &mut persistent);
            } else {
                in_php_block = true;
                scan = rest;
            }
        }
        if in_php_block {
            collect_assignments(scan, &mut persistent);
        }
        if scan.contains("@endphp") {
            in_php_block = false;
        }

        if let Some(rest) =
            find_directive(text, "@props").or_else(|| find_directive(text, "@aware"))
        {
            // `rest` is a suffix of `text`, and `text` is a slice of
            // `content`, so the directive's argument list starts here in the
            // whole document — which is what lets a multi-line
            // `@props([\n    'color' => 'blue',\n])` be read to its close.
            let args_at = this_line_start + (text.len() - rest.len());
            collect_prop_names(&content[args_at..], &mut persistent);
        }

        if let Some(rest) =
            find_directive(text, "@foreach").or_else(|| find_directive(text, "@forelse"))
        {
            loop_stack.push(loop_binding_names(rest));
        }
        if let Some(rest) = find_directive(text, "@for") {
            // `@for` only — `@foreach`/`@forelse` were consumed above and
            // find_directive requires a non-identifier char after the name.
            let mut names = Vec::new();
            collect_assignment_names(rest, &mut names);
            for_stack.push(names);
        }

        if idx == line as usize {
            break;
        }
        if text.contains("@endforeach") || text.contains("@endforelse") {
            loop_stack.pop();
        }
        if text.contains("@endfor")
            && !text.contains("@endforeach")
            && !text.contains("@endforelse")
        {
            for_stack.pop();
        }
    }

    if var == "loop" && !loop_stack.is_empty() {
        return true;
    }
    persistent.contains(var)
        || loop_stack
            .iter()
            .any(|names| names.iter().any(|n| n == var))
        || for_stack.iter().any(|names| names.iter().any(|n| n == var))
}

/// The text following `@{name}` on `line`, when the directive occurs with a
/// non-identifier character (or end of line) right after it — so `@for`
/// never matches inside `@foreach`.
fn find_directive<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(rel) = line[from..].find(name) {
        let at = from + rel;
        let after = at + name.len();
        let boundary = line[after..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if boundary {
            return Some(&line[after..]);
        }
        from = after;
    }
    None
}

/// The variables an `@foreach` / `@forelse` head binds: everything after
/// `as` — `$item`, or `$key => $item`.
fn loop_binding_names(rest: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(pos) = rest.find(" as ") {
        collect_dollar_names(&rest[pos + 4..], &mut names);
    }
    names
}

/// Every `$name` token in `s`.
fn collect_dollar_names(s: &str, out: &mut Vec<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                out.push(s[start..end].to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
}

/// Every `$name` that is being ASSIGNED (`$name =`, not `==`/`=>`/`<=` etc.)
/// in `s`, appended to `out`.
fn collect_assignment_names(s: &str, out: &mut Vec<String>) {
    let mut names = Vec::new();
    collect_dollar_names(s, &mut names);
    for name in names {
        // Find `$name` followed (after whitespace) by a single `=`.
        let needle = format!("${name}");
        let mut from = 0;
        while let Some(rel) = s[from..].find(&needle) {
            let after = from + rel + needle.len();
            let rest = s[after..].trim_start();
            let assigns = rest.starts_with('=')
                && !rest.starts_with("==")
                && !rest.starts_with("=>")
                && !rest.starts_with("===");
            if assigns {
                out.push(name.clone());
                break;
            }
            from = after;
        }
    }
}

/// [`collect_assignment_names`] into a set.
fn collect_assignments(s: &str, out: &mut std::collections::HashSet<String>) {
    let mut names = Vec::new();
    collect_assignment_names(s, &mut names);
    out.extend(names);
}

/// Every prop NAME declared by a `@props([...])` / `@aware([...])`
/// directive whose argument list starts at `after_directive` (the text
/// immediately after the directive name, in the whole document — the list
/// may span several lines).
///
/// Only array KEYS are prop names: `@props(['color' => 'blue'])` declares
/// `$color`, never `$blue`. An entry with no `=>` is a prop declared without
/// a default (`@props(['color'])`), so its own value is the name. Scanning
/// ends at the directive's closing `)`, so unrelated quoted text later on the
/// same line (`@props([...]) <div title="literal">`) contributes nothing.
fn collect_prop_names(after_directive: &str, out: &mut std::collections::HashSet<String>) {
    let Some(args) = balanced_call_args(after_directive) else {
        return;
    };
    for entry in top_level_entries(array_body(args)) {
        if let Some(name) = entry_key_name(entry) {
            out.insert(name);
        }
    }
}

/// The text between the outer parentheses of the call that starts at the
/// first non-whitespace character of `s`, which must be `(`. Quoted runs are
/// skipped, so a `)` inside a string does not close the list. `None` when `s`
/// does not open a call, or the parentheses never balance.
fn balanced_call_args(s: &str) -> Option<&str> {
    let open = s.find(|c: char| !c.is_whitespace())?;
    let bytes = s.as_bytes();
    if bytes[open] != b'(' {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    // Skip the escaped byte; every byte compared here is
                    // ASCII, so landing inside a multi-byte codepoint is
                    // harmless (continuation bytes are >= 0x80).
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&s[open + 1..i]);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// The element list inside an array literal argument: the body of `[...]` or
/// of `array(...)`. Anything else is returned unchanged, so a non-array
/// argument simply yields one entry.
fn array_body(args: &str) -> &str {
    let t = args.trim();
    if let Some(inner) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return inner;
    }
    if let Some(rest) = t.strip_prefix("array") {
        let r = rest.trim_start();
        if let Some(inner) = r.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            return inner;
        }
    }
    t
}

/// Split `body` on its TOP-LEVEL commas — commas nested in a sub-array, a
/// call, or a quoted string stay inside their entry.
fn top_level_entries(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    out.push(&body[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    out.push(&body[start..]);
    out
}

/// The prop name an array entry declares: the quoted key before a top-level
/// `=>`, or the entry itself when it has no `=>`. `None` for an entry that is
/// not a quoted identifier (a numeric key, a constant, a spread).
fn entry_key_name(entry: &str) -> Option<String> {
    let key = match top_level_arrow(entry) {
        Some(at) => &entry[..at],
        None => entry,
    };
    let key = key.trim();
    let mut chars = key.chars();
    let quote = chars.next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = key.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut inner_chars = inner.chars();
    let first = inner_chars.next()?;
    (first.is_ascii_alphabetic() || first == '_')
        .then(|| inner.to_string())
        .filter(|_| inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// The byte offset of a top-level `=>` in `entry`, skipping quoted runs and
/// nested brackets.
fn top_level_arrow(entry: &str) -> Option<usize> {
    let bytes = entry.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'=' if depth == 0 && bytes.get(i + 1) == Some(&b'>') => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

const VOLT_FUNCTIONAL_CALLS: &[&str] = &[
    "state(",
    "action(",
    "computed(",
    "mount(",
    "rendering(",
    "rendered(",
    "usesPagination(",
    "usesFileUploads(",
    "form(",
];

#[cfg(test)]
mod tests;
