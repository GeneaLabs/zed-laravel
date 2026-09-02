//! Tests for the Salsa-backed incremental computation actor.
//!
//! Originally lived as two inline `#[cfg(test)] mod *_tests {}` blocks
//! inside `salsa_impl.rs`. Flattened into a single submodule here
//! to keep the business-logic file clean while preserving access to
//! the parent module via `use super::*`.

use super::*;

// ─── Vite directive parsing ────────────────────────────────────────────

#[test]
fn test_vite_singular_syntax() {
    // @vite('resources/css/app.css') - args from tree-sitter
    let args = "('resources/css/app.css')";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "resources/css/app.css");
}

#[test]
fn test_vite_array_syntax() {
    // @vite(['resources/css/app.css', 'resources/js/app.js'])
    let args = "(['resources/css/app.css', 'resources/js/app.js'])";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "resources/css/app.css");
    assert_eq!(results[1].0, "resources/js/app.js");
}

#[test]
fn test_vite_double_quotes() {
    let args = r#"("resources/css/app.css")"#;
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "resources/css/app.css");
}

#[test]
fn test_extract_translation_from_echo() {
    // Test the regex extraction
    let content = r#"__("Welcome to our app")"#;
    let result = super::extract_translation_from_echo(content);
    assert!(result.is_some(), "Should extract translation from __()");
    let (key, start, end) = result.unwrap();
    assert_eq!(key, "Welcome to our app");
    println!("Extracted: key='{}', start={}, end={}", key, start, end);

    // Test with single quotes
    let content2 = "__('messages.welcome')";
    let result2 = super::extract_translation_from_echo(content2);
    assert!(
        result2.is_some(),
        "Should extract translation from __() with single quotes"
    );
    let (key2, _, _) = result2.unwrap();
    assert_eq!(key2, "messages.welcome");

    // Test trans()
    let content3 = "trans('auth.failed')";
    let result3 = super::extract_translation_from_echo(content3);
    assert!(result3.is_some(), "Should extract translation from trans()");
    let (key3, _, _) = result3.unwrap();
    assert_eq!(key3, "auth.failed");
}

#[test]
fn test_vite_column_positions() {
    // For @vite('resources/css/app.css'):
    // Position: 0123456789...
    //           @vite('resources/css/app.css')
    // @ at 0, v at 1, ... e at 4, ( at 5, ' at 6, r at 7
    // Path "resources/css/app.css" is 21 chars
    // LSP needs +1 offset, so start col is 8
    let args = "('resources/css/app.css')";
    let path = "resources/css/app.css";
    let results = parse_vite_directive_assets(args, 0, 0, 5);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, path);
    // Column should point to 'r' (first char of path), adjusted for LSP
    assert_eq!(results[0].2, 8, "start column should be 8");
    // End column should be 8 + 21 = 29
    assert_eq!(
        results[0].3,
        8 + path.len() as u32,
        "end column should be start + path.len()"
    );
}

// ─── Component alias parsing ────────────────────────────────────────────

fn make_config_with_alias(alias: &str, view: &str) -> LaravelConfigData {
    let mut aliases = HashMap::new();
    aliases.insert(alias.to_string(), view.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: aliases,
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

fn make_config_with_icon(tag: &str, svg_path: &str) -> LaravelConfigData {
    let mut icons = HashMap::new();
    icons.insert(tag.to_string(), svg_path.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: icons,
        class_component_files: HashMap::new(),
    }
}

#[test]
fn icon_tag_resolves_to_svg_path() {
    // The svg path lives under the project root — `scan_vendor_for_icon_sets`
    // always walks `root/vendor`, so registered icon paths are in-tree. This
    // also satisfies the #109 root-containment backstop, which drops any
    // candidate outside `self.root`.
    let config = make_config_with_icon(
        "heroicon-o-clock",
        "/project/vendor/blade-ui-kit/blade-heroicons/resources/svg/o-clock.svg",
    );
    let paths = config.resolve_component_path("heroicon-o-clock");
    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from("/project/vendor/blade-ui-kit/blade-heroicons/resources/svg/o-clock.svg"),
    );
}

#[test]
fn unregistered_icon_tag_falls_through() {
    let config = make_config_with_icon("heroicon-o-clock", "/abs/path/o-clock.svg");
    let paths = config.resolve_component_path("heroicon-o-bell");
    // Falls through to directory convention — no svg path returned.
    assert!(
        paths
            .iter()
            .all(|p| !p.to_string_lossy().ends_with("o-bell.svg")),
        "unregistered icon should not return a phantom svg path: {:?}",
        paths,
    );
}

#[test]
fn aliased_component_resolves_to_aliased_view_path() {
    let config = make_config_with_alias("light-button", "components.buttons.light-button");

    let paths = config.resolve_component_path("light-button");

    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("components/buttons/light-button.blade.php")),
        "expected aliased path, got: {:?}",
        paths,
    );
}

#[test]
fn unaliased_component_falls_back_to_directory_convention() {
    let config = make_config_with_alias("light-button", "components.buttons.light-button");

    // 'unaliased-component' is not registered; should fall through.
    let paths = config.resolve_component_path("unaliased-component");

    assert!(!paths.is_empty(), "expected fallback paths");
    assert!(
        paths
            .iter()
            .all(|p| !crate::path_segments::contains_segments(p, "buttons/light-button")),
        "alias must not bleed into unrelated lookups: {:?}",
        paths,
    );
}

#[test]
fn namespaced_component_bypasses_alias_map() {
    // Package components (`pkg::comp`) must not be intercepted by the alias map,
    // since namespace separators carry their own resolution rules.
    let config = make_config_with_alias("courier::alert", "components.never.this");

    let paths = config.resolve_component_path("courier::alert");

    assert!(
        paths
            .iter()
            .all(|p| !crate::path_segments::contains_segments(p, "components/never/this")),
        "namespaced lookup must bypass alias map: {:?}",
        paths,
    );
}

// ─── Anonymous component path / namespace resolution (issue #44) ────────

fn make_config_with_anonymous_path(prefix: &str, abs_dir: &str) -> LaravelConfigData {
    let mut anon = HashMap::new();
    anon.insert(prefix.to_string(), PathBuf::from(abs_dir));

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: anon,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

fn make_config_with_anonymous_namespace(prefix: &str, dir: &str) -> LaravelConfigData {
    let mut anon = HashMap::new();
    anon.insert(prefix.to_string(), dir.to_string());

    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: anon,
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

#[test]
fn anonymous_component_path_resolves_to_registered_directory() {
    // Issue #44: Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage')
    // <x-backstage::layout> must resolve to the registered directory directly —
    // not the package-publish `resources/views/vendor/backstage/...` guess.
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::layout");

    assert_eq!(
        paths.first(),
        Some(&PathBuf::from(
            "/project/resources/views/backstage/components/layout.blade.php"
        )),
        "registered anonymousComponentPath must be the first (expected) candidate: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_path_supports_index_convention() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::layout");

    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/resources/views/backstage/components/layout/index.blade.php"
            )),
        "expected the index.blade.php convention candidate, got: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_path_resolves_dotted_component_name() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    let paths = config.resolve_component_path("backstage::forms.input");

    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/resources/views/backstage/components/forms/input.blade.php"
            )),
        "dotted component name must map dots to slashes: {:?}",
        paths,
    );
}

#[test]
fn anonymous_component_namespace_resolves_relative_to_view_paths() {
    // Blade::anonymousComponentNamespace('components.flux', 'flux')
    // <x-flux::button> -> resources/views/components/flux/button.blade.php
    let config = make_config_with_anonymous_namespace("flux", "components/flux");

    let paths = config.resolve_component_path("flux::button");

    assert!(
        paths
            .iter()
            .any(|p| p
                == &PathBuf::from("/project/resources/views/components/flux/button.blade.php")),
        "anonymous namespace must resolve under the view path: {:?}",
        paths,
    );
}

// ─── Flux component resolution (issue #60) ─────────────────────────────

fn make_bare_config() -> LaravelConfigData {
    LaravelConfigData {
        root: PathBuf::from("/project"),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

#[test]
fn normalize_flux_tag_name_rewrites_single_colon_prefix() {
    assert_eq!(
        normalize_flux_tag_name("flux:button").as_deref(),
        Some("flux::button"),
    );
    assert_eq!(
        normalize_flux_tag_name("flux:icon.arrow-right").as_deref(),
        Some("flux::icon.arrow-right"),
    );
}

#[test]
fn normalize_flux_tag_name_leaves_non_flux_and_namespaced_alone() {
    // Already-namespaced `<x-flux::button>` arrives pre-normalized.
    assert_eq!(normalize_flux_tag_name("flux::button"), None);
    // Non-Flux names untouched.
    assert_eq!(normalize_flux_tag_name("button"), None);
    assert_eq!(normalize_flux_tag_name("livewire:counter"), None);
}

#[test]
fn flux_tag_resolves_to_conventional_sources() {
    // `<flux:button>` resolves with no explicit registration via the
    // convention fallback: app-published views, the package source, and Flux Pro.
    let config = make_bare_config();
    let paths = config.resolve_component_path("flux:button");

    assert!(
        paths
            .iter()
            .any(|p| p == &PathBuf::from("/project/resources/views/flux/button.blade.php")),
        "published view path missing: {:?}",
        paths,
    );
    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/vendor/livewire/flux/stubs/resources/views/flux/button.blade.php"
            )),
        "package source path missing: {:?}",
        paths,
    );
    assert!(
        paths.iter().any(|p| p
            == &PathBuf::from(
                "/project/vendor/livewire/flux-pro/stubs/resources/views/flux/button.blade.php"
            )),
        "Flux Pro path missing: {:?}",
        paths,
    );
}

#[test]
fn flux_dotted_tag_maps_dots_to_directories() {
    // `<flux:icon.arrow-right>` → `flux/icon/arrow-right.blade.php`.
    let config = make_bare_config();
    let paths = config.resolve_component_path("flux:icon.arrow-right");

    assert!(
        paths.iter().any(
            |p| p == &PathBuf::from("/project/resources/views/flux/icon/arrow-right.blade.php")
        ),
        "dotted Flux name must map dots to directories: {:?}",
        paths,
    );
}

#[test]
fn flux_namespace_tag_resolves_same_as_single_colon() {
    // The `<x-flux::button>` namespace form resolves through the same fallback.
    let config = make_bare_config();
    let paths = config.resolve_component_path("flux::button");
    assert!(
        paths
            .iter()
            .any(|p| p == &PathBuf::from("/project/resources/views/flux/button.blade.php")),
        "namespace form must resolve via the Flux fallback: {:?}",
        paths,
    );
}

// ─── Component-name validation & root containment (issue #109) ─────────

#[test]
fn empty_component_name_yields_no_candidates() {
    let config = make_bare_config();
    assert!(
        config.resolve_component_path("").is_empty(),
        "an empty component name must produce zero path candidates",
    );
}

#[test]
fn flux_empty_name_yields_no_candidates() {
    // `<flux:>` normalizes to the bare `flux::` namespace prefix with an empty
    // component — it must not resolve to `.blade.php` nonsense paths.
    let config = make_bare_config();
    assert!(
        config.resolve_component_path("flux:").is_empty(),
        "`<flux:>` (empty component) must produce zero candidates",
    );
    // The already-namespaced bare form behaves identically.
    assert!(
        config.resolve_component_path("flux::").is_empty(),
        "bare `flux::` must produce zero candidates",
    );
}

#[test]
fn leading_dot_component_name_yields_no_candidates() {
    // The `<flux:.etc.passwd>` attack shape: the dot→slash substitution would
    // turn `.etc.passwd` into the absolute `/etc/passwd`, which `PathBuf::join`
    // resolves outside the project root. Reject it at the source.
    let config = make_bare_config();
    assert!(
        config.resolve_component_path("flux:.etc.passwd").is_empty(),
        "a leading-dot component name must produce zero candidates",
    );
    // Same guard via an explicit namespace prefix.
    assert!(
        config.resolve_component_path("evil::.hidden").is_empty(),
        "a leading-dot namespaced component must produce zero candidates",
    );
}

#[test]
fn absolute_component_name_yields_no_candidates() {
    // A name that becomes an absolute path after dot→slash substitution must be
    // rejected — here a literal leading slash in the component part.
    let config = make_bare_config();
    assert!(
        config
            .resolve_component_path("evil::/etc/passwd")
            .is_empty(),
        "a component that resolves to an absolute path must produce zero candidates",
    );
}

#[test]
fn parent_dir_traversal_name_yields_no_candidates() {
    // A `../` traversal in the component name starts with a dot, so the source
    // guard rejects it before any path is built — it never reaches the
    // filesystem.
    let config = make_bare_config();
    assert!(
        config
            .resolve_component_path("flux::../../../../etc/passwd")
            .is_empty(),
        "a `../` traversal component name must produce zero candidates",
    );
}

#[test]
fn candidates_escaping_root_are_dropped() {
    // Root-containment backstop: even when a registered directory sits outside
    // the project root, any candidate built under it is dropped before return,
    // while the in-root vendor-publish guesses survive.
    let config = make_config_with_anonymous_path("evil", "/outside/root/components");

    let paths = config.resolve_component_path("evil::layout");

    assert!(!paths.is_empty(), "in-root candidates should still resolve");
    assert!(
        paths.iter().all(|p| p.starts_with("/project")),
        "every returned candidate must stay under the project root: {:?}",
        paths,
    );
    assert!(
        paths.iter().all(|p| !p.starts_with("/outside")),
        "candidates under an out-of-root registered directory must be dropped: {:?}",
        paths,
    );
}

#[test]
fn normal_component_names_resolve_under_root() {
    // Regression guard for the #109 changes: ordinary names still resolve, and
    // every candidate stays in-tree.
    let config = make_bare_config();
    for name in [
        "forms.input",
        "flux::button",
        "courier::alert",
        "flux:button",
    ] {
        let paths = config.resolve_component_path(name);
        assert!(
            !paths.is_empty(),
            "`{name}` should still resolve to candidates"
        );
        assert!(
            paths.iter().all(|p| p.starts_with("/project")),
            "`{name}` candidates must stay under the project root: {:?}",
            paths,
        );
    }
}

#[test]
fn slash_bearing_name_yields_no_candidates() {
    // Holmes's PR #150 review: a mid-path `..` traversal smuggled behind a
    // literal slash — `flux::foo/../../../../etc/passwd` — is NOT caught by the
    // leading-dot guard (no leading dot) nor by an absolute-after-substitution
    // check (`replace('.', "/")` leaves a non-`/`-leading string). No real
    // Blade/Flux component name contains a forward slash (nesting uses dots,
    // namespaces use `::`), so the source guard now rejects the whole
    // slash-bearing class — closing both the mid-path-traversal and the
    // `evil::/etc/passwd` absolute shapes before any path is built.
    let config = make_bare_config();
    for name in [
        "flux::foo/../../../../etc/passwd",
        "flux:foo/../../../../etc/passwd",
        "evil::foo/../bar",
        "components/../../etc/passwd",
        "evil::/etc/passwd",
    ] {
        assert!(
            config.resolve_component_path(name).is_empty(),
            "slash-bearing name `{name}` must produce zero candidates",
        );
    }
}

#[test]
fn candidates_with_interior_parent_dir_escape_are_dropped() {
    // Defense-in-depth backstop, independent of the source guard: a slash-free,
    // dot-free component name (`evil::layout`) sails past the front guard, but
    // the *registered* directory itself carries an interior `..`. A
    // misregistered `anonymousComponentPath` of `/project/sub/../../escape`
    // lexically resolves to `/escape`, outside the root. A plain component-wise
    // `starts_with("/project")` is fooled — the path's leading components are
    // still `/`, `project` — so the backstop must lexically normalize first.
    let config = make_config_with_anonymous_path("evil", "/project/sub/../../escape/components");

    let paths = config.resolve_component_path("evil::layout");

    assert!(
        paths
            .iter()
            .all(|p| !crate::route_discovery::normalize_path(p).starts_with("/escape")),
        "candidates that lexically escape the project root must be dropped: {:?}",
        paths,
    );
    // The in-root vendor-publish guesses still survive, so the filter isn't
    // just dropping everything.
    assert!(
        paths.iter().any(|p| p.starts_with("/project")),
        "in-root candidates should still resolve: {:?}",
        paths,
    );
}

#[test]
fn unregistered_anonymous_prefix_does_not_borrow_registered_directory() {
    let config = make_config_with_anonymous_path(
        "backstage",
        "/project/resources/views/backstage/components",
    );

    // A different prefix must not resolve into the backstage directory.
    let paths = config.resolve_component_path("other::layout");

    assert!(
        paths
            .iter()
            .all(|p| !crate::path_segments::contains_segments(p, "backstage/components")),
        "unregistered prefix must not resolve into a registered anon directory: {:?}",
        paths,
    );
}

#[cfg(unix)]
#[test]
fn symlink_escaping_candidate_is_dropped_by_canonicalize_backstop() {
    // Issue #152: the canonicalize *drop*-leg of the root-containment backstop.
    //
    // The backstop in `resolve_component_path` has two arms (PR #150):
    //
    //     paths.retain(|path| match (path.canonicalize(), &canonical_root) {
    //         (Ok(real_path), Ok(real_root)) => real_path.starts_with(real_root), // canonicalize
    //         _ => normalize_path(path).starts_with(&self.root),                  // textual fallback
    //     });
    //
    // Every #109 test runs against a fictional `/project` root, so its candidates
    // never exist on disk and only the *textual-fallback* `_` arm ever fires. The
    // canonicalize *keep*-leg (an in-root path that exists) is covered by the
    // `TempDir` tests, but the *drop*-leg is not: a candidate that EXISTS on disk
    // yet, once `canonicalize()` resolves its symlinks, points OUTSIDE the project
    // root — a symlink escape (`{root}/vendor/evil` → an out-of-root directory).
    // This pins that drop-leg. The registered directory is lexically in-root, so
    // it sails past the textual guard; only canonicalization reveals the escape.
    use std::os::unix::fs::symlink;

    // A real project root, plus a second tempdir OUTSIDE it standing in for the
    // attacker-controlled target (the moral equivalent of `/etc`).
    let (_dir, root) = project_with_files(&[]);
    let outside = TempDir::new().unwrap();

    // A real component file inside the out-of-root target, so the candidate built
    // through the symlink EXISTS on disk and `path.canonicalize()` returns
    // `Ok(real_path)` — exercising the canonicalize arm, not the `_` fallback.
    std::fs::write(
        outside.path().join("widget.blade.php"),
        "<div>escaped</div>\n",
    )
    .unwrap();

    // Symlink an in-root path (`{root}/vendor/evil`) at the out-of-root target,
    // then register THAT in-root directory as the `evil` anonymousComponentPath.
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    let symlinked = root.join("vendor/evil");
    symlink(outside.path(), &symlinked).unwrap();

    let mut anon = HashMap::new();
    anon.insert("evil".to_string(), symlinked.clone());
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: anon,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    // Precondition: the escaping candidate really does exist on disk (via the
    // symlink), so `canonicalize()` returns Ok and the canonicalize arm — not the
    // textual fallback — is what must drop it. Guards against a vacuous pass.
    let escaping = symlinked.join("widget.blade.php");
    assert!(
        escaping.exists(),
        "precondition: the symlink-escaping candidate must exist on disk so \
         `canonicalize()` returns Ok and the canonicalize arm runs: {:?}",
        escaping,
    );

    let paths = config.resolve_component_path("evil::widget");

    // The canonicalize drop-leg must remove every candidate whose canonicalized
    // form escapes the project root. Non-existent speculative candidates can't be
    // canonicalized and are treated as in-root (the textual fallback keeps them).
    let canonical_root = root.canonicalize().unwrap();
    assert!(
        paths.iter().all(|p| match p.canonicalize() {
            Ok(real) => real.starts_with(&canonical_root),
            Err(_) => true,
        }),
        "every on-disk candidate must canonicalize inside the project root; a \
         symlink-escaping candidate leaked: {:?}",
        paths,
    );

    // Specifically, the candidate that escapes via the symlink must be gone —
    // this is the drop-leg firing, not merely the absence of any candidate. We do
    // NOT assert the result is empty: in-root vendor-publish guesses survive via
    // the textual fallback.
    assert!(
        !paths.contains(&escaping),
        "the symlink-escaping candidate must be dropped by the canonicalize \
         backstop: {:?}",
        paths,
    );
}

// ─── PHP path-expression resolution ─────────────────────────────────────

#[test]
fn resolve_php_path_expr_handles_resource_path() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr(
            "resource_path('views/backstage/components')",
            &root,
            &provider_dir
        ),
        Some(PathBuf::from(
            "/project/resources/views/backstage/components"
        )),
    );
}

#[test]
fn resolve_php_path_expr_handles_base_and_app_path() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("base_path('resources/views/x')", &root, &provider_dir),
        Some(PathBuf::from("/project/resources/views/x")),
    );
    assert_eq!(
        resolve_php_path_expr("app_path('View/Components')", &root, &provider_dir),
        Some(PathBuf::from("/project/app/View/Components")),
    );
}

#[test]
fn resolve_php_path_expr_handles_dir_constant() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/pkg/src/Providers");
    assert_eq!(
        resolve_php_path_expr(
            "__DIR__ . '/../resources/views/components'",
            &root,
            &provider_dir
        ),
        Some(PathBuf::from("/pkg/src/resources/views/components")),
    );
}

#[test]
fn resolve_php_path_expr_handles_absolute_literal() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("'/abs/components'", &root, &provider_dir),
        Some(PathBuf::from("/abs/components")),
    );
}

#[test]
fn resolve_php_path_expr_rejects_unresolvable_expression() {
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");
    assert_eq!(
        resolve_php_path_expr("$this->componentPath", &root, &provider_dir),
        None,
    );
}

// ─── Service-provider registration extraction ───────────────────────────

#[test]
fn extract_anonymous_component_paths_reads_registration() {
    let src = r#"
        public function boot(): void
        {
            Blade::anonymousComponentPath(resource_path('views/backstage/components'), 'backstage');
        }
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_anonymous_component_paths(src, &root, &provider_dir);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "backstage");
    assert_eq!(
        regs[0].1,
        PathBuf::from("/project/resources/views/backstage/components"),
    );
}

#[test]
fn extract_anonymous_component_namespaces_normalizes_dots() {
    let src = "Blade::anonymousComponentNamespace('components.flux', 'flux');";

    let regs = extract_anonymous_component_namespaces(src);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "flux");
    assert_eq!(regs[0].1, "components/flux");
}

// ─── Salsa actor: config reflects provider registrations (issue #44) ────

/// A provider that registers both anonymous-component forms. Parsing is
/// text-based, so the paths need not exist on disk.
fn anon_component_provider_src() -> String {
    r#"<?php
namespace App\Providers;
use Illuminate\Support\Facades\Blade;
class AppServiceProvider {
    public function boot(): void {
        Blade::anonymousComponentPath(resource_path('views/test/components'), 'test');
        Blade::anonymousComponentNamespace('components.flux', 'flux');
    }
}
"#
    .to_string()
}

#[tokio::test]
async fn salsa_config_indexes_anonymous_component_registrations() {
    let handle = SalsaActor::spawn();
    let root = PathBuf::from("/tmp/zed-laravel-issue44");

    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            root.join("app/Providers/AppServiceProvider.php"),
            anon_component_provider_src(),
            2,
            root.clone(),
        )
        .await
        .unwrap();

    let config = handle.get_laravel_config().await.unwrap().unwrap();

    assert_eq!(
        config.anonymous_component_paths.get("test"),
        Some(&root.join("resources/views/test/components")),
        "anonymousComponentPath must be indexed into the Laravel config",
    );
    assert_eq!(
        config.anonymous_component_namespaces.get("flux"),
        Some(&"components/flux".to_string()),
        "anonymousComponentNamespace must be indexed into the Laravel config",
    );
}

