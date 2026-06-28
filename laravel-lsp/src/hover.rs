//! Hover content — a single rendering template plus the dispatch enum.
//!
//! The LSP `hover()` handler delegates to this module after its Salsa lookups:
//! caller-side code resolves whatever data each pattern needs (file paths,
//! env values, route definitions, property declarations, class FQNs) and
//! hands them to [`render`] via a [`HoverContent`] struct. Sections of the
//! template that aren't supplied are simply omitted — the same template
//! covers every pattern (view, route, env, Blade variable on property, …)
//! purely by which fields the caller populates.
//!
//! Earlier revisions had per-pattern `format_*` functions; that was overkill
//! since the visual style is uniform across patterns. Pattern variation
//! lives entirely in *what data we pass*, not *how it renders*.
//!
//! The [`HoverTarget`] enum lets `Backend::hover()` route both Salsa-indexed
//! Laravel patterns and ad-hoc Blade variables through one dispatch.
//!
//! # Template
//!
//! Sections, rendered in order with `\n\n` separators (paragraph breaks in
//! markdown). Each section is omitted entirely when absent:
//!
//! 1. **Bold header** — typically a fully-qualified class name. Wrap-free
//!    text; [`render`] adds the `**…**` markdown.
//! 2. **Detail line** — short inline markdown beneath the header
//!    (e.g. `` `GET /uri` → `Controller@show` ``).
//! 3. **Description** — a paragraph of prose (PHPDoc summary, etc.).
//! 4. **Code block** — fenced code with language hint. PHP-tagged blocks get
//!    the `<?php` opener prepended so Zed's `tree-sitter-php` grammar can
//!    parse them (the standard grammar variant requires the opening tag).
//! 5. **Tag lines** — one italic line per PHPDoc tag (`@param`, `@return`).
//! 6. **Source link** — markdown link to the source location, rendered
//!    verbatim (no prefix, no extra backticks; caller builds the link).
//! 7. **Trailer** — italic note like `*(file not found)*`.

use crate::livewire_resolver::extract_blade_variable_at_cursor;
use crate::salsa_impl::{ParsedPatternsData, PatternAtPosition};
use std::path::Path;

/// The italic [`HoverContent::trailer`] rendered when a hovered reference
/// resolves to no file on disk (view, component, asset, url, … — every
/// `if link.is_none()` arm in `main.rs`). Single source of truth for the
/// string so tests can assert against the production value instead of
/// re-typing the literal.
pub const FILE_NOT_FOUND_TRAILER: &str = "*(file not found)*";

/// The italic trailer [`translation_card`] renders when a key resolves to no
/// value in the default locale. Single source of truth so tests assert against
/// the production string.
pub const TRANSLATION_NOT_FOUND_TRAILER: &str = "*(translation not found for default locale)*";

/// Anything the cursor might be hovering. Pattern variants come straight from
/// the Salsa position index; the Blade-variable variant is extracted by line
/// scanning, and only matters in `.blade.php` files.
pub enum HoverTarget {
    Pattern(PatternAtPosition),
    BladeVariable {
        var_name: String,
        property: Option<String>,
    },
}

/// Decide what (if anything) the cursor is on. Patterns take precedence;
/// Blade-variable extraction is only attempted on Blade files when no pattern
/// matched. Returns `None` when neither lookup finds something hoverable.
pub fn find_hover_target(
    patterns: &ParsedPatternsData,
    line_text: &str,
    line: u32,
    column: u32,
    is_blade: bool,
) -> Option<HoverTarget> {
    if let Some(p) = patterns.find_at_position(line, column) {
        return Some(HoverTarget::Pattern(p));
    }
    if is_blade {
        if let Some((var_name, property)) = extract_blade_variable_at_cursor(line_text, column) {
            return Some(HoverTarget::BladeVariable { var_name, property });
        }
    }
    None
}

// ============================================================================
// The unified template
// ============================================================================

