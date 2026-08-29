use super::*;
use tempfile::TempDir;

/// Turns `worktree_root` into a linked worktree of `main_root`, reproducing
/// the on-disk layout `git worktree add` produces: a `.git` *file* in the
/// worktree pointing at an admin dir under the main repo's
/// `.git/worktrees/<name>/`, which names the shared `.git` dir via a
/// `commondir` file (test helper for `is_same_git_repo`).
fn link_worktree(main_root: &Path, worktree_root: &Path, name: &str) {
    let admin_dir = main_root.join(".git").join("worktrees").join(name);
    fs::create_dir_all(&admin_dir).unwrap();
    fs::write(admin_dir.join("commondir"), "../..\n").unwrap();

    fs::create_dir_all(worktree_root).unwrap();
    fs::write(
        worktree_root.join(".git"),
        format!("gitdir: {}\n", admin_dir.display()),
    )
    .unwrap();
}

#[test]
fn same_git_repo_true_for_main_and_linked_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();

    let worktree_root = tmp.path().join("worktree");
    link_worktree(&main_root, &worktree_root, "feature-branch");

    assert!(is_same_git_repo(&main_root, &worktree_root));
    assert!(
        is_same_git_repo(&worktree_root, &main_root),
        "the check must be symmetric"
    );
}

#[test]
fn same_git_repo_false_for_two_unrelated_repos() {
    let tmp = TempDir::new().unwrap();
    let repo_a = tmp.path().join("repo-a");
    let repo_b = tmp.path().join("repo-b");
    fs::create_dir_all(repo_a.join(".git")).unwrap();
    fs::create_dir_all(repo_b.join(".git")).unwrap();

    assert!(!is_same_git_repo(&repo_a, &repo_b));
}

#[test]
fn same_git_repo_false_when_neither_is_a_git_repo() {
    // No git plumbing at all on either side (e.g. a plain nested Laravel
    // project that isn't version-controlled) — must not be treated as a
    // worktree pair, and existing "nested project" discovery is unaffected.
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();

    assert!(!is_same_git_repo(&a, &b));
}

#[test]
fn same_git_repo_false_for_distinct_repo_nested_underneath() {
    // A nested Laravel project that happens to be its own git repository
    // (e.g. a submodule) must NOT be mistaken for a worktree of the outer
    // project just because it's physically nested underneath it — only a
    // shared git common dir means "same project".
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path().join("outer");
    fs::create_dir_all(outer.join(".git")).unwrap();

    let inner = outer.join("packages").join("billing");
    fs::create_dir_all(inner.join(".git")).unwrap();

    assert!(!is_same_git_repo(&outer, &inner));
}

#[test]
fn is_main_worktree_true_for_the_main_checkout() {
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();

    assert!(is_main_worktree(&main_root));
}

#[test]
fn is_main_worktree_false_for_a_linked_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();

    let worktree_root = tmp.path().join("worktree");
    link_worktree(&main_root, &worktree_root, "feature-branch");

    assert!(!is_main_worktree(&worktree_root));
}

#[test]
fn worktree_fallback_prefers_local_file_when_present() {
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();
    fs::write(main_root.join(".env"), "MAIN=1\n").unwrap();

    let worktree_root = tmp.path().join("worktree");
    link_worktree(&main_root, &worktree_root, "feature-branch");
    fs::write(worktree_root.join(".env"), "LOCAL=1\n").unwrap();

    assert_eq!(
        resolve_worktree_fallback(&worktree_root, ".env"),
        worktree_root.join(".env"),
        "a file that exists locally must never be shadowed by the main worktree's copy"
    );
}

#[test]
fn worktree_fallback_reaches_main_worktree_when_local_file_is_gitignored_absent() {
    // The motivating case: `.env` is gitignored, so `git worktree add` never
    // copies it — a fresh worktree has none, even though the project does.
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();
    fs::write(main_root.join(".env"), "DB_HOST=mysql\n").unwrap();

    let worktree_root = main_root
        .join(".claude")
        .join("worktrees")
        .join("gifted-heisenberg-3c0899");
    link_worktree(&main_root, &worktree_root, "gifted-heisenberg-3c0899");
    // No .env written under worktree_root — exactly the gitignored-absence case.

    // The resolved main root comes back canonicalized (derived from
    // `git_common_dir`, which canonicalizes to prove repo identity) — on
    // macOS that's `/private/var/...`, not the `/var/...` symlink `TempDir`
    // hands back, so compare against the canonical form.
    assert_eq!(
        resolve_worktree_fallback(&worktree_root, ".env"),
        main_root.canonicalize().unwrap().join(".env"),
        "a file missing locally must fall back to the main worktree's copy"
    );
}

#[test]
fn worktree_fallback_returns_local_path_when_absent_everywhere() {
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();
    // No .env anywhere, main root included.

    let worktree_root = tmp.path().join("worktree");
    link_worktree(&main_root, &worktree_root, "feature-branch");

    assert_eq!(
        resolve_worktree_fallback(&worktree_root, ".env"),
        worktree_root.join(".env"),
        "with no copy anywhere, callers must see the same 'missing' local path as before"
    );
}

#[test]
fn worktree_fallback_is_a_noop_outside_any_worktree() {
    // A plain checkout (no linked worktree involved) with a missing file
    // must behave exactly as a bare `root.join(relative)` always did — no
    // git plumbing means no fallback candidate to try.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    fs::create_dir_all(root.join(".git")).unwrap();

    assert_eq!(resolve_worktree_fallback(&root, ".env"), root.join(".env"));
}

#[test]
fn worktree_fallback_is_a_noop_for_the_main_worktree_itself() {
    // Calling the resolver FROM the main worktree (not a linked one) must
    // not try to fall back to itself — `git_main_worktree_root` returns
    // `None` when `path` already IS the main root.
    let tmp = TempDir::new().unwrap();
    let main_root = tmp.path().join("project");
    fs::create_dir_all(main_root.join(".git")).unwrap();

    let worktree_root = tmp.path().join("worktree");
    link_worktree(&main_root, &worktree_root, "feature-branch");

    // No .env anywhere; querying the MAIN root (not the worktree) for a
    // missing file must just return the local (main-root) path.
    assert_eq!(
        resolve_worktree_fallback(&main_root, ".env"),
        main_root.join(".env")
    );
}