#[tokio::test]
async fn salsa_config_refreshes_when_provider_registered_after_first_build() {
    // Regression for the stale-config bug: the config is built (and memoized)
    // before the provider is registered, then the provider arrives via the
    // init-time app rescan. get_laravel_config must rebuild — not serve the
    // cached empty-namespace config — or `<x-test::...>` stays a false
    // "not found" until an unrelated provider edit forces an invalidation.
    let handle = SalsaActor::spawn();
    let root = PathBuf::from("/tmp/zed-laravel-issue44-late");

    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();

    // First build — no providers registered yet. This memoizes config_cache.
    let before = handle.get_laravel_config().await.unwrap().unwrap();
    assert!(
        before.anonymous_component_paths.is_empty(),
        "precondition: no anon paths before the provider is registered",
    );

    // Provider registered late, as during the init-time app rescan.
    handle
        .register_service_provider_source(
            root.join("app/Providers/AppServiceProvider.php"),
            anon_component_provider_src(),
            2,
            root.clone(),
        )
        .await
        .unwrap();

    let after = handle.get_laravel_config().await.unwrap().unwrap();
    assert_eq!(
        after.anonymous_component_paths.get("test"),
        Some(&root.join("resources/views/test/components")),
        "config must refresh after late provider registration (config_cache must be invalidated)",
    );
}

#[tokio::test]
async fn snapshot_bindings_maps_keys_to_concrete() {
    // The build-pass snapshot must expose `app('key') → concrete FQCN` so
    // `app('currentTenant')->member` resolves while indexing. It merges both
    // registries (singletons + plain binds) and normalizes the concrete to the
    // leading-backslash-free form the class index keys on.
    let handle = SalsaActor::spawn();

    let reg = |abstract_name: &str, concrete: &str, bt: BindingTypeData| BindingRegistrationData {
        abstract_name: abstract_name.to_string(),
        concrete_class: concrete.to_string(),
        file_path: None,
        binding_type: bt,
        source_file: None,
        source_line: None,
        priority: 2,
    };

    let mut singletons = HashMap::new();
    // Leading backslash on purpose — the snapshot must strip it.
    singletons.insert(
        "currentTenant".to_string(),
        reg(
            "currentTenant",
            "\\App\\Models\\Tenant",
            BindingTypeData::Singleton,
        ),
    );
    let mut bindings = HashMap::new();
    bindings.insert(
        "reporter".to_string(),
        reg("reporter", "App\\Services\\Reporter", BindingTypeData::Bind),
    );

    handle
        .register_service_provider_registry(HashMap::new(), bindings, singletons)
        .await
        .unwrap();

    let snapshot = handle.snapshot_bindings().await.unwrap();
    assert_eq!(
        snapshot.get("currentTenant").map(String::as_str),
        Some("App\\Models\\Tenant"),
        "singleton key maps to concrete, leading backslash normalized",
    );
    assert_eq!(
        snapshot.get("reporter").map(String::as_str),
        Some("App\\Services\\Reporter"),
        "plain bind key maps to concrete",
    );
}

#[test]
fn container_aware_resolver_prefers_bindings_and_normalizes() {
    // The live-query-path resolver: bindings win over singletons on collision,
    // concrete FQCNs are backslash-normalized, and class_file delegates to the
    // class index.
    use crate::member_resolver::ClassFileResolver;

    let reg = |concrete: &str, bt: BindingTypeData| BindingRegistrationData {
        abstract_name: "x".to_string(),
        concrete_class: concrete.to_string(),
        file_path: None,
        binding_type: bt,
        source_file: None,
        source_line: None,
        priority: 2,
    };

    let mut bindings = HashMap::new();
    bindings.insert(
        "dup".to_string(),
        reg("App\\FromBind", BindingTypeData::Bind),
    );
    let mut singletons = HashMap::new();
    singletons.insert(
        "dup".to_string(),
        reg("App\\FromSingleton", BindingTypeData::Singleton),
    );
    singletons.insert(
        "tenant".to_string(),
        reg("\\App\\Models\\Tenant", BindingTypeData::Singleton),
    );

    let index = crate::class_hierarchy_index::ClassHierarchyIndex::default();
    let resolver = ContainerAwareResolver {
        index: &index,
        bindings: &bindings,
        singletons: &singletons,
        facade_aliases: std::sync::Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Default::default(),
    };

    // Singleton-only key resolves; leading backslash normalized.
    assert_eq!(
        resolver.binding_concrete("tenant").as_deref(),
        Some("App\\Models\\Tenant"),
    );
    // Bindings win over singletons on key collision.
    assert_eq!(
        resolver.binding_concrete("dup").as_deref(),
        Some("App\\FromBind"),
    );
    // Unknown key → None.
    assert_eq!(resolver.binding_concrete("nope"), None);
    // class_file delegates to the (here empty) class index.
    assert_eq!(resolver.class_file("App\\Models\\Tenant"), None);
}

// ─── collect_matches_for_symbol (find-references engine) ───────────────

use std::path::PathBuf;

fn dummy_path() -> PathBuf {
    PathBuf::from("/fixture/app.php")
}

#[test]
fn parse_file_patterns_extracts_views_from_blade_echo() {
    // Sanity check on the Salsa-cached side: `{{ view('partials.header') }}`
    // in a Blade file must show up in ParsedPatterns.views. Regression test
    // for the bug where tree-sitter-php couldn't see through Blade wrappers.
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/layout.blade.php");
    let source = "<div>{{ view('partials.header') }}</div>\n";
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let names: Vec<String> = patterns
        .views(&db)
        .iter()
        .map(|v| v.name(&db).name(&db).clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "partials.header"),
        "expected 'partials.header' view extracted from Blade echo, got {:?}",
        names
    );
}

#[test]
fn parse_file_patterns_extracts_translations_from_blade_echo() {
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/page.blade.php");
    let source = "<p>{{ __('auth.failed') }}</p>\n";
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let keys: Vec<String> = patterns
        .translation_refs(&db)
        .iter()
        .map(|t| t.key(&db).key(&db).clone())
        .collect();
    assert!(
        keys.iter().any(|k| k == "auth.failed"),
        "expected 'auth.failed' translation, got {:?}",
        keys
    );
}

#[test]
fn parse_file_patterns_extracts_views_from_blade_php_block() {
    // `@php ... @endphp` content gets the same re-parse treatment.
    let db = LaravelDatabase::default();
    let path = PathBuf::from("/fixture/php-block.blade.php");
    let source = r#"@php
    $partial = view('partials.alert');
@endphp
"#;
    let file = SourceFile::new(&db, path, 0, source.to_string());
    let patterns = parse_file_patterns(&db, file);
    let names: Vec<String> = patterns
        .views(&db)
        .iter()
        .map(|v| v.name(&db).name(&db).clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "partials.alert"),
        "expected 'partials.alert' view extracted from @php block, got {:?}",
        names
    );
}

#[test]
fn collect_view_matches_only_named_classifications() {
    let mut p = ParsedPatternsData::default();
    p.views.push(Arc::new(ViewReferenceData {
        name: "users.profile".into(),
        line: 1,
        column: 5,
        end_column: 24,
        is_route_view: false,
        is_property_site: false,
    }));
    p.views.push(Arc::new(ViewReferenceData {
        name: "other.view".into(),
        line: 2,
        column: 5,
        end_column: 20,
        is_route_view: false,
        is_property_site: false,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::View("users.profile".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1, "only matching view name should appear");
    assert_eq!(out[0].line, 1);
    assert_eq!(out[0].column, 5);
}

#[test]
fn collect_view_also_picks_up_include_directives() {
    let mut p = ParsedPatternsData::default();
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "include".into(),
        arguments: Some("('users.profile')".into()),
        line: 0,
        column: 0,
        end_column: 30,
        string_column: 10,
        string_end_column: 23,
    }));
    p.directives.push(Arc::new(DirectiveReferenceData {
        name: "include".into(),
        arguments: Some("('not.this.one')".into()),
        line: 1,
        column: 0,
        end_column: 30,
        string_column: 10,
        string_end_column: 22,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::View("users.profile".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].line, 0);
}

#[test]
fn collect_route_matches() {
    let mut p = ParsedPatternsData::default();
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "home".into(),
        line: 0,
        column: 6,
        end_column: 10,
    }));
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "home".into(),
        line: 4,
        column: 12,
        end_column: 16,
    }));
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "admin.users".into(),
        line: 5,
        column: 12,
        end_column: 23,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("home".into()),
        &mut out,
    );

    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|l| l.line == 0 || l.line == 4));
}

#[test]
fn collect_route_matches_dotted_name_exact() {
    // Folio-style dotted route names (e.g. `route('users.show')`) must match by
    // full-string equality, never by prefix. Two entries whose names overlap as
    // prefixes — "users.show" and the shorter "users" — let us prove neither
    // bleeds into the other. The `collect_route_matches` fixture above uses
    // distinct names ("home" / "admin.users"), so a `starts_with`-style
    // regression in the route arm would slip past it; this test guards that gap.
    let mut p = ParsedPatternsData::default();
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "users.show".into(),
        line: 0,
        column: 6,
        end_column: 16,
    }));
    p.route_refs.push(Arc::new(RouteReferenceData {
        name: "users".into(),
        line: 1,
        column: 6,
        end_column: 11,
    }));
    p.build_position_index();

    // Querying the full dotted name returns only its own entry (line 0); the
    // shorter "users" prefix must not be swept in.
    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("users.show".into()),
        &mut out,
    );
    assert_eq!(
        out.len(),
        1,
        "dotted name should match exactly its own entry"
    );
    assert_eq!(out[0].line, 0);

    // Querying the shorter prefix returns only its own entry (line 1); the
    // longer "users.show" must not be matched — catching a `starts_with`
    // regression that the distinct-name fixture above cannot.
    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("users".into()),
        &mut out,
    );
    assert_eq!(out.len(), 1, "prefix must not match the longer dotted name");
    assert_eq!(out[0].line, 1);
}

#[test]
fn collect_config_matches_by_key() {
    let mut p = ParsedPatternsData::default();
    p.config_refs.push(Arc::new(ConfigReferenceData {
        key: "app.name".into(),
        line: 0,
        column: 8,
        end_column: 16,
    }));
    p.config_refs.push(Arc::new(ConfigReferenceData {
        key: "different.key".into(),
        line: 1,
        column: 8,
        end_column: 21,
    }));
    p.build_position_index();

    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Config("app.name".into()),
        &mut out,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].line, 0);
}

#[test]
fn collect_returns_empty_for_no_matches() {
    // Negative guarantee: same-shape strings present in other pattern kinds
    // must NOT bleed across kinds.
    let mut p = ParsedPatternsData::default();
    p.views.push(Arc::new(ViewReferenceData {
        name: "home".into(),
        line: 0,
        column: 5,
        end_column: 9,
        is_route_view: false,
        is_property_site: false,
    }));
    p.build_position_index();

    // Asking for route "home" must NOT match the view "home".
    let mut out = Vec::new();
    collect_matches_for_symbol(
        &dummy_path(),
        &p,
        &SymbolRefData::Route("home".into()),
        &mut out,
    );

    assert!(
        out.is_empty(),
        "a view name must not satisfy a route reference query"
    );
}

// ─── Shared component resolution (issue #69) ────────────────────────────
//
// `component_candidate_paths` is the single source of truth shared by
// goto-definition and the "component not found" diagnostic. These tests pin
// the class-based `Blade::componentNamespace` (PSR-4) resolution that the
// naive guesses in `resolve_component_path` missed — the Filament / mail
// failure case from the issue — plus the false-negative guarantee.

use crate::composer_autoload::ComposerAutoload;
use tempfile::TempDir;

/// Build a Laravel-shaped tempdir with the given (relative path, body) pairs.
fn project_with_files(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for (relpath, body) in files {
        let full = dir.path().join(relpath);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, body).unwrap();
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// Config whose only interesting field is a set of `Blade::componentNamespace`
/// registrations (`prefix => PHP namespace`), rooted at `root`.
fn config_with_component_namespaces(root: &Path, ns: &[(&str, &str)]) -> LaravelConfigData {
    let mut component_namespaces = HashMap::new();
    for (prefix, php_ns) in ns {
        component_namespaces.insert(prefix.to_string(), php_ns.to_string());
    }
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces,
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// Mirror of the live resolver's existence check: a component "resolves" when
/// any candidate path exists on disk.
fn resolves(name: &str, config: &LaravelConfigData, autoload: &ComposerAutoload) -> bool {
    component_candidate_paths(name, config, autoload)
        .iter()
        .any(|p| p.exists())
}

/// End-to-end for issue #60's completion criterion: a Flux component resolved
/// from its conventional vendor source must surface its declared `@props` as
/// offered attributes. Exercises the same chain the completion handler runs —
/// `resolve_component_path` → first on-disk candidate → `extract_prop_names`.
#[test]
fn flux_component_props_are_offered_for_completion() {
    let (_dir, root) = project_with_files(&[(
        "vendor/livewire/flux/stubs/resources/views/flux/button.blade.php",
        "@props([\n    'variant' => 'primary',\n    'size',\n    'icon' => null,\n])\n\
         <button {{ $attributes }}>{{ $slot }}</button>\n",
    )]);
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    // Resolve `<flux:button>` the way goto/hover/completion do, then read the
    // first candidate that actually exists on disk.
    let resolved = config
        .resolve_component_path("flux:button")
        .into_iter()
        .find(|p| p.exists())
        .expect("flux:button should resolve to its vendor blade source");

    let content = std::fs::read_to_string(&resolved).unwrap();
    let props = crate::blade_props::extract_prop_names(&content);

    assert!(
        props.contains(&"variant".to_string()),
        "known prop `variant` must be offered for a resolved Flux component: {:?}",
        props,
    );
    assert_eq!(
        props,
        vec!["variant", "size", "icon"],
        "all declared props offered, in declaration order",
    );
}

/// End-to-end for issue #107's hover criterion: hovering a `<flux:button>`
/// whose only on-disk source is the conventional Flux *vendor* layout must
/// render a hover card whose source link points at `vendor/livewire/flux` —
/// not the published `resources/views/flux` path. Mirrors the anonymous-
/// component leg of `Backend::hover_for_component`: resolve → first on-disk
/// candidate → `extract_props_directive` + `source_link` → `hover::render`.
///
/// PR #99 pinned the *resolution* leg (`flux_component_props_are_offered_for_completion`,
/// above); this pins the *rendered hover card* the resolution feeds.
#[test]
fn flux_hover_source_link_points_at_vendor_source() {
    let (_dir, root) = project_with_files(&[(
        "vendor/livewire/flux/stubs/resources/views/flux/button.blade.php",
        "@props(['type' => 'button', 'variant' => 'primary'])\n\
         <button {{ $attributes }}>{{ $slot }}</button>\n",
    )]);
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    // Resolve the way hover does, then take the first candidate on disk. The
    // published `resources/views/flux` path is never created, so the vendor
    // source is the first (and only) existing candidate.
    let resolved = config
        .resolve_component_path("flux:button")
        .into_iter()
        .find(|p| p.exists())
        .expect("flux:button should resolve to its vendor blade source");
    assert!(
        resolved.starts_with(root.join("vendor/livewire/flux")),
        "first on-disk candidate must be the vendor source, not the published \
         resources/views/flux path: {:?}",
        resolved,
    );

    // Build the hover card exactly as `hover_for_component` does for an
    // anonymous component: the `@props` directive as the code block, a source
    // link built from the resolved path, and no trailer (the file exists).
    let url = tower_lsp::lsp_types::Url::from_file_path(&resolved).unwrap();
    let display = resolved
        .strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let link = crate::hover::source_link(&display, url.as_str(), None);
    let snippet = crate::blade_props::extract_props_directive(&resolved);
    let rendered = crate::hover::render(&crate::hover::HoverContent {
        code: snippet.as_deref().map(|s| crate::hover::CodeBlock {
            language: crate::hover::CodeLanguage::Php,
            content: s,
        }),
        source_link: Some(&link),
        trailer: None,
        ..Default::default()
    });

    assert!(
        rendered.contains("](file://"),
        "rendered hover must carry a markdown source link: {rendered}",
    );
    assert!(
        rendered.contains("vendor/livewire/flux"),
        "the source-link URL must point at the vendor flux source, not the \
         published path: {rendered}",
    );
}

/// Complementary to the above: when no Flux source exists on disk, the hover
/// card must degrade to the `*(file not found)*` trailer with no source link —
/// the same shape `hover_for_component` renders when resolution finds no file.
#[test]
fn flux_hover_without_vendor_file_shows_file_not_found() {
    // Project rooted at a temp dir with NO flux source anywhere, so every
    // resolve candidate is a guaranteed miss.
    let (_dir, root) = project_with_files(&[]);
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    let resolved = config
        .resolve_component_path("flux:button")
        .into_iter()
        .find(|p| p.exists());
    assert!(
        resolved.is_none(),
        "no flux source should exist on disk for an empty project, got {:?}",
        resolved,
    );

    // Derive the link from the (absent) resolution the same way the handler
    // does — `None` path → no link → file-not-found trailer.
    let link = resolved.as_ref().map(|p| {
        let url = tower_lsp::lsp_types::Url::from_file_path(p).unwrap();
        crate::hover::source_link(&p.to_string_lossy(), url.as_str(), None)
    });
    let trailer = if link.is_none() {
        Some(crate::hover::FILE_NOT_FOUND_TRAILER)
    } else {
        None
    };
    let rendered = crate::hover::render(&crate::hover::HoverContent {
        source_link: link.as_deref(),
        trailer,
        ..Default::default()
    });

    assert!(
        rendered.contains(crate::hover::FILE_NOT_FOUND_TRAILER),
        "absent flux source must render the file-not-found trailer: {rendered}",
    );
    assert!(
        !rendered.contains("](file://"),
        "no source link may be rendered when the file is absent: {rendered}",
    );
}

#[test]
fn flux_tag_skips_invalid_colon_class_candidate() {
    // A namespaced tag must not emit a conventional class candidate — the
    // colon-bearing `app/View/Components/Flux:button.php` is an invalid path on
    // Windows and a guaranteed-miss `stat` on POSIX. (Follow-up from PR #99
    // round-2 review; `component_candidate_paths` doesn't touch the FS, so an
    // empty project root is enough.)
    let (_dir, root) = project_with_files(&[]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(&root, &[]);

    let candidates = component_candidate_paths("flux:button", &config, &autoload);
    assert!(
        candidates.iter().all(|p| !p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .contains(':')),
        "no candidate may carry a `:` in its filename: {:#?}",
        candidates,
    );
    assert!(
        !candidates
            .iter()
            .any(|p| p.starts_with(root.join("app/View/Components"))),
        "a namespaced Flux tag must not probe the conventional class dir: {:#?}",
        candidates,
    );
}

#[test]
fn psr4_class_namespace_component_resolves_across_two_namespaces() {
    // Two separate package namespaces registered via componentNamespace, each
    // shipped under a PSR-4 vendor layout that the naive
    // `vendor/<Namespace>/...` guess in resolve_component_path can't find.
    // Both `<x-filament::badge>` and `<x-nightshade::alert-banner>` must
    // resolve through the autoload map (issue #69 — at least two namespaces).
    let installed = r#"{
        "packages": [
            {
                "name": "filament/support",
                "autoload": { "psr-4": { "Filament\\Support\\": "src/" } },
                "install-path": "../filament/support"
            },
            {
                "name": "nightshade/ui",
                "autoload": { "psr-4": { "Nightshade\\Ui\\": "src/" } },
                "install-path": "../nightshade/ui"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/support/src/View/Components/Badge.php",
            "<?php namespace Filament\\Support\\View\\Components; class Badge {}",
        ),
        (
            "vendor/nightshade/ui/src/View/Components/AlertBanner.php",
            "<?php namespace Nightshade\\Ui\\View\\Components; class AlertBanner {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[
            ("filament", "Filament\\Support\\View\\Components"),
            ("nightshade", "Nightshade\\Ui\\View\\Components"),
        ],
    );

    assert!(
        resolves("filament::badge", &config, &autoload),
        "filament::badge must resolve via PSR-4 autoload: {:#?}",
        component_candidate_paths("filament::badge", &config, &autoload),
    );
    // kebab tag → PascalCase class file under the same namespace.
    assert!(
        resolves("nightshade::alert-banner", &config, &autoload),
        "nightshade::alert-banner must resolve via PSR-4 autoload: {:#?}",
        component_candidate_paths("nightshade::alert-banner", &config, &autoload),
    );
}

#[test]
fn psr4_class_namespace_resolves_dotted_subnamespace() {
    // `<x-filament::forms.text-input>` → Forms/TextInput.php under the
    // registered namespace.
    let installed = r#"{
        "packages": [
            {
                "name": "filament/forms",
                "autoload": { "psr-4": { "Filament\\Forms\\": "src/" } },
                "install-path": "../filament/forms"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/forms/src/View/Components/Forms/TextInput.php",
            "<?php namespace Filament\\Forms\\View\\Components\\Forms; class TextInput {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[("filament", "Filament\\Forms\\View\\Components")],
    );

    assert!(
        resolves("filament::forms.text-input", &config, &autoload),
        "dotted namespaced component must map to a sub-namespaced class: {:#?}",
        component_candidate_paths("filament::forms.text-input", &config, &autoload),
    );
}

#[test]
fn missing_namespaced_component_still_reports_not_found() {
    // A registered namespace whose class file does NOT exist must NOT resolve —
    // diagnostics still fire (issue #69, no false negatives).
    let installed = r#"{
        "packages": [
            {
                "name": "filament/support",
                "autoload": { "psr-4": { "Filament\\Support\\": "src/" } },
                "install-path": "../filament/support"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/filament/support/src/View/Components/Badge.php",
            "<?php class Badge {}",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(
        &root,
        &[("filament", "Filament\\Support\\View\\Components")],
    );

    assert!(
        !resolves("filament::does-not-exist", &config, &autoload),
        "a namespaced component with no backing file must not resolve",
    );
    // An entirely unregistered namespace must not resolve either.
    assert!(
        !resolves("unknown::widget", &config, &autoload),
        "an unregistered namespace must not resolve",
    );
}

#[test]
fn package_view_namespace_resolves_directory_component_shapes() {
    // Laravel's ComponentTagCompiler accepts three file shapes for an
    // anonymous component; package view namespaces (loadViewsFrom /
    // fluent ->hasViews()) must honor the directory conventions too.
    // Filament 5 ships `<x-filament::button>` as `button/index.blade.php`
    // (issue #79, case 1).
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", r#"{"packages": []}"#),
        (
            "vendor/filament/support/resources/views/components/button/index.blade.php",
            "<button></button>",
        ),
        (
            "vendor/filament/support/resources/views/components/dropdown/dropdown.blade.php",
            "<div></div>",
        ),
        (
            "vendor/filament/support/resources/views/components/badge.blade.php",
            "<span></span>",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let mut config = config_with_component_namespaces(&root, &[]);
    config.view_namespaces.insert(
        "filament".to_string(),
        root.join("vendor/filament/support/resources/views"),
    );

    for component in ["filament::button", "filament::dropdown", "filament::badge"] {
        assert!(
            resolves(component, &config, &autoload),
            "{component} must resolve via a directory-component shape: {:#?}",
            component_candidate_paths(component, &config, &autoload),
        );
    }
    assert!(
        !resolves("filament::does-not-exist", &config, &autoload),
        "a missing namespaced component must still report not-found",
    );
}

#[test]
fn vendor_published_component_resolves_directory_index() {
    // Published package views (`resources/views/vendor/{ns}/components/`)
    // get the same directory-index treatment.
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", r#"{"packages": []}"#),
        (
            "resources/views/vendor/courier/components/alert/index.blade.php",
            "<div></div>",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(&root, &[]);

    assert!(
        resolves("courier::alert", &config, &autoload),
        "published vendor component must resolve via index.blade.php: {:#?}",
        component_candidate_paths("courier::alert", &config, &autoload),
    );
}

#[test]
fn markdown_mail_components_resolve_html_paths() {
    // `<x-mail::message>` is hardcoded in Laravel's ComponentTagCompiler to
    // the `mail::` view namespace, which Markdown points at `{path}/html` —
    // the published vendor dir first, then the framework's bundled views.
    // There is no `components/` segment (issue #79, case 2).
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", r#"{"packages": []}"#),
        (
            "vendor/laravel/framework/src/Illuminate/Mail/resources/views/html/message.blade.php",
            "<html></html>",
        ),
        (
            "resources/views/vendor/mail/html/header.blade.php",
            "<tr></tr>",
        ),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let config = config_with_component_namespaces(&root, &[]);

    assert!(
        resolves("mail::message", &config, &autoload),
        "mail::message must resolve to the framework's html view: {:#?}",
        component_candidate_paths("mail::message", &config, &autoload),
    );
    assert!(
        resolves("mail::header", &config, &autoload),
        "mail::header must resolve to the published html view: {:#?}",
        component_candidate_paths("mail::header", &config, &autoload),
    );
    assert!(
        !resolves("mail::does-not-exist", &config, &autoload),
        "a missing mail component must still report not-found",
    );
}

#[test]
fn anonymous_component_path_resolves_livewire_layout_namespace() {
    // The shape Livewire v4's config-driven registration produces:
    // anonymous_component_paths['layouts'] → resources/views/layouts, so
    // `<x-layouts::app>` resolves to resources/views/layouts/app.blade.php
    // (issue #79, case 3).
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", r#"{"packages": []}"#),
        ("resources/views/layouts/app.blade.php", "<html></html>"),
    ]);
    let autoload = ComposerAutoload::load(&root);
    let mut config = config_with_component_namespaces(&root, &[]);
    config
        .anonymous_component_paths
        .insert("layouts".to_string(), root.join("resources/views/layouts"));

    assert!(
        resolves("layouts::app", &config, &autoload),
        "layouts::app must resolve via the anonymous component path: {:#?}",
        component_candidate_paths("layouts::app", &config, &autoload),
    );
}

// ─── Member-access capture data model (M2) ──────────────────────────────

/// A captured property-form access with the capture-time defaults (the
/// resolution scaffold left unfilled, as M2 leaves it).
fn unresolved_member_access(member: &str, line: u32, col: u32) -> MemberAccessReferenceData {
    MemberAccessReferenceData {
        member: member.into(),
        receiver: "$user".into(),
        receiver_byte_start: 0,
        receiver_byte_end: 5,
        is_nullsafe: false,
        form: AccessForm::Property,
        line,
        column: col,
        end_column: col + member.len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    }
}

#[test]
fn member_access_is_indexed_and_found_at_position() {
    let mut p = ParsedPatternsData::default();
    // `$user->email` — member name spans cols 12..17 on row 1.
    p.member_access_refs
        .push(Arc::new(unresolved_member_access("email", 1, 12)));
    p.build_position_index();

    let found = p
        .find_at_position(1, 14)
        .expect("cursor inside the member name should hit the access");
    match found {
        PatternAtPosition::MemberAccess(m) => assert_eq!(m.member, "email"),
        other => panic!("expected MemberAccess, got {other:?}"),
    }

    // Cursor before the member name (on the receiver) must not match —
    // the access is indexed at the member span, not the whole expression.
    assert!(p.find_at_position(1, 2).is_none());
}

/// Win 2: `sorted_positions` is a lazily-initialized `OnceLock`, built on
/// `find_at_position`'s own first call rather than by an explicit
/// `build_position_index()` beforehand. This is the scenario a disk-cache
/// restore hits every time (the index is skipped on serialize): the very
/// first read of a restored `ParsedPatternsData` can be a lookup, with no
/// eager-build step in between.
#[test]
fn find_at_position_lazily_builds_the_index_without_an_explicit_call() {
    let mut p = ParsedPatternsData::default();
    p.member_access_refs
        .push(Arc::new(unresolved_member_access("email", 1, 12)));
    // Deliberately NOT calling `p.build_position_index()`.

    let found = p
        .find_at_position(1, 14)
        .expect("find_at_position must build its own index on first use");
    match found {
        PatternAtPosition::MemberAccess(m) => assert_eq!(m.member, "email"),
        other => panic!("expected MemberAccess, got {other:?}"),
    }

    // A second call reuses the now-cached index and must agree with the first.
    assert!(p.find_at_position(1, 2).is_none());
    assert!(p.find_at_position(1, 14).is_some());
}

#[test]
fn member_access_capture_defaults_are_unresolved() {
    let m = unresolved_member_access("email", 1, 12);
    assert!(m.declaring_fqcn.is_none());
    assert!(m.kind.is_none());
    assert_eq!(m.confidence, Confidence::Unresolved);
    assert_eq!(Confidence::default(), Confidence::Unresolved);
}

#[test]
fn member_access_deserializes_without_resolution_fields() {
    // A disk-cache entry written before the resolution scaffold existed:
    // only the capture fields are present. `#[serde(default)]` must fill the
    // rest rather than failing the whole entry.
    let json = r#"{
        "member": "email",
        "receiver": "$user",
        "receiver_byte_start": 0,
        "receiver_byte_end": 5,
        "is_nullsafe": false,
        "line": 1,
        "column": 12,
        "end_column": 17
    }"#;
    let m: MemberAccessReferenceData =
        serde_json::from_str(json).expect("legacy entry should deserialize");
    assert_eq!(m.member, "email");
    assert!(m.declaring_fqcn.is_none());
    assert!(m.kind.is_none());
    assert_eq!(m.confidence, Confidence::Unresolved);
}

#[test]
fn member_access_resolution_fields_round_trip() {
    // Once M3 fills the scaffold, it must survive (de)serialization.
    let mut m = unresolved_member_access("active", 3, 8);
    m.declaring_fqcn = Some("App\\Models\\User".into());
    m.kind = Some(MagicMemberKind::Scope);
    m.confidence = Confidence::High;

    let json = serde_json::to_string(&m).expect("serialize");
    let back: MemberAccessReferenceData = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back.declaring_fqcn.as_deref(), Some("App\\Models\\User"));
    assert_eq!(back.kind, Some(MagicMemberKind::Scope));
    assert_eq!(back.confidence, Confidence::High);
}

// ─── Project source-file discovery (magic-member index breadth) ─────────

#[test]
fn collect_source_files_covers_app_and_views_skips_vendor() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let write = |rel: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "<?php\n").unwrap();
        p
    };

    let model = write("app/Models/User.php");
    let provider = write("app/Providers/HorizonServiceProvider.php");
    let volt = write("resources/views/pages/users.php"); // Volt .php page
    let blade = write("resources/views/welcome.blade.php");
    let migration = write("database/migrations/2020_create_users.php");
    let vendor = write("vendor/laravel/framework/User.php"); // excluded
    let node = write("node_modules/pkg/x.php"); // excluded
    write("public/app.js"); // non-php, ignored

    let found = collect_source_files(root);

    // Included: app source (all of it), Volt .php under views, Blade, database.
    for p in [&model, &provider, &volt, &blade, &migration] {
        assert!(found.contains(p), "expected {p:?} in {found:?}");
    }
    // Excluded: vendor + node_modules.
    assert!(!found.contains(&vendor), "vendor must be skipped");
    assert!(!found.contains(&node), "node_modules must be skipped");
}

// ─── Blade @foreach iterable member-access capture ──────────────────────

#[test]
fn blade_loop_iterable_captures_this_member_access() {
    let content =
        "<div>\n    @foreach ($this->entities as $entity)\n        {{ $entity->name }}\n    @endforeach\n</div>\n";
    let accesses = blade_loop_iterable_accesses(content);
    assert_eq!(accesses.len(), 1, "got {accesses:?}");
    let a = &accesses[0];
    assert_eq!(a.member, "entities");
    assert_eq!(a.receiver, "$this");
    assert_eq!(a.line, 1); // 0-based; @foreach is the 2nd line
    let line = content.lines().nth(1).unwrap();
    assert_eq!(a.column, line.find("entities").unwrap() as u32);
    assert_eq!(a.end_column, a.column + "entities".len() as u32);
}

#[test]
fn blade_loop_iterable_bare_var_has_no_member_access() {
    // `@foreach($users as $user)` — a bare collection var, no `->member`.
    let content = "@foreach ($users as $user)\n{{ $user->x }}\n@endforeach\n";
    assert!(blade_loop_iterable_accesses(content).is_empty());
}

// ─── Generic builder-form view-namespace discovery (issue #69) ──────────
//
// Packages registered through a fluent package-builder (e.g. Filament via
// laravel-package-tools) declare views with `->name('x')->hasViews()`, and the
// real `loadViewsFrom` runs in a base class with runtime args — invisible to
// the literal `loadViewsFrom(__DIR__.'lit','lit')` extractor. These tests pin
// the builder-form recognizer that reconstructs the (namespace, directory)
// registration so `<x-x::component>` resolves through the existing
// view-namespace path.

#[test]
fn builder_short_name_strips_leading_laravel_prefix() {
    assert_eq!(builder_short_name("filament"), "filament");
    assert_eq!(builder_short_name("laravel-foo"), "foo");
    assert_eq!(builder_short_name("my-laravel-bar"), "bar");
}

/// Collect the (namespace, view_path) pairs a provider source registers.
fn discovered_view_namespaces(source: &str, provider_path: &str) -> Vec<(String, Option<PathBuf>)> {
    let db = LaravelDatabase::default();
    let file =
        ServiceProviderFile::new(&db, PathBuf::from(provider_path), 0, source.to_string(), 1);
    let parsed = parse_service_provider_source(&db, file, PathBuf::from("/proj"));
    parsed
        .view_namespaces(&db)
        .iter()
        .map(|vn| {
            (
                vn.namespace(&db).namespace(&db).clone(),
                vn.view_path(&db).clone(),
            )
        })
        .collect()
}

#[test]
fn builder_hasviews_registers_namespace_at_package_resources_views() {
    // Provider in `<pkg>/src` → views at `<pkg>/resources/views` (normalized,
    // since the path doesn't exist on disk in this unit test).
    let source = r#"<?php
class WidgetsServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews();
    }
}"#;
    let found = discovered_view_namespaces(
        source,
        "/proj/vendor/acme/widgets/src/WidgetsServiceProvider.php",
    );
    assert!(
        found.iter().any(|(ns, p)| ns == "acme"
            && *p == Some(PathBuf::from("/proj/vendor/acme/widgets/resources/views"))),
        "->name('acme')->hasViews() must register 'acme' → package resources/views, got {found:?}"
    );
}

