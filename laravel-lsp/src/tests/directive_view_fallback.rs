//! Tests for the third-party view-directive goto fallback and the
//! `blade.viewDirectives` escape hatch (issue #325).
//!
//! `create_directive_location_from_salsa` used to resolve view arguments from
//! two hardcoded lists, so a package registering its own view-rendering
//! directive through `Blade::directive()` got no goto at all. The fix adds a
//! permissive fallback to the **goto** path only, because goto and diagnostics
//! carry opposite risk: a wrong goto costs one keystroke, a wrong "view does not
//! exist" squiggle marks working code. The diagnostic gate
//! (`directive_takes_missing_view_diagnostic`) is unchanged, so a false
//! missing-view diagnostic remains impossible by construction.
//!
//! The fallback is gated on three things, each covered below:
//!   1. `dir.name` must have no dedicated branch — keyed on the *name*, never on
//!      "nothing has resolved yet", so a dedicated branch that fails to find its
//!      own target still returns `None` instead of leaking into the heuristic.
//!   2. `dir.name` must not be on the [`crate::NON_VIEW_DIRECTIVES`] denylist
//!      (`@section('content')` must not jump to `views/content.blade.php`).
//!   3. A user-declared `blade.viewDirectives` entry outranks the denylist but
//!      never outranks dedicated handling.
//!
//! Tests drive the private async method directly through
//! `tower_lsp::LspService` / `inner()`, the same way
//! `directive_navigation_containment.rs` does.

use crate::{directive_takes_missing_view_diagnostic, LaravelLanguageServer, LspSettings};
use laravel_lsp::salsa_impl::{DirectiveReferenceData, LaravelConfigData};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{GotoDefinitionResponse, LocationLink, Url};
use tower_lsp::LspService;

/// Blade view contents are irrelevant to resolution — only that the file exists,
/// so a `None` result can never be blamed on a missing file.
const VIEW_BODY: &str = "<div>view</div>\n";

/// A directive name with no dedicated branch and no denylist entry — the shape a
/// `Blade::directive('renderPartial', ...)` registration produces.
const CUSTOM: &str = "renderPartial";

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so its private
/// methods can be called directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A config rooted at `root` with the given view roots (in `resolve_view_path`
/// priority order) and `loadViewsFrom`-style namespaces.
fn config(
    root: &Path,
    view_paths: Vec<PathBuf>,
    view_namespaces: HashMap<String, PathBuf>,
) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths,
        component_paths: vec![(String::new(), root.join("resources/views/components"))],
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// The common single-view-root config: `{root}/resources/views`, no namespaces.
fn simple_config(root: &Path) -> LaravelConfigData {
    config(root, vec![root.join("resources/views")], HashMap::new())
}

/// Seed `cfg` as the cached config so `get_cached_config` returns it without
/// touching Salsa.
async fn seed(server: &LaravelLanguageServer, cfg: LaravelConfigData) {
    *server.cached_config.write().await = Some(std::sync::Arc::new(cfg));
}

/// Apply the `blade.viewDirectives` setting through the real
/// [`LaravelLanguageServer::update_settings`] path — the single function
/// `initialize`, `did_change_configuration`, and the `workspace/configuration`
/// pull all converge on — rather than poking the field directly. That way these
/// tests fail if the setting is ever parsed but not wired.
async fn set_view_directives(server: &LaravelLanguageServer, first: &[&str], second: &[&str]) {
    let json = serde_json::json!({
        "blade": {
            "viewDirectives": { "firstArg": first, "secondArg": second }
        }
    });
    let settings: LspSettings = serde_json::from_value(json).expect("settings must deserialize");
    server.update_settings(&settings).await;
}

/// Byte span of the first quoted argument's *contents* within `text`.
/// Test directives are ASCII, so byte offsets and character columns coincide.
fn first_quoted_span(text: &str) -> (u32, u32) {
    let open = text
        .find(['\'', '"'])
        .expect("directive text must carry a quoted argument");
    let quote = text[open..].chars().next().unwrap();
    let close = text[open + 1..]
        .find(quote)
        .expect("directive text must close its quote")
        + open
        + 1;
    ((open + 1) as u32, close as u32)
}