/// All data a hover can carry. Every field is optional — [`render`] omits
/// any section whose field is `None` / empty. Build one of these per
/// pattern at the dispatch site and call [`render`].
#[derive(Debug, Default, Clone)]
pub struct HoverContent<'a> {
    /// Bold header — typically a fully-qualified class name
    /// (e.g. `App\Livewire\Counter`). `**…**` wrapping is added by render.
    pub header: Option<&'a str>,
    /// Detail line under the header. Free-form inline markdown.
    pub detail: Option<&'a str>,
    /// Free-form description paragraph (e.g. PHPDoc summary).
    pub description: Option<&'a str>,
    /// Fenced code block with language hint.
    pub code: Option<CodeBlock<'a>>,
    /// Italic tag lines (`@param`, `@return`, `@throws`).
    pub tags: &'a [String],
    /// Pre-built markdown link string for the source location (e.g.
    /// `[app/Models/User.php:42](file:///abs/path)`). Rendered verbatim
    /// — no `at` prefix, no surrounding backticks.
    pub source_link: Option<&'a str>,
    /// Italic trailer (`*(file not found)*`, `*(commented out)*`).
    pub trailer: Option<&'a str>,
}

/// A fenced code block. [`CodeLanguage::Php`] auto-prepends `<?php\n` so
/// Zed's `tree-sitter-php` grammar (which requires the opening tag) parses
/// the snippet and applies highlighting.
#[derive(Debug, Clone, Copy)]
pub struct CodeBlock<'a> {
    pub language: CodeLanguage,
    pub content: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub enum CodeLanguage {
    /// `php` fence with a `<?php\n` opener prepended.
    Php,
    /// Plain fence (no language tag) — for raw values, translated strings,
    /// `.env` content where PHP highlighting would be misleading.
    Plain,
}

/// Render a [`HoverContent`] into the final hover markdown. Sections are
/// emitted in the documented order, joined with `\n\n`. Returns an empty
/// string when every field is absent — caller should treat that as
/// "no hover".
pub fn render(content: &HoverContent<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(h) = content.header {
        sections.push(format!("**{}**", h));
    }
    if let Some(d) = content.detail {
        sections.push(d.to_string());
    }
    if let Some(d) = content.description {
        sections.push(d.to_string());
    }
    if let Some(code) = &content.code {
        let block = match code.language {
            CodeLanguage::Php => format!("```php\n<?php\n{}\n```", code.content),
            CodeLanguage::Plain => format!("```\n{}\n```", code.content),
        };
        sections.push(block);
    }
    if !content.tags.is_empty() {
        let tag_lines = content
            .tags
            .iter()
            .map(|t| format!("*{}*", t))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(tag_lines);
    }
    if let Some(link) = content.source_link {
        sections.push(link.to_string());
    }
    if let Some(t) = content.trailer {
        sections.push(t.to_string());
    }

    sections.join("\n\n")
}

/// Build the hover card for a translation key. The detail line carries just
/// the key's leaf segment (inline code) and the locale it resolved against —
/// the full key is already under the cursor, so repeating it is noise. The
/// value follows on its own line wrapped in typographic quotes, which mark it
/// unmistakably as the resolved string. `value` is the already-unquoted,
/// length-capped string, or `None` when the key resolves to nothing — in which
/// case the not-found trailer renders instead. `source_link` is a pre-built
/// markdown link to the lang file, or `None`.
///
/// ```text
/// `title` · en
///
/// “Status changed”
///
/// [lang/app/en/notification.php](file://…)
/// ```
pub fn translation_card(
    key: &str,
    locale: &str,
    value: Option<&str>,
    source_link: Option<&str>,
) -> String {
    let detail = format!("`{}` · {locale}", leaf_segment(key));
    // Curly quotes delimit the value so it can't be mistaken for the key or a
    // path, and won't clash with any straight quotes inside the string itself.
    let quoted = value.map(|v| format!("“{v}”"));
    let trailer = value.is_none().then_some(TRANSLATION_NOT_FOUND_TRAILER);
    render(&HoverContent {
        detail: Some(&detail),
        description: quoted.as_deref(),
        source_link,
        trailer,
        ..Default::default()
    })
}

