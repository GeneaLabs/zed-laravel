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
//! 1. **Bold header** — typically a fully-qualified class name. Plain text,
//!    never markdown: [`render`] adds the `**…**` and backslash-escapes the
//!    text inside it ([`crate::markdown_safety`]), because `.env` keys reach
//!    this field with no charset restriction.
//! 2. **Detail line** — short inline markdown beneath the header
//!    (e.g. `` `GET /uri` → `Controller@show` ``).
//! 3. **Description** — a paragraph of prose (PHPDoc summary, etc.).
//! 4. **Code block** — fenced code with language hint. PHP-tagged blocks get
//!    the `<?php` opener prepended so Zed's `tree-sitter-php` grammar can
//!    parse them (the standard grammar variant requires the opening tag). The
//!    fence outgrows any backtick run in the content
//!    ([`crate::markdown_safety`]), so a value cannot close it early.
//! 5. **Tag lines** — one italic line per PHPDoc tag (`@param`, `@return`).
//! 6. **Source link** — markdown link to the source location, rendered
//!    verbatim (no prefix, no extra backticks; caller builds the link).
//! 7. **Trailer** — italic note like `*(file not found)*`.

use crate::livewire_resolver::extract_blade_variable_at_cursor;
use crate::markdown_safety;
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

/// Trailer for the multi-locale card: the key resolved in none of the locales
/// the project actually defines. Distinct from
/// [`TRANSLATION_NOT_FOUND_TRAILER`], which still says "default locale" because
/// the single-locale [`translation_card`] path really did only consult one.
pub const TRANSLATION_NOT_FOUND_ANY_LOCALE_TRAILER: &str =
    "*(translation not found in any locale)*";

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
    /// (e.g. `App\Livewire\Counter`). Plain text: `**…**` wrapping *and*
    /// markdown escaping are added by [`render`], so a caller may pass
    /// arbitrary text (a `.env` key) without it acting as markdown.
    pub header: Option<&'a str>,
    /// Detail line under the header. Free-form inline markdown — rendered
    /// verbatim, so a caller with untrusted text must escape it or use
    /// [`code`](Self::code), which is fence-safe.
    pub detail: Option<&'a str>,
    /// Free-form description paragraph (e.g. PHPDoc summary). Markdown-bearing
    /// on the same terms as [`detail`](Self::detail).
    pub description: Option<&'a str>,
    /// Fenced code block with language hint.
    pub code: Option<CodeBlock<'a>>,
    /// Italic tag lines (`@param`, `@return`, `@throws`).
    pub tags: &'a [String],
    /// One line per item in a repeated section — the multi-locale translation
    /// card's `**de** — “…” · [link]` rows. Rendered as a single block with
    /// markdown hard breaks between rows, so N entries render as N adjacent
    /// lines rather than N paragraphs.
    pub lines: &'a [String],
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
        // Escaped, not interpolated raw: `header` is the one field documented
        // as plain text, and `.env` keys reach it (`hover_for_env_declaration`)
        // with no charset restriction whatsoever.
        sections.push(format!("**{}**", markdown_safety::escape_inline(h)));
    }
    if let Some(d) = content.detail {
        sections.push(d.to_string());
    }
    if let Some(d) = content.description {
        sections.push(d.to_string());
    }
    if let Some(code) = &content.code {
        // Both arms negotiate the fence length against their own content: a
        // `.env` value carrying three backticks would otherwise close a fixed
        // fence early and render the rest as markdown.
        let block = match code.language {
            CodeLanguage::Php => {
                markdown_safety::fenced_block("php", &format!("<?php\n{}", code.content))
            }
            CodeLanguage::Plain => markdown_safety::fenced_block("", code.content),
        };
        sections.push(block);
    }
    if !content.lines.is_empty() {
        // Two trailing spaces is markdown's hard line break — a bare "\n"
        // would let adjacent rows run together into one paragraph.
        sections.push(content.lines.join("  \n"));
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

/// Multi-locale variant of [`translation_card`]: one line per locale that
/// defines the key, each carrying its own source link inline, so a `de` + `en`
/// catalogue shows both translations at once.
///
/// Each entry is a locale paired with `Some((value, source_link))` when that
/// locale defines the key, or `None` when it doesn't. Value and link travel
/// together because they cannot occur apart: a value only exists because a
/// file was read to produce it, and that file's path is what the link is built
/// from. Pairing them in the type keeps "resolved but unlinkable" — a state the
/// resolver cannot produce — out of the shape entirely.
///
/// Three renderings, chosen by how many locales actually *resolve* the key —
/// not how many locale directories exist, since a project can define a dozen
/// locales and have only one carry this key:
///
/// - **None** — the not-found trailer, naming that no locale had it.
/// - **Exactly one** — collapses to [`translation_card`]'s dense single-block
///   form. Most projects ship one locale, and stacking a lone value under a
///   locale heading would cost a line to say nothing.
/// - **More than one** — a line per locale:
///
/// ```text
/// `failed_title`
///
/// **de** — “Analyse fehlgeschlagen” · [lang/de/contract.php](file://…)
/// **en** — “Analysis failed” · [lang/en/contract.php](file://…)
/// ```
pub fn translation_card_locales(
    key: &str,
    entries: &[(String, Option<(String, String)>)],
) -> String {
    let detail = format!("`{}`", leaf_segment(key));
    let resolved: Vec<(&String, &(String, String))> = entries
        .iter()
        .filter_map(|(locale, hit)| hit.as_ref().map(|hit| (locale, hit)))
        .collect();

    match resolved.as_slice() {
        [] => render(&HoverContent {
            detail: Some(&detail),
            trailer: Some(TRANSLATION_NOT_FOUND_ANY_LOCALE_TRAILER),
            ..Default::default()
        }),
        [(locale, (value, link))] => translation_card(key, locale, Some(value), Some(link)),
        _ => {
            let lines: Vec<String> = resolved
                .iter()
                // Curly quotes delimit the value so it can't be mistaken for
                // the key or a path — same rule as `translation_card`.
                .map(|(locale, (value, link))| format!("**{locale}** — “{value}” · {link}"))
                .collect();
            render(&HoverContent {
                detail: Some(&detail),
                lines: &lines,
                ..Default::default()
            })
        }
    }
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
/// `description` populates the card's description line (third row). It's used by
/// the FacadeMethod kind to surface the chased declaration's PHPDoc summary; a
/// Column's resolved `type_hint` takes precedence over it when both are present
/// (the kinds are mutually exclusive in practice).
///
/// Returns an empty string for [`MagicMemberKind::PlainMember`] — a generic
/// property is Intelephense's job, and duplicating it would just add noise (the
/// multi-LSP dedup policy: suppress at the source).
#[allow(clippy::too_many_arguments)]
pub fn magic_member_card(
    kind: crate::salsa_impl::MagicMemberKind,
    member: &str,
    declaring_fqcn: &str,
    confidence: crate::salsa_impl::Confidence,
    definition: Option<&str>,
    type_hint: Option<&str>,
    description: Option<&str>,
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
        // `Model::factory()` — declaring_fqcn is the resolved factory class.
        MagicMemberKind::Factory => "Model factory",
        // A method on a factory-rooted chain (custom state, vendor `state`).
        MagicMemberKind::FactoryMethod => "Factory method",
        // `->pivot` on a model with a custom `$pivotClass`.
        MagicMemberKind::Pivot => "Pivot model",
        // A `__call`-forwarded Eloquent/Query builder method (`orderByDesc`,
        // `where`, …) — real vendor signature, sourced without ide-helper.
        // Worth a card for the same reason `FacadeMethod` is: Intelephense
        // can't see the forwarding either, ide-helper or not.
        MagicMemberKind::BuilderMethod => "Query builder method",
        // Generic property — Intelephense already covers it. Don't duplicate.
        MagicMemberKind::PlainMember => return String::new(),
    };
    let detail = format!("`{member}` on `{declaring_fqcn}`");
    // The description line. For a Column it's the resolved PHP type (cast-aware)
    // from the DB schema; for a FacadeMethod it's the chased declaration's
    // PHPDoc summary (passed in via `description`). The two kinds are mutually
    // exclusive, so `type_hint` takes precedence and `description` fills in
    // otherwise — neither set → no description line.
    let type_desc = type_hint.map(|t| format!("Type `{t}`"));
    let desc_line = type_desc.as_deref().or(description);
    // A MEDIUM-confidence resolution leaned on an inferred receiver type — flag
    // it so the reader knows it's a best-effort, not a static guarantee.
    let trailer = match confidence {
        Confidence::Medium => Some("*receiver type inferred*"),
        _ => None,
    };
    render(&HoverContent {
        header: Some(kind_label),
        detail: Some(&detail),
        description: desc_line,
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

/// Return the raw text of the PHPDoc block immediately preceding a declaration,
/// or `None` when none is present.
///
/// The declaration's `start_line` (0-based) points at the `function`/visibility
/// keyword; the `/** … */` docblock is a tree-sitter *sibling* sitting just
/// above. We scan up from `start_line - 1`, skipping blank and `#[attribute]`
/// lines, and when the run ends at a line closing with `*/` we walk back to its
/// opening `/**` and return everything in between (line ranges are inclusive,
/// joined with `\n`, no dedent — callers parse it line-by-line).
///
/// This replaces the old `extract_member_snippet_with_docblock`: instead of
/// gluing the docblock onto the code block, the FacadeMethod card now lifts the
/// docblock's *summary* into the card description and folds its `@return` into
/// the signature, keeping the code block itself docblock-free.
pub fn extract_leading_docblock(source: &str, start_line: u32) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = start_line as usize;
    if start == 0 || start >= lines.len() {
        return None;
    }
    // Walk up past blank / `#[attr]` lines to the first line of real content.
    let mut above = start as i64 - 1;
    while above >= 0 {
        let t = lines[above as usize].trim();
        if t.is_empty() || t.starts_with("#[") {
            above -= 1;
        } else {
            break;
        }
    }
    // That content line must close a docblock (`*/`); otherwise there's none.
    if above < 0 || !lines[above as usize].trim_end().ends_with("*/") {
        return None;
    }
    // Walk back to the docblock's opening `/**`.
    let mut doc_start = above;
    while doc_start >= 0 && !lines[doc_start as usize].trim_start().starts_with("/**") {
        doc_start -= 1;
    }
    if doc_start < 0 {
        return None;
    }
    Some(lines[doc_start as usize..=above as usize].join("\n"))
}

/// Strip the PHPDoc framing (`/**`, ` * `, ` */`) from one docblock line,
/// returning the bare content. `" * Determine if …"` → `"Determine if …"`,
/// `"/** Inline. */"` → `"Inline."`.
fn strip_docblock_line(line: &str) -> &str {
    let t = line.trim();
    let t = t.strip_prefix("/**").unwrap_or(t);
    let t = t.strip_prefix("/*").unwrap_or(t);
    let t = t.strip_suffix("*/").unwrap_or(t);
    // Leading ` * ` (or a bare `*`) on continuation lines.
    let t = t.trim();
    let t = t.strip_prefix('*').unwrap_or(t);
    t.trim()
}

/// Parse the summary paragraph from a raw PHPDoc block: the description text
/// before the first `@tag`, with multi-line prose collapsed onto one line.
/// Returns `None` when the block has no prose summary (tags only, or empty).
pub fn docblock_summary(docblock: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for line in docblock.lines() {
        let content = strip_docblock_line(line);
        // The summary ends at the first `@tag` line.
        if content.starts_with('@') {
            break;
        }
        if !content.is_empty() {
            parts.push(content.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Parse the type token from a `@return <type> [description]` tag in a raw
/// PHPDoc block. Returns the first whitespace-delimited token after `@return`
/// verbatim (unions/nullables kept as-written), or `None` when there's no
/// `@return` tag or it carries no type.
pub fn docblock_return_type(docblock: &str) -> Option<String> {
    for line in docblock.lines() {
        let content = strip_docblock_line(line);
        if let Some(rest) = content.strip_prefix("@return") {
            let token = rest.split_whitespace().next()?;
            return Some(token.to_string());
        }
    }
    None
}

/// Fold a docblock `@return` type into a method-signature snippet when the
/// source carries no *native* return type.
///
/// Display-only: edits the rendered string, not the source. The signature line
/// is the first line of `snippet` (the plain signature+body slice from
/// [`extract_member_snippet`]). If it already declares a native return type —
/// a `: type` between the parameter list's closing `)` and the body's `{`/`;`
/// — the snippet is returned unchanged. Otherwise `: <return_type>` is inserted
/// just before the `{` (or `;` for an abstract/interface method), or appended
/// when the body opens on a later line. `None` return type → unchanged.
pub fn fold_return_type(snippet: &str, return_type: Option<&str>) -> String {
    let Some(ret) = return_type else {
        return snippet.to_string();
    };
    let mut lines: Vec<String> = snippet.lines().map(str::to_string).collect();
    let Some(sig) = lines.first().cloned() else {
        return snippet.to_string();
    };
    // Find the parameter list's closing `)`. Everything after it on the line is
    // the return-type / body region we inspect.
    let Some(close_paren) = sig.rfind(')') else {
        return snippet.to_string();
    };
    let after = &sig[close_paren + 1..];
    // A native return type shows as a `:` before the body opener on this line.
    let body_start = after.find(['{', ';']).unwrap_or(after.len());
    if after[..body_start].contains(':') {
        // Already typed in source — keep as-is, don't double-append.
        return snippet.to_string();
    }
    let new_sig = if body_start < after.len() {
        // Body opener (`{`/`;`) is on the signature line — insert `: ret` before it.
        let insert_at = close_paren + 1 + body_start;
        let (head, tail) = sig.split_at(insert_at);
        let tail = tail.trim_start();
        // `{` body keeps a space (`(): bool {`); a `;`-terminated abstract /
        // interface method abuts (`(): bool;`).
        let sep = if tail.starts_with(';') { "" } else { " " };
        format!("{}: {}{}{}", head.trim_end(), ret, sep, tail)
    } else {
        // No body opener on this line (brace on the next) — append to the line.
        format!("{}: {}", sig.trim_end(), ret)
    };
    lines[0] = new_sig;
    lines.join("\n")
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

#[cfg(test)]
mod tests;
