//! Module config keys across every surface (issue #297).
//!
//! A key sourced ONLY from a module's `config/{group}.php` must behave
//! exactly like a root `config/` key on all seven surfaces: completion,
//! hover (value resolution), goto-definition, the "Config not found"
//! diagnostic, find-references, rename, and the code lens. All of them
//! consult the same two shared helpers — `config::config_group_files` owns
//! discovery + merge ORDER, `config_lookup`/`config_key_locator` own value
//! and position resolution over that order — so precedence can't drift
//! between surfaces.
//!
//! Documented precedence, mirroring the runtime `array_replace_recursive`
//! a module provider performs over the loaded repository state:
//! the project `config/{group}.php` merges FIRST, then each module in
//! `modules.paths` glob-match order — so a key declared in both resolves to
//! the LAST-merged module's value, and modules override the root file.
//! This applies uniformly, including when a module group name collides with
//! a core Laravel group (`app.php`, `database.php`).

use crate::LaravelLanguageServer;
use laravel_lsp::config_key_locator::locate_key_all;
use laravel_lsp::config_lookup::resolve_value_with_source;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::LspService;

/// Root + one module (`app/Legal/ContractManagement`) fixture matching the
/// issue's modular-monolith layout.
fn modular_fixture(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/app.php"),
        r#"<?php
return [
    'name' => 'RootApp',
    'features' => [
        'root_only' => 'from-root',
        'shared' => 'root-value',
    ],
];
"#,
    )
    .unwrap();

    let module = root.join("app/Legal/ContractManagement");
    fs::create_dir_all(module.join("config")).unwrap();
    fs::create_dir_all(module.join("app/Providers")).unwrap();
    fs::create_dir_all(module.join("resources/views")).unwrap();
    fs::create_dir_all(module.join("resources/lang")).unwrap();
    fs::write(module.join("composer.json"), "{}").unwrap();
    // The module-ONLY group: no root config/contract-management.php exists.
    fs::write(
        module.join("config/contract-management.php"),
        r#"<?php
return [
    'recalculate_terms_chunk_size' => 250,
];
"#,
    )
    .unwrap();
    // Collision with the core `app` group.
    fs::write(
        module.join("config/app.php"),
        r#"<?php
return [
    'features' => [
        'module_only' => 'from-module',
        'shared' => 'module-value',
    ],
];
"#,
    )
    .unwrap();
    module
}

// ---- the seven surfaces, one test each -------------------------------------

#[tokio::test]
async fn completion_offers_a_module_only_config_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(tmp.path().to_path_buf());
    *backend.module_path_patterns.write().await = vec!["app/*/*".to_string()];
    let _ = module;

    let keys = backend.get_all_config_keys().await;
    let entry = keys
        .iter()
        .find(|k| k.key == "contract-management.recalculate_terms_chunk_size")
        .expect("module-only key offered by completion");
    assert_eq!(entry.value, "250");
}

#[test]
fn hover_value_resolves_from_the_module_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let (value, source) = resolve_value_with_source(
        tmp.path(),
        std::slice::from_ref(&module),
        "contract-management.recalculate_terms_chunk_size",
    )
    .expect("module-only key resolves");
    assert_eq!(value, "250");
    assert_eq!(source, module.join("config/contract-management.php"));
}

#[test]
fn goto_definition_lands_in_the_module_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let hits = locate_key_all(
        tmp.path(),
        std::slice::from_ref(&module),
        "contract-management.recalculate_terms_chunk_size",
    );
    assert_eq!(hits.len(), 1, "one declaration site");
    assert_eq!(hits[0].0, module.join("config/contract-management.php"));
    assert_eq!(hits[0].1.line, 2, "0-based line of the key declaration");
}

#[test]
fn config_not_found_diagnostic_no_longer_fires_for_a_module_group() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let with_modules = LaravelLanguageServer::check_config_file(
        tmp.path(),
        &[module],
        "contract-management.recalculate_terms_chunk_size",
    );
    assert!(with_modules.exists, "module config satisfies the check");

    let without_modules = LaravelLanguageServer::check_config_file(
        tmp.path(),
        &[],
        "contract-management.recalculate_terms_chunk_size",
    );
    assert!(
        !without_modules.exists,
        "negative control: without modules.paths the diagnostic fires"
    );
}

#[test]
fn find_references_and_rename_see_every_merged_declaration() {
    // `locate_key_all` is the shared source for both find-references and
    // rename: a key declared in the root AND a module returns both
    // declaration sites, in descending merge precedence (module first).
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let hits = locate_key_all(
        tmp.path(),
        std::slice::from_ref(&module),
        "app.features.shared",
    );
    let files: Vec<&PathBuf> = hits.iter().map(|(p, _)| p).collect();
    assert_eq!(
        files,
        vec![
            &module.join("config/app.php"),
            &tmp.path().join("config/app.php"),
        ],
        "both declarations, winning file first"
    );
}