/// The leaf of a translation key: the last `.`-segment, after dropping any
/// `namespace::` prefix. `app::notification.task.title` → `title`,
/// `messages.welcome` → `welcome`, a spaced JSON text key → unchanged.
fn leaf_segment(key: &str) -> &str {
    let after_namespace = key.split_once("::").map(|(_, rest)| rest).unwrap_or(key);
    after_namespace
        .rsplit('.')
        .next()
        .unwrap_or(after_namespace)
}

/// Build a semantic hover card for a resolved magic member (M6) — the
/// Eloquent-magic sites Intelephense can't see through (`->active()` is a
/// scope, `$user->posts` a relationship, `$model->full_name` an accessor,
/// `$user->email` a column). `source_link` is a pre-built markdown link to the
/// declaring class, or `None` if it couldn't be located.
///
/// Returns an empty string for [`MagicMemberKind::PlainMember`] — a generic
/// property is Intelephense's job, and duplicating it would just add noise (the
/// multi-LSP dedup policy: suppress at the source).
pub fn magic_member_card(
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
    declaring_fqcn: &str,
    confidence: crate::salsa_impl::Confidence,
    definition: Option<&str>,
    type_hint: Option<&str>,
    source_link: Option<&str>,
) -> String {
    use crate::salsa_impl::{Confidence, MagicMemberKind};
    let kind_label = match kind {
        MagicMemberKind::Scope => "Eloquent scope",
        MagicMemberKind::Accessor => "Eloquent accessor",
        MagicMemberKind::Relationship => "Eloquent relationship",
        MagicMemberKind::Column => "Database column",
        MagicMemberKind::DynamicFinder => "Dynamic finder",
        MagicMemberKind::Macro => "Macro",
        // A method reached through a facade proxy — `declaring_fqcn` is the bound
        // concrete the facade forwards to. Worth a card precisely because
        // Intelephense can't see through the proxy.
        MagicMemberKind::FacadeMethod => "Facade method",
        // Generic property — Intelephense already covers it. Don't duplicate.
        MagicMemberKind::PlainMember => return String::new(),
    };
    let detail = format!("`{member}` on `{declaring_fqcn}`");
    // For a column, the resolved PHP type (cast-aware) from the DB schema.
    let type_desc = type_hint.map(|t| format!("Type `{t}`"));
    // A MEDIUM-confidence resolution leaned on an inferred receiver type — flag
    // it so the reader knows it's a best-effort, not a static guarantee.
    let trailer = match confidence {
        Confidence::Medium => Some("*receiver type inferred*"),
        _ => None,
    };
    render(&HoverContent {
        header: Some(kind_label),
        detail: Some(&detail),
        description: type_desc.as_deref(),
        // The declaring method's source — for a relationship this reveals the
        // target model (`$this->belongsTo(Account::class)`), for a scope its
        // query body, for an accessor what it computes.
        code: definition.map(|d| CodeBlock {
            language: CodeLanguage::Php,
            content: d,
        }),
        source_link,
        trailer,
        ..Default::default()
    })
}

// ============================================================================
// Curated helper-function hover (#58)
// ============================================================================

/// A curated hover card for one Laravel global helper function.
///
/// The seven curated helpers are exactly those whose framework `helpers.php`
/// docblock is thin or generic, so a Laravel-aware synopsis adds value over
/// what Intelephense already shows. Keeping the set narrow IS the dedup policy:
/// every other helper is simply never indexed, so we never emit a duplicate
/// card — no runtime Intelephense detection needed.
#[derive(Debug, Clone, Copy)]
pub struct HelperCard {
    /// One-line, Laravel-aware synopsis (the hover's detail line).
    pub synopsis: &'static str,
    /// Path, relative to the workspace root, of the framework file that DEFINES
    /// this helper. All seven curated helpers live in Foundation's `helpers.php`
    /// (`Support/helpers.php` holds the lower-level `collect`/`str`/… helpers).
    /// Used to build the `file://` source link when the framework is vendored.
    pub vendor_path: &'static str,
    /// Canonical laravel.com docs anchor — the source-link fallback used when
    /// the framework file above isn't present under the workspace root.
    pub docs_url: &'static str,
}

/// All seven curated framework helpers live in this one file.
const FOUNDATION_HELPERS: &str = "vendor/laravel/framework/src/Illuminate/Foundation/helpers.php";

