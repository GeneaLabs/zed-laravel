//! Namespaced translation-key completion (`ns::file.key`).
//!
//! Completion used to enumerate only the project's own root `lang/`
//! catalogue, so a project keeping a namespace's translations only under its
//! registered directory (vendor package, module, or an app
//! `loadTranslationsFrom` call — never published to `lang/vendor/…`) got
//! zero completions for that namespace's keys. The Salsa translation layer
//! now also walks every provider-registered namespace — the same map
//! hover/goto/diagnostics share — emitting `{ns}::{file}.{key}`. Module
//! service providers reach that map through
//! `set_translation_provider_extras`, the hook `module_dirs_for` feeds.
//!
//! These tests drive the private async method on a server built through the
//! `tower_lsp::LspService` harness, priming `root_path` and registering a
//! real module provider file so the scan runs purely against a tempdir.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::LspService;

/// Build a backend rooted at `root`, with `provider` (a real on-disk module
/// service provider registering translation namespaces) handed to the Salsa
/// translation layer the same way `module_dirs_for` does.
async fn backend_with_module_provider(root: &Path, provider: PathBuf) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
        .salsa
        .set_translation_provider_extras(vec![provider])
        .await
        .expect("salsa actor should accept provider extras");
    backend
}

/// Write a module service provider that registers `namespace` for the
/// module's own `lang/` directory (the `modules.paths` convention:
/// `app/{Parent}/{Module}/Providers/…` next to `app/{Parent}/{Module}/lang`).
fn write_module_provider(module_dir: &Path, namespace: &str) -> PathBuf {
    let providers = module_dir.join("Providers");
    fs::create_dir_all(&providers).unwrap();
    let path = providers.join("ModuleServiceProvider.php");
    let source = format!(
        "<?php\nclass ModuleServiceProvider {{\n    public function boot(): void {{\n        $this->loadTranslationsFrom(__DIR__.'/../lang', '{namespace}');\n    }}\n}}\n"
    );
    fs::write(&path, source).unwrap();
    path
}

/// Write a one-key catalogue at `<lang_dir>/en/<file>.php`. `key` may be
/// dotted (`"details.title"`) — each segment becomes its own nested,
/// one-per-line PHP array level.
fn write_catalogue(lang_dir: &Path, file: &str, key: &str, value: &str) {
    let dir = lang_dir.join("en");
    fs::create_dir_all(&dir).unwrap();

    let segments: Vec<&str> = key.split('.').collect();
    let mut source = String::from("<?php\nreturn [\n");
    for (depth, segment) in segments.iter().enumerate() {
        let indent = "    ".repeat(depth + 1);
        if depth + 1 == segments.len() {
            source.push_str(&format!("{indent}'{segment}' => '{value}',\n"));
        } else {
            source.push_str(&format!("{indent}'{segment}' => [\n"));
        }
    }
    for depth in (0..segments.len().saturating_sub(1)).rev() {
        let indent = "    ".repeat(depth + 1);
        source.push_str(&format!("{indent}],\n"));
    }
    source.push_str("];\n");

    fs::write(dir.join(format!("{file}.php")), source).unwrap();
}

#[tokio::test]
async fn namespaced_lang_dir_yields_prefixed_completions_alongside_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // The project's own root catalogue — must still be scanned.
    write_catalogue(&root.join("lang"), "messages", "welcome", "Welcome");

    // A namespace whose catalogues live only under its registered dir,
    // never published to `lang/vendor/…` (the fixture shape:
    // legal-contractmanagement::contract-management.details.title).
    let module_dir = root.join("app/Legal/ContractManagement");
    write_catalogue(
        &module_dir.join("lang"),
        "contract-management",
        "details.title",
        "Vertragsdetails",
    );
    let provider = write_module_provider(&module_dir, "legal-contractmanagement");

    let backend = backend_with_module_provider(&root, provider).await;
    let keys = backend.get_all_translation_keys().await;
    let all_keys: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();

    assert!(
        all_keys.contains(&"messages.welcome"),
        "root catalogue should still be scanned, got {all_keys:?}"
    );
    assert!(
        all_keys.contains(&"legal-contractmanagement::contract-management.details.title"),
        "namespaced catalogue should be scanned and key-prefixed, got {all_keys:?}"
    );
}