#[test]
fn code_lens_targets_cover_module_config_keys() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());
    let source = fs::read_to_string(module.join("config/contract-management.php")).unwrap();

    let targets = laravel_lsp::code_lens::config_lens_targets("contract-management", &source);
    assert!(
        targets.iter().any(|t| t.symbol
            == laravel_lsp::salsa_impl::SymbolRefData::Config(
                "contract-management.recalculate_terms_chunk_size".to_string()
            )),
        "a module config file's keys get reference-count lenses: {targets:?}"
    );
}

// ---- precedence -------------------------------------------------------------

#[test]
fn scalar_collision_resolves_to_the_module_value() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let (value, source) = resolve_value_with_source(
        tmp.path(),
        std::slice::from_ref(&module),
        "app.features.shared",
    )
    .expect("key resolves");
    assert_eq!(value, "'module-value'", "the module merges LAST and wins");
    assert_eq!(source, module.join("config/app.php"));
}

#[test]
fn nested_arrays_merge_instead_of_replacing() {
    // `array_replace_recursive` semantics: the nested `features` arrays
    // MERGE — the root-only leaf survives, the module-only leaf appears,
    // and only the genuinely shared leaf is overridden.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());
    let dirs = vec![module];

    let root_only = resolve_value_with_source(tmp.path(), &dirs, "app.features.root_only");
    assert_eq!(root_only.unwrap().0, "'from-root'");
    let module_only = resolve_value_with_source(tmp.path(), &dirs, "app.features.module_only");
    assert_eq!(module_only.unwrap().0, "'from-module'");
}

#[test]
fn module_vs_module_collision_follows_configured_order() {
    // Two modules define the same group with an overlapping key: the module
    // matched LATER by `modules.paths` merges later and wins — the same
    // last-merged-wins rule as everywhere else.
    let tmp = tempfile::TempDir::new().unwrap();
    let first = tmp.path().join("app/Legal/Alpha");
    let second = tmp.path().join("app/Legal/Beta");
    for (dir, value) in [(&first, "'from-alpha'"), (&second, "'from-beta'")] {
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(
            dir.join("config/shared-group.php"),
            format!("<?php\nreturn [\n    'key' => {value},\n];\n"),
        )
        .unwrap();
    }

    let (value, source) =
        resolve_value_with_source(tmp.path(), &[first, second.clone()], "shared-group.key")
            .expect("key resolves");
    assert_eq!(value, "'from-beta'");
    assert_eq!(source, second.join("config/shared-group.php"));
}

// ---- static parsing guarantee ------------------------------------------------

#[test]
fn module_config_is_parsed_statically_never_executed() {
    // A config file whose values are side-effecting or fatal expressions
    // still resolves — the reader is tree-sitter/scanner-based, no
    // require/include/eval ever runs project PHP.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = tmp.path().join("app/Legal/Hazmat");
    fs::create_dir_all(module.join("config")).unwrap();
    fs::write(
        module.join("config/hazmat.php"),
        r#"<?php
exit('booted by mistake');
return [
    'safe' => 'value',
    'dangerous' => exec('false'),
];
"#,
    )
    .unwrap();

    let resolved =
        resolve_value_with_source(tmp.path(), std::slice::from_ref(&module), "hazmat.safe")
            .unwrap();
    assert_eq!(resolved.0, "'value'");
    let dangerous = resolve_value_with_source(tmp.path(), &[module], "hazmat.dangerous").unwrap();
    assert_eq!(
        dangerous.0, "exec('false')",
        "the expression is returned as TEXT, never evaluated"
    );
}

// ---- zero-behavior-change guarantee ------------------------------------------

#[tokio::test]
async fn unset_and_stale_glob_produce_identical_output() {
    // The opt-in guarantee, as a diff check: completion output with
    // `modules.paths` unset and with a stale glob (matching nothing on
    // disk) is identical.
    let tmp = tempfile::TempDir::new().unwrap();
    modular_fixture(tmp.path());

    let run = |patterns: Vec<String>| {
        let root = tmp.path().to_path_buf();
        async move {
            let (service, _socket) = LspService::new(LaravelLanguageServer::new);
            let backend = service.inner().clone();
            *backend.root_path.write().await = Some(root);
            *backend.module_path_patterns.write().await = patterns;
            backend
                .get_all_config_keys()
                .await
                .into_iter()
                .map(|k| format!("{}={} ({})", k.key, k.value, k.source))
                .collect::<Vec<String>>()
        }
    };

    let unset = run(Vec::new()).await;
    let stale = run(vec!["app/Gone/*".to_string()]).await;
    assert_eq!(unset, stale, "a stale glob behaves exactly like unset");
    assert!(
        unset.iter().any(|k| k.starts_with("app.name")),
        "sanity: the root config is still scanned: {unset:?}"
    );
}