#[test]
fn builder_hasviews_explicit_namespace_overrides_package_name() {
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews('custom');
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/pkg/src/P.php");
    assert!(
        found.iter().any(|(ns, _)| ns == "custom"),
        "explicit ->hasViews('custom') must win over the package name, got {found:?}"
    );
    assert!(
        !found.iter().any(|(ns, _)| ns == "acme"),
        "the package name must not also register when an explicit namespace is given, got {found:?}"
    );
}

#[test]
fn builder_hasviews_strips_laravel_prefix_for_namespace() {
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('laravel-widgets')->hasViews();
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/widgets/src/P.php");
    assert!(
        found.iter().any(|(ns, _)| ns == "widgets"),
        "->name('laravel-widgets') must register namespace 'widgets', got {found:?}"
    );
}

#[test]
fn builder_name_without_hasviews_registers_no_view_namespace() {
    // A package that declares a name + commands but no views must not have a
    // view namespace synthesized from its `->name()` call.
    let source = r#"<?php
class P extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasCommands([SomeCommand::class]);
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/vendor/acme/pkg/src/P.php");
    assert!(
        found.is_empty(),
        "no ->hasViews() means no builder-form view namespace, got {found:?}"
    );
}

#[test]
fn builder_discovered_namespace_resolves_anonymous_view_component() {
    // End-to-end: a real builder provider + a real package view file. Discovery
    // must register the namespace so `resolve_component_path` finds the view —
    // the exact `<x-filament::input.wrapper>` failure from issue #69.
    let provider = r#"<?php
class SupportServiceProvider extends PackageServiceProvider
{
    public function configurePackage(Package $package): void
    {
        $package->name('acme')->hasViews();
    }
}"#;
    let (_dir, root) = project_with_files(&[
        (
            "vendor/acme/support/src/SupportServiceProvider.php",
            provider,
        ),
        (
            "vendor/acme/support/resources/views/components/input/wrapper.blade.php",
            "<div>{{ $slot }}</div>",
        ),
    ]);

    // Discover the namespace from the provider on disk.
    let db = LaravelDatabase::default();
    let provider_path = root.join("vendor/acme/support/src/SupportServiceProvider.php");
    let text = std::fs::read_to_string(&provider_path).unwrap();
    let file = ServiceProviderFile::new(&db, provider_path, 0, text, 1);
    let parsed = parse_service_provider_source(&db, file, root.clone());

    let mut view_namespaces = HashMap::new();
    for vn in parsed.view_namespaces(&db) {
        if let Some(p) = vn.view_path(&db).clone() {
            view_namespaces.insert(vn.namespace(&db).namespace(&db).clone(), p);
        }
    }
    assert!(
        view_namespaces.contains_key("acme"),
        "discovery must register the 'acme' view namespace, got {view_namespaces:?}"
    );

    // Build a config carrying that namespace and resolve the dotted component.
    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    let candidates = config.resolve_component_path("acme::input.wrapper");
    assert!(
        candidates.iter().any(|p| p.exists()),
        "acme::input.wrapper must resolve to the real package view via the \
         discovered namespace: {candidates:#?}"
    );
}

// ─── Imperative View::addNamespace() view-namespace discovery (issue #72) ─
//
// A namespace registered at runtime via `View::addNamespace('ns', <path>)`
// (or the `app('view')->`/`$factory->` receivers, or `prependNamespace`) is
// invisible to the literal `loadViewsFrom(__DIR__.'…', 'ns')` extractor, so
// `view('ns::name')` falsely reported "View file not found" and go-to-definition
// failed. These tests pin the imperative extractor that resolves the directory
// argument through the shared path-expression resolver (`app_path()`,
// `base_path()`, `resource_path()`, `__DIR__.'…'`, literals).

#[test]
fn extract_add_namespace_reads_app_path_registration() {
    let src = r#"
        public function boot(): void
        {
            View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
        }
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);

    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].0, "ai-prompts");
    assert_eq!(regs[0].1, PathBuf::from("/project/app/Ai/Prompts"));
}

#[test]
fn extract_add_namespace_handles_all_path_helper_forms() {
    let src = r#"
        View::addNamespace('with-app', app_path('Views/A'));
        View::addNamespace('with-base', base_path('packages/b/views'));
        View::addNamespace('with-resource', resource_path('views/c'));
        View::addNamespace('with-dir', __DIR__ . '/../views');
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);
    let by_ns: std::collections::HashMap<_, _> = regs
        .iter()
        .map(|(ns, dir, _)| (ns.as_str(), dir.clone()))
        .collect();

    assert_eq!(by_ns["with-app"], PathBuf::from("/project/app/Views/A"));
    assert_eq!(
        by_ns["with-base"],
        PathBuf::from("/project/packages/b/views")
    );
    assert_eq!(
        by_ns["with-resource"],
        PathBuf::from("/project/resources/views/c")
    );
    // __DIR__ resolves against the provider directory.
    assert_eq!(by_ns["with-dir"], PathBuf::from("/project/app/views"));
}

#[test]
fn extract_add_namespace_supports_factory_receivers_and_prepend() {
    let src = r#"
        app('view')->addNamespace('via-app', resource_path('views/a'));
        $factory->prependNamespace('via-factory', base_path('b'));
    "#;
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);
    let names: Vec<&str> = regs.iter().map(|(ns, _, _)| ns.as_str()).collect();

    assert!(
        names.contains(&"via-app"),
        "app('view')-> form must register, got {names:?}"
    );
    assert!(
        names.contains(&"via-factory"),
        "$factory->prependNamespace form must register, got {names:?}"
    );
}

#[test]
fn extract_add_namespace_skips_unresolvable_path_argument() {
    // A variable directory can't be resolved statically — must be skipped, not
    // registered with a bogus path.
    let src = "View::addNamespace('dynamic', $this->promptPath);";
    let root = PathBuf::from("/project");
    let provider_dir = PathBuf::from("/project/app/Providers");

    let regs = extract_add_namespace_view_registrations(src, &root, &provider_dir);

    assert!(
        regs.is_empty(),
        "unresolvable path must be skipped, got {regs:?}"
    );
}

#[test]
fn add_namespace_registers_view_namespace_through_provider_parse() {
    let source = r#"<?php
class AppServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
    }
}"#;
    let found = discovered_view_namespaces(source, "/proj/app/Providers/AppServiceProvider.php");
    assert!(
        found
            .iter()
            .any(|(ns, p)| ns == "ai-prompts" && *p == Some(PathBuf::from("/proj/app/Ai/Prompts"))),
        "View::addNamespace must register 'ai-prompts' → app/Ai/Prompts, got {found:?}"
    );
}

#[test]
fn add_namespace_resolves_view_path_for_two_namespaces_end_to_end() {
    // Two distinct namespaces registered via View::addNamespace, each backed by
    // a real blade file. The exact issue #72 failure: view('ns::name') must
    // resolve to the registered directory instead of falling back to the
    // resources/views/vendor convention.
    let provider = r#"<?php
class AppServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        View::addNamespace('ai-prompts', app_path('Ai/Prompts'));
        View::addNamespace('reports', resource_path('report-views'));
    }
}"#;
    let (_dir, root) = project_with_files(&[
        ("app/Providers/AppServiceProvider.php", provider),
        (
            "app/Ai/Prompts/candidate-proposer.blade.php",
            "{{ $topic }}",
        ),
        ("resources/report-views/monthly.blade.php", "report"),
    ]);

    let db = LaravelDatabase::default();
    let provider_path = root.join("app/Providers/AppServiceProvider.php");
    let text = std::fs::read_to_string(&provider_path).unwrap();
    let file = ServiceProviderFile::new(&db, provider_path, 0, text, 1);
    let parsed = parse_service_provider_source(&db, file, root.clone());

    let mut view_namespaces = HashMap::new();
    for vn in parsed.view_namespaces(&db) {
        if let Some(p) = vn.view_path(&db).clone() {
            view_namespaces.insert(vn.namespace(&db).namespace(&db).clone(), p);
        }
    }
    assert!(
        view_namespaces.contains_key("ai-prompts") && view_namespaces.contains_key("reports"),
        "both namespaces must be discovered, got {view_namespaces:?}"
    );

    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces,
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    };

    // Both namespaced views resolve to their registered, real files.
    let prompts = config.resolve_view_path("ai-prompts::candidate-proposer");
    assert!(
        prompts.iter().any(|p| p.exists()),
        "ai-prompts::candidate-proposer must resolve to the registered file: {prompts:#?}"
    );
    let reports = config.resolve_view_path("reports::monthly");
    assert!(
        reports.iter().any(|p| p.exists()),
        "reports::monthly must resolve to the registered file: {reports:#?}"
    );

    // An invalid view under a registered namespace still has no real candidate —
    // the diagnostic must keep firing (no false negative).
    let missing = config.resolve_view_path("ai-prompts::does-not-exist");
    assert!(
        !missing.is_empty() && !missing.iter().any(|p| p.exists()),
        "an invalid namespaced view must still produce only non-existent candidates: {missing:#?}"
    );
}

// ─── Class-backed component registrations (dynamic-component, issue #69) ─
//
// Laravel core registers `<x-dynamic-component>` with an ordinary class
// alias — `$blade->component('dynamic-component', DynamicComponent::class)`
// inside ViewServiceProvider — using the *instance* receiver and a *short*
// class name resolved by a `use` import. These tests pin the broadened
// registration parsing (both receivers, both argument orders, use-statement
// expansion) and the shared-resolver consumption of the resulting map.

#[test]
fn expand_class_via_use_statements_resolves_short_names() {
    let source = r#"<?php
namespace Illuminate\View;

use Illuminate\View\DynamicComponent;
use Foo\Bar as Baz;
use function array_map;

class P {}
"#;
    assert_eq!(
        expand_class_via_use_statements("DynamicComponent", source),
        "Illuminate\\View\\DynamicComponent"
    );
    // Aliased import resolves through the alias.
    assert_eq!(expand_class_via_use_statements("Baz", source), "Foo\\Bar");
    // Already-qualified names pass through untouched.
    assert_eq!(
        expand_class_via_use_statements("App\\View\\Alert", source),
        "App\\View\\Alert"
    );
    // No matching import → unchanged (resolution fails downstream, as before).
    assert_eq!(
        expand_class_via_use_statements("Unknown", source),
        "Unknown"
    );
}

/// Parse a provider source against a root and return (tag, class, file) per
/// class-backed component registration.
fn parsed_blade_components(
    source: &str,
    provider_path: PathBuf,
    root: PathBuf,
) -> Vec<(String, String, Option<PathBuf>)> {
    let db = LaravelDatabase::default();
    let file = ServiceProviderFile::new(&db, provider_path, 0, source.to_string(), 0);
    let parsed = parse_service_provider_source(&db, file, root);
    parsed
        .blade_components(&db)
        .iter()
        .map(|bc| {
            (
                bc.tag_name(&db).name(&db).clone(),
                bc.class_name(&db).clone(),
                bc.file_path(&db).clone(),
            )
        })
        .collect()
}

#[test]
fn instance_form_component_registration_is_discovered_and_resolved() {
    // The exact framework shape: instance receiver inside a tap() closure,
    // short class name brought in by a use import.
    let provider = r#"<?php
namespace Illuminate\View;

use Illuminate\View\DynamicComponent;

class ViewServiceProvider
{
    public function registerBladeCompiler()
    {
        $this->app->singleton('blade.compiler', function ($app) {
            return tap(new BladeCompiler(), function ($blade) {
                $blade->component('dynamic-component', DynamicComponent::class);
            });
        });
    }
}
"#;
    let (_dir, root) = project_with_files(&[(
        "vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php",
        "<?php namespace Illuminate\\View; class DynamicComponent {}",
    )]);

    let found = parsed_blade_components(
        provider,
        root.join("vendor/laravel/framework/src/Illuminate/View/ViewServiceProvider.php"),
        root.clone(),
    );

    assert!(
        found.iter().any(|(tag, class, file)| {
            tag == "dynamic-component"
                && class == "Illuminate\\View\\DynamicComponent"
                && file
                    .as_ref()
                    .is_some_and(|f| f.ends_with("Illuminate/View/DynamicComponent.php"))
        }),
        "instance-form registration must be discovered with the use-expanded \
         FQN and a resolved file, got {found:?}"
    );
}

#[test]
fn class_first_argument_order_is_discovered() {
    // Canonical order: Blade::component(AlertComponent::class, 'alert').
    let provider = r#"<?php
use App\View\Components\AlertComponent;

class AppServiceProvider
{
    public function boot()
    {
        Blade::component(AlertComponent::class, 'alert');
    }
}
"#;
    let (_dir, root) = project_with_files(&[(
        "app/View/Components/AlertComponent.php",
        "<?php namespace App\\View\\Components; class AlertComponent {}",
    )]);

    let found = parsed_blade_components(
        provider,
        root.join("app/Providers/AppServiceProvider.php"),
        root.clone(),
    );

    assert!(
        found.iter().any(|(tag, class, file)| {
            tag == "alert" && class == "App\\View\\Components\\AlertComponent" && file.is_some()
        }),
        "class-first argument order must be discovered, got {found:?}"
    );
}