#[tokio::test]
async fn namespace_only_project_still_completes() {
    // No root `lang/` at all — a project whose ONLY catalogues live under a
    // registered namespace used to get zero completions.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let module_dir = root.join("app/Legal/ContractManagement");
    write_catalogue(
        &module_dir.join("lang"),
        "contract-management",
        "details.title",
        "Vertragsdetails",
    );
    let provider = write_module_provider(&module_dir, "legal-contractmanagement");

    let backend = backend_with_module_provider(&root, provider).await;
    let keys = backend.get_all_translation_keys().await;
    let all_keys: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();

    assert_eq!(keys.len(), 1, "got {all_keys:?}");
    assert_eq!(
        keys[0].key,
        "legal-contractmanagement::contract-management.details.title"
    );
    assert!(
        keys[0].source.starts_with("legal-contractmanagement::"),
        "namespaced source label should carry the namespace too, got {:?}",
        keys[0].source
    );
}

#[test]
fn translation_call_context_passes_namespace_prefix_through_unmangled() {
    // The `::` in a namespaced prefix must survive into `StringContext.prefix`
    // verbatim — the completion filter does a plain `key.starts_with(prefix)`,
    // so any mangling here would silently break namespaced completion.
    let line = "__('legal-contractmanagement::contract-management.details.";
    let ctx = LaravelLanguageServer::get_translation_call_context(line, line.len() as u32)
        .expect("cursor sits inside a __() call");
    assert_eq!(
        ctx.prefix,
        "legal-contractmanagement::contract-management.details."
    );
}

#[tokio::test]
async fn translation_namespace_disappears_when_the_registration_is_removed() {
    // Negative control for the module-provider registration: the identical
    // fixture minus the loadTranslationsFrom call resolves nothing.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let module_dir = root.join("app/Legal/ContractManagement");
    write_catalogue(
        &module_dir.join("lang"),
        "contract-management",
        "details.title",
        "Vertragsdetails",
    );
    let providers = module_dir.join("Providers");
    fs::create_dir_all(&providers).unwrap();
    let provider = providers.join("ModuleServiceProvider.php");
    fs::write(
        &provider,
        "<?php\nclass ModuleServiceProvider {\n    public function boot(): void {}\n}\n",
    )
    .unwrap();

    let backend = backend_with_module_provider(&root, provider).await;
    assert!(
        backend.get_all_translation_keys().await.is_empty(),
        "no loadTranslationsFrom call, no namespaced keys"
    );
}

#[tokio::test]
async fn conflicting_namespace_registrations_resolve_last_registered_wins() {
    // Two module providers register the SAME namespace for different lang
    // dirs. Documented rule: among module providers the LAST-registered one
    // wins (matching the last-merged-wins config rule); a real
    // `app/Providers/` registration would override both, because the app
    // boots last.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    let alpha = root.join("app/Legal/Alpha");
    write_catalogue(&alpha.join("lang"), "shared", "origin", "from-alpha");
    let beta = root.join("app/Legal/Beta");
    write_catalogue(&beta.join("lang"), "shared", "origin", "from-beta");

    let mut providers = Vec::new();
    for module in [&alpha, &beta] {
        let dir = module.join("Providers");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ModuleServiceProvider.php");
        fs::write(
            &path,
            "<?php\nclass ModuleServiceProvider {\n    public function boot(): void {\n        $this->loadTranslationsFrom(__DIR__.'/../lang', 'legal-shared');\n    }\n}\n",
        )
        .unwrap();
        providers.push(path);
    }

    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.clone());
    backend
        .salsa
        .set_translation_provider_extras(providers)
        .await
        .unwrap();

    let keys = backend.get_all_translation_keys().await;
    let entry = keys
        .iter()
        .find(|k| k.key == "legal-shared::shared.origin")
        .expect("namespace resolves");
    assert_eq!(
        entry.value, "from-beta",
        "the later-registered module's directory wins"
    );
}