/// A `DirectiveReferenceData` for `@{name}{args}` at the document origin, e.g.
/// `directive_ref("include", "('layouts.app')")`.
///
/// `column`/`end_column` span the whole directive (the range
/// `create_location_link` reports); `string_column`/`string_end_column` span
/// only the first quoted argument (the narrower range
/// `create_location_link_with_string_range` reports). Keeping the two genuinely
/// different is what lets a test tell which branch produced a link.
fn directive_ref(name: &str, args: &str) -> DirectiveReferenceData {
    let text = format!("@{name}{args}");
    let (string_column, string_end_column) = first_quoted_span(&text);
    DirectiveReferenceData {
        name: name.to_string(),
        arguments: Some(args.to_string()),
        line: 0,
        column: 0,
        end_column: text.len() as u32,
        string_column,
        string_end_column,
    }
}

/// Resolve `dir` and unwrap the single expected `LocationLink`.
async fn resolve(server: &LaravelLanguageServer, dir: &DirectiveReferenceData) -> LocationLink {
    match server.create_directive_location_from_salsa(dir).await {
        Some(GotoDefinitionResponse::Link(links)) => {
            assert_eq!(links.len(), 1, "exactly one definition link is expected");
            links.into_iter().next().unwrap()
        }
        other => panic!("expected a Link definition response, got {other:?}"),
    }
}

/// Assert `link` points at `expected` — the path itself, not merely `is_some()`.
fn assert_targets(link: &LocationLink, expected: &Path) {
    assert_eq!(
        link.target_uri,
        Url::from_file_path(expected).unwrap(),
        "the definition must point at {}",
        expected.display()
    );
}

// ───────────────────────── the plain fallback ─────────────────────────