#[test]
fn vendor_literal_registration_resolves_class_via_psr4() {
    // MaryUI's literal form: `Blade::component('mary-card', Card::class)` in
    // a vendor provider. The class file must resolve through the composer
    // PSR-4 map — `Mary\` is in no hardcoded namespace mapping (issue #79,
    // the <x-mary-card> case).
    let provider = r#"<?php
namespace Mary;

use Mary\View\Components\Card;

class MaryServiceProvider
{
    public function registerComponents()
    {
        Blade::component('mary-card', Card::class);
    }
}
"#;
    let installed = r#"{
        "packages": [
            {
                "name": "robsontenorio/mary",
                "autoload": { "psr-4": { "Mary\\": "src/" } },
                "install-path": "../robsontenorio/mary"
            }
        ]
    }"#;
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        (
            "vendor/robsontenorio/mary/src/View/Components/Card.php",
            "<?php namespace Mary\\View\\Components; class Card {}",
        ),
    ]);

    let found = parsed_blade_components(
        provider,
        root.join("vendor/robsontenorio/mary/src/MaryServiceProvider.php"),
        root.clone(),
    );

    assert!(
        found.iter().any(|(tag, class, file)| {
            tag == "mary-card"
                && class == "Mary\\View\\Components\\Card"
                && file
                    .as_ref()
                    .is_some_and(|f| f.ends_with("src/View/Components/Card.php"))
        }),
        "vendor literal registration must resolve its class via PSR-4, got {found:?}"
    );
}

#[test]
fn prefix_computed_registration_resolves_config_prefix() {
    // MaryUI's catalog form: the tag is computed from a config value.
    //   $prefix = config('mary.prefix');
    //   Blade::component($prefix . 'card', Card::class);
    // With the package's bundled default ('') the tag is `card`; an app
    // config override ('mary-') changes it to `mary-card`.
    let provider = r#"<?php
namespace Mary;

use Mary\View\Components\Card;

class MaryServiceProvider
{
    public function registerComponents()
    {
        $prefix = config('mary.prefix');

        Blade::component($prefix . 'card', Card::class);
    }
}
"#;
    let installed = r#"{
        "packages": [
            {
                "name": "robsontenorio/mary",
                "autoload": { "psr-4": { "Mary\\": "src/" } },
                "install-path": "../robsontenorio/mary"
            }
        ]
    }"#;
    let package_config = r#"<?php
return [
    'prefix' => '',
];
"#;

    // Package default prefix: ''.
    let (_dir, root) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        ("vendor/robsontenorio/mary/config/mary.php", package_config),
        (
            "vendor/robsontenorio/mary/src/View/Components/Card.php",
            "<?php namespace Mary\\View\\Components; class Card {}",
        ),
    ]);
    let found = parsed_blade_components(
        provider,
        root.join("vendor/robsontenorio/mary/src/MaryServiceProvider.php"),
        root.clone(),
    );
    assert!(
        found
            .iter()
            .any(|(tag, _, file)| tag == "card" && file.is_some()),
        "default '' prefix must register the bare tag, got {found:?}"
    );

    // App config override: 'mary-'.
    let (_dir2, root2) = project_with_files(&[
        ("vendor/composer/installed.json", installed),
        ("vendor/robsontenorio/mary/config/mary.php", package_config),
        ("config/mary.php", "<?php return ['prefix' => 'mary-'];"),
        (
            "vendor/robsontenorio/mary/src/View/Components/Card.php",
            "<?php namespace Mary\\View\\Components; class Card {}",
        ),
    ]);
    let found = parsed_blade_components(
        provider,
        root2.join("vendor/robsontenorio/mary/src/MaryServiceProvider.php"),
        root2.clone(),
    );
    assert!(
        found
            .iter()
            .any(|(tag, _, file)| tag == "mary-card" && file.is_some()),
        "app config prefix override must win, got {found:?}"
    );
}

#[tokio::test]
async fn livewire_config_namespaces_merge_into_laravel_config() {
    // The SalsaActor assembly wiring (the `or_insert` merge): a vendor
    // Livewire v4 config's component_namespaces must surface in
    // LaravelConfigData.anonymous_component_paths, and an explicit
    // Blade::anonymousComponentPath registration for the same namespace
    // must win over the Livewire-derived entry.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let lw_config = root.join("vendor/livewire/livewire/config/livewire.php");
    std::fs::create_dir_all(lw_config.parent().unwrap()).unwrap();
    std::fs::write(
        &lw_config,
        "<?php return ['component_namespaces' => ['layouts' => resource_path('views/layouts')]];",
    )
    .unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    let config = handle
        .get_laravel_config()
        .await
        .unwrap()
        .expect("config should build");
    assert_eq!(
        config.anonymous_component_paths.get("layouts"),
        Some(&root.join("resources/views/layouts")),
        "Livewire config namespaces must merge into anonymous_component_paths"
    );

    // Explicit registration wins.
    let provider = r#"<?php
class AppServiceProvider
{
    public function boot()
    {
        Blade::anonymousComponentPath(resource_path('views/custom-layouts'), 'layouts');
    }
}
"#;
    handle
        .register_service_provider_source(
            root.join("app/Providers/AppServiceProvider.php"),
            provider.to_string(),
            2,
            root.clone(),
        )
        .await
        .unwrap();
    let config = handle
        .get_laravel_config()
        .await
        .unwrap()
        .expect("config should rebuild");
    assert_eq!(
        config.anonymous_component_paths.get("layouts"),
        Some(&root.join("resources/views/custom-layouts")),
        "an explicit anonymousComponentPath registration must win over the Livewire default"
    );
}

#[test]
fn class_component_registration_resolves_via_candidate_paths() {
    // End of the chain: a tag present in `class_component_files` must surface
    // from `component_candidate_paths`, so the shared diagnostic/goto resolver
    // stops flagging `<x-dynamic-component>`.
    let (_dir, root) = project_with_files(&[(
        "vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php",
        "<?php namespace Illuminate\\View; class DynamicComponent {}",
    )]);
    let class_file = root.join("vendor/laravel/framework/src/Illuminate/View/DynamicComponent.php");

    let mut class_component_files = HashMap::new();
    class_component_files.insert("dynamic-component".to_string(), class_file.clone());

    let config = LaravelConfigData {
        root: root.clone(),
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files,
    };
    let autoload = ComposerAutoload::load(&root);

    let candidates = component_candidate_paths("dynamic-component", &config, &autoload);
    assert!(
        candidates.iter().any(|p| *p == class_file && p.exists()),
        "a class-registered tag must resolve to its class file via the shared \
         resolver: {candidates:#?}"
    );

    // An unregistered tag must not pick up the class file.
    let other = component_candidate_paths("some-other-component", &config, &autoload);
    assert!(
        !other.contains(&class_file),
        "class registrations must not bleed into unrelated lookups"
    );
}

// ─── Call-form magic members at the cursor (#77) ───────────────────────────
//
// End-to-end through the actor: register a model + a caller, then resolve the
// cursor on a CALL-form usage (`User::active()`, `$user->active()`). These
// exercise the `member_ref.form` plumbing — before #77 the resolvers
// hardcoded `AccessForm::Property`, so a scope call could never classify.

const SCOPE_MODEL_SRC: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#;

const SCOPE_CALLER_SRC: &str = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class UserController {
    public function index() {
        return User::active()->get();
    }
}
"#;

/// `(line, column)` of the first `needle` occurrence, 0-based.
fn position_of(src: &str, needle: &str) -> (u32, u32) {
    for (row, line) in src.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return (row as u32, col as u32);
        }
    }
    panic!("{needle} not found in fixture");
}

/// Spawn an actor over a tempdir project holding the scope model + caller.
async fn scope_project() -> (TempDir, SalsaHandle, PathBuf) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, SCOPE_MODEL_SRC).unwrap();
    let caller_path = root.join("app/Http/Controllers/UserController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, SCOPE_CALLER_SRC).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, SCOPE_MODEL_SRC.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, SCOPE_CALLER_SRC.to_string())
        .await
        .unwrap();
    // Patterns parse lazily; forcing the model's parse is what feeds the
    // class-hierarchy index (the on-demand population path).
    handle.get_patterns(model_path).await.unwrap();
    (dir, handle, caller_path)
}

#[tokio::test]
async fn resolve_magic_member_at_classifies_static_scope_call() {
    let (_dir, handle, caller_path) = scope_project().await;
    let (line, col) = position_of(SCOPE_CALLER_SRC, "active");

    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("scope call should resolve");
    assert_eq!(data.kind, MagicMemberKind::Scope);
    assert_eq!(data.declaring_fqcn, "App\\Models\\User");
    assert_eq!(data.member, "active");
    // Method-backed: both decl lines present so hover can slice the source.
    assert!(data.decl_file.is_some());
    assert!(data.decl_line.is_some());
    assert!(data.decl_end_line.is_some());
}

#[tokio::test]
async fn resolve_magic_member_rename_at_maps_scope_call_to_declaration() {
    let (_dir, handle, caller_path) = scope_project().await;
    let (line, col) = position_of(SCOPE_CALLER_SRC, "active");

    let data = handle
        .resolve_magic_member_rename_at(caller_path, line, col)
        .await
        .unwrap()
        .expect("scope call should be renameable");
    assert_eq!(data.method_name, "scopeActive");
    assert_eq!(data.member, "active");
    assert_eq!(data.kind, MagicMemberKind::Scope);
    assert!(data.decl_file.ends_with("app/Models/User.php"));
}

#[tokio::test]
async fn unclassified_call_does_not_become_tentative_column() {
    // `$user->somethingUnknown()` — receiver resolves to the model, but the
    // member classifies as nothing. The tentative-column fallback is a
    // property-read concept and must NOT fire for calls.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, SCOPE_MODEL_SRC).unwrap();
    let caller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function x(User $user) { return $user->somethingUnknown(); }
}
"#;
    let caller_path = root.join("app/Http/Controllers/C.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, SCOPE_MODEL_SRC.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    // Force the model's parse so the receiver RESOLVES — otherwise this test
    // passes vacuously without exercising the call-form tentative gate.
    handle.get_patterns(model_path).await.unwrap();

    let (line, col) = position_of(caller_src, "somethingUnknown");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap();
    assert!(
        data.is_none(),
        "an unclassified CALL must not resolve as a tentative column; got {data:?}"
    );
}

#[tokio::test]
async fn dynamic_finder_is_not_renameable() {
    // `whereEmail` has no declared method to rewrite — finder rename must be
    // refused BY KIND, not by accident of the candidate-method lookup
    // missing (PR #76 review finding).
    let model_src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $casts = ['email' => 'string'];
}
"#;
    let caller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function find() { return User::whereEmail('a@b.test')->first(); }
}
"#;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, model_src).unwrap();
    let caller_path = root.join("app/Http/Controllers/C.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, model_src.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    handle.get_patterns(model_path).await.unwrap();

    let (line, col) = position_of(caller_src, "whereEmail");
    // Precondition: the finder itself resolves (hover/goto see it)...
    let hover = handle
        .resolve_magic_member_at(caller_path.clone(), line, col, None)
        .await
        .unwrap()
        .expect("finder should classify for hover/goto");
    assert_eq!(hover.kind, MagicMemberKind::DynamicFinder);
    // ...but rename refuses it by kind.
    let rename = handle
        .resolve_magic_member_rename_at(caller_path, line, col)
        .await
        .unwrap();
    assert!(
        rename.is_none(),
        "a dynamic finder must not be renameable; got {rename:?}"
    );
}

#[tokio::test]
async fn resolve_magic_member_at_classifies_mid_chain_scope_call() {
    // Cursor on `active` in `User::query()->active()` — the chain-aware
    // receiver resolution (#77 review round 2) must classify it, lighting up
    // hover, goto, and rename for mid-chain scope usages.
    let model_src = SCOPE_MODEL_SRC;
    let caller_src = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        return User::query()->active()->get();
    }
}
"#;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    std::fs::write(&model_path, model_src).unwrap();
    let caller_path = root.join("app/Http/Controllers/C.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_file(model_path.clone(), 1, model_src.to_string())
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    handle.get_patterns(model_path).await.unwrap();

    let (line, col) = position_of(caller_src, "active");
    let data = handle
        .resolve_magic_member_at(caller_path.clone(), line, col, None)
        .await
        .unwrap()
        .expect("mid-chain scope call should resolve");
    assert_eq!(data.kind, MagicMemberKind::Scope);
    assert_eq!(data.declaring_fqcn, "App\\Models\\User");

    // And rename maps it to the declaration — the completeness that makes
    // scope rename safe.
    let rename = handle
        .resolve_magic_member_rename_at(caller_path, line, col)
        .await
        .unwrap()
        .expect("mid-chain scope call should be renameable");
    assert_eq!(rename.method_name, "scopeActive");
}

// ─── Container bindings: tree-sitter extraction + closure concrete resolution ─

/// Minimal on-disk Tenant model so closure concretes resolve to a real file.
const TENANT_FILE: (&str, &str) = (
    "app/Models/Tenant.php",
    "<?php namespace App\\Models; class Tenant {}",
);

/// A provider whose `register()` body is `binding_line`, with the Tenant model
/// imported (so a bare `Tenant` resolves via the use-alias).
fn tenant_provider(binding_line: &str) -> String {
    format!(
        r#"<?php
namespace App\Providers;

use Illuminate\Support\ServiceProvider;
use App\Models\Tenant;

class AppServiceProvider extends ServiceProvider
{{
    public function register(): void
    {{
        {binding_line}
    }}
}}
"#
    )
}

/// The (abstract, concrete, file_path) triples a provider source registers.
fn discovered_bindings(source: &str, root: PathBuf) -> Vec<(String, String, Option<PathBuf>)> {
    let db = LaravelDatabase::default();
    let file = ServiceProviderFile::new(
        &db,
        root.join("app/Providers/AppServiceProvider.php"),
        0,
        source.to_string(),
        2,
    );
    let parsed = parse_service_provider_source(&db, file, root);
    parsed
        .bindings(&db)
        .iter()
        .map(|b| {
            (
                b.abstract_name(&db).name(&db).clone(),
                b.concrete_class(&db).clone(),
                b.file_path(&db).clone(),
            )
        })
        .collect()
}

fn concrete_for<'a>(
    bindings: &'a [(String, String, Option<PathBuf>)],
    key: &str,
) -> Option<&'a (String, String, Option<PathBuf>)> {
    bindings.iter().find(|(a, _, _)| a == key)
}

#[test]
fn closure_arrow_static_chain_resolves_to_bound_model() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider(
        "$this->app->singleton('currentTenant', fn () => Tenant::where('domain', request()->host())->first());",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, file) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "App\\Models\\Tenant");
    assert!(file
        .as_ref()
        .is_some_and(|f| f.ends_with("app/Models/Tenant.php")));
}

#[test]
fn closure_new_model_resolves_to_bound_model() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src =
        tenant_provider("$this->app->bind('currentTenant', function () { return new Tenant(); });");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "App\\Models\\Tenant");
}

// ─── FIX #1: bare same-namespace `new X` in a binding closure ──────────────
//
// The REAL Laravel `AuthServiceProvider` shape: a provider that lives in the
// same namespace as the concrete it binds, registering
// `singleton('auth', fn ($app) => new AuthManager($app))` with NO `use` import
// — PHP resolves the bare `AuthManager` against the current namespace. Before
// the fix `resolve_expression_type` returned the bare `AuthManager`, which
// failed the on-disk gate (it looked up `AuthManager`, not
// `Illuminate\Auth\AuthManager`) and degraded the binding to "Closure" — so
// `binding_concrete("auth")` resolved nothing and EVERY `Auth::*` facade goto
// died at the source. The fix qualifies the bare name against the closure's
// file namespace before the gate.

/// The vendor `AuthManager`, at the PSR-4-mapped `Illuminate\` path so
/// `resolve_class_to_file_internal` finds it on disk.
const AUTH_MANAGER_FILE: (&str, &str) = (
    "vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php",
    "<?php namespace Illuminate\\Auth; class AuthManager { public function check() {} }",
);

/// A provider in `Illuminate\Auth` (the concrete's OWN namespace) whose
/// `register()` body is `binding_line` — with NO import, so a bare `AuthManager`
/// only resolves if qualified against this file namespace.
fn same_namespace_auth_provider(binding_line: &str) -> String {
    format!(
        r#"<?php
namespace Illuminate\Auth;

use Illuminate\Support\ServiceProvider;

class AuthServiceProvider extends ServiceProvider
{{
    public function register(): void
    {{
        {binding_line}
    }}
}}
"#
    )
}

#[test]
fn closure_bare_same_namespace_new_resolves_to_concrete() {
    // FIX #1: `fn ($app) => new AuthManager($app)` in a provider that lives in
    // `Illuminate\Auth` — the bare `AuthManager` (no import) must qualify to
    // `Illuminate\Auth\AuthManager` and resolve to its file, NOT degrade to
    // "Closure".
    let (_dir, root) = project_with_files(&[AUTH_MANAGER_FILE]);
    let src = same_namespace_auth_provider(
        "$this->app->singleton('auth', fn ($app) => new AuthManager($app));",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, file) = concrete_for(&found, "auth").expect("binding registered");
    assert_eq!(concrete, "Illuminate\\Auth\\AuthManager");
    assert!(file
        .as_ref()
        .is_some_and(|f| f.ends_with("Illuminate/Auth/AuthManager.php")));
}

#[test]
fn closure_fully_qualified_new_resolves_unchanged_control() {
    // Control: the fully-qualified `new \Illuminate\Auth\AuthManager($app)` was
    // already resolvable (absolute names don't need qualification) — the fix's
    // qualify step must leave it byte-for-byte identical, never double-qualify.
    let (_dir, root) = project_with_files(&[AUTH_MANAGER_FILE]);
    let src = same_namespace_auth_provider(
        "$this->app->singleton('auth', fn ($app) => new \\Illuminate\\Auth\\AuthManager($app));",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "auth").expect("binding registered");
    assert_eq!(concrete, "Illuminate\\Auth\\AuthManager");
}

#[test]
fn closure_return_type_hint_resolves_to_bound_model() {
    // The body call is opaque; the explicit `: Tenant` return type is the signal.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider(
        "$this->app->singleton('currentTenant', fn (): Tenant => $this->makeTenant());",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "App\\Models\\Tenant");
}

#[test]
fn scoped_closure_binding_is_extracted_and_resolved() {
    // `scoped` is new surface (the old regex only matched bind/singleton).
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src =
        tenant_provider("$this->app->scoped('currentTenant', fn () => Tenant::query()->first());");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) =
        concrete_for(&found, "currentTenant").expect("scoped binding registered");
    assert_eq!(concrete, "App\\Models\\Tenant");
}

#[test]
fn unresolvable_closure_falls_back_to_closure_marker() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src =
        tenant_provider("$this->app->singleton('currentTenant', fn () => collect([])->first());");
    let found = discovered_bindings(&src, root);
    let (_, concrete, file) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "Closure");
    assert!(file.is_none());
}

#[test]
fn union_return_type_degrades_to_closure() {
    // A union return type can't be classified against a single member surface.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider(
        "$this->app->singleton('currentTenant', fn (): Tenant|TenantStub => $this->make());",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "Closure");
}

#[test]
fn nullable_return_type_does_not_guess_when_body_opaque() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src =
        tenant_provider("$this->app->singleton('currentTenant', fn (): ?Tenant => $this->make());");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "Closure");
}

#[test]
fn multiple_return_closure_degrades_to_closure() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider(
        "$this->app->bind('currentTenant', function () { if (request()->secure()) { return new Tenant(); } return new Tenant(); });",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "Closure");
}

#[test]
fn class_const_binding_records_raw_name_unchanged() {
    // Regression: the class-concrete form the regex used to handle records the
    // name exactly as written (no use-alias expansion) — the tree-sitter walker
    // preserves that behavior byte-for-byte.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider("$this->app->singleton('currentTenant', Tenant::class);");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "Tenant");
}

#[test]
fn class_const_binding_with_fqn_resolves_file() {
    // A fully-qualified `::class` concrete resolves to its file, exactly as the
    // former regex did (the regex captured `[A-Za-z0-9_\\]+` including the `\`s).
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src =
        tenant_provider("$this->app->singleton('currentTenant', \\App\\Models\\Tenant::class);");
    let found = discovered_bindings(&src, root);
    let (_, concrete, file) = concrete_for(&found, "currentTenant").expect("binding registered");
    assert_eq!(concrete, "App\\Models\\Tenant");
    assert!(file
        .as_ref()
        .is_some_and(|f| f.ends_with("app/Models/Tenant.php")));
}

#[test]
fn bare_binding_uses_abstract_as_concrete() {
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider("$this->app->bind('my.service');");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) = concrete_for(&found, "my.service").expect("bare binding registered");
    assert_eq!(concrete, "my.service");
}

#[test]
fn variable_concrete_binding_is_skipped() {
    // A variable concrete can't be statically resolved — preserved regex
    // behavior: it isn't registered at all.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider("$impl = new Tenant(); $this->app->bind('currentTenant', $impl);");
    let found = discovered_bindings(&src, root);
    assert!(
        concrete_for(&found, "currentTenant").is_none(),
        "variable concrete must not register a binding, got {found:?}"
    );
}

#[test]
fn non_class_constant_concrete_is_skipped() {
    // tree-sitter-php parses `Tenant::TABLE` as the same node kind as
    // `Tenant::class`; only the latter names a class. A non-`::class` constant
    // must be skipped, not misclassified as the scope class (which would point
    // `app('currentTenant')->member` at the wrong target).
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider("$this->app->singleton('currentTenant', Tenant::TABLE);");
    let found = discovered_bindings(&src, root);
    assert!(
        concrete_for(&found, "currentTenant").is_none(),
        "a `::SOME_CONST` concrete must not register a binding, got {found:?}"
    );
}

#[test]
fn named_argument_binding_is_extracted() {
    // Named arguments wrap the value behind a `name` label child; reading the
    // value (not the label) keeps the binding from being silently dropped.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider(
        "$this->app->singleton(abstract: 'currentTenant', concrete: Tenant::class);",
    );
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) =
        concrete_for(&found, "currentTenant").expect("named-argument binding registered");
    assert_eq!(concrete, "Tenant");
}

#[test]
fn quoted_key_content_is_read_without_greedy_trim() {
    // The key is read from the `string_content` child, so a key whose content
    // includes quote characters is preserved exactly rather than greedily
    // stripped from both ends.
    let (_dir, root) = project_with_files(&[TENANT_FILE]);
    let src = tenant_provider("$this->app->bind(\"'wrapped'\", Tenant::class);");
    let found = discovered_bindings(&src, root);
    let (_, concrete, _) =
        concrete_for(&found, "'wrapped'").expect("key with embedded quotes registered verbatim");
    assert_eq!(concrete, "Tenant");
}

// ─── Facade aliases (bootstrap/app.php withAliases + merge precedence) ──────