/// The curated allow-list of Laravel helper identifiers we provide hover cards
/// for, keyed by helper name. This is the canonical allow-list; the `#any-of?`
/// predicate in `queries/php.scm` mirrors it as a parse-time pre-filter.
pub static HELPER_CARDS: &[(&str, HelperCard)] = &[
    (
        "route",
        HelperCard {
            synopsis: "Generate a URL for a named route.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-route",
        },
    ),
    (
        "view",
        HelperCard {
            synopsis: "Get the evaluated view contents for the given view.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-view",
        },
    ),
    (
        "config",
        HelperCard {
            synopsis: "Get / set the value of a configuration variable.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-config",
        },
    ),
    (
        "auth",
        HelperCard {
            synopsis: "Get the available auth guard / authenticator instance.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-auth",
        },
    ),
    (
        "app",
        HelperCard {
            synopsis: "Get the container instance, or resolve a binding from it.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-app",
        },
    ),
    (
        "session",
        HelperCard {
            synopsis: "Get / set a session value, or the session store instance.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-session",
        },
    ),
    (
        "cache",
        HelperCard {
            synopsis: "Get / set a cache value, or the cache store instance.",
            vendor_path: FOUNDATION_HELPERS,
            docs_url: "https://laravel.com/docs/helpers#method-cache",
        },
    ),
];

/// Look up the curated card for a helper name. `None` for anything outside the
/// allow-list (the caller then renders no card).
pub fn helper_card(name: &str) -> Option<&'static HelperCard> {
    HELPER_CARDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, card)| card)
}

/// Build a hover card for a curated Laravel helper identifier (`route`, `view`,
/// `config`, …). `header` is the function name, `detail` the curated synopsis
/// (both from [`HELPER_CARDS`]). `source_link` is the pre-resolved markdown link
/// — built caller-side because it needs the workspace root to decide between a
/// `file://` link into the vendored framework `helpers.php` and the docs-URL
/// fallback.
///
/// Returns `None` when `name` isn't in the curated allow-list, so the caller
/// renders nothing (the structural dedup policy — Intelephense owns the rest).
pub fn helper_identifier_card(name: &str, source_link: Option<&str>) -> Option<String> {
    let card = helper_card(name)?;
    Some(render(&HoverContent {
        header: Some(name),
        detail: Some(card.synopsis),
        source_link,
        ..Default::default()
    }))
}

/// Resolve the bottom-of-hover source link for a curated helper card, driving
/// the vendored-vs-docs decision off a real on-disk probe under `root`.
///
/// When the framework's `helpers.php` (`card.vendor_path`) is vendored under
/// `root`, returns a `file://` link into it, labelled with the path relative to
/// `root`. Otherwise — no root, or the file isn't present — falls back to the
/// canonical `laravel.com/docs` anchor (`card.docs_url`).
///
/// This is the single source of truth for the branch: the binary's
/// `Backend::hover_for_helper` delegates to it, and the behavior tests drive it
/// directly with a real `TempDir`, so the test and the production decision can
/// never diverge.
pub async fn resolve_helper_source_link(root: Option<&Path>, card: &HelperCard) -> String {
    use tower_lsp::lsp_types::Url;

    // Probe the workspace for the vendored framework helpers file — a single
    // existence stat, no vendor scan. `try_exists` treats a probe error
    // (permission denied, broken symlink) as "absent".
    let vendored = match root {
        Some(r) => {
            let path = r.join(card.vendor_path);
            tokio::fs::try_exists(&path)
                .await
                .unwrap_or(false)
                .then_some((r, path))
        }
        None => None,
    };

    let Some((root, path)) = vendored else {
        // No root, or the framework isn't vendored: the curated docs anchor.
        return source_link("Laravel documentation", card.docs_url, None);
    };

    // A `file://` link into the vendored helpers.php, labelled relative to the
    // workspace root — mirrors `Backend::source_link` (the `root` here IS the
    // workspace root, so the label reduces to `card.vendor_path`). Falls back to
    // an unlinked monospace path when the URL can't be built (non-absolute path).
    let display = path
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    match Url::from_file_path(&path) {
        Ok(url) => source_link(&display, url.as_str(), None),
        Err(_) => format!("`{}`", display),
    }
}