#[tokio::test]
async fn custom_directive_resolves_through_the_fallback() {
    // The headline case: a directive registered only via `Blade::directive()`
    // appears on no hardcoded list, so before #325 it resolved to nothing at
    // all. With the fallback its first quoted argument is treated as a view
    // name — no configuration required.
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/dashboard.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    let link = resolve(&server, &directive_ref(CUSTOM, "('dashboard')")).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn fallback_reports_the_whole_directive_as_the_origin_range() {
    // The fallback builds its link with `create_location_link`, like every other
    // view-directive branch — so the clickable range covers the whole directive,
    // not just the quoted name. Pinning this is what makes the narrow-range
    // assertions further down discriminating: the two ranges must differ, or
    // those tests could not tell the branches apart.
    let root = tempfile::TempDir::new().unwrap();
    write(
        &root.path().join("resources/views/dashboard.blade.php"),
        VIEW_BODY,
    );

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    let dir = directive_ref(CUSTOM, "('dashboard')");
    let link = resolve(&server, &dir).await;
    let range = link.origin_selection_range.expect("origin range is set");

    assert_eq!(range.start.character, dir.column);
    assert_eq!(range.end.character, dir.end_column);
    assert_ne!(
        range.start.character, dir.string_column,
        "the whole-directive range must be distinguishable from the string range"
    );
}

#[tokio::test]
async fn fallback_tries_every_resolve_candidate_not_just_the_first() {
    // `resolve_view_path` returns one candidate per configured view root. A
    // first-candidate-only fallback would resolve nothing here: the view exists
    // ONLY under the second root. Mirrors the loop every other branch runs.
    let root = tempfile::TempDir::new().unwrap();
    let first_root = root.path().join("resources/views");
    let second_root = root.path().join("modules/blog/views");

    let view = second_root.join("dashboard.blade.php");
    write(&view, VIEW_BODY);
    assert!(
        !first_root.join("dashboard.blade.php").exists(),
        "precondition: the first candidate must be absent"
    );

    let server = test_server();
    seed(
        &server,
        config(root.path(), vec![first_root, second_root], HashMap::new()),
    )
    .await;

    let link = resolve(&server, &directive_ref(CUSTOM, "('dashboard')")).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn out_of_root_fallback_target_returns_none() {
    // The fallback is subject to the same containment guard as every other
    // branch (issues #130/#148). A `loadViewsFrom(__DIR__ . '/../../etc', 'pkg')`
    // -style namespace can point outside the project root; the target exists on
    // disk, so only `path_within_root` can explain a `None`.
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    write(&outside.path().join("card.blade.php"), VIEW_BODY);

    let mut namespaces = HashMap::new();
    namespaces.insert("pkg".to_string(), outside.path().to_path_buf());

    let server = test_server();
    seed(
        &server,
        config(
            root.path(),
            vec![root.path().join("resources/views")],
            namespaces,
        ),
    )
    .await;

    assert!(
        server
            .create_directive_location_from_salsa(&directive_ref(CUSTOM, "('pkg::card')"))
            .await
            .is_none(),
        "an out-of-root fallback target must not resolve, even though it exists \
         on disk — the containment guard refuses it"
    );
}

#[tokio::test]
async fn in_root_namespaced_fallback_target_still_resolves() {
    // Positive control for the case above: the same namespace pointing INSIDE
    // the root resolves normally, so the `None` there is the containment guard
    // and not a namespace the fallback simply cannot handle.
    let root = tempfile::TempDir::new().unwrap();
    let namespace_dir = root.path().join("packages/pkg/resources/views");
    let view = namespace_dir.join("card.blade.php");
    write(&view, VIEW_BODY);

    let mut namespaces = HashMap::new();
    namespaces.insert("pkg".to_string(), namespace_dir);

    let server = test_server();
    seed(
        &server,
        config(
            root.path(),
            vec![root.path().join("resources/views")],
            namespaces,
        ),
    )
    .await;

    let link = resolve(&server, &directive_ref(CUSTOM, "('pkg::card')")).await;
    assert_targets(&link, &view);
}

// ───────────────────────────── the denylist ─────────────────────────────

#[tokio::test]
async fn denylisted_directive_does_not_resolve_through_the_fallback() {
    // The reason the denylist exists: `@section('content')` names a *section*,
    // and a project that also happens to have `views/content.blade.php` would
    // otherwise get a confident jump to an unrelated file. The identical
    // argument under a non-denylisted name resolves, which proves the config and
    // the file on disk are wired — only the name decides the outcome.
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/content.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    assert!(
        server
            .create_directive_location_from_salsa(&directive_ref("section", "('content')"))
            .await
            .is_none(),
        "@section names a section, not a view — the denylist must refuse it \
         even though resources/views/content.blade.php exists"
    );

    let link = resolve(&server, &directive_ref(CUSTOM, "('content')")).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn section_name_directives_share_the_section_denylist_entry() {
    // `@hasSection`/`@sectionMissing` take a section name with exactly the
    // false-positive shape `@section` has, so they are denylisted alongside it.
    // `@endsection` covers the closing-directive half of the list.
    let root = tempfile::TempDir::new().unwrap();
    write(
        &root.path().join("resources/views/content.blade.php"),
        VIEW_BODY,
    );

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    for name in [
        "hasSection",
        "sectionMissing",
        "endsection",
        "yield",
        "stack",
    ] {
        assert!(
            server
                .create_directive_location_from_salsa(&directive_ref(name, "('content')"))
                .await
                .is_none(),
            "@{name} takes a section/stack name, not a view — the denylist must refuse it"
        );
    }
}

// ─────────────────── dedicated handling always wins ───────────────────

#[tokio::test]
async fn dedicated_directive_that_fails_its_own_branch_returns_none() {
    // The exclusion is keyed on `dir.name`, not on "nothing has resolved yet".
    // `@feature` resolves against `app/Features/*.php`; the fallback's naive
    // heuristic would guess `resources/views/unknown-flag.blade.php`, which is
    // deliberately present here. A reachability-based gate would hand back that
    // wrong file; a name-keyed gate returns `None`, exactly as before #325.
    let root = tempfile::TempDir::new().unwrap();
    write(
        &root.path().join("resources/views/unknown-flag.blade.php"),
        VIEW_BODY,
    );
    assert!(
        !root.path().join("app/Features/UnknownFlag.php").exists(),
        "precondition: the feature class must be absent so the dedicated branch fails"
    );

    let server = test_server();
    seed(&server, simple_config(root.path())).await;
    *server.root_path.write().await = Some(root.path().to_path_buf());

    assert!(
        server
            .create_directive_location_from_salsa(&directive_ref("feature", "('unknown-flag')"))
            .await
            .is_none(),
        "a dedicated-handled directive whose own branch finds nothing must still \
         return None — it must never fall through into the fallback heuristic"
    );
}

#[tokio::test]
async fn successful_dedicated_resolution_keeps_its_narrow_string_range() {
    // The other half of the guarantee above: when a dedicated branch *does*
    // resolve, the link is still the one it built. `@livewire` and `@feature`
    // both use `create_location_link_with_string_range`, so their origin range
    // covers only the quoted name — narrower than the whole-directive range the
    // fallback would have produced.
    let root = tempfile::TempDir::new().unwrap();
    write(
        &root.path().join("resources/views/card.blade.php"),
        VIEW_BODY,
    );
    write(
        &root.path().join("app/Features/BillingPortal.php"),
        "<?php class BillingPortal {}\n",
    );

    let server = test_server();
    seed(&server, simple_config(root.path())).await;
    *server.root_path.write().await = Some(root.path().to_path_buf());

    for (name, args) in [("livewire", "('card')"), ("feature", "('billing-portal')")] {
        let dir = directive_ref(name, args);
        let link = resolve(&server, &dir).await;
        let range = link.origin_selection_range.expect("origin range is set");

        assert_eq!(
            (range.start.character, range.end.character),
            (dir.string_column, dir.string_end_column),
            "@{name} must keep its own narrow string range — a whole-directive \
             range would mean the fallback produced the link"
        );
    }
}

// ──────────────── the `blade.viewDirectives` escape hatch ────────────────

#[tokio::test]
async fn view_directives_first_arg_resolves_a_configured_name() {
    // The escape hatch's basic contract: a name under `firstArg` resolves
    // exactly like a hardcoded `view_directives_first_arg` entry. Denylisted
    // here on purpose — see the override test below for why that matters.
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/dashboard.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;
    set_view_directives(&server, &["myPanel"], &[]).await;

    let link = resolve(&server, &directive_ref("myPanel", "('dashboard')")).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn view_directives_first_arg_overrides_the_denylist() {
    // The user's declaration outranks the fixed denylist — someone who really
    // does render views through a `@section`-named directive can say so. Without
    // the override the same call returns `None`, which is asserted in
    // `denylisted_directive_does_not_resolve_through_the_fallback`; this test
    // would pass on a dead-code override only if that one failed too.
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/content.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    let dir = directive_ref("section", "('content')");
    assert!(
        server
            .create_directive_location_from_salsa(&dir)
            .await
            .is_none(),
        "precondition: unconfigured, the denylist refuses @section"
    );

    set_view_directives(&server, &["section"], &[]).await;

    let link = resolve(&server, &dir).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn view_directives_second_arg_resolves_the_second_argument() {
    // Second-arg support must be real, not silently collapsed into a first-arg
    // implementation. `@renderWhen($cond, 'partials.banner')` puts a boolean
    // expression first, exactly like `@includeWhen`: a first-arg reading yields
    // nothing, so only genuine second-arg handling can resolve this.
    let root = tempfile::TempDir::new().unwrap();
    let view = root
        .path()
        .join("resources/views/partials/banner.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    let dir = directive_ref("renderWhen", "($user->admin, 'partials.banner')");

    // Configured as a FIRST-arg directive it must not resolve: the first
    // argument is an expression, not a view name.
    set_view_directives(&server, &["renderWhen"], &[]).await;
    assert!(
        server
            .create_directive_location_from_salsa(&dir)
            .await
            .is_none(),
        "a condition-first directive read as first-arg must resolve nothing"
    );

    set_view_directives(&server, &[], &["renderWhen"]).await;
    let link = resolve(&server, &dir).await;
    assert_targets(&link, &view);
}

#[tokio::test]
async fn view_directives_never_override_dedicated_handling() {
    // Collision rule: naming a dedicated-handled directive in `viewDirectives`
    // is a no-op. Two halves, because a no-op has to be proved in both
    // directions:
    //   * `@feature` still fails closed when its own branch finds nothing, even
    //     though the configured first-arg reading would have found the view;
    //   * `@livewire` still resolves through its own branch, proved by the
    //     narrow string range the configured path could not have produced.
    let root = tempfile::TempDir::new().unwrap();
    write(
        &root.path().join("resources/views/card.blade.php"),
        VIEW_BODY,
    );
    write(
        &root.path().join("resources/views/unknown-flag.blade.php"),
        VIEW_BODY,
    );

    let server = test_server();
    seed(&server, simple_config(root.path())).await;
    *server.root_path.write().await = Some(root.path().to_path_buf());
    set_view_directives(&server, &["livewire", "feature", "include"], &[]).await;

    assert!(
        server
            .create_directive_location_from_salsa(&directive_ref("feature", "('unknown-flag')"))
            .await
            .is_none(),
        "configuring `feature` under viewDirectives must not change @feature — \
         dedicated handling always wins"
    );

    let dir = directive_ref("livewire", "('card')");
    let link = resolve(&server, &dir).await;
    let range = link.origin_selection_range.expect("origin range is set");
    assert_eq!(
        (range.start.character, range.end.character),
        (dir.string_column, dir.string_end_column),
        "@livewire must still resolve through its own branch, keeping the narrow \
         string range — the configured path would have reported the whole directive"
    );
}

#[tokio::test]
async fn view_directives_setting_parses_and_applies_without_a_restart() {
    // The setting travels the same `update_settings` path `directiveSpacing`
    // uses, which `initialize`, `did_change_configuration`, and the
    // `workspace/configuration` pull all funnel into — so a later payload
    // replaces the earlier one live. Also pins the documented JSON shape:
    // `blade.viewDirectives.{firstArg,secondArg}`, both defaulting to empty.
    let server = test_server();
    assert_eq!(
        *server.view_directives.read().await,
        Default::default(),
        "both lists default to empty"
    );

    set_view_directives(&server, &["myPanel"], &["myPanelWhen"]).await;
    {
        let applied = server.view_directives.read().await;
        assert_eq!(applied.first_arg, vec!["myPanel".to_string()]);
        assert_eq!(applied.second_arg, vec!["myPanelWhen".to_string()]);
    }

    // A payload that omits the block resets it — the same live-replacement
    // semantics every other setting has.
    let settings: LspSettings = serde_json::from_value(serde_json::json!({})).unwrap();
    server.update_settings(&settings).await;
    assert_eq!(
        *server.view_directives.read().await,
        Default::default(),
        "an omitted viewDirectives block falls back to the empty default"
    );
}

// ─────────────────────── diagnostics stay untouched ───────────────────────

#[tokio::test]
async fn fallback_resolved_directive_produces_no_missing_view_diagnostic() {
    // The whole point of splitting the two consumers: a directive reachable only
    // through the fallback or the `viewDirectives` setting must never raise a
    // "View file not found" diagnostic, so a false squiggle on working code is
    // impossible by construction. The diagnostic gate still admits exactly
    // `@extends` and `@include` — including none of the *other* names goto has
    // always resolved (`includeIf`, `includeWhen`, `each`, `component`).
    for name in ["extends", "include"] {
        assert!(
            directive_takes_missing_view_diagnostic(name),
            "@{name} must keep raising missing-view diagnostics"
        );
    }

    for name in [
        CUSTOM,
        "myPanel",
        "renderWhen",
        "section",
        "includeIf",
        "includeWhen",
        "includeUnless",
        "each",
        "component",
        "livewire",
        "includeFirst",
    ] {
        assert!(
            !directive_takes_missing_view_diagnostic(name),
            "@{name} must never raise a missing-view diagnostic — goto resolves \
             far more names than diagnostics validate, and that asymmetry is the fix"
        );
    }
}

#[tokio::test]
async fn fallback_ignores_a_directive_with_no_quoted_argument() {
    // Fail closed on the shapes the reused extractor cannot read: a variable
    // argument, an empty string, or no arguments at all yield no candidate and
    // no link — never a guess. Multi-argument custom directives
    // (`@renderPartial('dashboard', $data)`) are out of scope for #325, so the
    // first-quoted-string reading of that form is asserted here as-is.
    let root = tempfile::TempDir::new().unwrap();
    let view = root.path().join("resources/views/dashboard.blade.php");
    write(&view, VIEW_BODY);

    let server = test_server();
    seed(&server, simple_config(root.path())).await;

    for args in ["($view)", "('')", "()"] {
        let mut dir = directive_ref(CUSTOM, "('dashboard')");
        dir.arguments = Some(args.to_string());
        assert!(
            server
                .create_directive_location_from_salsa(&dir)
                .await
                .is_none(),
            "args {args} carry no readable view name — the fallback must not guess"
        );
    }

    // Documented current behaviour, unchanged by #325: the first quoted string
    // wins and the trailing data argument is ignored.
    let link = resolve(&server, &directive_ref(CUSTOM, "('dashboard', ['a' => 1])")).await;
    assert_targets(&link, &view);
}