#[test]
fn extract_with_aliases_resolves_imported_class_consts() {
    // `use` import resolution: the bare `Auth::class` becomes the full FQCN.
    let src = r#"<?php
use Illuminate\Support\Facades\Auth;
use App\Support\CustomCache;

return Application::configure(basePath: __DIR__)
    ->withAliases([
        'Auth' => Auth::class,
        'Cache' => CustomCache::class,
    ])
    ->create();
"#;
    let tree = parse_php(src).unwrap();
    let aliases = extract_with_aliases(&tree, src);
    assert_eq!(
        aliases.get("Auth").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
    assert_eq!(
        aliases.get("Cache").map(String::as_str),
        Some("App\\Support\\CustomCache")
    );
}

#[test]
fn extract_with_aliases_skips_non_class_values() {
    let src = r#"<?php
return Application::configure(basePath: __DIR__)
    ->withAliases([
        'Legacy' => 'not.a.class',
    ])
    ->create();
"#;
    let tree = parse_php(src).unwrap();
    assert!(extract_with_aliases(&tree, src).is_empty());
}

#[tokio::test]
async fn facade_alias_snapshot_seeds_defaults() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();

    let aliases = handle.snapshot_facade_aliases().await.unwrap();
    // A built-in default is present even with no config/bootstrap sources.
    assert_eq!(
        aliases.get("Auth").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[tokio::test]
async fn facade_alias_snapshot_merges_sources_by_precedence() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // config/app.php: overrides the default `Auth` to a custom class and adds
    // a new `LegacyAlias`.
    let app_config = r#"<?php
return [
    'aliases' => [
        'Auth' => App\Facades\LegacyAuth::class,
        'LegacyAlias' => App\Facades\Legacy::class,
    ],
];
"#;
    let app_config_path = root.join("config/app.php");
    std::fs::create_dir_all(app_config_path.parent().unwrap()).unwrap();
    std::fs::write(&app_config_path, app_config).unwrap();

    // bootstrap/app.php: withAliases overrides `Auth` again (highest source)
    // and adds a brand-new `Modern` alias.
    let bootstrap = r#"<?php
use App\Facades\ModernAuth;
use App\Facades\Modern;

return Application::configure(basePath: __DIR__)
    ->withAliases([
        'Auth' => ModernAuth::class,
        'Modern' => Modern::class,
    ])
    ->create();
"#;
    let bootstrap_path = root.join("bootstrap/app.php");
    std::fs::create_dir_all(bootstrap_path.parent().unwrap()).unwrap();
    std::fs::write(&bootstrap_path, bootstrap).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .update_config_file(app_config_path, app_config.to_string())
        .await
        .unwrap();
    handle
        .register_service_provider_source(bootstrap_path, bootstrap.to_string(), 2, root.clone())
        .await
        .unwrap();

    let aliases = handle.snapshot_facade_aliases().await.unwrap();

    // withAliases wins over config/app.php wins over the seed for `Auth`.
    assert_eq!(
        aliases.get("Auth").map(String::as_str),
        Some("App\\Facades\\ModernAuth"),
        "bootstrap withAliases has the highest precedence"
    );
    // config/app.php adds a new alias the seed lacks.
    assert_eq!(
        aliases.get("LegacyAlias").map(String::as_str),
        Some("App\\Facades\\Legacy")
    );
    // withAliases adds a new alias.
    assert_eq!(
        aliases.get("Modern").map(String::as_str),
        Some("App\\Facades\\Modern")
    );
    // An untouched default is still present.
    assert_eq!(
        aliases.get("DB").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\DB")
    );
}

// ─── Macro registry extraction (commit 1) ─────────────────────────────────

#[test]
fn extract_provider_macros_reads_scalar_macro() {
    // `Str::macro('uuid7', fn () => …)` in a provider, with `Str` imported:
    // the receiver token resolves to the Macroable host FQCN, the name is the
    // first string argument, and the definition line is the closure's.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    let src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('uuid7', fn () => 'x');
    }
}
"#;
    let tree = parse_php(src).unwrap();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, src);
    let macros = extract_provider_macros(&tree, src, &provider, dir.path(), &aliases);
    assert_eq!(macros.len(), 1);
    assert_eq!(macros[0].receiver_fqcn, "Illuminate\\Support\\Str");
    assert_eq!(macros[0].macro_name, "uuid7");
    assert_eq!(macros[0].decl_file, provider);
    // The closure is on the same line as the call (0-based line 6).
    assert_eq!(macros[0].decl_line, 6);
}

#[test]
fn extract_provider_macros_ignores_non_macro_static_calls() {
    // A `Str::upper(...)` call (a real method, not a macro registration) and a
    // `bind(...)` are not macro registrations — only `macro`/`mixin` count.
    let src = r#"<?php
use Illuminate\Support\Str;
Str::upper('x');
"#;
    let tree = parse_php(src).unwrap();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, src);
    let macros = extract_provider_macros(
        &tree,
        src,
        std::path::Path::new("/x/Provider.php"),
        std::path::Path::new("/x"),
        &aliases,
    );
    assert!(macros.is_empty());
}

#[test]
fn extract_provider_macros_expands_mixin_methods() {
    // `Str::mixin(new StrMixin)` reflects every PUBLIC and PROTECTED method of
    // `StrMixin` onto the host as a macro — mirroring Laravel's
    // `getMethods(IS_PUBLIC | IS_PROTECTED)` + `setAccessible(true)`. Each one's
    // definition site is its own declaration in the mixin file. PRIVATE methods
    // are the only visibility excluded; Laravel does NOT filter by `__` name, so
    // `__construct` reflects too (a real mixin never returns a closure from it,
    // but the registry faithfully mirrors the reflection scope rather than
    // second-guessing it).
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // composer PSR-4 so `App\Mixins\StrMixin` resolves to its file.
    std::fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mixin_path = root.join("app/Mixins/StrMixin.php");
    std::fs::create_dir_all(mixin_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mixin_path,
        r#"<?php
namespace App\Mixins;
class StrMixin {
    public function __construct() {}
    public function shout(): callable { return fn () => ''; }
    protected function helper(): callable { return fn () => ''; }
    private function secret(): void {}
    public function whisper(): callable { return fn () => ''; }
}
"#,
    )
    .unwrap();

    let provider_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use App\Mixins\StrMixin;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::mixin(new StrMixin);
    }
}
"#;
    let tree = parse_php(provider_src).unwrap();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, provider_src);
    let provider = root.join("app/Providers/AppServiceProvider.php");
    let mut macros = extract_provider_macros(&tree, provider_src, &provider, root, &aliases);
    macros.sort_by(|a, b| a.macro_name.cmp(&b.macro_name));

    // Public + protected register on the `Str` host (4 of the 5 methods); the
    // PRIVATE `secret` is the only one excluded. After sorting by name:
    // `__construct`, `helper`, `shout`, `whisper`. Each points at its own
    // declaration line in the mixin file.
    let names: Vec<&str> = macros.iter().map(|m| m.macro_name.as_str()).collect();
    assert_eq!(names, vec!["__construct", "helper", "shout", "whisper"]);
    assert!(
        !macros.iter().any(|m| m.macro_name == "secret"),
        "the PRIVATE mixin method must NOT register as a macro"
    );
    for m in &macros {
        assert_eq!(m.receiver_fqcn, "Illuminate\\Support\\Str");
        assert_eq!(m.decl_file, mixin_path);
    }
    // The PROTECTED `helper` registers at its own 0-based declaration line (line
    // 5: `<?php`=0, `namespace`=1, `class`=2, `__construct`=3, `shout`=4,
    // `helper`=5) — protected mixin methods ARE live macros, with correct goto.
    let helper = macros
        .iter()
        .find(|m| m.macro_name == "helper")
        .expect("protected mixin method registers");
    assert_eq!(helper.decl_line, 5);
}

#[test]
fn extract_provider_macros_expands_relative_name_mixin() {
    // `Str::mixin(new namespace\StrMixin)` — the `relative_name` argument shape
    // (PHP's `namespace\…` keyword form, an `object_creation_expression` whose
    // class node is `relative_name`, not `name`/`qualified_name`). The arm must
    // reach it and resolve exactly as the sibling `new X` resolvers in
    // `query_chain::extractor`/`flow` do: feed the raw `namespace\StrMixin` text
    // through `resolve_class_name` (which leaves the literal `namespace\` prefix
    // alone, since there is no `use namespace …` alias) and resolve THAT FQCN to
    // a file. PSR-4 here maps the literal prefix so the mixin resolves and its
    // public methods expand — proving the `relative_name` arm is wired without
    // inventing a new resolution path.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // PSR-4 keyed on the literal `namespace\` prefix the sibling resolvers leave
    // intact, so `namespace\StrMixin` → `mixins/StrMixin.php`.
    std::fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "namespace\\": "mixins/" } } }"#,
    )
    .unwrap();
    let mixin_path = root.join("mixins/StrMixin.php");
    std::fs::create_dir_all(mixin_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mixin_path,
        r#"<?php
namespace namespace;
class StrMixin {
    public function shout(): callable { return fn () => ''; }
}
"#,
    )
    .unwrap();

    let provider_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::mixin(new namespace\StrMixin);
    }
}
"#;
    let tree = parse_php(provider_src).unwrap();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, provider_src);
    let provider = root.join("app/Providers/AppServiceProvider.php");
    let macros = extract_provider_macros(&tree, provider_src, &provider, root, &aliases);

    // The relative_name mixin resolved and its public method expanded — before
    // the fix, the arm matched only `name`/`qualified_name` and yielded nothing.
    assert_eq!(macros.len(), 1, "the relative_name mixin must resolve");
    assert_eq!(macros[0].macro_name, "shout");
    assert_eq!(macros[0].receiver_fqcn, "Illuminate\\Support\\Str");
    assert_eq!(macros[0].decl_file, mixin_path);
}

#[tokio::test]
async fn snapshot_macros_registers_provider_macro_at_definition_site() {
    // End-to-end: register a provider source declaring `Str::macro('uuid7', …)`,
    // then assert the macro registry snapshot keys it on the resolved host FQCN
    // and points the definition site at the provider/closure line.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let provider = root.join("app/Providers/AppServiceProvider.php");
    let src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('uuid7', fn () => 'x');
    }
}
"#;

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(provider.clone(), src.to_string(), 2, root.clone())
        .await
        .unwrap();

    let macros = handle.snapshot_macros().await.unwrap();
    let target = macros.get(&("Illuminate\\Support\\Str".to_string(), "uuid7".to_string()));
    assert_eq!(target, Some(&(provider, 6)));
}

#[tokio::test]
async fn snapshot_macros_priority_merges_vendor_and_app() {
    // A package provider (priority 1) and an app provider (priority 3) both
    // register `Str::shared`; the app's site wins on key collision. The package
    // also ships `Str::pkgOnly`, which is resolvable on its own. This is the
    // framework=0 < package=1 < module=2 < app=3 merge the binding registry uses,
    // applied to
    // macros — vendor providers are already `ServiceProviderFile` Salsa inputs,
    // so the same plumbing covers them.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Package provider (priority 1): two macros.
    let pkg = root.join("vendor/acme/pkg/src/PkgServiceProvider.php");
    let pkg_src = r#"<?php
namespace Acme\Pkg;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class PkgServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('shared', fn () => 'pkg');
        Str::macro('pkgOnly', fn () => 'pkg');
    }
}
"#;

    // App provider (priority 3): overrides `shared`.
    let app = root.join("app/Providers/AppServiceProvider.php");
    let app_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('shared', fn () => 'app');
    }
}
"#;

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(pkg.clone(), pkg_src.to_string(), 1, root.clone())
        .await
        .unwrap();
    handle
        .register_service_provider_source(app.clone(), app_src.to_string(), 3, root.clone())
        .await
        .unwrap();

    let macros = handle.snapshot_macros().await.unwrap();

    // App override wins for the colliding key — points at the app provider.
    let shared = macros.get(&("Illuminate\\Support\\Str".to_string(), "shared".to_string()));
    assert_eq!(shared, Some(&(app, 6)));

    // The package-only macro still resolves to the package provider.
    let pkg_only = macros.get(&(
        "Illuminate\\Support\\Str".to_string(),
        "pkgOnly".to_string(),
    ));
    assert_eq!(pkg_only, Some(&(pkg, 7)));
}

#[tokio::test]
async fn equal_priority_collision_resolves_to_smallest_provider_path() {
    // #255 Bug B regression: providers at the SAME priority register the same
    // macro key and the same binding key. `salsa_sp_files` is a HashMap
    // (unspecified iteration order), so before the sorted merge the winner was
    // whichever provider the map happened to yield first — flipping across LSP
    // restarts. The documented rule: the lexicographically smallest provider
    // path wins on an equal-priority collision, in BOTH registries
    // (`sorted_sp_files`).
    //
    // The old 2-provider form caught a reverted (raw-HashMap) `sorted_sp_files`
    // only ~50% of the time — the per-process seed decided the winner. This form
    // is deterministic instead: it asserts `sorted_sp_files`' full ORDER via
    // `snapshot_sorted_provider_paths`, so a reverted sort yields the raw HashMap
    // order and matches the sorted order only 1/N! of the time — negligible at
    // N=6 (~0.14%) — while still checking both registries' merge winners (#267).
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Six colliding providers whose class names sort A→F. `P0` is the
    // documented winner; every provider registers the same macro + binding key
    // with a distinct concrete, so a wrong merge order is observable.
    let names = ["Aa", "Bb", "Cc", "Dd", "Ee", "Ff"];
    let providers: Vec<(PathBuf, String, String)> = names
        .iter()
        .map(|n| {
            let path = root.join(format!("app/Providers/{n}ServiceProvider.php"));
            let concrete = format!("App\\Services\\{n}Impl");
            let src = format!(
                r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class {n}ServiceProvider extends ServiceProvider {{
    public function boot(): void {{
        Str::macro('shared', fn () => '{n}');
        $this->app->singleton('svc', \{concrete}::class);
    }}
}}
"#
            );
            (path, src, concrete)
        })
        .collect();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    // Register in the REVERSE of the winning order (F→A), so an insertion-ordered
    // merge would pick the wrong provider and a raw-HashMap merge could.
    for (path, src, _) in providers.iter().rev() {
        handle
            .register_service_provider_source(path.clone(), src.clone(), 2, root.clone())
            .await
            .unwrap();
    }

    // Deterministic sort assertion: the paths in merge order MUST be the
    // lexicographically sorted paths. A reverted `sorted_sp_files` yields raw
    // HashMap order, which equals the sorted order only 1/6! of the time.
    let mut expected_order: Vec<PathBuf> = providers.iter().map(|(p, ..)| p.clone()).collect();
    expected_order.sort();
    let sorted_paths = handle.snapshot_sorted_provider_paths().await.unwrap();
    assert_eq!(
        sorted_paths, expected_order,
        "sorted_sp_files must merge providers in lexicographically ascending path order",
    );

    // …and both registries' winners must be the smallest path (`P0` = Aa).
    let (smallest_path, _, smallest_concrete) = &providers[0];

    let macros = handle.snapshot_macros().await.unwrap();
    let (decl_file, _) = macros
        .get(&("Illuminate\\Support\\Str".to_string(), "shared".to_string()))
        .expect("colliding macro key must still resolve");
    assert_eq!(
        decl_file, smallest_path,
        "equal-priority macro collision must resolve to the lexicographically smallest provider path",
    );

    let bindings = handle.get_all_parsed_bindings().await.unwrap();
    let svc = bindings
        .iter()
        .find(|b| b.abstract_name == "svc")
        .expect("colliding binding key must still resolve");
    assert_eq!(
        svc.concrete_class.trim_start_matches('\\'),
        smallest_concrete.as_str(),
        "equal-priority binding collision must resolve to the lexicographically smallest provider path",
    );
}

#[tokio::test]
async fn provider_registration_snapshot_diffs_body_only_macro_rename() {
    // #255 Bug A regression: a macro rename inside `boot()` — a body-only
    // edit whose class-surface diff is EMPTY — must still yield ripple keys,
    // so the save path re-resolves dependent call sites without a restart.
    // The pre/post snapshots come from `file_provider_registrations`; the
    // post-save call passes the fresh text because the App rescan a provider
    // save queues is asynchronous (the provider input would otherwise still
    // hold the pre-save source).
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let provider = root.join("app/Providers/AppServiceProvider.php");
    let before_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('before', fn () => 'x');
    }
}
"#;
    let after_src = before_src.replace("'before'", "'after'");

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(provider.clone(), before_src.to_string(), 2, root.clone())
        .await
        .unwrap();

    // Pure read: `after` side carries the current contribution (baseline is
    // empty until a save transaction).
    let (_, before) = handle
        .file_provider_registrations(provider.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        before.macros,
        vec![("Illuminate\\Support\\Str".to_string(), "before".to_string())],
        "pre-save snapshot must carry the provider's registered macro",
    );

    let (_, after) = handle
        .file_provider_registrations(provider.clone(), Some(after_src))
        .await
        .unwrap();
    assert_eq!(
        after.macros,
        vec![("Illuminate\\Support\\Str".to_string(), "after".to_string())],
        "fresh_text must re-register the provider before the post-save snapshot",
    );

    let keys = registration_ripple_keys(&before, &after, &provider);
    assert!(
        keys.contains(&"Illuminate\\Support\\Str".to_string()),
        "ripple keys must carry the macro host FQCN dependent call sites recorded",
    );
    assert!(
        keys.contains(&provider.to_string_lossy().into_owned()),
        "ripple keys must carry the registering provider's own path (macro decl-file deps)",
    );
}

#[test]
fn registration_ripple_keys_empty_diff_yields_no_keys() {
    // The overwhelming majority of saves change no registrations — the diff
    // must be empty (no provider-path key either), so the save path's
    // early-return still stops body-only NON-registration edits.
    let snap = ProviderRegistrationsData {
        macros: vec![("Illuminate\\Support\\Str".to_string(), "uuid7".to_string())],
        bindings: vec![("svc".to_string(), "App\\Services\\Impl".to_string())],
        aliases: vec![("Str".to_string(), "Illuminate\\Support\\Str".to_string())],
    };
    assert!(
        registration_ripple_keys(&snap, &snap, Path::new("/prov.php")).is_empty(),
        "an unchanged registration set must not ripple",
    );
}

#[test]
fn registration_ripple_keys_binding_retarget_emits_both_concretes() {
    // A binding retarget must emit BOTH sides of the diff: the old concrete
    // finds sites holding the now-stale classification; the new concrete
    // finds sites already referencing the target directly.
    let before = ProviderRegistrationsData {
        bindings: vec![("svc".to_string(), "App\\Old".to_string())],
        ..Default::default()
    };
    let after = ProviderRegistrationsData {
        bindings: vec![("svc".to_string(), "App\\New".to_string())],
        ..Default::default()
    };
    let keys = registration_ripple_keys(&before, &after, Path::new("/prov.php"));
    assert!(keys.contains(&"App\\Old".to_string()));
    assert!(keys.contains(&"App\\New".to_string()));
    assert!(
        keys.contains(&"/prov.php".to_string()),
        "a non-empty diff must include the provider's own path",
    );
}

#[test]
fn registration_ripple_keys_alias_retarget_emits_both_targets() {
    // A facade-alias retarget must emit BOTH sides of the diff: the old
    // target finds sites holding the now-stale classification; the new one
    // finds direct references (mirrors the binding-retarget case).
    let before = ProviderRegistrationsData {
        aliases: vec![("Str".to_string(), "Illuminate\\Support\\Str".to_string())],
        ..Default::default()
    };
    let after = ProviderRegistrationsData {
        aliases: vec![("Str".to_string(), "App\\Support\\MyStr".to_string())],
        ..Default::default()
    };
    let keys = registration_ripple_keys(&before, &after, Path::new("/prov.php"));
    assert!(keys.contains(&"Illuminate\\Support\\Str".to_string()));
    assert!(keys.contains(&"App\\Support\\MyStr".to_string()));
    // …and the stable `alias:<token>` attempt key both sides share, which is the
    // ONLY key that reaches the old target's sites on a first (empty-baseline)
    // save (#267). Lower-cased to match the case-insensitive facade matching.
    assert!(
        keys.contains(&"alias:str".to_string()),
        "an alias retarget must emit the alias:<token> attempt key; got {keys:?}",
    );
}

#[tokio::test]
async fn config_app_alias_edit_ripples_through_save_transaction() {
    // #255 Bug A regression (config/app.php): the legacy `aliases` array
    // lives in the `config_files` input — a SEPARATE map from
    // `salsa_sp_files` — so the save transaction must route the fresh text
    // through the config input, or the post-save alias snapshot reads the
    // same entry as the baseline and an alias edit never ripples.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let cfg = root.join("config/app.php");
    let v1 = r#"<?php
return [
    'aliases' => [
        'Str' => Illuminate\Support\Str::class,
    ],
];
"#;
    let v2 = v1.replace("Illuminate\\Support\\Str", "App\\Support\\MyStr");

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();

    // First save transaction establishes the baseline at v1.
    let (_, after1) = handle
        .file_provider_registrations(cfg.clone(), Some(v1.to_string()))
        .await
        .unwrap();
    assert_eq!(
        after1.aliases,
        vec![("Str".to_string(), "Illuminate\\Support\\Str".to_string())],
        "save transaction must register config/app.php's aliases",
    );

    // Second save retargets the alias: before = v1 baseline, after = v2.
    let (before2, after2) = handle
        .file_provider_registrations(cfg.clone(), Some(v2))
        .await
        .unwrap();
    assert_eq!(
        before2.aliases, after1.aliases,
        "baseline must be the last-saved contribution"
    );
    let keys = registration_ripple_keys(&before2, &after2, &cfg);
    assert!(
        keys.contains(&"Illuminate\\Support\\Str".to_string())
            && keys.contains(&"App\\Support\\MyStr".to_string()),
        "a config/app.php alias retarget must ripple both targets; got {keys:?}",
    );
}

#[tokio::test]
async fn resolve_magic_member_at_classifies_macro_call() {
    // The headline feature, end-to-end through the actor: register a provider
    // declaring `Str::macro('uuid7', fn () => …)`, then resolve a `Str::uuid7()`
    // call site. The receiver `Str` is a VENDOR class the index doesn't carry —
    // it resolves only because the macro registry knows it as a Macroable host
    // (`has_macro_host`) — and the member classifies as `Macro`, with decl_file /
    // decl_line read straight from the registry (the closure's location, NOT a
    // method on the host class). Drives the `kind == Macro` arm of
    // `handle_resolve_magic_member_at` (`salsa_impl.rs`).
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let provider = root.join("app/Providers/AppServiceProvider.php");
    let provider_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('uuid7', fn () => 'x');
    }
}
"#;

    // The call site. `Str` is imported so it qualifies to the framework host.
    let caller_path = root.join("app/Support/Ids.php");
    let caller_src = r#"<?php
namespace App\Support;
use Illuminate\Support\Str;
class Ids {
    public function make(): string { return Str::uuid7(); }
}
"#;
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            provider.clone(),
            provider_src.to_string(),
            2,
            root.clone(),
        )
        .await
        .unwrap();
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    handle.get_patterns(caller_path.clone()).await.unwrap();

    // Cursor on the `uuid7` member token.
    let (line, col) = position_of(caller_src, "uuid7");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("a registered macro call should resolve");

    assert_eq!(data.kind, MagicMemberKind::Macro);
    assert_eq!(data.declaring_fqcn, "Illuminate\\Support\\Str");
    assert_eq!(data.member, "uuid7");
    // The definition site is the registered closure's location in the provider —
    // line 6 (0-based) where `Str::macro('uuid7', …)` lives — not a method on the
    // vendor host. No end line (the registry stores only the start).
    assert_eq!(data.decl_file, Some(provider));
    assert_eq!(data.decl_line, Some(6));
    assert_eq!(data.decl_end_line, None);
}

// ─── Facade goto/hover end-to-end (the gap that hid both breaks) ───────────
//
// The PERMANENT regression test for the whole facade feature, driving the REAL
// request path (`SalsaHandle::resolve_magic_member_at` — the exact call
// goto_definition / hover make), not a direct resolver call. It models a real
// Laravel 12 project:
//
//   - a vendor `Illuminate\Auth\AuthManager` declaring `check()` (but NOT
//     `guard()`, which Laravel forwards via `__call`/a guard),
//   - a vendor `Illuminate\Auth\AuthServiceProvider` registering the EXACT
//     arrow-fn `singleton('auth', fn ($app) => new AuthManager($app))` — the
//     bare same-namespace `new` that FIX #1 must namespace-qualify, and
//   - a namespaced `AboutController` calling `Auth::check()` in each of the
//     three facade forms.
//
// Without the two fixes this resolves NOTHING: FIX #1's break degrades the
// `auth` binding to "Closure" so `binding_concrete("auth")` is empty and the
// receiver never reaches the concrete; FIX #2's break classifies the method as
// `PlainMember` which the consumer drops as Intelephense's. With both, the call
// resolves to `FacadeMethod` on `AuthManager` with a non-None decl site.