/// Extract base_path(...) calls from a line (test helper)
fn extract_base_path(line: &str) -> Option<&str> {
    // Match: base_path('some/path') or base_path("some/path")
    if let Some(start) = line.find("base_path(") {
        let after = &line[start + 10..];
        if let Some(quote_start) = after.find(['\'', '"']) {
            let quote_char = after.chars().nth(quote_start)?;
            let after_quote = &after[quote_start + 1..];
            if let Some(quote_end) = after_quote.find(quote_char) {
                return Some(&after_quote[..quote_end]);
            }
        }
    }
    None
}

#[test]
fn test_kebab_to_pascal_case() {
    assert_eq!(kebab_to_pascal_case("user-profile"), "UserProfile");
    assert_eq!(kebab_to_pascal_case("admin-dashboard"), "AdminDashboard");
    assert_eq!(kebab_to_pascal_case("simple"), "Simple");
}

#[test]
fn test_extract_base_path() {
    let line = "base_path('resources/templates'),";
    assert_eq!(extract_base_path(line), Some("resources/templates"));

    let line = "base_path(\"some/other/path\"),";
    assert_eq!(extract_base_path(line), Some("some/other/path"));
}

#[test]
fn test_parse_component_aliases_extracts_string_pairs() {
    let source = r#"<?php
return [
'aliases' => [
    'light-button' => 'components.buttons.light-button',
    'danger-button' => 'components.buttons.danger-button',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
    assert_eq!(
        aliases.get("danger-button").map(String::as_str),
        Some("components.buttons.danger-button"),
    );
}

#[test]
fn test_parse_component_aliases_skips_class_references() {
    let source = r#"<?php
return [
'aliases' => [
    'success-alert' => App\View\Components\Alerts\SuccessAlert::class,
    'light-button' => 'components.buttons.light-button',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(!aliases.contains_key("success-alert"));
    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
}

#[test]
fn test_parse_component_aliases_honors_comments() {
    let source = r#"<?php
return [
'aliases' => [
    // 'commented-out' => 'components.commented',
    'real-button' => 'components.buttons.real',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(!aliases.contains_key("commented-out"));
    assert_eq!(
        aliases.get("real-button").map(String::as_str),
        Some("components.buttons.real"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_instance_form() {
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    $blade->component('components.buttons.light-button', 'light-button');
    $blade->component('components.alerts.danger', 'danger-alert');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert_eq!(
        aliases.get("light-button").map(String::as_str),
        Some("components.buttons.light-button"),
    );
    assert_eq!(
        aliases.get("danger-alert").map(String::as_str),
        Some("components.alerts.danger"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_static_form() {
    let php = r#"<?php
namespace App\Providers;

use Illuminate\Support\Facades\Blade;

class AppServiceProvider {
public function boot() {
    Blade::component('components.modal', 'modal');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert_eq!(
        aliases.get("modal").map(String::as_str),
        Some("components.modal"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_skips_class_fqn_view() {
    // When the first arg is a PHP class FQN (contains backslashes), it
    // points at a class-based component which the directory convention
    // handles. We skip those to avoid pretending they're view paths.
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    $blade->component('App\\View\\Components\\Alert', 'alert-class');
    $blade->component('components.regular', 'regular');
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert!(!aliases.contains_key("alert-class"));
    assert_eq!(
        aliases.get("regular").map(String::as_str),
        Some("components.regular"),
    );
}

#[test]
fn test_extract_provider_blade_aliases_ignores_loop_with_variables() {
    // The decisioncloud-style pattern (loop with variable args) cannot
    // produce literal captures and is properly handled by the config
    // file source instead. This verifies the extractor doesn't crash
    // or hallucinate aliases when args aren't literals.
    let php = r#"<?php
namespace App\Providers;

class AppServiceProvider {
public function boot($blade) {
    foreach (config('component.aliases', []) as $alias => $component) {
        $blade->component($component, $alias);
    }
}
}
"#;
    let mut aliases = HashMap::new();
    extract_provider_blade_aliases(php, &mut aliases);

    assert!(
        aliases.is_empty(),
        "no literal pairs to extract from variable args"
    );
}

#[test]
fn test_scan_vendor_uncached_finds_provider_aliases() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!("laravel-lsp-test-vendor-{}", std::process::id(),));
    let _ = std_fs::remove_dir_all(&tmp);

    let provider_dir = tmp.join("vendor/acme/widgets/src");
    std_fs::create_dir_all(&provider_dir).unwrap();

    let provider_php = r#"<?php
namespace Acme\Widgets;

use Illuminate\Support\Facades\Blade;
use Illuminate\Support\ServiceProvider;

class WidgetsServiceProvider extends ServiceProvider {
public function boot() {
    Blade::component('widgets.spinner', 'widget-spinner');
}
}
"#;
    std_fs::write(
        provider_dir.join("WidgetsServiceProvider.php"),
        provider_php,
    )
    .unwrap();

    // Non-provider file with no relevant calls — should be skipped.
    std_fs::write(
        provider_dir.join("SomeOtherClass.php"),
        "<?php namespace Acme\\Widgets; class SomeOtherClass {}",
    )
    .unwrap();

    let aliases = scan_vendor_uncached(&tmp);

    assert_eq!(
        aliases.get("widget-spinner").map(String::as_str),
        Some("widgets.spinner"),
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_uncached_skips_non_serviceprovider_files() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-vendor-skip-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/acme/lib/src");
    std_fs::create_dir_all(&pkg_dir).unwrap();

    // File contains a Blade::component call but isn't named like a
    // service provider — should be skipped by the filename gate.
    let helper_php = r#"<?php
namespace Acme\Lib;

class Helper {
public function setup($blade) {
    $blade->component('lib.thing', 'lib-thing');
}
}
"#;
    std_fs::write(pkg_dir.join("Helper.php"), helper_php).unwrap();

    let aliases = scan_vendor_uncached(&tmp);

    assert!(
        !aliases.contains_key("lib-thing"),
        "non-ServiceProvider files must be ignored",
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_finds_heroicon_style_set() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!("laravel-lsp-test-icons-{}", std::process::id(),));
    let _ = std_fs::remove_dir_all(&tmp);

    // Replicate the heroicons layout: flat SVG dir + blade-*.php config
    // with 'prefix' => 'heroicon'.
    let pkg_dir = tmp.join("vendor/blade-ui-kit/blade-heroicons");
    let svg_dir = pkg_dir.join("resources/svg");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    std_fs::write(
        config_dir.join("blade-heroicons.php"),
        "<?php\nreturn [\n    'prefix' => 'heroicon',\n];\n",
    )
    .unwrap();

    // Drop a couple of SVG files matching the real heroicons naming.
    std_fs::write(svg_dir.join("o-clock.svg"), "<svg></svg>").unwrap();
    std_fs::write(svg_dir.join("s-bell.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);

    assert!(
        icons.contains_key("heroicon-o-clock"),
        "expected heroicon-o-clock entry, got keys: {:?}",
        icons.keys().collect::<Vec<_>>(),
    );
    assert!(icons.contains_key("heroicon-s-bell"));
    assert!(
        icons["heroicon-o-clock"].ends_with("o-clock.svg"),
        "value should point to the svg file",
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_handles_nested_directories() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-icons-nested-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/some-vendor/some-icons");
    let svg_dir = pkg_dir.join("resources/svg/outline");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    std_fs::write(
        config_dir.join("blade-some-icons.php"),
        "<?php return ['prefix' => 'someicon'];",
    )
    .unwrap();

    std_fs::write(svg_dir.join("user.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);

    // Nested file `outline/user.svg` should produce tag `someicon-outline-user`.
    assert!(
        icons.contains_key("someicon-outline-user"),
        "nested dirs should produce dashed tag names, got: {:?}",
        icons.keys().collect::<Vec<_>>(),
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_vendor_icons_skips_packages_without_prefix_config() {
    use std::fs as std_fs;

    let tmp = std::env::temp_dir().join(format!(
        "laravel-lsp-test-icons-noconfig-{}",
        std::process::id(),
    ));
    let _ = std_fs::remove_dir_all(&tmp);

    let pkg_dir = tmp.join("vendor/some-vendor/some-pkg");
    let svg_dir = pkg_dir.join("resources/svg");
    let config_dir = pkg_dir.join("config");
    std_fs::create_dir_all(&svg_dir).unwrap();
    std_fs::create_dir_all(&config_dir).unwrap();

    // Config file exists but no 'prefix' key — should be skipped.
    std_fs::write(
        config_dir.join("blade-something.php"),
        "<?php return ['something' => 'else'];",
    )
    .unwrap();
    std_fs::write(svg_dir.join("icon.svg"), "<svg></svg>").unwrap();

    let icons = scan_vendor_icons_uncached(&tmp);
    assert!(
        icons.is_empty(),
        "should not register icons without a declared prefix"
    );

    let _ = std_fs::remove_dir_all(&tmp);
}

#[test]
fn test_scan_prefix_string_handles_both_quote_styles() {
    assert_eq!(
        scan_prefix_string("'prefix' => 'heroicon'"),
        Some("heroicon".into())
    );
    assert_eq!(
        scan_prefix_string("\"prefix\" => \"heroicon\""),
        Some("heroicon".into())
    );
    assert_eq!(
        scan_prefix_string("'prefix'=>'tight'"),
        Some("tight".into())
    );
    assert_eq!(scan_prefix_string("no prefix here"), None);
}

#[test]
fn test_scan_vendor_uncached_returns_empty_when_no_vendor() {
    let tmp =
        std::env::temp_dir().join(format!("laravel-lsp-test-no-vendor-{}", std::process::id(),));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let aliases = scan_vendor_uncached(&tmp);
    assert!(aliases.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_parse_component_aliases_does_not_cross_into_sibling_keys() {
    // Ensures we walk bracket depth and stop at the closing ] of the aliases array.
    let source = r#"<?php
return [
'aliases' => [
    'light-button' => 'components.buttons.light-button',
],
'other-config' => [
    'unrelated-alias' => 'should.not.be.captured',
],
];
"#;
    let mut aliases = HashMap::new();
    parse_component_aliases(source, &mut aliases);
    assert!(aliases.contains_key("light-button"));
    assert!(!aliases.contains_key("unrelated-alias"));
}

// ─── Livewire v4 component namespaces (issue #79, case 3) ────────────────

const LIVEWIRE_VENDOR_CONFIG: &str = r#"<?php
return [
    'component_namespaces' => [
        'layouts' => resource_path('views/layouts'),
        'pages' => resource_path('views/pages'),
    ],
];
"#;

#[test]
fn livewire_component_namespaces_reads_vendor_defaults() {
    let tmp =
        std::env::temp_dir().join(format!("laravel-lsp-test-lw-vendor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let config_dir = tmp.join("vendor/livewire/livewire/config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("livewire.php"), LIVEWIRE_VENDOR_CONFIG).unwrap();

    let namespaces = livewire_component_namespaces(&tmp);
    assert_eq!(
        namespaces,
        vec![
            ("layouts".to_string(), tmp.join("resources/views/layouts")),
            ("pages".to_string(), tmp.join("resources/views/pages")),
        ]
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn livewire_app_config_overrides_vendor_defaults() {
    let tmp = std::env::temp_dir().join(format!("laravel-lsp-test-lw-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let vendor_dir = tmp.join("vendor/livewire/livewire/config");
    std::fs::create_dir_all(&vendor_dir).unwrap();
    std::fs::write(vendor_dir.join("livewire.php"), LIVEWIRE_VENDOR_CONFIG).unwrap();
    let app_dir = tmp.join("config");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(
        app_dir.join("livewire.php"),
        r#"<?php
return [
    'component_namespaces' => [
        'shells' => base_path('themes/shells'),
    ],
];
"#,
    )
    .unwrap();

    let namespaces = livewire_component_namespaces(&tmp);
    assert_eq!(
        namespaces,
        vec![("shells".to_string(), tmp.join("themes/shells"))]
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn livewire_component_namespaces_empty_without_livewire() {
    let tmp = std::env::temp_dir().join(format!("laravel-lsp-test-lw-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    assert!(livewire_component_namespaces(&tmp).is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn resolve_php_path_expression_handles_helpers_and_literals() {
    let root = Path::new("/proj");
    assert_eq!(
        resolve_php_path_expression("resource_path('views/layouts')", root),
        Some(PathBuf::from("/proj/resources/views/layouts"))
    );
    assert_eq!(
        resolve_php_path_expression("base_path('themes')", root),
        Some(PathBuf::from("/proj/themes"))
    );
    assert_eq!(
        resolve_php_path_expression("app_path('Views')", root),
        Some(PathBuf::from("/proj/app/Views"))
    );
    assert_eq!(
        resolve_php_path_expression("'relative/dir'", root),
        Some(PathBuf::from("/proj/relative/dir"))
    );
    assert_eq!(
        resolve_php_path_expression("'/absolute/dir'", root),
        Some(PathBuf::from("/absolute/dir"))
    );
    // Unparseable expressions are skipped, not mangled.
    assert_eq!(resolve_php_path_expression("env('SOME_DIR')", root), None);
}

#[test]
fn livewire_empty_app_override_disables_vendor_defaults() {
    // 'component_namespaces' => [] in the app config is a deliberate
    // disable — it must NOT fall through to the vendor defaults.
    let tmp =
        std::env::temp_dir().join(format!("laravel-lsp-test-lw-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let vendor_dir = tmp.join("vendor/livewire/livewire/config");
    std::fs::create_dir_all(&vendor_dir).unwrap();
    std::fs::write(vendor_dir.join("livewire.php"), LIVEWIRE_VENDOR_CONFIG).unwrap();
    let app_dir = tmp.join("config");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(
        app_dir.join("livewire.php"),
        "<?php return ['component_namespaces' => []];",
    )
    .unwrap();

    assert!(
        livewire_component_namespaces(&tmp).is_empty(),
        "an explicit empty override must disable the vendor defaults"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ─── Config string value resolution (MaryUI prefix, issue #79) ────────────

#[test]
fn php_top_level_string_value_ignores_nested_keys() {
    let source = r#"<?php
return [
    'components' => [
        'prefix' => 'nested-should-not-match',
    ],
    'prefix' => 'mary-',
];
"#;
    assert_eq!(
        php_top_level_string_value(source, "prefix"),
        Some("mary-".to_string())
    );
}

#[test]
fn php_top_level_string_value_takes_env_default() {
    let source = r#"<?php
return [
    'prefix' => env('MARY_PREFIX', 'mary-'),
];
"#;
    assert_eq!(
        php_top_level_string_value(source, "prefix"),
        Some("mary-".to_string())
    );
}

#[test]
fn php_top_level_string_value_handles_empty_string() {
    let source = "<?php\nreturn [\n    'prefix' => '',\n];\n";
    assert_eq!(
        php_top_level_string_value(source, "prefix"),
        Some(String::new())
    );
}

#[test]
fn resolve_config_string_app_override_wins_over_package_default() {
    let tmp = std::env::temp_dir().join(format!("laravel-lsp-test-cfgstr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let pkg_config = tmp.join("vendor/robsontenorio/mary/config");
    std::fs::create_dir_all(&pkg_config).unwrap();
    std::fs::write(
        pkg_config.join("mary.php"),
        "<?php return ['prefix' => ''];",
    )
    .unwrap();
    let provider_path = tmp.join("vendor/robsontenorio/mary/src/MaryServiceProvider.php");

    // Package default only.
    assert_eq!(
        resolve_config_string_for_package(&tmp, "mary.prefix", &provider_path),
        Some(String::new())
    );

    // App override wins.
    let app_config = tmp.join("config");
    std::fs::create_dir_all(&app_config).unwrap();
    std::fs::write(
        app_config.join("mary.php"),
        "<?php return ['prefix' => 'mary-'];",
    )
    .unwrap();
    assert_eq!(
        resolve_config_string_for_package(&tmp, "mary.prefix", &provider_path),
        Some("mary-".to_string())
    );

    // Unknown key resolves to None (PHP null).
    assert_eq!(
        resolve_config_string_for_package(&tmp, "mary.nonexistent", &provider_path),
        None
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ============================================================================
// Facade aliases (config/app.php 'aliases')
// ============================================================================

#[test]
fn parse_facade_aliases_reads_fully_qualified_class_consts() {
    // The canonical config/app.php form: fully-qualified `::class` values.
    let source = r#"<?php
return [
    'name' => env('APP_NAME', 'Laravel'),
    'aliases' => [
        'App' => Illuminate\Support\Facades\App::class,
        'Auth' => Illuminate\Support\Facades\Auth::class,
    ],
];
"#;
    let aliases = parse_facade_aliases(source);
    assert_eq!(
        aliases.get("App").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\App")
    );
    assert_eq!(
        aliases.get("Auth").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn parse_facade_aliases_strips_leading_backslash() {
    let source = r#"<?php
return [
    'aliases' => [
        'Cache' => \Illuminate\Support\Facades\Cache::class,
    ],
];
"#;
    let aliases = parse_facade_aliases(source);
    assert_eq!(
        aliases.get("Cache").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Cache")
    );
}

#[test]
fn parse_facade_aliases_skips_non_class_values() {
    // A non-`::class` value can't name a facade class statically — skip it,
    // but keep the valid sibling. (Contrast `parse_component_aliases`, which
    // skips `::class` and keeps the string.)
    let source = r#"<?php
return [
    'aliases' => [
        'Legacy' => 'some.string.binding',
        'Auth' => Illuminate\Support\Facades\Auth::class,
    ],
];
"#;
    let aliases = parse_facade_aliases(source);
    assert!(!aliases.contains_key("Legacy"));
    assert_eq!(
        aliases.get("Auth").map(String::as_str),
        Some("Illuminate\\Support\\Facades\\Auth")
    );
}

#[test]
fn parse_facade_aliases_absent_key_yields_empty() {
    let source = "<?php return ['name' => 'Laravel'];";
    assert!(parse_facade_aliases(source).is_empty());
}

// ---------------------------------------------------------------------------
// find_project_root — nested-module (composer-merge-plugin) layouts
// ---------------------------------------------------------------------------

/// Lay down a minimal workspace: root with composer.json + artisan + app/ +
/// resources/, and one module at app/Legal/GuaranteeLabel with its own
/// composer.json + app/ + resources/ + config/ (the merge-plugin shape).
fn modular_workspace() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::write(root.join("composer.json"), "{}").unwrap();
    fs::write(root.join("artisan"), "").unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::create_dir_all(root.join("resources")).unwrap();
    fs::create_dir_all(root.join("config")).unwrap();

    let module = root.join("app/Legal/GuaranteeLabel");
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("resources/views")).unwrap();
    fs::create_dir_all(module.join("config")).unwrap();
    fs::write(module.join("composer.json"), "{}").unwrap();
    (tmp, root, module)
}

#[test]
fn project_root_walks_past_nested_module_to_workspace_root() {
    let (_tmp, root, module) = modular_workspace();
    let file = module.join("config/legal-guaranteelabel.php");
    fs::write(&file, "<?php return [];").unwrap();

    let found = find_project_root(&file, None).unwrap();
    assert_eq!(found, root, "module dir must not hijack the project root");
}

#[test]
fn project_root_keeps_nested_app_with_its_own_artisan() {
    let (_tmp, _root, module) = modular_workspace();
    fs::write(module.join("artisan"), "").unwrap();
    let file = module.join("config/legal-guaranteelabel.php");
    fs::write(&file, "<?php return [];").unwrap();

    let found = find_project_root(&file, None).unwrap();
    assert_eq!(found, module, "a genuine nested app stays its own root");
}

#[test]
fn project_root_standalone_package_with_src_and_vendor_unchanged() {
    let tmp = TempDir::new().unwrap();
    let pkg = tmp.path().join("package");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::create_dir_all(pkg.join("vendor")).unwrap();
    fs::write(pkg.join("composer.json"), "{}").unwrap();
    let file = pkg.join("src/Provider.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, None).unwrap(), pkg);
}

#[test]
fn project_root_app_without_artisan_but_with_vendor_unchanged() {
    let tmp = TempDir::new().unwrap();
    let app = tmp.path().join("app-root");
    fs::create_dir_all(app.join("app")).unwrap();
    fs::create_dir_all(app.join("resources")).unwrap();
    fs::create_dir_all(app.join("vendor")).unwrap();
    fs::write(app.join("composer.json"), "{}").unwrap();
    let file = app.join("app/Thing.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, None).unwrap(), app);
}

#[test]
fn project_root_tentative_match_returned_without_stronger_ancestor() {
    let tmp = TempDir::new().unwrap();
    let app = tmp.path().join("bare-app");
    fs::create_dir_all(app.join("app")).unwrap();
    fs::create_dir_all(app.join("resources")).unwrap();
    fs::write(app.join("composer.json"), "{}").unwrap();
    let file = app.join("app/Thing.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, None).unwrap(), app);
}

// ---------------------------------------------------------------------------
// find_project_root — outermost match within the workspace fence (issue #289)
// ---------------------------------------------------------------------------

#[test]
fn fenced_modular_monolith_resolves_to_the_workspace_root() {
    let (_tmp, root, module) = modular_workspace();
    let file = module.join("config/legal-guaranteelabel.php");
    fs::write(&file, "<?php return [];").unwrap();

    assert_eq!(find_project_root(&file, Some(&root)).unwrap(), root);
}

/// The hole #286's heuristic left open: a module carrying its own `vendor/`
/// satisfied the standalone check and hijacked the root anyway. Outermost-wins
/// never consults `vendor/` for that decision.
#[test]
fn fenced_module_with_its_own_vendor_still_resolves_to_the_workspace_root() {
    let (_tmp, root, module) = modular_workspace();
    fs::create_dir_all(module.join("vendor")).unwrap();
    let file = module.join("config/legal-guaranteelabel.php");
    fs::write(&file, "<?php return [];").unwrap();

    assert_eq!(find_project_root(&file, Some(&root)).unwrap(), root);
}

/// Even a module with its own `artisan` loses to the workspace: you opened the
/// monolith, so you get the monolith. Opening the module directly still yields
/// the module, because then it *is* the workspace root.
#[test]
fn fenced_module_with_its_own_artisan_loses_to_the_workspace_root() {
    let (_tmp, root, module) = modular_workspace();
    fs::write(module.join("artisan"), "").unwrap();
    let file = module.join("config/legal-guaranteelabel.php");
    fs::write(&file, "<?php return [];").unwrap();

    assert_eq!(find_project_root(&file, Some(&root)).unwrap(), root);
    assert_eq!(find_project_root(&file, Some(&module)).unwrap(), module);
}

/// A monorepo manifest that only pins dev tooling has no Laravel markers, so
/// the walk descends past it to the real app.
#[test]
fn fenced_walk_descends_past_a_tooling_only_manifest() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::write(root.join("composer.json"), "{}").unwrap();

    let app = root.join("apps/web");
    fs::create_dir_all(app.join("app")).unwrap();
    fs::create_dir_all(app.join("resources")).unwrap();
    fs::write(app.join("composer.json"), "{}").unwrap();
    let file = app.join("app/Thing.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, Some(&root)).unwrap(), app);
}

/// A parent folder holding many unrelated projects: the descent is per-file,
/// so each resolves to its own project rather than to the shared parent.
#[test]
fn fenced_walk_descends_per_file_from_a_multi_project_parent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for name in ["alpha", "beta"] {
        let app = root.join(name);
        fs::create_dir_all(app.join("app")).unwrap();
        fs::create_dir_all(app.join("resources")).unwrap();
        fs::write(app.join("composer.json"), "{}").unwrap();
        fs::write(app.join("artisan"), "").unwrap();
        fs::write(app.join("app/Thing.php"), "<?php").unwrap();
    }

    for name in ["alpha", "beta"] {
        let file = root.join(name).join("app/Thing.php");
        assert_eq!(
            find_project_root(&file, Some(&root)).unwrap(),
            root.join(name),
            "{name} resolved to the wrong project"
        );
    }
}

/// A fresh clone that has never run `composer install`: no `vendor/` anywhere,
/// and it still resolves, because `artisan` is committed.
#[test]
fn fenced_fresh_clone_without_vendor_resolves_via_artisan() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::write(root.join("composer.json"), "{}").unwrap();
    fs::write(root.join("artisan"), "").unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    let file = root.join("app/Thing.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, Some(&root)).unwrap(), root);
}

#[test]
fn fenced_package_opened_as_its_own_workspace_resolves_to_the_package() {
    let tmp = TempDir::new().unwrap();
    let pkg = tmp.path().join("package");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::create_dir_all(pkg.join("vendor")).unwrap();
    fs::write(pkg.join("composer.json"), "{}").unwrap();
    let file = pkg.join("src/Provider.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, Some(&pkg)).unwrap(), pkg);
}

/// A file outside the workspace — a vendor file, a globally installed package —
/// has no fence to descend, so the upward walk takes over.
#[test]
fn file_outside_the_workspace_falls_back_to_the_upward_walk() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    let other = tmp.path().join("elsewhere");
    fs::create_dir_all(other.join("app")).unwrap();
    fs::create_dir_all(other.join("resources")).unwrap();
    fs::write(other.join("composer.json"), "{}").unwrap();
    fs::write(other.join("artisan"), "").unwrap();
    let file = other.join("app/Thing.php");
    fs::write(&file, "<?php").unwrap();

    assert_eq!(find_project_root(&file, Some(&workspace)).unwrap(), other);
}

#[test]
fn looks_like_laravel_project_requires_composer_json() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("artisan"), "").unwrap();
    assert!(
        !looks_like_laravel_project(dir),
        "artisan alone is not a root"
    );

    fs::write(dir.join("composer.json"), "{}").unwrap();
    assert!(looks_like_laravel_project(dir));
}

#[test]
fn looks_like_laravel_project_rejects_a_bare_manifest() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("composer.json"), "{}").unwrap();
    assert!(!looks_like_laravel_project(dir));

    // app/ alone is not enough — resources/ must be there too.
    fs::create_dir_all(dir.join("app")).unwrap();
    assert!(!looks_like_laravel_project(dir));

    fs::create_dir_all(dir.join("resources")).unwrap();
    assert!(looks_like_laravel_project(dir));
}

// ---------------------------------------------------------------------------
// is_outermost_project — the "may this nested dir be the root?" predicate
// ---------------------------------------------------------------------------

#[test]
fn outermost_project_accepts_the_workspace_root_itself() {
    let (_tmp, root, _module) = modular_workspace();
    assert!(is_outermost_project(&root, &root));
}

#[test]
fn outermost_project_rejects_a_nested_module() {
    let (_tmp, root, module) = modular_workspace();
    assert!(
        !is_outermost_project(&root, &module),
        "a module is not the outermost project of the workspace"
    );
}

/// The signal is committed state only — adding or removing a gitignored
/// `vendor/` must not change the answer either way.
#[test]
fn outermost_project_ignores_vendor_entirely() {
    let (_tmp, root, module) = modular_workspace();
    assert!(!is_outermost_project(&root, &module));

    fs::create_dir_all(module.join("vendor")).unwrap();
    fs::write(module.join("vendor/autoload.php"), "<?php").unwrap();
    assert!(
        !is_outermost_project(&root, &module),
        "a fully installed vendor/ must not promote a module"
    );
}

/// A genuine sub-app under a tooling-only workspace manifest *is* outermost on
/// its own path, so it is accepted.
#[test]
fn outermost_project_accepts_a_real_sub_app_below_a_tooling_manifest() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fs::write(root.join("composer.json"), "{}").unwrap();

    let app = root.join("apps/web");
    fs::create_dir_all(app.join("app")).unwrap();
    fs::create_dir_all(app.join("resources")).unwrap();
    fs::write(app.join("composer.json"), "{}").unwrap();

    assert!(is_outermost_project(&root, &app));
}

#[test]
fn outermost_project_fails_closed_outside_the_workspace() {
    let (_tmp, root, _module) = modular_workspace();
    let outside = TempDir::new().unwrap();
    assert!(
        !is_outermost_project(&root, outside.path()),
        "a directory outside the workspace is never outermost within it"
    );
}

// ---------------------------------------------------------------------------
// expand_module_dirs / config_group_files (`modules.paths`)
// ---------------------------------------------------------------------------

#[test]
fn expand_module_dirs_matches_star_segments_in_pattern_order() {
    let (_tmp, root, module) = modular_workspace();
    let common = root.join("app/Common/UI");
    fs::create_dir_all(common.join("config")).unwrap();

    let patterns = vec!["app/Common/*".to_string(), "app/*/*".to_string()];
    let dirs = expand_module_dirs(&root, &patterns);

    let common_pos = dirs.iter().position(|d| d == &common).unwrap();
    let module_pos = dirs.iter().position(|d| d == &module).unwrap();
    assert!(
        common_pos < module_pos,
        "earlier pattern keeps its (lower-precedence) position"
    );
    // Dedup: Common/UI also matches app/*/* but must appear once.
    assert_eq!(dirs.iter().filter(|d| *d == &common).count(), 1);
}

#[test]
fn expand_module_dirs_empty_patterns_is_off() {
    let (_tmp, root, _module) = modular_workspace();
    assert!(expand_module_dirs(&root, &[]).is_empty());
}

#[test]
fn expand_module_dirs_rejects_escaping_patterns() {
    let (_tmp, root, _module) = modular_workspace();
    let dirs = expand_module_dirs(&root, &["../*".to_string()]);
    assert!(dirs.is_empty());
}

#[test]
fn config_group_files_orders_by_descending_precedence() {
    let (_tmp, root, module) = modular_workspace();
    fs::write(root.join("config/app.php"), "<?php return [];").unwrap();
    fs::write(module.join("config/app.php"), "<?php return [];").unwrap();
    fs::write(
        module.join("config/legal-guaranteelabel.php"),
        "<?php return [];",
    )
    .unwrap();

    let module_dirs = vec![module.clone()];
    let app_files = config_group_files(&root, &module_dirs, "app");
    assert_eq!(
        app_files,
        vec![module.join("config/app.php"), root.join("config/app.php")],
        "the winning file (last-merged module) comes FIRST — the helper owns \
         precedence, consumers take the first hit"
    );

    let module_only = config_group_files(&root, &module_dirs, "legal-guaranteelabel");
    assert_eq!(
        module_only,
        vec![module.join("config/legal-guaranteelabel.php")]
    );
}

// ---- composer-driven module provider discovery -----------------------------

/// A module with a composer.json declaring one conventional and one
/// non-conventional provider, plus a decoy `*ServiceProvider.php` the
/// manifest does NOT name.
fn module_with_manifest(root: &Path) -> PathBuf {
    let module = root.join("app/Legal/ContractManagement");
    let providers = module.join("app/Providers");
    fs::create_dir_all(&providers).unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{
    "name": "acme/legal-contractmanagement",
    "autoload": {
        "psr-4": {
            "App\\Legal\\ContractManagement\\": "app/"
        }
    },
    "extra": {
        "laravel": {
            "providers": [
                "App\\Legal\\ContractManagement\\Providers\\ContractServiceProvider",
                "App\\Legal\\ContractManagement\\Providers\\Bootstrap"
            ]
        }
    }
}"#,
    )
    .unwrap();
    fs::write(
        providers.join("ContractServiceProvider.php"),
        "<?php class ContractServiceProvider {}",
    )
    .unwrap();
    fs::write(providers.join("Bootstrap.php"), "<?php class Bootstrap {}").unwrap();
    fs::write(
        providers.join("UnregisteredServiceProvider.php"),
        "<?php class UnregisteredServiceProvider {}",
    )
    .unwrap();
    module
}

#[test]
fn module_providers_come_from_composer_extra_laravel_providers() {
    let tmp = TempDir::new().unwrap();
    let module = module_with_manifest(tmp.path());

    let mut files = module_provider_files(std::slice::from_ref(&module));
    files.sort();

    // The manifest-listed providers are indexed — including `Bootstrap`,
    // whose filename matches no `*ServiceProvider.php` convention…
    assert_eq!(
        files,
        vec![
            module.join("app/Providers/Bootstrap.php"),
            module.join("app/Providers/ContractServiceProvider.php"),
        ],
        "…and the conventionally-named but UNLISTED provider is not: only \
         what composer boots is a provider"
    );
}

#[test]
fn module_without_manifest_contributes_no_providers() {
    let tmp = TempDir::new().unwrap();
    let module = tmp.path().join("app/Legal/Bare");
    let providers = module.join("app/Providers");
    fs::create_dir_all(&providers).unwrap();
    fs::write(
        providers.join("BareServiceProvider.php"),
        "<?php class BareServiceProvider {}",
    )
    .unwrap();

    assert!(module_provider_files(&[module]).is_empty());
}

#[test]
fn provider_fqcn_resolves_via_basename_walk_without_matching_psr4() {
    let tmp = TempDir::new().unwrap();
    let module = tmp.path().join("app/Common/Ui");
    let src = module.join("src/Support");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{
    "extra": { "laravel": { "providers": ["Acme\\Ui\\Support\\UiServiceProvider"] } }
}"#,
    )
    .unwrap();
    fs::write(
        src.join("UiServiceProvider.php"),
        "<?php class UiServiceProvider {}",
    )
    .unwrap();

    assert_eq!(
        module_provider_files(std::slice::from_ref(&module)),
        vec![module.join("src/Support/UiServiceProvider.php")]
    );
}

// ---- modules.paths glob behavior -------------------------------------------

#[test]
fn expand_module_dirs_single_and_double_wildcard_depths() {
    let tmp = TempDir::new().unwrap();
    // `app/Common/*` matches one level below Common…
    fs::create_dir_all(tmp.path().join("app/Common/Ui")).unwrap();
    fs::create_dir_all(tmp.path().join("app/Common/Billing")).unwrap();
    // …while `app/*/*` matches two levels below app/ — a different depth.
    fs::create_dir_all(tmp.path().join("app/Legal/ContractManagement")).unwrap();

    let single = expand_module_dirs(tmp.path(), &["app/Common/*".to_string()]);
    assert_eq!(
        single,
        vec![
            tmp.path().join("app/Common/Billing"),
            tmp.path().join("app/Common/Ui"),
        ]
    );

    let double = expand_module_dirs(tmp.path(), &["app/*/*".to_string()]);
    assert!(
        double.contains(&tmp.path().join("app/Legal/ContractManagement")),
        "double wildcard reaches two levels deep: {double:?}"
    );
    assert!(
        double.contains(&tmp.path().join("app/Common/Ui")),
        "double wildcard also spans the single-wildcard matches: {double:?}"
    );
}

#[test]
fn expand_module_dirs_stale_glob_matches_nothing() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("app/Common/Ui")).unwrap();
    assert!(
        expand_module_dirs(tmp.path(), &["app/Removed/*".to_string()]).is_empty(),
        "a glob whose literal segments no longer exist is simply off"
    );
}

#[test]
fn expand_module_dirs_malformed_entries_are_a_no_op() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("app/Common/Ui")).unwrap();
    // `**`, stray brackets, and an empty string are not crash inputs — each
    // entry that matches no directory contributes nothing, while a valid
    // entry in the same list still expands.
    let dirs = expand_module_dirs(
        tmp.path(),
        &[
            "**".to_string(),
            "app/[oops".to_string(),
            String::new(),
            "app/Common/*".to_string(),
        ],
    );
    assert_eq!(dirs, vec![tmp.path().join("app/Common/Ui")]);
}

#[cfg(unix)]
#[test]
fn discovered_provider_walk_refuses_symlink_escapes_but_keeps_symlinked_modules() {
    // The configured/discovered split: a module dir that IS a symlink
    // (composer path repository) is trusted configuration and keeps
    // working; a symlink INSIDE a module pointing outside it is a
    // discovered path and is refused (#228 convention).
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(outside.join("Providers")).unwrap();
    fs::write(
        outside.join("Providers/EscapeServiceProvider.php"),
        "<?php class EscapeServiceProvider {}",
    )
    .unwrap();

    // Module with a composer manifest naming a provider that only exists
    // BEHIND an escaping symlink.
    let module = tmp.path().join("proj/app/Legal/Sneaky");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{ "extra": { "laravel": { "providers": ["Acme\\EscapeServiceProvider"] } } }"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, module.join("linked")).unwrap();
    assert!(
        module_provider_files(std::slice::from_ref(&module)).is_empty(),
        "a symlink escaping the module must not become a provider source"
    );

    // The module dir itself being a symlink stays supported.
    let real = tmp.path().join("packages/real-module");
    fs::create_dir_all(real.join("Providers")).unwrap();
    fs::write(
        real.join("composer.json"),
        r#"{ "extra": { "laravel": { "providers": ["Acme\\RealServiceProvider"] } } }"#,
    )
    .unwrap();
    fs::write(
        real.join("Providers/RealServiceProvider.php"),
        "<?php class RealServiceProvider {}",
    )
    .unwrap();
    let linked_module = tmp.path().join("proj/app/Legal/Linked");
    fs::create_dir_all(linked_module.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &linked_module).unwrap();
    let files = module_provider_files(std::slice::from_ref(&linked_module));
    assert_eq!(files.len(), 1, "symlinked composer path repo keeps working");
}

#[test]
fn psr4_entries_escaping_the_module_resolve_nothing() {
    // `autoload.psr-4` values are manifest-derived — DISCOVERED data. An
    // absolute value replaces `Path::join`'s base entirely, and `..`
    // segments walk out lexically; both must be refused before the
    // candidate is ever probed.
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside/Providers");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("EscapeServiceProvider.php"),
        "<?php class X {}",
    )
    .unwrap();

    for psr4_dir in [
        outside.to_string_lossy().to_string(), // absolute
        // Four levels: `{n}` -> `Legal` -> `app` -> `proj` -> the temp dir,
        // so the candidate lands ON the decoy written above. `../../outside`
        // normalized to `proj/app/outside`, which holds nothing — the
        // assertion then passed with or without the containment gate.
        "../../../../outside".to_string(), // traversal
    ] {
        let module = tmp
            .path()
            .join(format!("proj/app/Legal/{}", psr4_dir.len()));
        fs::create_dir_all(&module).unwrap();
        fs::write(
            module.join("composer.json"),
            format!(
                r#"{{
    "autoload": {{ "psr-4": {{ "Acme\\": {} }} }},
    "extra": {{ "laravel": {{ "providers": ["Acme\\Providers\\EscapeServiceProvider"] }} }}
}}"#,
                serde_json::to_string(&psr4_dir).unwrap()
            ),
        )
        .unwrap();

        assert!(
            module_provider_files(std::slice::from_ref(&module)).is_empty(),
            "psr-4 value {psr4_dir:?} must not resolve outside the module"
        );
    }
}

#[test]
fn psr4_prefix_matches_only_at_a_namespace_boundary() {
    // Composer compares prefixes WITH their trailing separator, so
    // `App\Legal\ContractManagement` must not match
    // `App\Legal\ContractManagementSupport\…`. Without the boundary check
    // the textual match wins the longest-prefix tie-break and resolves the
    // wrong file.
    let tmp = TempDir::new().unwrap();
    let module = tmp.path().join("app/Legal/Suite");
    fs::create_dir_all(module.join("support/Providers")).unwrap();
    fs::create_dir_all(module.join("contract/Providers")).unwrap();
    fs::write(
        module.join("support/Providers/Registrar.php"),
        "<?php class Support {}",
    )
    .unwrap();
    // The decoy the bogus prefix match would resolve to.
    fs::create_dir_all(module.join("contract/Support/Providers")).unwrap();
    fs::write(
        module.join("contract/Support/Providers/Registrar.php"),
        "<?php class Decoy {}",
    )
    .unwrap();
    fs::write(
        module.join("composer.json"),
        r#"{
    "autoload": { "psr-4": {
        "App\\Legal\\ContractManagement\\": "contract/",
        "App\\Legal\\ContractManagementSupport\\": "support/"
    } },
    "extra": { "laravel": { "providers": [
        "App\\Legal\\ContractManagementSupport\\Providers\\Registrar"
    ] } }
}"#,
    )
    .unwrap();

    assert_eq!(
        module_provider_files(std::slice::from_ref(&module)),
        vec![module.join("support/Providers/Registrar.php")],
        "the true mapping wins; the overlapping-prefix decoy is not matched"
    );
}

#[cfg(unix)]
#[test]
fn expand_module_dirs_follows_a_symlinked_module_directory() {
    // The documented configured-path behaviour, driven through the glob
    // expansion itself rather than only through the provider walk: a
    // composer path repository symlinked into the module tree expands like
    // any real directory.
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("packages/ui-kit");
    fs::create_dir_all(&real).unwrap();
    let modules = tmp.path().join("proj/app/Common");
    fs::create_dir_all(&modules).unwrap();
    std::os::unix::fs::symlink(&real, modules.join("Ui")).unwrap();

    let dirs = expand_module_dirs(&tmp.path().join("proj"), &["app/Common/*".to_string()]);
    assert_eq!(
        dirs,
        vec![modules.join("Ui")],
        "a symlinked module expands (configured paths are trusted)"
    );
}

// ============================================================================
// owning_module — the shared module-ownership + `modules.paths` rank lookup
// ============================================================================

#[test]
fn owning_module_ranks_by_configured_order_not_by_name() {
    // The rank is the module's position in `modules.paths`, so it must be
    // readable off a list whose order is neither alphabetical nor its
    // reverse — the property both the containment gate and the merge
    // tie-break depend on.
    let root = Path::new("/proj");
    let dirs = vec![
        root.join("app/Legal/Alpha"),
        root.join("app/Legal/Gamma"),
        root.join("app/Legal/Beta"),
    ];

    for (name, rank) in [("Alpha", 1), ("Gamma", 2), ("Beta", 3)] {
        let provider = root.join(format!("app/Legal/{name}/app/Providers/Registrar.php"));
        assert_eq!(
            owning_module(&dirs, &provider),
            Some((rank, root.join(format!("app/Legal/{name}")).as_path())),
            "{name} is owned by its own module at configured rank {rank}"
        );
    }
}

#[test]
fn owning_module_is_none_outside_every_module() {
    let root = Path::new("/proj");
    let dirs = vec![root.join("app/Legal/Alpha")];

    assert_eq!(
        owning_module(&dirs, &root.join("app/Providers/AppServiceProvider.php")),
        None,
        "an app provider has no owning module — the caller falls back to the root"
    );
    assert_eq!(
        owning_module(&[], &root.join("app/Legal/Alpha/app/Providers/X.php")),
        None,
        "no configured modules, no ownership"
    );
}

#[test]
fn owning_module_prefers_the_innermost_module_on_nesting() {
    // A module nested inside another is booted by its own composer.json, so
    // it owns its files — the longest match wins regardless of rank order.
    let root = Path::new("/proj");
    // The outer module is listed FIRST so that both modules match the inner
    // file and only the longest-match rule can pick the inner one — a
    // first-match implementation returns the outer module here.
    let dirs = vec![
        root.join("app/Legal/Suite"),
        root.join("app/Legal/Suite/packages/Billing"),
    ];

    assert_eq!(
        owning_module(
            &dirs,
            &root.join("app/Legal/Suite/packages/Billing/src/Provider.php")
        ),
        Some((2, root.join("app/Legal/Suite/packages/Billing").as_path())),
        "the inner module owns its own file even though the outer one is listed first"
    );
    assert_eq!(
        owning_module(&dirs, &root.join("app/Legal/Suite/src/Provider.php")),
        Some((1, root.join("app/Legal/Suite").as_path())),
        "a file only the outer module contains stays with the outer module"
    );
}

#[test]
fn owning_module_does_not_match_a_sibling_sharing_a_name_prefix() {
    // `Path::starts_with` is component-wise, so this is a pin against a
    // future string-prefix rewrite: `.../Contract` must not own
    // `.../ContractSupport/…`.
    let root = Path::new("/proj");
    let dirs = vec![root.join("app/Legal/Contract")];

    assert_eq!(
        owning_module(
            &dirs,
            &root.join("app/Legal/ContractSupport/app/Providers/X.php")
        ),
        None,
        "a name-prefix sibling is a different module"
    );
}

#[test]
fn owning_module_collapses_traversal_before_matching() {
    // The gate reads ownership off provider paths that may still carry
    // `..`/`.`; a raw component compare would call an escaping path in-module.
    let root = Path::new("/proj");
    let dirs = vec![root.join("app/Legal/Alpha")];

    assert_eq!(
        owning_module(&dirs, &root.join("app/Legal/Alpha/../Beta/src/X.php")),
        None,
        "a path that walks out of the module is not owned by it"
    );
    assert_eq!(
        owning_module(&dirs, &root.join("app/Legal/./Alpha/src/X.php")),
        Some((1, root.join("app/Legal/Alpha").as_path())),
        "a `.` segment is noise, not an escape"
    );
}