/// The declaring method names a magic-member usage name could map to, by kind.
/// Relationships/finders are accessed under their method name verbatim
/// (`$user->account` ← `account()`); scopes and accessors transform
/// (`active` ← `scopeActive`, `full_name` ← `getFullNameAttribute` or the
/// new-style `fullName(): Attribute`). Used to locate the declaration for the
/// hover snippet.
pub fn candidate_method_names(
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
) -> Vec<String> {
    use crate::salsa_impl::MagicMemberKind;
    let pascal = crate::naming::snake_to_pascal(member);
    match kind {
        MagicMemberKind::Scope => vec![format!("scope{pascal}")],
        MagicMemberKind::Accessor => {
            // Old-style `get{Pascal}Attribute` + new-style camelCase method.
            let camel = {
                let mut c = pascal.chars();
                match c.next() {
                    Some(first) => first.to_ascii_lowercase().to_string() + c.as_str(),
                    None => String::new(),
                }
            };
            vec![format!("get{pascal}Attribute"), camel]
        }
        // Relationship / DynamicFinder / PlainMember: accessed by method name.
        _ => vec![member.to_string()],
    }
}

/// Slice a declaration's source (0-based `start_line..=end_line`) into a snippet
/// for a hover code block: dedents by the first line's indentation and caps
/// runaway bodies. Returns `""` if the range is out of bounds.
pub fn extract_member_snippet(source: &str, start_line: u32, end_line: u32) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = source.lines().collect();
    let start = start_line as usize;
    if start >= lines.len() {
        return String::new();
    }
    let end = (end_line as usize).min(lines.len() - 1);
    if end < start {
        return String::new();
    }
    let slice = &lines[start..=end];
    let indent = slice
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    let mut out: Vec<String> = slice
        .iter()
        .map(|l| {
            if l.len() >= indent {
                l[indent..].to_string()
            } else {
                l.trim_start().to_string()
            }
        })
        .collect();
    if out.len() > MAX_LINES {
        out.truncate(MAX_LINES);
        out.push("// …".to_string());
    }
    out.join("\n")
}

// ============================================================================
// Caller utilities
// ============================================================================

/// Heuristic: a type string represents a class (rather than a PHP primitive)
/// if it contains a namespace separator OR its first character is an
/// uppercase ASCII letter. Catches `App\Models\User`, `Carbon`, `Collection`
/// while excluding `mixed`, `string`, `int`, `?int`, `null`, etc.
///
/// `pub` because the LSP server uses this predicate to decide whether to
/// run a `find_php_class_file` lookup on a resolved variable type — calling
/// it for primitive sentinels like `"mixed"` always misses.
pub fn is_class_like_type(t: &str) -> bool {
    let t = t.trim_start_matches('?').trim_start_matches('\\');
    if t.contains('\\') {
        return true;
    }
    t.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

/// Build the markdown link string used for the bottom-line source location.
/// The label is the display path (relative to the project root, optionally
/// with `:line`); the URL is a `file://` URI that Zed resolves to "open
/// this file at this line".
///
/// Label is rendered as plain markdown link text — NOT wrapped in backticks
/// — so it doesn't pick up the inline-code background/styling and looks
/// like a normal hyperlink.
///
/// Caller is expected to pre-resolve the absolute file URL via
/// [`tower_lsp::lsp_types::Url::from_file_path`] so percent-encoding for
/// spaces and other URL-unsafe path bytes is handled correctly.
pub fn source_link(display: &str, file_url: &str, line: Option<u32>) -> String {
    match line {
        Some(l) => format!("[{}:{}]({}#L{})", display, l, file_url, l),
        None => format!("[{}]({})", display, file_url),
    }
}

/// Truncate strings longer than `limit` chars with a `…` ellipsis. Operates
/// on chars (not bytes) so it never splits a multibyte character.
///
/// Used by config/translation dispatch code to clip long resolved values
/// before stuffing them into a code block.
pub fn truncate_for_display(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{}…", head)
}

#[cfg(test)]
mod tests;