const VENDOR_AUTH_MANAGER_SRC: &str = r#"<?php
namespace Illuminate\Auth;
class AuthManager {
    public function check() { return true; }
}
"#;

/// The vendor `AuthServiceProvider`, in `AuthManager`'s OWN namespace, binding
/// `auth` via the bare same-namespace arrow-fn `new` — the FIX #1 shape.
const VENDOR_AUTH_PROVIDER_SRC: &str = r#"<?php
namespace Illuminate\Auth;
use Illuminate\Support\ServiceProvider;
class AuthServiceProvider extends ServiceProvider {
    public function register(): void {
        $this->app->singleton('auth', fn ($app) => new AuthManager($app));
    }
}
"#;

/// Spawn an actor over a tempdir Laravel project with the vendor `AuthManager`
/// plus `AuthServiceProvider` registered and a namespaced caller whose body is
/// `caller_body` (the `Auth::…` call). Returns the caller path so the test can
/// position on the member token.
async fn auth_facade_e2e_project(caller_body: &str) -> (TempDir, SalsaHandle, PathBuf, String) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Vendor AuthManager on disk at its PSR-4-mapped path so the binding's
    // concrete resolves to a real file.
    let manager_path = root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php");
    std::fs::create_dir_all(manager_path.parent().unwrap()).unwrap();
    std::fs::write(&manager_path, VENDOR_AUTH_MANAGER_SRC).unwrap();

    let provider_path =
        root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthServiceProvider.php");
    std::fs::write(&provider_path, VENDOR_AUTH_PROVIDER_SRC).unwrap();

    let caller_src = format!(
        r#"<?php
namespace App\Http\Controllers;

class AboutController {{
    public function show() {{
        {caller_body}
    }}
}}
"#
    );
    let caller_path = root.join("app/Http/Controllers/AboutController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, &caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    // Registering the provider source parses the `auth → AuthManager` binding
    // (via FIX #1's qualification); `register_cached_binding_batch` below mirrors
    // production's `populate_cache_from_salsa` to land it in the container
    // registry the resolver reads. (FIX #1 is what makes the parsed concrete
    // `AuthManager` instead of "Closure"; without it this binding is useless.)
    handle
        .register_service_provider_source(
            provider_path,
            VENDOR_AUTH_PROVIDER_SRC.to_string(),
            0,
            root.clone(),
        )
        .await
        .unwrap();
    register_parsed_bindings_into_registry(&handle).await;
    handle
        .update_file(caller_path.clone(), 1, caller_src.clone())
        .await
        .unwrap();
    // Force the vendor AuthManager's parse so it lands in the class-hierarchy
    // index (the on-demand population path) — the consumer needs its file/line.
    handle.get_patterns(manager_path).await.unwrap();
    handle.get_patterns(caller_path.clone()).await.unwrap();
    (dir, handle, caller_path, caller_src)
}

/// Mirror production's `populate_cache_from_salsa` binding step: pull the bindings
/// the actor parsed from registered provider SOURCES and feed them back through
/// `register_cached_binding_batch`, which is what actually fills the `sp_bindings`
/// registry the live-query resolver (`binding_concrete`) reads. The source path
/// alone only fills the lazy `salsa_sp_files` Salsa input, not that registry — so
/// without this round-trip a facade's accessor never reaches its concrete.
async fn register_parsed_bindings_into_registry(handle: &SalsaHandle) {
    let parsed = handle.get_all_parsed_bindings().await.unwrap();
    let entries: Vec<_> = parsed
        .into_iter()
        .map(|b| {
            (
                b.abstract_name,
                b.concrete_class,
                format!("{:?}", b.binding_type).to_lowercase(),
                b.file_path.map(|p| p.to_string_lossy().into_owned()),
                Some(b.source_file.to_string_lossy().into_owned()),
                b.source_line,
            )
        })
        .collect();
    handle.register_cached_binding_batch(entries).await.unwrap();
}

/// Resolve `member` at its position in `caller_body` through the real request
/// path. Asserts it classifies as a `FacadeMethod` on `AuthManager` with a
/// non-None decl site (NEVER None — the break this guards).
async fn assert_facade_method_resolves(caller_body: &str, member: &str) -> MagicMemberHoverData {
    let (_dir, handle, caller_path, caller_src) = auth_facade_e2e_project(caller_body).await;
    let (line, col) = position_of(&caller_src, member);
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .unwrap_or_else(|| {
            panic!("`{caller_body}` must resolve to a facade method, got None (the break)")
        });
    assert_eq!(data.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(data.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
    assert_eq!(data.member, member);
    // A non-None decl site is the whole point: goto/hover must have somewhere to
    // jump. (For a declared method it's the method line; for the __call degrade
    // it's the class line — either way, present.)
    assert!(
        data.decl_file.is_some() && data.decl_line.is_some(),
        "facade goto must have a decl site, got {data:?}"
    );
    // The chased declaration carries an end line so the hover can slice the full
    // method (signature + body) for its rich source snippet — never `None` for a
    // method that was actually located.
    assert!(
        data.decl_end_line.is_some(),
        "facade method decl must carry an end line for the hover snippet, got {data:?}"
    );
    data
}

#[tokio::test]
async fn facade_e2e_receiver_resolves_to_concrete_class() {
    // Cursor on the RECEIVER token `\Auth` (not the method) navigates to the
    // bound concrete class `AuthManager` — bullet #1. The position index only
    // marks the method token, so this exercises the receiver fallback path.
    let (_dir, handle, caller_path, caller_src) =
        auth_facade_e2e_project(r#"\Auth::check();"#).await;

    let (line, col) = position_of(&caller_src, "Auth");
    let target = handle
        .resolve_facade_receiver_at(caller_path.clone(), line, col)
        .await
        .unwrap()
        .expect("receiver `\\Auth` must resolve to the concrete class");
    assert!(
        target.file.ends_with("Illuminate/Auth/AuthManager.php"),
        "expected AuthManager file, got {:?}",
        target.file
    );
    // `class AuthManager {` is line 2 (0-based) in VENDOR_AUTH_MANAGER_SRC.
    assert_eq!(target.decl_line, 2);
    assert_eq!(target.fqcn, "Illuminate\\Auth\\AuthManager");

    // The METHOD token is NOT the receiver path — it must decline so the normal
    // member-access goto (FacadeMethod) handles it.
    let (mline, mcol) = position_of(&caller_src, "check");
    assert!(
        handle
            .resolve_facade_receiver_at(caller_path, mline, mcol)
            .await
            .unwrap()
            .is_none(),
        "cursor on the method name must not resolve as a receiver"
    );
}

#[tokio::test]
async fn facade_e2e_global_alias_check_resolves_in_namespaced_file() {
    // Global-alias form in a NAMESPACED controller: `\Auth::check();`. The
    // leading `\` forces global resolution despite the file's namespace, so the
    // seed alias `Auth → …\Facades\Auth` applies. This is the headline real-world
    // shape (`\Auth::check()` in `App\Http\Controllers\AboutController`).
    let data = assert_facade_method_resolves(r#"\Auth::check();"#, "check").await;
    assert_eq!(data.decl_line, Some(3));
}

#[tokio::test]
async fn facade_e2e_inline_fully_qualified_check_resolves() {
    // Inline fully-qualified form: `\Illuminate\Support\Facades\Auth::check();`
    // with no import — the path lands directly in the Facades namespace.
    let data =
        assert_facade_method_resolves(r#"\Illuminate\Support\Facades\Auth::check();"#, "check")
            .await;
    assert_eq!(data.decl_line, Some(3));
}

#[tokio::test]
async fn facade_e2e_imported_alias_check_resolves() {
    // The true imported form, with the `use` inside the file. Built bespoke (not
    // via the shared helper) so the import sits above the class.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let manager_path = root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php");
    std::fs::create_dir_all(manager_path.parent().unwrap()).unwrap();
    std::fs::write(&manager_path, VENDOR_AUTH_MANAGER_SRC).unwrap();
    let provider_path =
        root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthServiceProvider.php");
    std::fs::write(&provider_path, VENDOR_AUTH_PROVIDER_SRC).unwrap();

    let caller_src = r#"<?php
namespace App\Http\Controllers;

use Illuminate\Support\Facades\Auth;

class AboutController {
    public function show() {
        Auth::check();
    }
}
"#;
    let caller_path = root.join("app/Http/Controllers/AboutController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            provider_path,
            VENDOR_AUTH_PROVIDER_SRC.to_string(),
            0,
            root.clone(),
        )
        .await
        .unwrap();
    register_parsed_bindings_into_registry(&handle).await;
    handle
        .update_file(caller_path.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    handle.get_patterns(manager_path).await.unwrap();
    handle.get_patterns(caller_path.clone()).await.unwrap();

    // Position on the `check` member of `Auth::check()` (skip the `use` line's
    // none — `check` appears only in the call).
    let (line, col) = position_of(caller_src, "check");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("imported `Auth::check()` must resolve, not None");
    assert_eq!(data.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(data.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
    assert_eq!(data.decl_line, Some(3));
}

#[tokio::test]
async fn facade_e2e_undeclared_method_degrades_to_class_never_none() {
    // `\Auth::guard()` — `guard` is NOT declared on the AuthManager stub (real
    // Laravel forwards it via `__call`/a guard). We must NOT chase the forwarding
    // chain and must NOT return None: DEGRADE to the concrete CLASS line. This is
    // the __call caveat, end-to-end.
    let (_dir, handle, caller_path, caller_src) =
        auth_facade_e2e_project(r#"\Auth::guard();"#).await;
    let (line, col) = position_of(&caller_src, "guard");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("undeclared facade method must degrade, not return None");
    assert_eq!(data.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(data.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
    // No `guard` method token → decl line falls back to the class declaration
    // line (2, 0-based: `class AuthManager {` — line 0 `<?php`, 1 `namespace`),
    // still a real target.
    assert!(data.decl_file.is_some());
    assert_eq!(data.decl_line, Some(2));
}

// ─── Rich FacadeMethod hover card (description summary + typed signature) ────
//
// The decl site a facade method resolves to must carry an END line. From the
// declaring file we build the two enriched card pieces the way `main.rs` does:
// the leading PHPDoc summary becomes the card's DESCRIPTION line, and the code
// block is the docblock-free signature+body with the `@return` type folded into
// the signature (the `AuthManager::check()` fixture has only `@return bool`, no
// native return type, so the fold is exercised). Covered for BOTH the static-
// facade form (`\Auth::check()`) and the helper-chain form (`auth()->check()`),
// since both tag `FacadeMethod`.

/// `AuthManager` whose `check()` carries a real PHPDoc block — the source the
/// rich hover snippet is sliced from. Kept separate from `VENDOR_AUTH_MANAGER_SRC`
/// so the `decl_line == Some(3)` assertions on the lean stub stay valid.
const VENDOR_AUTH_MANAGER_DOCBLOCK_SRC: &str = r#"<?php
namespace Illuminate\Auth;
class AuthManager {
    /**
     * Determine if the current user is authenticated.
     *
     * @return bool
     */
    public function check()
    {
        return true;
    }
}
"#;

/// Provider binding `auth → AuthManager` for the docblock variant. Identical
/// shape to `VENDOR_AUTH_PROVIDER_SRC`; named distinctly for clarity.
const VENDOR_AUTH_PROVIDER_DOCBLOCK_SRC: &str = VENDOR_AUTH_PROVIDER_SRC;

/// Resolve `member` at its position in `caller_body` against an `AuthManager`
/// whose `check()` has a docblock, then build the two enriched FacadeMethod card
/// pieces the production `hover_for_magic_member` does. Returns
/// `(description, code)`: the docblock summary that feeds the card's description
/// line, and the docblock-free signature+body code block with the `@return` type
/// folded into the signature.
async fn facade_method_rich_snippet(caller_body: &str, member: &str) -> (Option<String>, String) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let manager_path = root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php");
    std::fs::create_dir_all(manager_path.parent().unwrap()).unwrap();
    std::fs::write(&manager_path, VENDOR_AUTH_MANAGER_DOCBLOCK_SRC).unwrap();
    let provider_path =
        root.join("vendor/laravel/framework/src/Illuminate/Auth/AuthServiceProvider.php");
    std::fs::write(&provider_path, VENDOR_AUTH_PROVIDER_DOCBLOCK_SRC).unwrap();

    let caller_src = format!(
        r#"<?php
namespace App\Http\Controllers;

class AboutController {{
    public function show() {{
        {caller_body}
    }}
}}
"#
    );
    let caller_path = root.join("app/Http/Controllers/AboutController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, &caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            provider_path,
            VENDOR_AUTH_PROVIDER_DOCBLOCK_SRC.to_string(),
            0,
            root.clone(),
        )
        .await
        .unwrap();
    register_parsed_bindings_into_registry(&handle).await;
    handle
        .update_file(caller_path.clone(), 1, caller_src.clone())
        .await
        .unwrap();
    handle.get_patterns(manager_path).await.unwrap();
    handle.get_patterns(caller_path.clone()).await.unwrap();

    let (line, col) = position_of(&caller_src, member);
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("`{caller_body}` must resolve to a facade method, got None"));
    assert_eq!(data.kind, MagicMemberKind::FacadeMethod);

    let decl_file = data.decl_file.expect("decl file");
    let start = data.decl_line.expect("decl start line");
    let end = data
        .decl_end_line
        .expect("decl end line must be present for the rich snippet");
    let src = std::fs::read_to_string(&decl_file).unwrap();
    let snippet = crate::hover::extract_member_snippet(&src, start, end);
    let docblock = crate::hover::extract_leading_docblock(&src, start);
    let description = docblock.as_deref().and_then(crate::hover::docblock_summary);
    let return_type = docblock
        .as_deref()
        .and_then(crate::hover::docblock_return_type);
    let code = crate::hover::fold_return_type(&snippet, return_type.as_deref());
    (description, code)
}

#[tokio::test]
async fn facade_static_check_hover_card_has_summary_description_and_typed_signature() {
    // Static facade form: `\Auth::check()`.
    let (description, code) = facade_method_rich_snippet(r#"\Auth::check();"#, "check").await;
    // Summary is promoted to the card description — NOT left in the code block.
    assert_eq!(
        description.as_deref(),
        Some("Determine if the current user is authenticated.")
    );
    // The code block is docblock-free and carries the folded `@return` type.
    assert!(!code.contains("/**"), "docblock leaked into code: {code}");
    assert!(
        !code.contains("@return"),
        "@return leaked into code: {code}"
    );
    assert!(
        code.contains("public function check(): bool"),
        "return type not folded into signature: {code}"
    );
    assert!(code.contains("return true;"), "body missing: {code}");
}

#[tokio::test]
async fn facade_helper_chain_check_hover_card_has_summary_description_and_typed_signature() {
    // Helper-chain form: `auth()->check()`. Same `FacadeMethod` tag, same card —
    // both paths reach the docblocked `AuthManager::check()`.
    let (description, code) = facade_method_rich_snippet(r#"auth()->check();"#, "check").await;
    assert_eq!(
        description.as_deref(),
        Some("Determine if the current user is authenticated.")
    );
    assert!(!code.contains("/**"), "docblock leaked into code: {code}");
    assert!(
        !code.contains("@return"),
        "@return leaked into code: {code}"
    );
    assert!(
        code.contains("public function check(): bool"),
        "return type not folded into signature: {code}"
    );
    assert!(code.contains("return true;"), "body missing: {code}");
}

// ─── Helper-chain goto/hover end-to-end (#253) ─────────────────────────────
//
// The permanent regression test for the helper-chain feature, driving the REAL
// request path (`resolve_magic_member_at`). Models a Laravel project where the
// `view` container binding resolves to `Illuminate\View\Factory`, whose
// `make()` returns the `View` CONTRACT — so a single-hop `view()->make()`
// classifies as a `FacadeMethod` on the concrete `Factory`, and a two-hop
// `view()->make()->render()` resolves the second receiver through the
// contract→concrete implementors scan to `Illuminate\View\View`.

const VENDOR_VIEW_FACTORY_SRC: &str = r#"<?php
namespace Illuminate\View;
use Illuminate\Contracts\View\View as ViewContract;
class Factory {
    public function make($view): ViewContract { }
}
"#;

const VENDOR_VIEW_CONTRACT_SRC: &str = r#"<?php
namespace Illuminate\Contracts\View;
interface View {
    public function render(): string;
}
"#;

const VENDOR_VIEW_CONCRETE_SRC: &str = r#"<?php
namespace Illuminate\View;
use Illuminate\Contracts\View\View as ViewContract;
class View implements ViewContract {
    public function render(): string { return ''; }
}
"#;

/// A vendor `ViewServiceProvider` (in `Factory`'s own namespace) binding `view`
/// via the bare same-namespace arrow-fn `new` — mirrors the FIX #1 shape the
/// facade e2e uses, so the parsed concrete is `Factory`, not "Closure".
const VENDOR_VIEW_PROVIDER_SRC: &str = r#"<?php
namespace Illuminate\View;
use Illuminate\Support\ServiceProvider;
class ViewServiceProvider extends ServiceProvider {
    public function register(): void {
        $this->app->singleton('view', fn ($app) => new Factory($app));
    }
}
"#;

/// Spawn an actor over a tempdir project with the vendor View Factory / View /
/// contract + a `view`-binding provider, and a namespaced caller whose body is
/// `caller_body`. Returns the caller path + source for positioning.
async fn view_helper_e2e_project(caller_body: &str) -> (TempDir, SalsaHandle, PathBuf, String) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let base = root.join("vendor/laravel/framework/src/Illuminate");
    let factory_path = base.join("View/Factory.php");
    std::fs::create_dir_all(factory_path.parent().unwrap()).unwrap();
    std::fs::write(&factory_path, VENDOR_VIEW_FACTORY_SRC).unwrap();
    let contract_path = base.join("Contracts/View/View.php");
    std::fs::create_dir_all(contract_path.parent().unwrap()).unwrap();
    std::fs::write(&contract_path, VENDOR_VIEW_CONTRACT_SRC).unwrap();
    let concrete_path = base.join("View/View.php");
    std::fs::write(&concrete_path, VENDOR_VIEW_CONCRETE_SRC).unwrap();
    let provider_path = base.join("View/ViewServiceProvider.php");
    std::fs::write(&provider_path, VENDOR_VIEW_PROVIDER_SRC).unwrap();

    let caller_src = format!(
        r#"<?php
namespace App\Http\Controllers;

class PageController {{
    public function show() {{
        {caller_body}
    }}
}}
"#
    );
    let caller_path = root.join("app/Http/Controllers/PageController.php");
    std::fs::create_dir_all(caller_path.parent().unwrap()).unwrap();
    std::fs::write(&caller_path, &caller_src).unwrap();

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(
            provider_path,
            VENDOR_VIEW_PROVIDER_SRC.to_string(),
            0,
            root.clone(),
        )
        .await
        .unwrap();
    register_parsed_bindings_into_registry(&handle).await;
    handle
        .update_file(caller_path.clone(), 1, caller_src.clone())
        .await
        .unwrap();
    // Force the vendor parses so Factory / contract / concrete land in the
    // class-hierarchy index (the consumer needs their file/line + the
    // interface→implementors edge for the second hop).
    handle.get_patterns(factory_path).await.unwrap();
    handle.get_patterns(contract_path).await.unwrap();
    handle.get_patterns(concrete_path).await.unwrap();
    handle.get_patterns(caller_path.clone()).await.unwrap();
    (dir, handle, caller_path, caller_src)
}

#[tokio::test]
async fn helper_chain_e2e_view_make_resolves_to_concrete_factory() {
    // Single hop: `view()->make('welcome')` — cursor on `make`. Routes through
    // the real request path to the concrete `Factory` the `view` binding
    // resolves to, classified as a `FacadeMethod` with a non-None decl site.
    let (_dir, handle, caller_path, caller_src) =
        view_helper_e2e_project(r#"view()->make('welcome');"#).await;
    let (line, col) = position_of(&caller_src, "make");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("`view()->make()` must resolve, not None");
    assert_eq!(data.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(data.declaring_fqcn, "Illuminate\\View\\Factory");
    assert!(
        data.decl_file.is_some() && data.decl_line.is_some(),
        "goto must have a decl site, got {data:?}"
    );
}

#[tokio::test]
async fn helper_chain_e2e_two_hop_render_resolves_to_concrete_implementor() {
    // Two hops: `view()->make('welcome')->render()` — cursor on `render`. The
    // first hop types the receiver as `Factory`; `make()` returns the `View`
    // CONTRACT; the implementors scan lands the second receiver on the concrete
    // `Illuminate\View\View`, where `render` is declared. The decl site must be
    // inside the concrete implementing class, never the contract.
    let (_dir, handle, caller_path, caller_src) =
        view_helper_e2e_project(r#"view()->make('welcome')->render();"#).await;
    let (line, col) = position_of(&caller_src, "render");
    let data = handle
        .resolve_magic_member_at(caller_path, line, col, None)
        .await
        .unwrap()
        .expect("two-hop `view()->make()->render()` must resolve, not None");
    assert_eq!(data.declaring_fqcn, "Illuminate\\View\\View");
    assert!(
        data.decl_file
            .as_ref()
            .is_some_and(|f| f.ends_with("Illuminate/View/View.php")),
        "render must land inside the concrete View, got {data:?}"
    );
    assert!(data.decl_line.is_some());
}

#[tokio::test]
async fn snapshot_macros_invalidates_when_provider_body_changes() {
    // Salsa invalidation for macros: register a provider source, snapshot, then
    // re-register the SAME path with a renamed macro and a removed one. The next
    // snapshot must reflect the edit — the renamed key replaces the old, and the
    // removed macro is gone. Mirrors
    // `salsa_config_refreshes_when_provider_registered_after_first_build`: the
    // provider is a `ServiceProviderFile` Salsa input, so re-registering the path
    // bumps the input and recomputes `parse_service_provider_source`.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let provider = root.join("app/Providers/AppServiceProvider.php");

    let before_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('oldName', fn () => 'x');
        Str::macro('doomed', fn () => 'y');
    }
}
"#;

    let handle = SalsaActor::spawn();
    handle
        .register_config_files(root.clone(), None, None, None)
        .await
        .unwrap();
    handle
        .register_service_provider_source(provider.clone(), before_src.to_string(), 2, root.clone())
        .await
        .unwrap();

    let key = |name: &str| ("Illuminate\\Support\\Str".to_string(), name.to_string());

    let before = handle.snapshot_macros().await.unwrap();
    assert!(
        before.contains_key(&key("oldName")),
        "precondition: oldName registered"
    );
    assert!(
        before.contains_key(&key("doomed")),
        "precondition: doomed registered"
    );

    // Re-register the SAME path: `oldName` → `newName`, and `doomed` removed.
    let after_src = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('newName', fn () => 'x');
    }
}
"#;
    handle
        .register_service_provider_source(provider.clone(), after_src.to_string(), 2, root.clone())
        .await
        .unwrap();

    let after = handle.snapshot_macros().await.unwrap();
    assert!(
        after.contains_key(&key("newName")),
        "the renamed macro must appear after re-registration (Salsa recomputed the input)"
    );
    assert!(
        !after.contains_key(&key("oldName")),
        "the old macro name must be GONE after the rename — stale registry entry would mean no invalidation"
    );
    assert!(
        !after.contains_key(&key("doomed")),
        "the removed macro must be GONE after re-registration"
    );
}

// ─── M1 single-parse capture: member_context gating via the ACTOR ─────────
//
// The vendor gate + zero-cost `None` must also hold through the actor's
// `handle_get_patterns` (the on-demand / save-refresh constructor), so warm
// build and save-refresh agree on which files carry context.

#[tokio::test]
async fn handle_get_patterns_captures_context_non_vendor() {
    let handle = SalsaActor::spawn();
    let path = std::path::PathBuf::from("/proj/app/Http/Controllers/C.php");
    let src = "<?php\nnamespace App;\nclass C { public function f(\\App\\Models\\User $u) { return $u->email; } }\n";
    handle
        .update_file(path.clone(), 1, src.to_string())
        .await
        .unwrap();
    let data = handle.get_patterns(path).await.unwrap().expect("patterns");
    let ctx = data
        .member_context
        .as_ref()
        .expect("a non-vendor member reader must capture context");
    assert_eq!(ctx.sites.len(), data.member_access_refs.len());
}

#[tokio::test]
async fn handle_get_patterns_skips_capture_for_vendor() {
    let handle = SalsaActor::spawn();
    let path = std::path::PathBuf::from("/proj/vendor/acme/pkg/src/C.php");
    let src = "<?php\nnamespace Acme;\nclass C { public function f(\\App\\Models\\User $u) { return $u->email; } }\n";
    handle
        .update_file(path.clone(), 1, src.to_string())
        .await
        .unwrap();
    let data = handle.get_patterns(path).await.unwrap().expect("patterns");
    assert!(
        !data.member_access_refs.is_empty(),
        "the file DOES have a member access (proving the gate drops context)"
    );
    assert!(
        data.member_context.is_none(),
        "a vendor file must capture no context through the actor either"
    );
}

// ─── Two-constructor parity ───────────────────────────────────────────────
//
// A file's patterns must not depend on which constructor built them — the warm
// path (`pattern_indexer::parse_owned`) and the on-demand / save-refresh path
// (`handle_get_patterns`) both feed the same index, so any divergence shows up
// as reference counts that change when a file happens to be edited.

/// Component tags built as PHP string literals must be emitted identically by
/// both constructors — same names, same spans, same order.
#[tokio::test]
async fn both_constructors_agree_on_string_built_component_tags() {
    let path = std::path::PathBuf::from("/proj/app/Jobs/Render.php");
    let src = "<?php\nclass Render {\n    public function html(int $id): string {\n        return \"<x-reader.cross-reference :id=\\\"{$id}\\\" />\" . '<livewire:counter />';\n    }\n}\n";

    let handle = SalsaActor::spawn();
    handle
        .update_file(path.clone(), 1, src.to_string())
        .await
        .unwrap();
    let actor = handle
        .get_patterns(path.clone())
        .await
        .unwrap()
        .expect("patterns");
    let warm = crate::pattern_indexer::parse_owned(&path, src);

    let shape = |p: &ParsedPatternsData| {
        (
            p.components
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        c.tag_name.clone(),
                        c.line,
                        c.column,
                        c.end_column,
                    )
                })
                .collect::<Vec<_>>(),
            p.livewire_refs
                .iter()
                .map(|l| (l.name.clone(), l.line, l.column, l.end_column))
                .collect::<Vec<_>>(),
        )
    };

    let (components, livewire) = shape(&warm);
    assert_eq!(
        components.len(),
        1,
        "the string-built component tag must be captured, got {components:?}"
    );
    assert_eq!(components[0].0, "reader.cross-reference");
    assert_eq!(livewire.len(), 1, "got {livewire:?}");
    assert_eq!(livewire[0].0, "counter");
    assert_eq!(
        shape(&actor),
        shape(&warm),
        "warm and on-demand constructors disagree"
    );
}

/// Same parity requirement for the `@use` class references derived from a
/// Blade file's directives.
#[tokio::test]
async fn both_constructors_agree_on_blade_use_class_refs() {
    let path = std::path::PathBuf::from("/proj/resources/views/page.blade.php");
    let src = "@use(\"App\\Support\\Reader\\VerseMarkerResolver\")\n@use('App\\Models\\Flight', 'FlightModel')\n<div></div>\n";

    let handle = SalsaActor::spawn();
    handle
        .update_file(path.clone(), 1, src.to_string())
        .await
        .unwrap();
    let actor = handle
        .get_patterns(path.clone())
        .await
        .unwrap()
        .expect("patterns");
    let warm = crate::pattern_indexer::parse_owned(&path, src);

    let shape = |p: &ParsedPatternsData| {
        p.class_refs
            .iter()
            .map(|c| (c.name.clone(), c.line, c.column, c.end_column))
            .collect::<Vec<_>>()
    };

    let warm_refs = shape(&warm);
    assert_eq!(
        warm_refs.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        [
            r"App\Support\Reader\VerseMarkerResolver",
            r"App\Models\Flight"
        ],
        "both imports captured, in source order"
    );
    assert_eq!(
        shape(&actor),
        warm_refs,
        "warm and on-demand constructors disagree"
    );
}

/// Parity for the PHP side of the class-reference index: `use` statements are
/// derived off each constructor's own full-file parse, so they are the most
/// likely of the three sources to drift.
#[tokio::test]
async fn both_constructors_agree_on_php_use_class_refs() {
    let path = std::path::PathBuf::from("/proj/app/Http/Controllers/FlightController.php");
    let src = "<?php\nnamespace App\\Http\\Controllers;\n\nuse App\\Models\\Flight;\nuse App\\Models\\{Airport, Gate as G};\nuse function App\\Helpers\\fmt;\n\nclass FlightController {}\n";

    let handle = SalsaActor::spawn();
    handle
        .update_file(path.clone(), 1, src.to_string())
        .await
        .unwrap();
    let actor = handle
        .get_patterns(path.clone())
        .await
        .unwrap()
        .expect("patterns");
    let warm = crate::pattern_indexer::parse_owned(&path, src);

    let shape = |p: &ParsedPatternsData| {
        p.class_refs
            .iter()
            .map(|c| (c.name.clone(), c.line, c.column, c.end_column))
            .collect::<Vec<_>>()
    };

    let warm_refs = shape(&warm);
    assert_eq!(
        warm_refs.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        [
            r"App\Models\Flight",
            r"App\Models\Airport",
            r"App\Models\Gate"
        ],
        "grouped clauses expand; the `function` import binds no class"
    );
    assert_eq!(
        shape(&actor),
        warm_refs,
        "warm and on-demand constructors disagree"
    );
}

// ─── loadViewsFrom() path resolution (issues #285, #290) ───────────────────
//
// `LOAD_VIEWS_RE` captures the concatenated fragment with its leading slash
// (`__DIR__ . '/../resources/views'` captures "/../resources/views"), and
// `Path::join` REPLACES its receiver when handed an absolute path — so every
// `loadViewsFrom(__DIR__.'…')` registration silently resolved to a
// root-relative nonsense path and the namespace never worked (#285).
//
// The strip-and-join rule itself is unit-tested next to the function it lives
// on, in `path_join`. These tests pin the *behaviour* the bug broke: that a
// registration ends up pointing at a real directory. They fail against the
// pre-#285 parser, which yielded `Some("/../resources/views")`.

#[test]
fn load_views_from_dir_relative_registers_real_directory() {
    let ns = discovered_view_namespaces(
        r#"<?php
class CourierServiceProvider extends ServiceProvider
{
    public function boot()
    {
        $this->loadViewsFrom(__DIR__ . '/../resources/views', 'courier');
    }
}
"#,
        "/pkg/src/Providers/CourierServiceProvider.php",
    );
    assert_eq!(
        ns,
        vec![(
            "courier".to_string(),
            Some(PathBuf::from("/pkg/src/resources/views"))
        )]
    );
}

#[test]
fn load_views_from_accepts_whitespace_around_the_arrow() {
    // Space before the arrow, space after it, and the Pint-style line break
    // that puts a fluent `->` on its own line.
    for (label, receiver) in [
        ("space before", "$this ->loadViewsFrom"),
        ("space after", "$this-> loadViewsFrom"),
        ("line break", "$this\n            ->loadViewsFrom"),
    ] {
        let source = format!(
            "<?php\nclass P extends ServiceProvider\n{{\n    public function boot()\n    {{\n        {receiver}(__DIR__ . '/views', 'spaced');\n    }}\n}}\n"
        );
        let ns = discovered_view_namespaces(&source, "/pkg/src/P.php");
        assert_eq!(
            ns,
            vec![("spaced".to_string(), Some(PathBuf::from("/pkg/src/views")))],
            "{label} form did not register"
        );
    }
}

#[test]
fn load_views_from_unwraps_realpath() {
    let ns = discovered_view_namespaces(
        r#"<?php
class P extends ServiceProvider
{
    public function boot()
    {
        $this->loadViewsFrom(realpath(__DIR__ . '/../resources/views'), 'wrapped');
    }
}
"#,
        "/pkg/src/Providers/P.php",
    );
    assert_eq!(
        ns,
        vec![(
            "wrapped".to_string(),
            Some(PathBuf::from("/pkg/src/resources/views"))
        )]
    );
}

#[test]
fn load_views_from_resolves_path_helper_arguments() {
    let ns = discovered_view_namespaces(
        r#"<?php
class P extends ServiceProvider
{
    public function boot()
    {
        $this->loadViewsFrom(resource_path('views/vendor/shop'), 'shop');
    }
}
"#,
        "/pkg/src/P.php",
    );
    assert_eq!(
        ns,
        vec![(
            "shop".to_string(),
            Some(PathBuf::from("/proj/resources/views/vendor/shop"))
        )]
    );
}

#[test]
fn load_views_from_ignores_unsupported_receivers() {
    // Only `$this->loadViewsFrom(...)` is recognised. Static and
    // property-chain receivers register nothing rather than registering a
    // wrong directory — see the note above `LOAD_VIEWS_RE`.
    let ns = discovered_view_namespaces(
        r#"<?php
class P extends ServiceProvider
{
    public function boot()
    {
        static::loadViewsFrom(__DIR__ . '/views', 'nope');
        $this->app->loadViewsFrom(__DIR__ . '/views', 'alsonope');
    }
}
"#,
        "/pkg/src/P.php",
    );
    assert!(ns.is_empty(), "unexpected registrations: {ns:?}");
}

/// Guards the widened first-argument capture against swallowing across calls:
/// three differently-shaped registrations in one provider must each resolve
/// independently.
#[test]
fn multiple_differently_shaped_registrations_resolve_independently() {
    let ns = discovered_view_namespaces(
        r#"<?php
class P extends ServiceProvider
{
    public function boot()
    {
        $this->loadViewsFrom(__DIR__ . '/../resources/views', 'plain');
        $this->loadViewsFrom(realpath(__DIR__ . '/../resources/admin'), 'wrapped');
        $this->loadViewsFrom(resource_path('views/vendor/shop'), 'helper');
    }
}
"#,
        "/pkg/src/Providers/P.php",
    );
    assert_eq!(
        ns,
        vec![
            (
                "plain".to_string(),
                Some(PathBuf::from("/pkg/src/resources/views"))
            ),
            (
                "wrapped".to_string(),
                Some(PathBuf::from("/pkg/src/resources/admin"))
            ),
            (
                "helper".to_string(),
                Some(PathBuf::from("/proj/resources/views/vendor/shop"))
            ),
        ]
    );
}

/// The first-argument capture is non-greedy. A greedy `(.+)` backtracks to the
/// LAST `, 'ns')` on the line, swallowing an intervening registration whole —
/// so two calls sharing a line collapse into one. Line-separated calls cannot
/// catch this, because `.` does not match a newline.
#[test]
fn two_registrations_on_one_line_are_captured_separately() {
    let ns = discovered_view_namespaces(
        r#"<?php
class P extends ServiceProvider
{
    public function boot()
    {
        $this->loadViewsFrom(__DIR__ . '/a', 'one'); $this->loadViewsFrom(__DIR__ . '/b', 'two');
    }
}
"#,
        "/pkg/src/P.php",
    );
    assert_eq!(
        ns,
        vec![
            ("one".to_string(), Some(PathBuf::from("/pkg/src/a"))),
            ("two".to_string(), Some(PathBuf::from("/pkg/src/b"))),
        ]
    );
}

// ─── Pattern-cache sizing and publication ─────────────────────────────────

/// A temp project of `php_files` trivial `.php` files under `app/`, registered
/// with `handle`. Returns the `TempDir` so the caller keeps it alive.
async fn register_temp_project(handle: &SalsaHandle, php_files: usize) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    for i in 0..php_files {
        std::fs::write(app.join(format!("F{i}.php")), "<?php\n").unwrap();
    }
    handle
        .register_project_files(
            dir.path().to_path_buf(),
            vec![PathBuf::from("app/Http/Controllers")],
            vec![dir.path().join("resources/views")],
            None,
            PathBuf::from("routes"),
            // The shared vendor walk the production caller passes in
            // (issue #371) — built from the same root, so the actor
            // registers exactly what it would in production.
            crate::vendor_index::VendorIndex::build(dir.path())
                .files()
                .iter()
                .map(|f| f.path.clone())
                .collect(),
        )
        .await
        .unwrap();
    dir
}

#[tokio::test]
async fn pattern_cache_is_unpublished_until_a_project_is_registered() {
    let handle = SalsaActor::spawn();

    assert!(
        handle.pattern_cache().is_none(),
        "no project walked yet, so there is no correctly-sized table to hand out"
    );
    assert!(
        handle
            .bulk_import_patterns(vec![(
                PathBuf::from("/proj/app/Models/User.php"),
                Arc::new(ParsedPatternsData::default()),
            )])
            .await
            .is_err(),
        "importing before publication must fail closed, not write to an unshared table"
    );

    let _project = register_temp_project(&handle, 4).await;
    assert!(
        handle.pattern_cache().is_some(),
        "registration must publish the table"
    );
}

#[tokio::test]
async fn registration_sizes_the_pattern_cache_for_the_discovered_file_count() {
    // Comfortably past the bootstrap table's real capacity (~1.8k regardless
    // of shard count), so a published table that was NOT resized fails here.
    const FILES: usize = 1_500;
    let handle = SalsaActor::spawn();
    let _project = register_temp_project(&handle, FILES).await;

    let cache = handle.pattern_cache().expect("registration must publish");
    assert!(
        cache.capacity() >= FILES + PATTERN_CACHE_CAPACITY_PADDING,
        "table must be sized for the walk's file count plus padding, got {}",
        cache.capacity()
    );
}

#[tokio::test]
async fn entries_cached_before_registration_survive_the_sizing_swap() {
    let handle = SalsaActor::spawn();

    // An editor can send `didOpen` for an already-open buffer before project
    // registration finishes; that lands in the bootstrap table.
    let early = tempfile::TempDir::new().unwrap();
    let early_file = early.path().join("Early.php");
    std::fs::write(&early_file, "<?php\nclass Early {}\n").unwrap();
    handle.get_patterns(early_file.clone()).await.unwrap();

    let _project = register_temp_project(&handle, 1_500).await;

    let cache = handle.pattern_cache().expect("registration must publish");
    assert!(
        cache.contains_key(&early_file),
        "a pre-registration entry must be migrated into the resized table, not dropped"
    );
}

#[tokio::test]
async fn re_registration_leaves_the_published_table_live() {
    // The whole point of publishing once: a later registration must not leave
    // the actor writing to a table that handle holders can no longer see. A
    // mid-session project-root change re-registers with no flight guard, so
    // this is reachable in production, not just in theory.
    let handle = SalsaActor::spawn();
    let _first = register_temp_project(&handle, 1_500).await;
    let published = handle.pattern_cache().expect("registration must publish");

    // Second registration, deliberately larger than the first.
    let _second = register_temp_project(&handle, 3_000).await;

    // Drive an actor-side write through `handle_get_patterns`, then look for
    // it through the Arc taken BEFORE the re-registration.
    let late = tempfile::TempDir::new().unwrap();
    let late_file = late.path().join("Late.php");
    std::fs::write(&late_file, "<?php\nclass Late {}\n").unwrap();
    handle.get_patterns(late_file.clone()).await.unwrap();

    assert!(
        published.contains_key(&late_file),
        "the actor must still be writing to the table it published, not a newer one"
    );
    assert!(
        Arc::ptr_eq(
            &published,
            &handle.pattern_cache().expect("still published")
        ),
        "re-registration must not swap the published table"
    );
}

// ─── Which locale answers translation autocomplete (issue #340) ─────────
//
// Completion previews values from exactly one locale, so `completion_locale`
// decides what every key's preview says. It used to be the alphabetically
// first directory, which previews German on a `de`/`en`/`fr` project that
// renders `en`. These drive the chain directly; the end-to-end proof that
// `completion_keys` actually consults it lives in
// `tests/translation_salsa_cache.rs`.

/// A project root whose `config/app.php` returns exactly `entries`.
fn root_with_app_config(entries: &str) -> (TempDir, PathBuf) {
    project_with_files(&[(
        "config/app.php",
        &format!("<?php\n\nreturn [\n{entries}\n];\n"),
    )])
}

fn locales(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn configured_locale_beats_the_alphabetically_first() {
    let (_tmp, root) = root_with_app_config("    'locale' => 'en',");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("en"),
        "the app renders `en`; previewing `de` because it sorts first is issue #340"
    );
}

#[test]
fn a_missing_app_locale_falls_through_to_fallback_locale() {
    let (_tmp, root) = root_with_app_config("    'fallback_locale' => 'fr',");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("fr"),
        "no `locale` key at all must consult `fallback_locale`, not give up on the config"
    );
}

#[test]
fn a_configured_locale_with_no_directory_falls_through_to_fallback_locale() {
    let (_tmp, root) =
        root_with_app_config("    'locale' => 'es',\n    'fallback_locale' => 'fr',");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("fr"),
        "a locale the project does not translate has nothing to preview — but that \
         is a reason to try `fallback_locale`, not to jump straight to alphabetical"
    );
}

#[test]
fn a_non_literal_app_locale_is_unresolved() {
    let (_tmp, root) =
        root_with_app_config("    'locale' => APP_DEFAULT_LOCALE,\n    'fallback_locale' => 'fr',");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("fr"),
        "a constant reference is not statically readable; it must never be matched \
         as raw text, and must not panic"
    );
}

#[test]
fn a_concatenated_app_locale_is_unresolved() {
    let (_tmp, root) = root_with_app_config("    'locale' => 'e' . 'n',");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["fr", "de", "en"]))
            .as_deref(),
        Some("de"),
        "a concatenation is not a literal — fall all the way through to alphabetical \
         rather than evaluating PHP"
    );
}

