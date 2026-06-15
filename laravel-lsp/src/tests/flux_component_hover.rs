//! End-to-end coverage for Flux component hover (issue #110 — follow-up from
//! Holmes's review of #60 / PR #99).
//!
//! AC #2 of #60 ("hover shows a Flux component card") is satisfied in code:
//! `<flux:…>` routes through `Backend::hover_for_component` (`main.rs`), which
//! resolves the tag to a file and renders a source-link + `@props` card. But it
//! was only covered *indirectly* — the resolution step (`resolve_component_path`),
//! props extraction (`extract_props_directive`), and the generic renderer
//! (`hover::render`) were each tested in isolation, yet no test fed a Flux tag
//! through the whole chain and asserted the returned card. A regression in the
//! Flux hover dispatch arm would have gone uncaught.
//!
//! This module walks the exact runtime chain of `hover_for_component("flux:button")`
//! without standing up an async LSP backend:
//!   1. normalize the `flux:` tag into the `flux::` namespace form,
//!   2. resolve the component to a real on-disk file,
//!   3. extract its `@props(...)` directive,
//!   4. render the anonymous-component card and assert it carries the source
//!      link + props — and that the "not found" path renders the trailer instead.

use laravel_lsp::blade_props::extract_props_directive;
use laravel_lsp::hover::{self, CodeBlock, CodeLanguage, HoverContent, FILE_NOT_FOUND_TRAILER};
use laravel_lsp::salsa_impl::{normalize_flux_tag_name, LaravelConfigData};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tower_lsp::lsp_types::Url;

/// A `<flux:button>` component file the way Flux ships one — a `@props([...])`
/// declaration over the variant/size it accepts. The trailing markup is real so
/// the extractor has to stop at the matching `)`, not run to end-of-file.
const BUTTON_BLADE: &str =
    "@props(['variant' => 'default', 'size' => null])\n\n<button {{ $attributes }}>{{ $slot }}</button>\n";

/// The `@props(...)` directive `BUTTON_BLADE` should yield verbatim.
const BUTTON_PROPS: &str = "@props(['variant' => 'default', 'size' => null])";

