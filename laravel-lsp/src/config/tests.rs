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