#[test]
fn neither_key_resolving_falls_back_to_alphabetically_first() {
    let (_tmp, root) = root_with_app_config("    'name' => 'Laravel',");

    // Deliberately not in sorted order: a fallback that took the list's first
    // entry rather than its minimum would answer `fr` here, which is precisely
    // the filesystem-order bug the sort was introduced to kill.
    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["fr", "de", "en"]))
            .as_deref(),
        Some("de"),
        "the pre-#340 guarantee: deterministic on every filesystem"
    );
}

#[test]
fn a_project_with_no_config_directory_falls_back_to_alphabetically_first() {
    let (dir, root) = project_with_files(&[]);
    let _ = dir;

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["en", "de"]))
            .as_deref(),
        Some("de"),
        "an unreadable config must degrade to the old behaviour, not to no completions"
    );
}

#[test]
fn the_alphabetical_fallback_sees_every_candidate_the_chain_tried() {
    let (_tmp, root) =
        root_with_app_config("    'locale' => 'de',\n    'fallback_locale' => 'en',");

    // Both configured locales exist as directories, so the chain matches at
    // step 1 — but if a failing step ever removed what it tried from the list,
    // this project's fallback would answer `fr` instead of `de`.
    //
    // One cache across both calls, deliberately: the config read is memoized
    // per instance since #349, so a second call answers from cached text. A
    // cache that mutated or consumed what it stored would show up right here.
    let mut cache = TranslationCache::default();
    assert_eq!(
        cache
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("de"),
    );
    assert_eq!(
        cache
            .completion_locale(&root, &locales(&["gr", "fr"]))
            .as_deref(),
        Some("fr"),
        "neither configured locale exists here, so the fallback must still see the \
         whole candidate list"
    );
}

#[test]
fn an_env_wrapped_app_locale_uses_its_default_argument() {
    let (_tmp, root) = root_with_app_config("    'locale' => env('APP_LOCALE', 'en'),");

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en"]))
            .as_deref(),
        Some("en"),
        "Laravel ships `app.locale` env-wrapped; matching the raw `env(...)` text \
         against directory names would silently reproduce issue #340"
    );
}

#[test]
fn an_env_wrapped_fallback_locale_uses_its_default_argument() {
    let (_tmp, root) = root_with_app_config(
        "    'locale' => env('APP_LOCALE'),\n    'fallback_locale' => env('APP_FALLBACK_LOCALE', 'fr'),",
    );

    assert_eq!(
        TranslationCache::default()
            .completion_locale(&root, &locales(&["de", "en", "fr"]))
            .as_deref(),
        Some("fr"),
        "the env unwrap applies to both lookups — `fallback_locale` is env-wrapped \
         in a stock Laravel skeleton too"
    );
}

#[test]
fn no_candidates_resolves_to_no_locale() {
    let (_tmp, root) = root_with_app_config("    'locale' => 'en',");

    assert_eq!(
        TranslationCache::default().completion_locale(&root, &[]),
        None,
        "a project with no locale directories has nothing to preview, whatever its \
         config says"
    );
}

#[test]
fn config_string_literal_reads_both_quote_styles() {
    assert_eq!(config_string_literal("'en'").as_deref(), Some("en"));
    assert_eq!(config_string_literal("\"en\"").as_deref(), Some("en"));
    assert_eq!(
        config_string_literal("  'en'  ").as_deref(),
        Some("en"),
        "surrounding whitespace is not part of the literal"
    );
}

#[test]
fn config_string_literal_rejects_unresolvable_forms() {
    for raw in [
        "env('APP_LOCALE')",
        "env('APP_LOCALE', APP_DEFAULT)",
        "APP_LOCALE",
        "'en",
        "en'",
        "'",
        "",
        "config('app.locale')",
    ] {
        assert_eq!(
            config_string_literal(raw),
            None,
            "`{raw}` denotes no static string and must not be matched as raw text"
        );
    }
}

#[test]
fn config_string_literal_reads_an_empty_literal() {
    assert_eq!(
        config_string_literal("''").as_deref(),
        Some(""),
        "an empty literal resolves to the empty string — it simply matches no \
         directory name"
    );
}

// ─── Blade backing-class resolution, memoized (issue #339, item 7) ───────

/// A render index holding `(view name, rendering file)` pairs.
fn render_index(db: &LaravelDatabase, entries: &[(&str, &str)]) -> RenderIndex {
    RenderIndex::new(
        db,
        1,
        entries
            .iter()
            .map(|(view, path)| (view.to_string(), PathBuf::from(path)))
            .collect(),
    )
}

#[test]
fn render_source_files_returns_every_contributor_sorted() {
    let db = LaravelDatabase::default();
    let index = render_index(
        &db,
        &[
            ("users.show", "/proj/zeta/Controller.php"),
            ("users.show", "/proj/alpha/Page.php"),
            ("other.view", "/proj/Other.php"),
        ],
    );

    assert_eq!(
        render_source_files(&db, index, "users.show".to_string()),
        vec![
            PathBuf::from("/proj/alpha/Page.php"),
            PathBuf::from("/proj/zeta/Controller.php"),
        ],
        "contributors come back in lexicographic path order, not index order",
    );
    assert!(render_source_files(&db, index, "missing.view".to_string()).is_empty());
}