/// A `LaravelConfigData` rooted at `root`, optionally registering
/// `anonymous_component_paths["flux"] -> flux_dir`. Mirrors `make_bare_config`
/// in `salsa_impl/tests.rs` — every other field is empty.
fn flux_config(root: PathBuf, flux_dir: Option<PathBuf>) -> LaravelConfigData {
    let mut anonymous_component_paths = HashMap::new();
    if let Some(dir) = flux_dir {
        anonymous_component_paths.insert("flux".to_string(), dir);
    }
    LaravelConfigData {
        root,
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A project whose `flux` anonymous-component directory holds `button.blade.php`.
/// The registered directory IS the components dir, so `<flux:button>` resolves to
/// `{dir}/button.blade.php` with no `components/` segment. The `TempDir` is
/// returned so the caller keeps it alive for the test's duration.
fn flux_project() -> (TempDir, LaravelConfigData) {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("button.blade.php"), BUTTON_BLADE).unwrap();
    let config = flux_config(dir.path().to_path_buf(), Some(dir.path().to_path_buf()));
    (dir, config)
}

// ─── 1. tag normalization (the first link in the chain) ──────────────────

#[test]
fn normalize_rewrites_single_colon_and_rejects_already_namespaced() {
    // The `flux:` sugar is rewritten to the `flux::` namespace form the resolver
    // understands…
    assert_eq!(
        normalize_flux_tag_name("flux:button").as_deref(),
        Some("flux::button"),
    );
    // …while an already-namespaced tag arrives pre-normalized (no double rewrite):
    // after stripping `flux:`, the remainder begins with `:`, which the
    // `rest.starts_with(':')` guard rejects (`salsa_impl.rs`)…
    assert_eq!(normalize_flux_tag_name("flux::button"), None);
    // …and any other already-`flux::` name is rejected by that same guard — the
    // remainder still leads with `:`, so it's left untouched too.
    assert_eq!(normalize_flux_tag_name("flux::nested"), None);
}

// ─── 2. resolution to a real file ────────────────────────────────────────

#[test]
fn resolve_component_path_finds_the_registered_flux_file() {
    let (dir, config) = flux_project();

    let paths = config.resolve_component_path("flux:button");
    assert!(
        !paths.is_empty(),
        "`flux:button` must resolve to at least one candidate path",
    );

    // The registered anonymous-component directory is consulted first, so the
    // top candidate is the file we wrote — and it exists on disk.
    let first = &paths[0];
    assert_eq!(
        first,
        &dir.path().join("button.blade.php"),
        "first candidate is the registered anonymous-component file",
    );
    assert!(
        first.exists(),
        "first candidate resolves to the existing file: {first:?}",
    );
}

// ─── 3. props extraction from the resolved file ──────────────────────────

#[test]
fn extract_props_directive_reads_props_from_the_flux_file() {
    let (dir, _config) = flux_project();

    let props = extract_props_directive(&dir.path().join("button.blade.php"))
        .expect("the Flux button file declares @props, so extraction is non-None");
    assert_eq!(
        props, BUTTON_PROPS,
        "the raw `@props(...)` directive is captured verbatim, stopping at the matching `)`",
    );
}

// ─── 4. end-to-end render: resolved file → card with link + props ────────

#[test]
fn rendered_card_carries_source_link_and_props() {
    let (_dir, config) = flux_project();

    // Walk the resolver → extractor → render chain exactly as the
    // anonymous-component arm of `hover_for_component` does.
    let resolved = config.resolve_component_path("flux:button");
    let path = resolved
        .iter()
        .find(|p| p.exists())
        .expect("a resolved Flux candidate exists on disk");
    let props =
        extract_props_directive(path).expect("props are extractable from the resolved file");

    // Source link built the same way `Backend::source_link` does: a file:// URL
    // with a *root-relative* display label, mirroring `relative_display_path`
    // (which strips the project root, falling back to the full path on
    // mismatch). Here the fixture root IS the flux dir, so this reduces to
    // `button.blade.php` — but deriving it from the root keeps the assertion
    // faithful to production instead of coincidentally equal to `file_name()`.
    let url = Url::from_file_path(path).expect("absolute path → file URL");
    let display = path
        .strip_prefix(&config.root)
        .unwrap_or(path)
        .to_string_lossy();
    let link = hover::source_link(&display, url.as_str(), None);

    let card = hover::render(&HoverContent {
        code: Some(CodeBlock {
            language: CodeLanguage::Php,
            content: &props,
        }),
        source_link: Some(&link),
        ..Default::default()
    });

    assert!(
        card.contains(&link),
        "the card carries the source link string:\n{card}",
    );
    assert!(
        card.contains(BUTTON_PROPS),
        "the card carries the props text:\n{card}",
    );
    assert!(
        !card.contains(FILE_NOT_FOUND_TRAILER),
        "a resolved file must not render the not-found trailer:\n{card}",
    );
}

// ─── 5. "not found" path: trailer only, no link or props ─────────────────

#[test]
fn not_found_path_renders_trailer_without_link_or_props() {
    // An empty project with no `flux` registration and no files on disk: every
    // candidate for `flux:button` misses.
    let dir = TempDir::new().unwrap();
    let config = flux_config(dir.path().to_path_buf(), None);

    // Walk the *production* not-found decision (`main.rs`, anonymous-component
    // arm). `resolve_component_file` keeps the first candidate that exists on
    // disk; production derives `link`/`snippet` from it and selects the trailer
    // with `if link.is_none()`. The resolution outcome therefore *drives* the
    // trailer — it isn't hand-fed. With nothing on disk, `blade_path` is None,
    // so no link, no props, and the not-found trailer is what renders.
    let blade_path = config
        .resolve_component_path("flux:button")
        .into_iter()
        .find(|p| p.exists());
    assert!(
        blade_path.is_none(),
        "no candidate file exists for an unregistered, empty project",
    );

    let link = blade_path.as_deref().map(|p| {
        let url = Url::from_file_path(p).expect("absolute path → file URL");
        let display = p.strip_prefix(&config.root).unwrap_or(p).to_string_lossy();
        hover::source_link(&display, url.as_str(), None)
    });
    let snippet = blade_path.as_deref().and_then(extract_props_directive);
    // The trailer is selected exactly as production does: `link.is_none()`.
    let trailer = link.is_none().then_some(FILE_NOT_FOUND_TRAILER);

    let card = hover::render(&HoverContent {
        code: snippet.as_deref().map(|s| CodeBlock {
            language: CodeLanguage::Php,
            content: s,
        }),
        source_link: link.as_deref(),
        trailer,
        ..Default::default()
    });

    assert_eq!(
        card, FILE_NOT_FOUND_TRAILER,
        "the card is the trailer alone — no other sections",
    );
    assert!(
        !card.contains("file://"),
        "no source link is rendered when the file is missing",
    );
    assert!(
        !card.contains("@props"),
        "no props section is rendered when the file is missing",
    );
}