#[test]
fn non_leaf_lookup_returns_the_winning_files_subtree_by_design() {
    // Pinned intentionally: `config('group')` for a PARENT array split
    // across files returns the winning file's subtree, not an
    // `array_replace_recursive` merge of all contributors. Per-key nested
    // resolution (the tests above) is merged; whole-array hover shows the
    // highest-precedence declaration. Full-array merging is a deliberate
    // non-goal for now.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let (value, source) =
        resolve_value_with_source(tmp.path(), std::slice::from_ref(&module), "app.features")
            .expect("parent key resolves");
    assert_eq!(source, module.join("config/app.php"), "winning file");
    assert!(
        value.contains("module_only") && !value.contains("root_only"),
        "the subtree is the winning file's alone: {value}"
    );
}

#[tokio::test]
async fn unset_and_stale_glob_produce_identical_translation_completions() {
    // The other half of the opt-in diff check: translation completion —
    // the surface the Salsa namespaced-catalogue scan actually changed —
    // is byte-identical between `modules.paths` unset and a stale glob.
    // (Namespaces registered by VENDOR packages appear in both alike:
    // that scan is deliberately unconditional — the #293/#328 gap — and
    // documented as such, independent of the modules setting.)
    let tmp = tempfile::TempDir::new().unwrap();
    let lang = tmp.path().join("lang/en");
    fs::create_dir_all(&lang).unwrap();
    fs::write(
        lang.join("messages.php"),
        "<?php\nreturn [\n    'welcome' => 'Welcome',\n];\n",
    )
    .unwrap();

    let run = |patterns: Vec<String>| {
        let root = tmp.path().to_path_buf();
        async move {
            let (service, _socket) = LspService::new(LaravelLanguageServer::new);
            let backend = service.inner().clone();
            *backend.root_path.write().await = Some(root);
            *backend.module_path_patterns.write().await = patterns;
            backend
                .get_all_translation_keys()
                .await
                .into_iter()
                .map(|k| format!("{}={} ({})", k.key, k.value, k.source))
                .collect::<Vec<String>>()
        }
    };

    let unset = run(Vec::new()).await;
    let stale = run(vec!["app/Gone/*".to_string()]).await;
    assert_eq!(unset, stale);
    assert!(
        unset.iter().any(|k| k.starts_with("messages.welcome")),
        "sanity: the root catalogue is scanned: {unset:?}"
    );
}

#[test]
fn rename_rewrites_the_key_in_every_declaring_file() {
    // The seventh surface. A key merged from the project config AND a
    // module config must be rewritten in BOTH — leaving one behind would
    // resurrect the old key at runtime through the merge.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());

    let targets = crate::collect_config_declaration_target(
        tmp.path(),
        std::slice::from_ref(&module),
        "app.features.shared",
        "app.features.renamed",
    );
    let mut files: Vec<PathBuf> = targets.iter().map(|t| t.file_path.clone()).collect();
    files.sort();
    let mut expected = vec![
        module.join("config/app.php"),
        tmp.path().join("config/app.php"),
    ];
    expected.sort();
    assert_eq!(files, expected, "both declarations are rewritten");
    assert!(
        targets.iter().all(|t| t.new_text == "renamed"),
        "each edit replaces the leaf segment only: {targets:?}"
    );
}

#[tokio::test]
async fn completion_and_the_shared_helper_agree_on_precedence() {
    // Binds the two implementations: enumeration finds WHICH groups exist,
    // but the winning declaration of a key must be the one
    // `config_group_files` puts first. A future change landing in only one
    // of them fails here.
    let tmp = tempfile::TempDir::new().unwrap();
    let module = modular_fixture(tmp.path());
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(tmp.path().to_path_buf());
    *backend.module_path_patterns.write().await = vec!["app/*/*".to_string()];

    let completions = backend.get_all_config_keys().await;
    let shared = completions
        .iter()
        .find(|k| k.key == "app.features.shared")
        .expect("collision key offered");

    let winner =
        laravel_lsp::config::config_group_files(tmp.path(), std::slice::from_ref(&module), "app")
            .into_iter()
            .next()
            .expect("at least one contributing file");
    let winner_rel = winner.strip_prefix(tmp.path()).unwrap();

    // Compared by path COMPONENTS, not as strings: the fixture builds its
    // module path from a forward-slash literal, so on Windows the two sides
    // spell the same path with different separators.
    assert_eq!(
        Path::new(&shared.source).components().collect::<Vec<_>>(),
        winner_rel.components().collect::<Vec<_>>(),
        "completion's winning source must be the helper's first file"
    );
    assert_eq!(
        shared.value, "module-value",
        "…which is the module's value, per the documented rule"
    );
}