#[test]
fn render_source_files_is_memoized_until_the_index_changes() {
    let mut db = LaravelDatabase::default();
    let index = render_index(&db, &[("users.show", "/proj/UserController.php")]);

    let before = db
        .query_run_counts()
        .render_source_files
        .load(Ordering::Relaxed);
    let first = render_source_files(&db, index, "users.show".to_string());
    let second = render_source_files(&db, index, "users.show".to_string());
    assert_eq!(first, second);
    assert_eq!(
        db.query_run_counts()
            .render_source_files
            .load(Ordering::Relaxed)
            - before,
        1,
        "a second lookup over an unchanged index must be served from the memo",
    );

    index.set_entries(&mut db).to(vec![
        (
            "users.show".to_string(),
            PathBuf::from("/proj/UserController.php"),
        ),
        (
            "users.show".to_string(),
            PathBuf::from("/proj/Filament/UserPage.php"),
        ),
    ]);
    let third = render_source_files(&db, index, "users.show".to_string());
    assert_eq!(
        third,
        vec![
            PathBuf::from("/proj/Filament/UserPage.php"),
            PathBuf::from("/proj/UserController.php"),
        ],
        "a changed index must recompute, not serve the old contributor list",
    );
    assert_eq!(
        db.query_run_counts()
            .render_source_files
            .load(Ordering::Relaxed)
            - before,
        2,
    );
}

#[test]
fn blade_backing_class_files_puts_render_sites_before_the_livewire_convention() {
    let db = LaravelDatabase::default();
    let index = render_index(&db, &[("livewire.counter", "/proj/app/Filament/Page.php")]);

    let files = blade_backing_class_files(
        &db,
        index,
        Some("livewire.counter".to_string()),
        vec![PathBuf::from("/proj/app/Livewire/Counter.php")],
    );

    assert_eq!(
        files,
        vec![
            PathBuf::from("/proj/app/Filament/Page.php"),
            PathBuf::from("/proj/app/Livewire/Counter.php"),
        ],
        "the direct render site outranks the conventionally-resolved class",
    );
}

#[test]
fn blade_backing_class_files_drops_templates_and_duplicates() {
    let db = LaravelDatabase::default();
    let index = render_index(
        &db,
        &[
            ("livewire.counter", "/proj/app/Livewire/Counter.php"),
            ("livewire.counter", "/proj/resources/views/x.blade.php"),
            ("livewire.counter", "/proj/resources/views/x.blade"),
        ],
    );

    let files = blade_backing_class_files(
        &db,
        index,
        Some("livewire.counter".to_string()),
        vec![
            // Already contributed by the render index — must not appear twice.
            PathBuf::from("/proj/app/Livewire/Counter.php"),
            // A v4 MFC component directory: no `.php` extension.
            PathBuf::from("/proj/resources/views/components/counter"),
        ],
    );

    assert_eq!(
        files,
        vec![PathBuf::from("/proj/app/Livewire/Counter.php")],
        "only plain `.php` paths survive, and each appears once",
    );
}

#[test]
fn blade_backing_class_files_without_a_view_name_uses_only_the_livewire_paths() {
    let db = LaravelDatabase::default();
    let index = render_index(&db, &[("users.show", "/proj/UserController.php")]);

    assert_eq!(
        blade_backing_class_files(
            &db,
            index,
            None,
            vec![PathBuf::from("/proj/app/Livewire/Counter.php")],
        ),
        vec![PathBuf::from("/proj/app/Livewire/Counter.php")],
        "a template outside every view root contributes no render sites",
    );
}

#[test]
fn blade_backing_class_sources_reads_each_file_and_appends_an_inline_component() {
    let db = LaravelDatabase::default();
    let class = SourceFile::new(
        &db,
        PathBuf::from("/proj/app/Livewire/Counter.php"),
        0,
        "<?php class Counter extends Component { public $count = 0; }".to_string(),
    );
    let inline = SourceFile::new(
        &db,
        PathBuf::from("/proj/resources/views/livewire/counter.blade.php"),
        0,
        "<?php new class extends Component { public $tally = 0; }; ?>\n<div></div>".to_string(),
    );

    let sources = blade_backing_class_sources(&db, vec![class], Some(inline));

    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources[0].0,
        PathBuf::from("/proj/app/Livewire/Counter.php")
    );
    assert!(sources[0].1.contains("public $count"));
    assert_eq!(
        sources[1].0,
        PathBuf::from("/proj/resources/views/livewire/counter.blade.php"),
        "the template itself is appended AFTER the standalone classes",
    );
}

#[test]
fn blade_backing_class_sources_skips_a_template_with_no_inline_component() {
    let db = LaravelDatabase::default();
    let plain = SourceFile::new(
        &db,
        PathBuf::from("/proj/resources/views/partial.blade.php"),
        0,
        "<div>{{ $name }}</div>".to_string(),
    );

    assert!(
        blade_backing_class_sources(&db, Vec::new(), Some(plain)).is_empty(),
        "a template with neither an inline class nor a Volt signature contributes nothing",
    );
}

#[test]
fn blade_backing_class_sources_invalidates_when_a_backing_class_is_edited() {
    let mut db = LaravelDatabase::default();
    let class = SourceFile::new(
        &db,
        PathBuf::from("/proj/app/Livewire/Counter.php"),
        0,
        "<?php class Counter { public $count = 0; }".to_string(),
    );

    let before = db
        .query_run_counts()
        .blade_backing_class_sources
        .load(Ordering::Relaxed);
    let first = blade_backing_class_sources(&db, vec![class], None);
    assert!(first[0].1.contains("public $count"));

    // Same inputs: served from the memo.
    let _ = blade_backing_class_sources(&db, vec![class], None);
    assert_eq!(
        db.query_run_counts()
            .blade_backing_class_sources
            .load(Ordering::Relaxed)
            - before,
        1,
        "an unchanged backing class must not be re-read",
    );

    // The BACKING CLASS changes — not the blade file. The memo must drop.
    class
        .set_text(&mut db)
        .to("<?php class Counter { public $tally = 0; }".to_string());
    let after = blade_backing_class_sources(&db, vec![class], None);
    assert!(
        after[0].1.contains("public $tally"),
        "editing the backing class must invalidate the cached source, got {:?}",
        after[0].1,
    );
    assert_eq!(
        db.query_run_counts()
            .blade_backing_class_sources
            .load(Ordering::Relaxed)
            - before,
        2,
    );
}

#[test]
fn is_plain_php_path_separates_classes_from_templates() {
    assert!(is_plain_php_path(Path::new(
        "/proj/app/Livewire/Counter.php"
    )));
    assert!(!is_plain_php_path(Path::new(
        "/proj/resources/views/counter.blade.php"
    )));
    assert!(!is_plain_php_path(Path::new(
        "/proj/resources/views/counter"
    )));
    assert!(!is_plain_php_path(Path::new("/proj/composer.json")));
}

// ─── External-PHP loader containment (#364) ────────────────────────────
//
// `ensure_external_php_source_loaded` is a read primitive whose result is
// emitted as a goto-definition target, so it carries its own containment
// guard rather than trusting each caller to have sanitised the path. The
// criteria for #364 ask for assertions on the actor's own `files` and
// `external_php_text` maps — "returned `None`" alone does not prove the
// rejected path left no cached entry behind for the ~8 other
// `self.files.get(path)` sites in this module to serve. No `SalsaHandle`
// message exposes those maps, so these tests build a `SalsaActor` directly
// (see `SalsaActor::new`) and call the loader as `&mut self`.

/// An actor with no thread behind it. The receiver is live but never polled —
/// these tests call `&mut self` methods directly instead of sending requests.
fn loader_test_actor(config_root: Option<PathBuf>) -> SalsaActor {
    let (_tx, rx) = mpsc::channel(1);
    let mut actor = SalsaActor::new(rx, Arc::new(OnceLock::new()), Arc::new(OnceLock::new()));
    actor.config_root = config_root;
    actor
}

/// A project root and a sibling directory outside it, under one tempdir.
/// Returned alongside the `TempDir` so the caller keeps it alive.
fn root_and_outside() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(root.join("app/Livewire")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    (dir, root, outside)
}

/// Precondition for every rejection test below: assert the path really does
/// resolve outside the root, so a mis-built fixture can't pass vacuously.
fn assert_outside_root(path: &Path, root: &Path) {
    let real = path
        .canonicalize()
        .expect("the fixture target must exist on disk");
    let real_root = root.canonicalize().expect("the project root must exist");
    assert!(
        !real.starts_with(&real_root),
        "fixture is not actually out of root: {real:?} is under {real_root:?}"
    );
}

#[test]
fn loader_refuses_an_out_of_root_candidate_and_caches_nothing() {
    let (_dir, root, outside) = root_and_outside();
    let escapee = outside.join("Secret.php");
    std::fs::write(&escapee, "<?php\nclass Secret { public $token = 'x'; }\n").unwrap();
    assert_outside_root(&escapee, &root);

    let mut actor = loader_test_actor(Some(root.clone()));

    assert!(
        actor.ensure_external_php_source_loaded(&escapee).is_none(),
        "an out-of-root candidate must not load, even though it reads fine",
    );
    assert!(
        !actor.files.contains_key(&escapee),
        "a rejected path must not be left as a Salsa input for other sites to read",
    );
    assert!(
        !actor.external_php_text.contains_key(&escapee),
        "a rejected path must not be recorded as loaded",
    );
}

#[test]
fn loader_refuses_a_candidate_when_the_root_is_unknown() {
    let (_dir, root, _outside) = root_and_outside();
    let class = root.join("app/Livewire/Counter.php");
    let source = "<?php\nclass Counter { public $count = 0; }\n";
    std::fs::write(&class, source).unwrap();

    // Root unknown: the short-circuit fires before anything is touched.
    let mut actor = loader_test_actor(None);
    assert!(
        actor.ensure_external_php_source_loaded(&class).is_none(),
        "with no project root there is nothing to contain against — fail closed",
    );
    assert!(actor.files.is_empty(), "no Salsa input may be created");
    assert!(
        actor.external_php_text.is_empty(),
        "nothing may be recorded as loaded",
    );

    // The same path, same actor state, with the root known: it loads. That is
    // what proves the `None` above came from the short-circuit and not from a
    // coincidental read failure.
    actor.config_root = Some(root);
    let file = actor
        .ensure_external_php_source_loaded(&class)
        .expect("the very same path loads once the root is known");
    assert_eq!(*file.text(&actor.db), source);
}

#[cfg(unix)]
#[test]
fn loader_refuses_an_under_root_symlink_that_escapes() {
    let (_dir, root, outside) = root_and_outside();

    // Distinguishable content, so a rejection can't be confused with an empty
    // or unreadable target.
    let secret_src = "<?php\nclass Counter { public $secret = 'escaped'; }\n";
    let target = outside.join("Secret.php");
    std::fs::write(&target, secret_src).unwrap();

    let link = root.join("app/Livewire/Counter.php");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_outside_root(&link, &root);

    let mut actor = loader_test_actor(Some(root.clone()));
    assert!(
        actor.ensure_external_php_source_loaded(&link).is_none(),
        "a path lexically under the root that resolves outside it must be refused",
    );
    assert!(!actor.files.contains_key(&link));
    assert!(!actor.external_php_text.contains_key(&link));

    // The contrast: the SAME bytes at a genuine in-root path load normally, so
    // the rejection tracks containment, not a read that happened to fail.
    let genuine = root.join("app/Livewire/Real.php");
    std::fs::write(&genuine, secret_src).unwrap();
    let file = actor
        .ensure_external_php_source_loaded(&genuine)
        .expect("an in-root file with identical content loads");
    assert_eq!(*file.text(&actor.db), secret_src);
}

#[cfg(unix)]
#[test]
fn retargeting_a_symlink_out_of_root_cannot_disturb_the_cached_load() {
    let (_dir, root, outside) = root_and_outside();

    let good_src = "<?php\nclass Counter { public $from = 'in-root'; }\n";
    let good = root.join("app/Livewire/Real.php");
    std::fs::write(&good, good_src).unwrap();

    let bad_src = "<?php\nclass Counter { public $from = 'escaped'; }\n";
    let bad = outside.join("Secret.php");
    std::fs::write(&bad, bad_src).unwrap();

    let link = root.join("app/Livewire/Counter.php");
    std::os::unix::fs::symlink(&good, &link).unwrap();

    let mut actor = loader_test_actor(Some(root.clone()));
    let file = actor
        .ensure_external_php_source_loaded(&link)
        .expect("an in-root symlink to an in-root target loads");
    assert_eq!(*file.text(&actor.db), good_src);
    let recorded = *actor
        .external_php_text
        .get(&link)
        .expect("the load is recorded");

    // Swap the link out from under the cached entry.
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&bad, &link).unwrap();
    assert_outside_root(&link, &root);

    assert!(
        actor.ensure_external_php_source_loaded(&link).is_none(),
        "the retargeted link now escapes the root and must be refused",
    );
    assert_eq!(
        *file.text(&actor.db),
        good_src,
        "the previously-cached text must survive the refused reload",
    );
    assert_eq!(
        actor.external_php_text.get(&link),
        Some(&recorded),
        "the recorded mtime must survive the refused reload",
    );
}

#[test]
fn a_client_pushed_out_of_root_path_is_refused_even_when_cached() {
    let (_dir, root, outside) = root_and_outside();
    let escapee = outside.join("Secret.php");
    std::fs::write(&escapee, "<?php\nclass Secret {}\n").unwrap();
    assert_outside_root(&escapee, &root);

    let mut actor = loader_test_actor(Some(root.clone()));

    // Install it the way a client push does — this is the branch that returns
    // without touching disk, and whose return value becomes a goto target.
    let buffer = "<?php\nclass Secret { public $unsaved = true; }\n";
    actor
        .ensure_blade_source_registered(&escapee, Some(buffer.to_string()))
        .expect("the push installs a Salsa input");
    assert_eq!(
        actor.external_php_text.get(&escapee),
        Some(&ExternalPhpText::PushedByClient),
        "precondition: the ownership fast path is what answers next",
    );

    assert!(
        actor.ensure_external_php_source_loaded(&escapee).is_none(),
        "client ownership is not a containment exemption — the path is emitted",
    );
}

#[test]
fn a_client_pushed_in_root_buffer_still_answers_while_its_file_is_absent() {
    let (_dir, root, _outside) = root_and_outside();
    // Never written to disk: this is the #361 case the emit-safe guard has to
    // keep admitting.
    let class = root.join("app/Livewire/Counter.php");
    assert!(!class.exists(), "precondition: the file is absent on disk");

    let mut actor = loader_test_actor(Some(root.clone()));
    let buffer = "<?php\nclass Counter { public function unsaved() {} }\n";
    actor
        .ensure_blade_source_registered(&class, Some(buffer.to_string()))
        .expect("the push installs a Salsa input");

    let file = actor
        .ensure_external_php_source_loaded(&class)
        .expect("a genuinely-absent in-root buffer must still resolve");
    assert_eq!(*file.text(&actor.db), buffer);
}

// ─── Module-gated containment (#364, review round 1) ───────────────────
//
// Gating this read against the project root alone dropped every backing
// class inside a module symlinked in from a composer path repository — the
// layout `config::expand_module_dirs` admits on purpose and
// `livewire_namespaces::contained_class_path` gates against the owning
// module precisely to keep working. The loader now makes the same choice.
// These three tests pin the widening AND its limits: a module's own subtree
// is reachable, everything else is still refused.

/// A project with `Modules/Blog` symlinked to a package directory that lives
/// outside the project root. Returns the tempdir, the root, the module dir,
/// and the external package dir.
#[cfg(unix)]
fn symlinked_module_project() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("project");
    let package = dir.path().join("packages/blog");
    std::fs::create_dir_all(root.join("Modules")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::create_dir_all(package.join("src/Livewire")).unwrap();

    let module = root.join("Modules/Blog");
    std::os::unix::fs::symlink(&package, &module).unwrap();
    (dir, root, module, package)
}

#[cfg(unix)]
#[test]
fn a_backing_class_inside_a_symlinked_module_still_loads() {
    let (_dir, root, module, package) = symlinked_module_project();

    let source = "<?php\nclass Post { public $title = 'x'; }\n";
    std::fs::write(package.join("src/Livewire/Post.php"), source).unwrap();
    let class = module.join("src/Livewire/Post.php");

    // Precondition: this really is out of root — the case a root-only gate
    // refused, and the reason this test exists.
    let real = class.canonicalize().unwrap();
    assert!(
        !real.starts_with(root.canonicalize().unwrap()),
        "fixture must resolve outside the project root, got {real:?}",
    );

    let mut actor = loader_test_actor(Some(root));
    actor.module_dirs = vec![module];

    let file = actor
        .ensure_external_php_source_loaded(&class)
        .expect("a class inside a symlinked path-repository module must load");
    assert_eq!(*file.text(&actor.db), source);
}

#[cfg(unix)]
#[test]
fn a_path_escaping_its_own_module_is_still_refused() {
    let (dir, root, module, _package) = symlinked_module_project();

    // Neither in the module nor in the project: the escape the widened gate
    // must still catch.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let secret = elsewhere.join("Secret.php");
    std::fs::write(&secret, "<?php\nclass Secret { public $token = 'x'; }\n").unwrap();

    // A path that LOOKS like it belongs to the module, so `owning_module`
    // elects the module as its gate — and the gate then refuses it.
    let link = module.join("src/Livewire/Escape.php");
    std::os::unix::fs::symlink(&secret, &link).unwrap();

    let mut actor = loader_test_actor(Some(root));
    actor.module_dirs = vec![module];

    assert!(
        actor.ensure_external_php_source_loaded(&link).is_none(),
        "electing a module gate must not become a way out of it",
    );
    assert!(!actor.files.contains_key(&link));
    assert!(!actor.external_php_text.contains_key(&link));
}

#[cfg(unix)]
#[test]
fn a_module_path_reaching_back_into_the_app_is_refused() {
    let (_dir, root, module, _package) = symlinked_module_project();

    // Inside the project root, but outside the module that owns the candidate.
    let app_file = root.join("app/Shared.php");
    std::fs::write(&app_file, "<?php\nclass Shared {}\n").unwrap();

    let link = module.join("src/Livewire/Shared.php");
    std::os::unix::fs::symlink(&app_file, &link).unwrap();

    let mut actor = loader_test_actor(Some(root));
    actor.module_dirs = vec![module];

    // Deliberately STRICTER than a root-only gate, matching the rule
    // `livewire_namespaces::contained_class_path` already applies to the
    // registrations that mint these paths: a module reaching into bare `app/`
    // is inside the root and still outside its own module.
    assert!(
        actor.ensure_external_php_source_loaded(&link).is_none(),
        "a module candidate must resolve inside its own module, not merely in-root",
    );
}

#[test]
fn a_traversing_path_cannot_elect_a_module_as_its_gate() {
    // A REAL module directory, not a symlinked one: an interior `..` after a
    // symlink is resolved by the OS against the link's target, which would
    // send this path nowhere and let the test pass with no guard at all. On a
    // real directory the escape lands on a real, readable file, so the
    // assertion can only hold because the gate refused it.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("project");
    let module = root.join("Modules/Blog");
    std::fs::create_dir_all(module.join("src/Livewire")).unwrap();

    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("Secret.php");
    std::fs::write(&secret, "<?php\nclass Secret { public $token = 'x'; }\n").unwrap();

    // Textually prefixed by the module dir, and it resolves to a real file.
    let traversing = module.join("../../../outside/Secret.php");
    assert!(
        traversing.canonicalize().is_ok(),
        "precondition: the escape target must be readable, or the test is vacuous",
    );

    let mut actor = loader_test_actor(Some(root));
    actor.module_dirs = vec![module];

    // `owning_module` collapses `..` before its prefix test, so this never
    // elects the module gate — and the root gate it falls back to refuses it.
    assert!(
        actor
            .ensure_external_php_source_loaded(&traversing)
            .is_none(),
        "an interior-`..` escape must not borrow the module's gate",
    );
}

#[test]
fn without_modules_the_gate_is_the_project_root() {
    // The common case: no `modules.paths`, so `owning_module` never matches
    // and the gate is exactly the root. Pins that the widening costs nothing
    // for a project without modules.
    let (_dir, root, outside) = root_and_outside();
    let escapee = outside.join("Secret.php");
    std::fs::write(&escapee, "<?php\nclass Secret {}\n").unwrap();

    let mut actor = loader_test_actor(Some(root.clone()));
    assert!(actor.module_dirs.is_empty(), "precondition: no modules");
    assert!(actor.ensure_external_php_source_loaded(&escapee).is_none());

    let inside = root.join("app/Livewire/Counter.php");
    let source = "<?php\nclass Counter {}\n";
    std::fs::write(&inside, source).unwrap();
    let file = actor
        .ensure_external_php_source_loaded(&inside)
        .expect("an in-root class still loads with no modules configured");
    assert_eq!(*file.text(&actor.db), source);
}

// ─── Ownership release, at the actor's own state (#365) ────────────────
//
// The release does two things — hands the `PushedByClient` stamp back, and
// drops the text that stamp was protecting — and the second MASKS the first
// from every behavioural test. With the input gone, the loader's
// "marked as pushed, yet the input is gone" arm falls through to the disk
// read anyway, so a release that dropped the text and kept the stamp answers
// identically through every handle-driven assertion. It is not identical:
// a stamp left behind re-arms the original defect the moment anything
// re-registers `files[path]` without clearing it — `ensure_file_registered`
// does exactly that on the next hover of the closed file, and the loader then
// treats the path as client-owned again and never re-reads disk.
//
// So the stamp gets its own assertion, against the actor's own map.

#[test]
fn releasing_the_last_buffer_hands_the_stamp_back_and_drops_the_text() {
    let (_dir, root, _outside) = root_and_outside();
    let class = root.join("app/Livewire/Counter.php");
    std::fs::write(&class, "<?php\nclass Counter {}\n").unwrap();

    let mut actor = loader_test_actor(Some(root));

    // The acquire-then-push pair `did_open` performs, in that order.
    actor.acquire_external_php_ownership(&class);
    actor.handle_update_file(
        class.clone(),
        1,
        "<?php\nclass Counter { public $unsaved = true; }\n".to_string(),
    );
    assert_eq!(
        actor.external_php_text.get(&class),
        Some(&ExternalPhpText::PushedByClient),
        "precondition: the push claims ownership",
    );
    assert!(actor.files.contains_key(&class));

    actor.release_external_php_ownership(&class);

    assert!(
        !actor.external_php_text.contains_key(&class),
        "the stamp must be handed back, not merely rendered inert by the \
         text going away — a stamp left behind re-arms the defect as soon as \
         anything re-registers this path",
    );
    assert!(
        !actor.external_php_open_buffers.contains_key(&class),
        "the last buffer's count must go with it",
    );
    assert!(
        !actor.files.contains_key(&class),
        "and the text it installed must go, so every reader re-derives",
    );
}

#[test]
fn a_release_below_the_last_buffer_keeps_both_the_stamp_and_the_text() {
    let (_dir, root, _outside) = root_and_outside();
    let class = root.join("app/Livewire/Counter.php");
    std::fs::write(&class, "<?php\nclass Counter {}\n").unwrap();

    let mut actor = loader_test_actor(Some(root));

    // Two buffers on one path — a split view, or a reopen that overtook its
    // own close.
    actor.acquire_external_php_ownership(&class);
    actor.acquire_external_php_ownership(&class);
    let buffer = "<?php\nclass Counter { public $unsaved = true; }\n";
    actor.handle_update_file(class.clone(), 1, buffer.to_string());

    actor.release_external_php_ownership(&class);

    assert_eq!(
        actor.external_php_text.get(&class),
        Some(&ExternalPhpText::PushedByClient),
        "one buffer is still open, so the path is still owned",
    );
    let file = actor
        .files
        .get(&class)
        .copied()
        .expect("the surviving buffer's text must not be dropped");
    assert_eq!(*file.text(&actor.db), buffer);
    assert_eq!(actor.external_php_open_buffers.get(&class), Some(&1));
}
