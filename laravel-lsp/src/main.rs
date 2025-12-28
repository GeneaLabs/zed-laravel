use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use tokio::time::sleep;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use walkdir::WalkDir;

// Use the library crate for all modules
use laravel_lsp::config::find_project_root;
use laravel_lsp::middleware_parser::resolve_class_to_file;
use laravel_lsp::cache_manager::{CacheManager, RescanType, ScanResult, MiddlewareEntry, BindingEntry, CachedLaravelConfig, CachedEnvVars};

// Salsa 0.25 database - integrated via actor pattern for async compatibility
use laravel_lsp::salsa_impl::{
    SalsaActor, SalsaHandle, PatternAtPosition, LaravelConfigData,
    ViewReferenceData, ComponentReferenceData, DirectiveReferenceData,
    EnvReferenceData, ConfigReferenceData, LivewireReferenceData,
    MiddlewareReferenceData, TranslationReferenceData, AssetReferenceData, BindingReferenceData,
    RouteReferenceData, UrlReferenceData, ActionReferenceData,
};

// ============================================================================
// PART 1: Core Language Server Implementation
// ============================================================================

/// Extract middleware configuration class imports from PHP content
///
/// Parses `use` statements to find imported middleware classes (like
/// `Illuminate\Foundation\Configuration\Middleware`) and resolves them
/// to file paths for scanning.
fn extract_middleware_imports(content: &str, root: &Path) -> Vec<PathBuf> {
    use regex::Regex;
    use lazy_static::lazy_static;

    lazy_static! {
        // Match: use Illuminate\...\Middleware;
        // or: use Some\Namespace\Configuration\Middleware;
        static ref USE_RE: Regex = Regex::new(
            r#"use\s+((?:[A-Za-z0-9_\\]+\\)?(?:Configuration\\)?Middleware)\s*;"#
        ).unwrap();
    }

    let mut files = Vec::new();

    for cap in USE_RE.captures_iter(content) {
        if let Some(class_match) = cap.get(1) {
            let class_name = class_match.as_str();

            // Resolve the class to a file path using PSR-4 conventions
            if let Some(file_path) = resolve_class_to_vendor_file(class_name, root) {
                if file_path.exists() {
                    files.push(file_path);
                }
            }
        }
    }

    files
}

/// Resolve a class name to a vendor file path using PSR-4 conventions
fn resolve_class_to_vendor_file(class_name: &str, root: &Path) -> Option<PathBuf> {
    // Common namespace to directory mappings
    let mappings = [
        ("Illuminate\\", "vendor/laravel/framework/src/Illuminate/"),
        ("Laravel\\", "vendor/laravel/"),
        ("App\\", "app/"),
    ];

    for (namespace, dir) in &mappings {
        if class_name.starts_with(namespace) {
            let relative = class_name.strip_prefix(namespace)?;
            let file_path = root
                .join(dir)
                .join(relative.replace('\\', "/"))
                .with_extension("php");
            return Some(file_path);
        }
    }

    None
}

// Removed: Old cache structures (FileReferences, ParsedMatches, ReferenceCache)
// These have been replaced by the high-performance PerformanceCache system

/// Result of checking if a translation exists
struct TranslationCheck {
    /// Whether the translation exists
    exists: bool,
    /// Whether this is a dotted key (validation.required) vs text key ("Welcome")
    is_dotted_key: bool,
}

/// The main Laravel Language Server struct
/// This holds all the state for our LSP
#[derive(Clone)]
struct LaravelLanguageServer {
    /// LSP client for sending messages to the editor
    client: Client,
    /// Store document contents and versions for analysis (content, version)
    documents: Arc<RwLock<HashMap<Url, (String, i32)>>>,
    /// The root path of the Laravel project
    root_path: Arc<RwLock<Option<PathBuf>>>,
    /// Store diagnostics per file (for hover filtering)
    diagnostics: Arc<RwLock<HashMap<Url, Vec<Diagnostic>>>>,
    /// Pending debounced diagnostic tasks (uri -> task handle)
    pending_diagnostics: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Debounce delay for diagnostics in milliseconds (default: 200ms)
    debounce_delay_ms: u64,
    /// Salsa 0.25 database handle (runs on dedicated thread via actor pattern)
    salsa: SalsaHandle,
    /// Smart cache manager for middleware/bindings (mtime-based invalidation)
    cache: Arc<RwLock<Option<CacheManager>>>,
    /// Pending background rescans (debounced)
    pending_rescans: Arc<RwLock<HashSet<RescanType>>>,
    /// Handle for the rescan debounce timer
    rescan_debounce_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// File existence cache with TTL (path -> (exists, cached_at))
    /// This avoids blocking I/O in async context for file_exists checks
    file_exists_cache: Arc<RwLock<HashMap<PathBuf, (bool, Instant)>>>,
    /// Cached Laravel config to avoid repeated Salsa lookups
    cached_config: Arc<RwLock<Option<LaravelConfigData>>>,
    /// Track last goto_definition request per file for coalescing rapid requests
    /// Maps URI to (position, timestamp) - skip duplicate requests within coalesce window
    last_goto_request: Arc<RwLock<HashMap<Url, (Position, Instant)>>>,
    /// Track which root we've fully initialized for (to avoid re-initialization on file open)
    initialized_root: Arc<RwLock<Option<PathBuf>>>,
    /// Pending debounced Salsa updates per file (uri -> task handle)
    /// Used to debounce did_change events before updating Salsa
    pending_salsa_updates: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Configurable debounce delay for Salsa updates in milliseconds (default: 200ms)
    /// Can be configured via LSP settings: { "laravel": { "debounceMs": 200 } }
    salsa_debounce_ms: Arc<RwLock<u64>>,
}

/// Default Salsa debounce delay in milliseconds
const DEFAULT_SALSA_DEBOUNCE_MS: u64 = 200;

/// Settings structure for Laravel LSP configuration
/// Configured via: { "lsp": { "laravel-lsp": { "settings": { "laravel": { ... } } } } }
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LaravelSettings {
    /// Debounce delay for Salsa updates in milliseconds (default: 250)
    /// Lower values = faster updates but more CPU usage during typing
    /// Higher values = less CPU but slower feedback
    #[serde(default = "default_debounce_ms")]
    debounce_ms: u64,
}

fn default_debounce_ms() -> u64 {
    DEFAULT_SALSA_DEBOUNCE_MS
}

/// Wrapper for the full settings object from Zed
#[derive(Debug, Clone, serde::Deserialize, Default)]
struct LspSettings {
    #[serde(default)]
    laravel: LaravelSettings,
}

impl LaravelLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            root_path: Arc::new(RwLock::new(None)),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            pending_diagnostics: Arc::new(RwLock::new(HashMap::new())),
            debounce_delay_ms: 200,  // 200ms for diagnostics
            salsa: SalsaActor::spawn(),
            cache: Arc::new(RwLock::new(None)),
            pending_rescans: Arc::new(RwLock::new(HashSet::new())),
            rescan_debounce_handle: Arc::new(RwLock::new(None)),
            file_exists_cache: Arc::new(RwLock::new(HashMap::new())),
            cached_config: Arc::new(RwLock::new(None)),
            last_goto_request: Arc::new(RwLock::new(HashMap::new())),
            initialized_root: Arc::new(RwLock::new(None)),
            pending_salsa_updates: Arc::new(RwLock::new(HashMap::new())),
            salsa_debounce_ms: Arc::new(RwLock::new(DEFAULT_SALSA_DEBOUNCE_MS)),
        }
    }

    /// Update settings from LSP configuration
    async fn update_settings(&self, settings: &LspSettings) {
        let new_debounce = settings.laravel.debounce_ms;
        let old_debounce = *self.salsa_debounce_ms.read().await;

        if new_debounce != old_debounce {
            info!("⚙️  Updating Salsa debounce: {}ms → {}ms", old_debounce, new_debounce);
            *self.salsa_debounce_ms.write().await = new_debounce;
        }
    }

    /// Check if a position has a diagnostic (yellow squiggle).
    /// Returns true if there's a diagnostic at this position.
    async fn has_diagnostic_at_position(&self, uri: &Url, position: Position) -> bool {
        let diagnostics_guard = self.diagnostics.read().await;
        let Some(file_diagnostics) = diagnostics_guard.get(uri) else {
            return false;
        };

        // Check if any diagnostic range contains this position
        file_diagnostics.iter().any(|diagnostic| {
            let range = diagnostic.range;
            // Check if position is within the diagnostic range
            (position.line > range.start.line ||
             (position.line == range.start.line && position.character >= range.start.character)) &&
            (position.line < range.end.line ||
             (position.line == range.end.line && position.character <= range.end.character))
        })
    }

    /// Register config files with Salsa for incremental computation
    ///
    /// This reads the config file contents from disk and registers them
    /// with the Salsa actor. The Salsa system will then use these files
    /// for incremental config parsing and caching.
    async fn register_config_with_salsa(&self, root_path: &Path) {
        use std::fs;

        // Read composer.json
        let composer_json = fs::read_to_string(root_path.join("composer.json")).ok();

        // Read config/view.php
        let view_config = fs::read_to_string(root_path.join("config/view.php")).ok();

        // Read config/livewire.php
        let livewire_config = fs::read_to_string(root_path.join("config/livewire.php")).ok();

        // Register with Salsa
        if let Err(e) = self.salsa.register_config_files(
            root_path.to_path_buf(),
            composer_json,
            view_config,
            livewire_config,
        ).await {
            debug!("Failed to register config files with Salsa: {}", e);
        } else {
            info!("Laravel LSP: Config files registered with Salsa for incremental caching");
        }
    }

    /// Register project files with Salsa for reference finding
    ///
    /// This scans key directories (controllers, views, Livewire, routes) and
    /// registers all PHP/Blade files with Salsa. The Salsa system will then
    /// cache parsed patterns for efficient reference lookups.
    async fn register_project_files_with_salsa(&self, root_path: &Path) {
        let config = match self.get_cached_config().await {
            Some(c) => c,
            None => {
                debug!("Cannot register project files - no config available");
                return;
            }
        };

        // Get view paths from config
        let view_paths = config.view_paths.clone();

        // Get Livewire path from config
        let livewire_path = config.livewire_path.clone();

        // Register with Salsa
        if let Err(e) = self.salsa.register_project_files(
            root_path.to_path_buf(),
            vec![PathBuf::from("app/Http/Controllers")], // Default controller path
            view_paths,
            livewire_path,
            PathBuf::from("routes"),
        ).await {
            debug!("Failed to register project files with Salsa: {}", e);
        } else {
            info!("Laravel LSP: Project files registered with Salsa for reference finding");
        }
    }

    /// Register environment files directly with Salsa for parsing
    ///
    /// This registers raw .env file content with Salsa, which parses them
    /// using the tracked `parse_env_source` function. Salsa handles caching
    /// and incremental updates automatically.
    ///
    /// Priority: .env.example=0, .env.local=1, .env=2 (higher wins)
    async fn register_env_files_with_salsa(&self, root: &Path) {
        // Define env files with their priorities
        // Priority: 0=.env.example, 1=.env.local, 2=.env
        let env_files = [
            (root.join(".env.example"), 0u8),
            (root.join(".env.local"), 1u8),
            (root.join(".env"), 2u8),
        ];

        let documents = self.documents.read().await;
        let mut registered_count = 0;

        for (env_path, priority) in env_files {
            // Get content from editor buffer or disk
            let content = if let Ok(env_uri) = Url::from_file_path(&env_path) {
                if let Some((buffer_content, _version)) = documents.get(&env_uri) {
                    // Use editor buffer content (includes unsaved changes)
                    debug!("Laravel LSP: Registering .env from buffer: {:?}", env_path);
                    Some(buffer_content.clone())
                } else if env_path.exists() {
                    // Read from disk
                    debug!("Laravel LSP: Registering .env from disk: {:?}", env_path);
                    std::fs::read_to_string(&env_path).ok()
                } else {
                    None
                }
            } else if env_path.exists() {
                std::fs::read_to_string(&env_path).ok()
            } else {
                None
            };

            if let Some(text) = content {
                if let Err(e) = self.salsa.register_env_source(
                    env_path.clone(),
                    text,
                    priority,
                ).await {
                    debug!("Failed to register env file {:?} with Salsa: {}", env_path, e);
                } else {
                    registered_count += 1;
                }
            }
        }

        if registered_count > 0 {
            info!("Laravel LSP: {} env files registered with Salsa", registered_count);
        }
    }

    /// Register service provider files directly with Salsa for parsing
    ///
    /// This scans for service provider files and registers their raw content
    /// with Salsa, which parses them using the tracked `parse_service_provider_source`
    /// function. Salsa handles caching and incremental updates automatically.
    ///
    /// Priority: framework=0, packages=1, app=2 (higher wins)
    async fn register_service_provider_files_with_salsa(&self, root: &Path) {

        let documents = self.documents.read().await;
        let mut registered_count = 0;

        // Priority 0: Framework providers
        let framework_path = root.join("vendor/laravel/framework/src/Illuminate");
        if framework_path.exists() {
            for entry in WalkDir::new(&framework_path)
                .max_depth(10)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy().ends_with("ServiceProvider.php")
                    })
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self.salsa.register_service_provider_source(
                            path.to_path_buf(),
                            content,
                            0, // framework priority
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        // Priority 1: Package providers
        let vendor_path = root.join("vendor");
        if vendor_path.exists() {
            for entry in WalkDir::new(&vendor_path)
                .max_depth(6)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                // Skip framework (already done with priority 0)
                if path.starts_with(&framework_path) {
                    continue;
                }
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy().ends_with("ServiceProvider.php")
                    })
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self.salsa.register_service_provider_source(
                            path.to_path_buf(),
                            content,
                            1, // package priority
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        // Priority 2: Application providers (app/Providers/)
        let app_providers_path = root.join("app/Providers");
        if app_providers_path.exists() {
            for entry in WalkDir::new(&app_providers_path)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                {
                    // Check if file is open in editor
                    let content = if let Ok(uri) = Url::from_file_path(path) {
                        if let Some((buffer_content, _)) = documents.get(&uri) {
                            buffer_content.clone()
                        } else {
                            std::fs::read_to_string(path).unwrap_or_default()
                        }
                    } else {
                        std::fs::read_to_string(path).unwrap_or_default()
                    };

                    if !content.is_empty()
                        && self
                            .salsa
                            .register_service_provider_source(
                                path.to_path_buf(),
                                content,
                                2, // app priority
                                root.to_path_buf(),
                            )
                            .await
                            .is_ok()
                    {
                        registered_count += 1;
                    }
                }
            }
        }

        // Priority 2: bootstrap/app.php (Laravel 11+)
        let bootstrap_app = root.join("bootstrap/app.php");
        if bootstrap_app.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&bootstrap_app) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
            };

            if !content.is_empty() {
                // First, extract and scan imported middleware configuration classes
                // This handles Laravel's default middleware aliases (auth, guest, etc.)
                for imported_file in extract_middleware_imports(&content, root) {
                    if let Ok(imported_content) = std::fs::read_to_string(&imported_file) {
                        if self.salsa.register_service_provider_source(
                            imported_file,
                            imported_content,
                            0, // framework priority (can be overridden by app)
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }

                // Then scan bootstrap/app.php itself for user-defined middleware
                if self
                    .salsa
                    .register_service_provider_source(
                        bootstrap_app,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
                {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: app/Http/Kernel.php (Laravel 10)
        let kernel_path = root.join("app/Http/Kernel.php");
        if kernel_path.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&kernel_path) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&kernel_path).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&kernel_path).unwrap_or_default()
            };

            if !content.is_empty()
                && self
                    .salsa
                    .register_service_provider_source(
                        kernel_path,
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    )
                    .await
                    .is_ok()
            {
                registered_count += 1;
            }
        }

        if registered_count > 0 {
            info!("Laravel LSP: {} service provider files registered with Salsa", registered_count);
        }
    }

    /// Load ALL cached data directly into memory (NO Salsa calls - instant)
    /// Returns the list of rescans needed for background processing
    async fn load_cache_data(&self, root: &Path) -> Vec<RescanType> {
        let start = std::time::Instant::now();

        // Load cache from disk
        let cache = CacheManager::load(root);

        if cache.has_cached_data() {
            // 1. Store cached Laravel config directly in memory (bypasses Salsa)
            if let Some(cached_config) = cache.get_laravel_config() {
                info!("📋 Loading cached Laravel config: {} view paths, root: {:?}",
                    cached_config.view_paths.len(), cached_config.root);
                let config_data = LaravelConfigData {
                    root: cached_config.root.clone(),
                    view_paths: cached_config.view_paths.clone(),
                    component_paths: cached_config.component_paths.clone(),
                    livewire_path: cached_config.livewire_path.clone(),
                    has_livewire: cached_config.has_livewire,
                };
                // Store directly in memory - no Salsa channel call!
                *self.cached_config.write().await = Some(config_data);

                // Update root_path to the cached config's root (the actual Laravel project)
                // and mark it as initialized to prevent re-discovery on file open
                let actual_root = cached_config.root.clone();
                info!("📂 Setting actual Laravel root to {:?} from cache", actual_root);
                *self.root_path.write().await = Some(actual_root.clone());
                *self.initialized_root.write().await = Some(actual_root);
            }

            // 2-4: Register middleware/bindings/env with Salsa in background
            // These are needed for goto but not for basic diagnostics
            let middleware_count = cache.get_all_middleware().len();
            let binding_count = cache.get_all_bindings().len();
            let env_count = cache.get_env_vars().map(|e| e.variables.len()).unwrap_or(0);
            info!("📦 Queuing {} middleware, {} bindings, {} env vars for background registration",
                middleware_count, binding_count, env_count);

            // Spawn background registration (doesn't block initialize)
            let salsa = self.salsa.clone();
            let middleware_entries: Vec<_> = cache.get_all_middleware()
                .into_iter()
                .map(|(alias, entry)| (alias, entry.class, entry.class_file, entry.source_file, entry.line))
                .collect();
            let binding_entries: Vec<_> = cache.get_all_bindings()
                .into_iter()
                .map(|(name, entry)| (name, entry.class, entry.binding_type, entry.class_file, entry.source_file, entry.line))
                .collect();
            let env_vars = cache.get_env_vars().map(|e| e.variables.clone());
            let cached_config_for_salsa = cache.get_laravel_config().map(|c| LaravelConfigData {
                root: c.root.clone(),
                view_paths: c.view_paths.clone(),
                component_paths: c.component_paths.clone(),
                livewire_path: c.livewire_path.clone(),
                has_livewire: c.has_livewire,
            });

            tokio::spawn(async move {
                // Register with Salsa in background for incremental updates
                if let Some(config) = cached_config_for_salsa {
                    let _ = salsa.register_cached_config(config).await;
                }
                if let Some(vars) = env_vars {
                    let _ = salsa.register_cached_env_vars(vars).await;
                }
                let _ = salsa.register_cached_middleware_batch(middleware_entries).await;
                let _ = salsa.register_cached_binding_batch(binding_entries).await;
                info!("✅ Background Salsa registration complete");
            });
        }

        // Check what needs rescanning before storing cache
        let needs_rescans = cache.get_needed_rescans();

        // Store cache manager
        *self.cache.write().await = Some(cache);

        info!("⚡ Cache loaded in {:?}", start.elapsed());

        if needs_rescans.is_empty() {
            info!("✅ Cache is valid, no rescans needed");
        } else {
            info!("🔄 Will queue background rescans: {:?}", needs_rescans);
        }

        needs_rescans
    }

    /// Run background rescans and save cache (called from initialized())
    async fn run_background_rescans(&self, root: &Path, needs_rescans: Vec<RescanType>) {
        for rescan_type in needs_rescans {
            match rescan_type {
                RescanType::Vendor => self.rescan_vendor_providers(root).await,
                RescanType::App => self.rescan_app_providers(root).await,
                RescanType::NodeModules => self.rescan_node_modules(root).await,
            }
        }

        // Save updated cache
        if let Some(ref cache) = *self.cache.read().await {
            if let Err(e) = cache.save() {
                warn!("Failed to save cache: {}", e);
            }
        }

        // Re-validate open documents with new data
        self.revalidate_open_documents().await;
    }

    /// Rescan vendor directory (framework + packages)
    async fn rescan_vendor_providers(&self, root: &Path) {
        info!("🔍 Rescanning vendor providers...");
        let start = std::time::Instant::now();

        let documents = self.documents.read().await;
        let mut registered_count = 0;
        let mut middleware_count = 0;
        let mut bindings_count = 0;

        // Priority 0: Framework providers
        let framework_path = root.join("vendor/laravel/framework/src/Illuminate");
        if framework_path.exists() {
            for entry in WalkDir::new(&framework_path)
                .max_depth(10)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy().ends_with("ServiceProvider.php")
                    })
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self.salsa.register_service_provider_source(
                            path.to_path_buf(),
                            content,
                            0, // framework priority
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        // Priority 1: Package providers
        let vendor_path = root.join("vendor");
        if vendor_path.exists() {
            for entry in WalkDir::new(&vendor_path)
                .max_depth(6)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                // Skip framework (already done with priority 0)
                if path.starts_with(&framework_path) {
                    continue;
                }
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "php")
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy().ends_with("ServiceProvider.php")
                    })
                {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if self.salsa.register_service_provider_source(
                            path.to_path_buf(),
                            content,
                            1, // package priority
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }
            }
        }

        drop(documents);

        // Get counts for logging (cache population happens in execute_pending_rescans)
        if let Ok(all_mw) = self.salsa.get_all_parsed_middleware().await {
            middleware_count = all_mw.len();
        }
        if let Ok(all_bindings) = self.salsa.get_all_parsed_bindings().await {
            bindings_count = all_bindings.len();
        }

        // Update mtime (cache data population happens in populate_cache_from_salsa)
        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("composer.lock");
        }

        let duration = start.elapsed();
        info!("✅ Vendor rescan complete: {} providers, {} middleware, {} bindings in {:?}",
            registered_count, middleware_count, bindings_count, duration);
    }

    /// Rescan app providers (app/Providers + bootstrap/app.php)
    async fn rescan_app_providers(&self, root: &Path) {
        info!("🔍 Rescanning app providers...");
        let start = std::time::Instant::now();

        let documents = self.documents.read().await;
        let mut registered_count = 0;

        // Priority 2: Application providers (app/Providers/)
        let app_providers_path = root.join("app/Providers");
        if app_providers_path.exists() {
            for entry in WalkDir::new(&app_providers_path)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                    let content = if let Ok(uri) = Url::from_file_path(path) {
                        if let Some((buffer_content, _)) = documents.get(&uri) {
                            buffer_content.clone()
                        } else {
                            std::fs::read_to_string(path).unwrap_or_default()
                        }
                    } else {
                        std::fs::read_to_string(path).unwrap_or_default()
                    };

                    if !content.is_empty() && self.salsa.register_service_provider_source(
                        path.to_path_buf(),
                        content,
                        2, // app priority
                        root.to_path_buf(),
                    ).await.is_ok() {
                        registered_count += 1;
                    }
                }
            }
        }

        // Priority 2: bootstrap/app.php (Laravel 11+)
        let bootstrap_app = root.join("bootstrap/app.php");
        if bootstrap_app.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&bootstrap_app) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&bootstrap_app).unwrap_or_default()
            };

            if !content.is_empty() {
                // First, extract and scan imported middleware configuration classes
                for imported_file in extract_middleware_imports(&content, root) {
                    if let Ok(imported_content) = std::fs::read_to_string(&imported_file) {
                        if self.salsa.register_service_provider_source(
                            imported_file,
                            imported_content,
                            0, // framework priority
                            root.to_path_buf(),
                        ).await.is_ok() {
                            registered_count += 1;
                        }
                    }
                }

                // Then scan bootstrap/app.php itself
                if self.salsa.register_service_provider_source(
                    bootstrap_app,
                    content,
                    2, // app priority
                    root.to_path_buf(),
                ).await.is_ok() {
                    registered_count += 1;
                }
            }
        }

        // Priority 2: app/Http/Kernel.php (Laravel 10)
        let kernel_path = root.join("app/Http/Kernel.php");
        if kernel_path.exists() {
            let content = if let Ok(uri) = Url::from_file_path(&kernel_path) {
                if let Some((buffer_content, _)) = documents.get(&uri) {
                    buffer_content.clone()
                } else {
                    std::fs::read_to_string(&kernel_path).unwrap_or_default()
                }
            } else {
                std::fs::read_to_string(&kernel_path).unwrap_or_default()
            };

            if !content.is_empty() && self.salsa.register_service_provider_source(
                kernel_path,
                content,
                2, // app priority
                root.to_path_buf(),
            ).await.is_ok() {
                registered_count += 1;
            }
        }

        drop(documents);

        // Update cache
        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("bootstrap/app.php");
            cache.update_mtime_glob("app/Providers/*.php");
        }

        let duration = start.elapsed();
        info!("✅ App rescan complete: {} providers in {:?}", registered_count, duration);
    }

    /// Rescan node_modules (for Flux, etc.)
    async fn rescan_node_modules(&self, _root: &Path) {
        info!("🔍 Rescanning node_modules...");
        let start = std::time::Instant::now();

        // TODO: Scan for Flux components in node_modules
        // For now, just update the mtime

        let mut cache_guard = self.cache.write().await;
        if let Some(ref mut cache) = *cache_guard {
            cache.update_mtime("package-lock.json");
            cache.update_mtime("yarn.lock");
            cache.update_mtime("pnpm-lock.yaml");
        }

        let duration = start.elapsed();
        info!("✅ Node modules rescan complete in {:?}", duration);
    }

    /// Queue a background rescan (debounced)
    async fn queue_background_rescan(&self, rescan_type: RescanType) {
        // Add to pending set
        self.pending_rescans.write().await.insert(rescan_type);

        // Cancel existing debounce timer
        if let Some(handle) = self.rescan_debounce_handle.write().await.take() {
            handle.abort();
        }

        // Start new debounce timer (500ms)
        let server = self.clone_for_spawn();
        let handle = tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            server.execute_pending_rescans().await;
        });

        *self.rescan_debounce_handle.write().await = Some(handle);
    }

    /// Execute all pending rescans
    async fn execute_pending_rescans(&self) {
        let pending: Vec<RescanType> = self.pending_rescans.write().await.drain().collect();

        if pending.is_empty() {
            return;
        }

        let root = self.root_path.read().await.clone();
        let Some(root) = root else {
            warn!("Cannot execute rescans: no root path");
            return;
        };

        info!("🔄 Executing pending rescans: {:?}", pending);

        for rescan_type in &pending {
            match rescan_type {
                RescanType::Vendor => self.rescan_vendor_providers(&root).await,
                RescanType::App => self.rescan_app_providers(&root).await,
                RescanType::NodeModules => self.rescan_node_modules(&root).await,
            }
        }

        // Populate cache with ALL parsed middleware/bindings AFTER all rescans complete
        // This ensures we capture middleware from both vendor and app sources
        self.populate_cache_from_salsa().await;

        // Save cache
        if let Some(ref cache) = *self.cache.read().await {
            if let Err(e) = cache.save() {
                warn!("Failed to save cache: {}", e);
            } else {
                info!("💾 Cache saved successfully");
            }
        }

        // Re-validate open documents
        self.revalidate_open_documents().await;
    }

    /// Populate cache with all data from Salsa (config, env, middleware, bindings)
    async fn populate_cache_from_salsa(&self) {
        let mut cache_guard = self.cache.write().await;
        let Some(ref mut cache) = *cache_guard else {
            return;
        };

        // 1. Cache Laravel config
        if let Ok(Some(config)) = self.salsa.get_laravel_config().await {
            let cached_config = CachedLaravelConfig {
                root: config.root.clone(),
                view_paths: config.view_paths.clone(),
                component_paths: config.component_paths.clone(),
                livewire_path: config.livewire_path.clone(),
                has_livewire: config.has_livewire,
            };
            info!("📋 Caching Laravel config: {} view paths", config.view_paths.len());
            cache.set_laravel_config(cached_config);
        }

        // 2. Cache env variables
        if let Ok(env_vars) = self.salsa.get_all_parsed_env_vars().await {
            let mut variables = std::collections::HashMap::new();
            for var in &env_vars {
                variables.insert(var.name.clone(), var.value.clone());
            }
            debug!("Caching {} env variables", variables.len());
            cache.set_env_vars(CachedEnvVars { variables });
        }

        // 3. Cache middleware
        if let Ok(all_mw) = self.salsa.get_all_parsed_middleware().await {
            let mut vendor_scan = ScanResult::default();
            for mw in &all_mw {
                vendor_scan.middleware.insert(mw.alias.clone(), MiddlewareEntry {
                    class: mw.class_name.clone(),
                    class_file: mw.file_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    source_file: Some(mw.source_file.to_string_lossy().into_owned()),
                    line: mw.source_line,
                });
            }
            info!("📦 Caching {} middleware aliases", all_mw.len());
            cache.set_vendor_scan(vendor_scan);
        }

        // 4. Cache bindings
        if let Ok(all_bindings) = self.salsa.get_all_parsed_bindings().await {
            let mut app_scan = ScanResult::default();
            for binding in &all_bindings {
                app_scan.bindings.insert(binding.abstract_name.clone(), BindingEntry {
                    class: binding.concrete_class.clone(),
                    binding_type: format!("{:?}", binding.binding_type),
                    class_file: binding.file_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    source_file: Some(binding.source_file.to_string_lossy().into_owned()),
                    line: binding.source_line,
                });
            }
            info!("📦 Caching {} bindings", all_bindings.len());
            cache.set_app_scan(app_scan);
        }
    }

    /// Re-validate all open documents after a rescan
    async fn revalidate_open_documents(&self) {
        let documents = self.documents.read().await;
        let uris: Vec<Url> = documents.keys().cloned().collect();
        drop(documents);

        for uri in uris {
            if let Some((content, _)) = self.documents.read().await.get(&uri).cloned() {
                self.validate_and_publish_diagnostics(&uri, &content).await;
            }
        }
    }

    /// Try to discover Laravel config from a file path
    ///
    /// This implements a hybrid discovery strategy:
    /// - Always tries to find Laravel root from the opened file
    /// - Updates config if discovered root is more specific or file is outside current root
    /// - This handles both nested Laravel projects and files outside initial workspace
    async fn try_discover_from_file(&self, file_path: &Path) {
        // Always try to find the Laravel project root from this file
        let Some(discovered_root) = find_project_root(file_path) else {
            debug!("Could not find Laravel project root from file: {:?}", file_path);
            return;
        };

        // Check if we've already fully initialized for this root - if so, skip everything
        {
            let init_root = self.initialized_root.read().await;
            if let Some(ref init) = *init_root {
                if init == &discovered_root {
                    debug!("Already initialized for root {:?}, skipping", discovered_root);
                    return;
                }
            }
        }

        // Get current root to compare
        let current_root_guard = self.root_path.read().await;
        let current_root = current_root_guard.as_ref();

        // Decide if we should use the discovered root
        let should_update = match current_root {
            None => {
                // No current root, so always use discovered
                debug!("No current root, using discovered root: {:?}", discovered_root);
                true
            }
            Some(current) => {
                // Check if file is outside current root
                let file_outside_root = !file_path.starts_with(current);

                // Check if discovered root is more specific (nested within current root)
                let more_specific = discovered_root.starts_with(current) && discovered_root != *current;

                if file_outside_root {
                    info!(
                        "File {:?} is outside current root {:?}, switching to discovered root: {:?}",
                        file_path, current, discovered_root
                    );
                    true
                } else if more_specific {
                    info!(
                        "Discovered more specific Laravel root {:?} (current: {:?})",
                        discovered_root, current
                    );
                    true
                } else {
                    // File is within current root and discovered isn't more specific
                    debug!("Keeping current root {:?} for file {:?}", current, file_path);
                    false
                }
            }
        };

        drop(current_root_guard);

        if !should_update {
            return;
        }

        info!("Updating Laravel project root to: {:?}", discovered_root);

        // Store the new root path
        *self.root_path.write().await = Some(discovered_root.clone());

        // Register config files with Salsa for incremental computation
        self.register_config_with_salsa(&discovered_root).await;

        // Verify config is available (checks memory cache first)
        match self.get_cached_config().await {
            Some(config) => {
                info!("Laravel configuration available: {} view paths", config.view_paths.len());

                // Register project files with Salsa for reference finding
                self.register_project_files_with_salsa(&discovered_root).await;

                // Re-validate all open documents since config changed (view paths, component paths, etc.)
                info!("Laravel LSP: Re-validating all open documents due to config change");
                let documents = self.documents.read().await;
                for (doc_uri, (doc_text, _version)) in documents.iter() {
                    self.validate_and_publish_diagnostics(doc_uri, doc_text).await;
                }
            }
            None => {
                info!("Failed to get Laravel config");
            }
        }

        // Initialize service provider registry with Salsa
        info!("========================================");
        info!("🛡️  Initializing service provider registry from root: {:?}", discovered_root);
        info!("========================================");
        self.register_service_provider_files_with_salsa(&discovered_root).await;

        // Initialize environment variables with Salsa
        info!("========================================");
        info!("📁 Initializing env cache from root: {:?}", discovered_root);
        info!("========================================");
        self.register_env_files_with_salsa(&discovered_root).await;

        // Mark this root as fully initialized
        *self.initialized_root.write().await = Some(discovered_root);
    }

    /// Refresh the env cache and re-validate open documents
    async fn refresh_env_cache_from_buffers(&self, root: &Path) {
        // Register with Salsa (handles buffer vs disk automatically)
        self.register_env_files_with_salsa(root).await;

        // Re-validate all open PHP documents since env vars changed
        info!("Laravel LSP: Re-validating all open documents due to .env change");
        let documents = self.documents.read().await;
        for (doc_uri, (doc_text, _version)) in documents.iter() {
            if doc_uri.path().ends_with(".php") {
                self.validate_and_publish_diagnostics(doc_uri, doc_text).await;
            }
        }
    }

    /// Check if a file exists either in editor buffers (unsaved) or on disk
    async fn file_exists(&self, path: &PathBuf) -> bool {
        // First check if file is open in editor (includes unsaved files)
        if let Ok(uri) = Url::from_file_path(path) {
            let documents = self.documents.read().await;
            if documents.contains_key(&uri) {
                return true;
            }
        }

        // Fall back to disk check
        path.exists()
    }

    /// Check if a file exists with async I/O and TTL caching
    ///
    /// This method improves goto_definition performance by:
    /// 1. Checking editor buffers first (for unsaved files)
    /// 2. Using a TTL cache (5 seconds) to avoid repeated disk I/O
    /// 3. Using async file I/O (tokio::fs) to avoid blocking the runtime
    async fn file_exists_cached(&self, path: &PathBuf) -> bool {
        const CACHE_TTL: Duration = Duration::from_secs(5);

        // First check if file is open in editor (includes unsaved files)
        if let Ok(uri) = Url::from_file_path(path) {
            let documents = self.documents.read().await;
            if documents.contains_key(&uri) {
                return true;
            }
        }

        // Check TTL cache
        {
            let cache = self.file_exists_cache.read().await;
            if let Some((exists, cached_at)) = cache.get(path) {
                if cached_at.elapsed() < CACHE_TTL {
                    return *exists;
                }
            }
        }

        // Cache miss - check disk asynchronously
        let exists = tokio::fs::metadata(path).await.is_ok();

        // Update cache
        self.file_exists_cache.write().await.insert(path.clone(), (exists, Instant::now()));

        exists
    }

    /// Get Laravel config with local caching
    ///
    /// This avoids repeated Salsa lookups on every goto_definition request.
    /// Cache is invalidated when config files change (in did_save).
    async fn get_cached_config(&self) -> Option<LaravelConfigData> {
        // Return cached config if available
        if let Some(config) = self.cached_config.read().await.clone() {
            return Some(config);
        }

        // Fetch from Salsa and cache
        match self.salsa.get_laravel_config().await {
            Ok(Some(config)) => {
                *self.cached_config.write().await = Some(config.clone());
                Some(config)
            }
            _ => None,
        }
    }

    /// Invalidate the local config cache
    /// Call this when config files change (composer.json, config/*.php)
    async fn invalidate_config_cache(&self) {
        *self.cached_config.write().await = None;
    }

    /// Get middleware from cache first, then Salsa
    /// Returns (class_name, class_file, source_file, source_line)
    /// - class_file: for checking if the middleware class exists
    /// - source_file + source_line: for navigation to alias declaration
    async fn get_cached_middleware(&self, name: &str) -> Option<(String, Option<PathBuf>, Option<PathBuf>, Option<u32>)> {
        // First check disk cache (instant)
        if let Some(ref cache) = *self.cache.read().await {
            let all_middleware = cache.get_all_middleware();
            if let Some(entry) = all_middleware.get(name) {
                return Some((
                    entry.class.clone(),
                    entry.class_file.as_ref().map(PathBuf::from),
                    entry.source_file.as_ref().map(PathBuf::from),
                    Some(entry.line),
                ));
            }
        }

        // Fall back to Salsa (may not be ready yet)
        if let Ok(Some(mw_data)) = self.salsa.get_parsed_middleware(name.to_string()).await {
            return Some((
                mw_data.class_name.clone(),
                mw_data.file_path.clone(),
                Some(mw_data.source_file.clone()),
                Some(mw_data.source_line),
            ));
        }

        None
    }

    /// Get binding from cache first, then Salsa
    /// Returns (class_name, class_file, source_file, source_line)
    /// - class_file: for checking if the concrete class exists
    /// - source_file + source_line: for navigation to binding declaration
    async fn get_cached_binding(&self, name: &str) -> Option<(String, Option<PathBuf>, Option<PathBuf>, Option<u32>)> {
        // First check disk cache (instant)
        if let Some(ref cache) = *self.cache.read().await {
            let all_bindings = cache.get_all_bindings();
            if let Some(entry) = all_bindings.get(name) {
                return Some((
                    entry.class.clone(),
                    entry.class_file.as_ref().map(PathBuf::from),
                    entry.source_file.as_ref().map(PathBuf::from),
                    Some(entry.line),
                ));
            }
        }

        // Fall back to Salsa (may not be ready yet)
        if let Ok(Some(binding_data)) = self.salsa.get_parsed_binding(name.to_string()).await {
            return Some((
                binding_data.concrete_class.clone(),
                binding_data.file_path.clone(),
                Some(binding_data.source_file.clone()),
                Some(binding_data.source_line),
            ));
        }

        None
    }

    /// Clear the file exists cache
    /// Call this periodically or when files change significantly
    async fn clear_file_exists_cache(&self) {
        self.file_exists_cache.write().await.clear();
    }

    // ========================================================================
    // Debounced Salsa Updates (Cache Invalidation Architecture)
    // ========================================================================

    /// Queue a debounced Salsa update for a file
    ///
    /// This is the core of the cache invalidation architecture:
    /// `did_change(file) → Debounce (configurable) → Update Salsa input → Queries recompute → UI updates`
    ///
    /// The debounce prevents excessive Salsa updates during rapid typing.
    /// After the debounce delay (default 250ms, configurable via settings),
    /// the file is updated in Salsa which triggers incremental recomputation
    /// of all affected queries.
    async fn queue_salsa_update(&self, uri: Url, content: String, version: i32) {
        let debounce_ms = *self.salsa_debounce_ms.read().await;
        let debounce_delay = Duration::from_millis(debounce_ms);

        // Cancel any existing pending Salsa update for this file
        if let Some(handle) = self.pending_salsa_updates.write().await.remove(&uri) {
            handle.abort();
        }

        // Clone values needed for the async task
        let uri_for_spawn = uri.clone();
        let server = self.clone_for_spawn();

        // Spawn a task that updates Salsa after debounce delay
        let handle = tokio::spawn(async move {
            // Wait for the debounce delay
            sleep(debounce_delay).await;

            debug!("⏰ Salsa debounce expired for {} - updating Salsa", uri_for_spawn);

            // Execute the Salsa update based on file type
            server.execute_salsa_update(&uri_for_spawn, &content, version).await;
        });

        // Store the task handle so we can cancel it if needed
        self.pending_salsa_updates.write().await.insert(uri, handle);
    }

    /// Execute a Salsa update based on file type
    ///
    /// Determines the file type and calls the appropriate Salsa update method:
    /// - SourceFile: PHP and Blade files (pattern extraction)
    /// - ConfigFile: config/*.php, composer.json (view paths, namespaces)
    /// - EnvFile: .env, .env.local, .env.example (environment variables)
    /// - ServiceProviderFile: bootstrap/app.php, Providers/*.php (middleware, bindings)
    async fn execute_salsa_update(&self, uri: &Url, content: &str, version: i32) {
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return,
        };

        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let path_str = path.to_string_lossy();

        // Get root path for service provider registration
        let root_path = self.root_path.read().await.clone();

        // Determine file type and update appropriate Salsa input
        if filename == "app.php" && path_str.contains("bootstrap") {
            // bootstrap/app.php - Service provider file (middleware aliases)
            if let Some(root) = root_path {
                debug!("📦 Updating Salsa: ServiceProviderFile (bootstrap/app.php)");
                if let Err(e) = self.salsa.register_service_provider_source(
                    path.clone(),
                    content.to_string(),
                    2, // priority: app = 2
                    root,
                ).await {
                    debug!("Failed to update service provider in Salsa: {}", e);
                }
            }
        } else if path_str.contains("app/Providers") && filename.ends_with(".php") {
            // App service provider - Service provider file
            if let Some(root) = root_path {
                debug!("📦 Updating Salsa: ServiceProviderFile ({})", filename);
                if let Err(e) = self.salsa.register_service_provider_source(
                    path.clone(),
                    content.to_string(),
                    2, // priority: app = 2
                    root,
                ).await {
                    debug!("Failed to update service provider in Salsa: {}", e);
                }
            }
        } else if filename.starts_with(".env") {
            // Env file (.env, .env.local, .env.example)
            let priority = match filename {
                ".env" => 2,
                ".env.local" => 1,
                _ => 0, // .env.example
            };
            debug!("📦 Updating Salsa: EnvFile ({}, priority={})", filename, priority);
            if let Err(e) = self.salsa.register_env_source(
                path.clone(),
                content.to_string(),
                priority,
            ).await {
                debug!("Failed to update env file in Salsa: {}", e);
            }
        } else if path_str.contains("/config/") && filename.ends_with(".php") {
            // Config file (config/*.php)
            debug!("📦 Updating Salsa: ConfigFile ({})", filename);
            if let Err(e) = self.salsa.update_config_file(path.clone(), content.to_string()).await {
                debug!("Failed to update config file in Salsa: {}", e);
            }
            // Also invalidate the cached config so next lookup refetches
            *self.cached_config.write().await = None;
        } else if filename == "composer.json" {
            // composer.json - Config file
            debug!("📦 Updating Salsa: ConfigFile (composer.json)");
            if let Err(e) = self.salsa.update_config_file(path.clone(), content.to_string()).await {
                debug!("Failed to update config file in Salsa: {}", e);
            }
            // Also invalidate the cached config so next lookup refetches
            *self.cached_config.write().await = None;
        } else if filename.ends_with(".php") || filename.ends_with(".blade.php") {
            // Source file (PHP or Blade) - pattern extraction
            debug!("📦 Updating Salsa: SourceFile ({})", filename);
            if let Err(e) = self.salsa.update_file(path.clone(), version, content.to_string()).await {
                debug!("Failed to update source file in Salsa: {}", e);
            }
        }

        // After Salsa update, re-run diagnostics for this file
        // This ensures diagnostics reflect the latest Salsa state
        self.validate_and_publish_diagnostics(uri, content).await;
    }

    // ========================================================================
    // Tree-sitter-based helper functions
    // ========================================================================

    /// Extract view name from directive arguments
    /// e.g., "('layouts.app')" → "layouts.app"
    fn extract_view_from_directive_args(args: &str) -> Option<String> {
        // Remove parentheses and quotes
        let trimmed = args.trim().trim_matches('(').trim_matches(')').trim();
        let unquoted = trimmed.trim_matches('\'').trim_matches('"');

        if !unquoted.is_empty() && !unquoted.contains(',') {
            Some(unquoted.to_string())
        } else {
            None
        }
    }

    /// Convert kebab-case to PascalCase
    /// e.g., "user-profile" → "UserProfile"
    fn kebab_to_pascal_case(s: &str) -> String {
        s.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                }
            })
            .collect()
    }

    // ========================================================================
    // Translation validation helpers
    // ========================================================================

    /// Check if a translation file exists for the given key
    ///
    /// Dotted keys like "validation.required" look in lang/en/validation.php
    /// Text keys like "Welcome to our app" look in lang/en.json
    fn check_translation_file(root: &Path, translation_key: &str) -> TranslationCheck {
        let is_dotted_key = translation_key.contains('.') && !translation_key.contains(' ');
        let is_multi_word = translation_key.contains(' ');

        let mut exists = false;

        if is_multi_word || (!is_dotted_key && !translation_key.contains('.')) {
            // Text key: check JSON files for the KEY, not just file existence
            let json_paths = [
                root.join("lang/en.json"),
                root.join("resources/lang/en.json"),
            ];

            for json_path in &json_paths {
                if json_path.exists() {
                    // Parse JSON and check if key exists
                    if let Ok(content) = std::fs::read_to_string(&json_path) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                            if json.get(translation_key).is_some() {
                                exists = true;
                                break;
                            }
                        }
                    }
                }
            }
        } else if is_dotted_key {
            // Dotted key: check PHP file based on first segment
            let parts: Vec<&str> = translation_key.split('.').collect();
            if !parts.is_empty() {
                let file_name = parts[0];
                let php_paths = [
                    root.join("lang/en").join(format!("{}.php", file_name)),
                    root.join("resources/lang/en").join(format!("{}.php", file_name)),
                ];

                for php_path in &php_paths {
                    if php_path.exists() {
                        exists = true;
                        break;
                    }
                }
            }
        }

        TranslationCheck {
            exists,
            is_dotted_key,
        }
    }

    /// Create a diagnostic for a missing translation
    ///
    /// - `dotted_severity`: Severity for dotted keys (ERROR in PHP, WARNING in @lang)
    /// - Text keys always get INFORMATION severity
    fn create_translation_diagnostic(
        translation_key: &str,
        check: &TranslationCheck,
        line: u32,
        column: u32,
        end_column: u32,
        dotted_severity: DiagnosticSeverity,
    ) -> Diagnostic {
        let (severity, message) = if check.is_dotted_key {
            (
                dotted_severity,
                format!(
                    "Translation not found for '{}'",
                    translation_key
                )
            )
        } else {
            (
                DiagnosticSeverity::INFORMATION,
                format!(
                    "Translation not found, displayed as '{}'",
                    translation_key
                )
            )
        };

        Diagnostic {
            range: Range {
                start: Position { line, character: column },
                end: Position { line, character: end_column },
            },
            severity: Some(severity),
            code: None,
            source: Some("laravel-lsp".to_string()),
            message,
            related_information: None,
            tags: None,
            code_description: None,
            data: None,
        }
    }

    // ========================================================================
    // Salsa-based helper functions (for cached pattern data)
    // ========================================================================

    /// Create LocationLink for a view reference from Salsa data
    async fn create_view_location_from_salsa(&self, view: &ViewReferenceData) -> Option<GotoDefinitionResponse> {
        let config = self.get_cached_config().await?;
        let possible_paths = config.resolve_view_path(&view.name);

        for path in possible_paths {
            if self.file_exists_cached(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    let origin_selection_range = Range {
                        start: Position { line: view.line, character: view.column },
                        end: Position { line: view.line, character: view.end_column },
                    };
                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: Range::default(),
                        target_selection_range: Range::default(),
                    }]));
                }
            }
        }
        None
    }

    /// Create LocationLink for a component reference from Salsa data
    async fn create_component_location_from_salsa(&self, comp: &ComponentReferenceData) -> Option<GotoDefinitionResponse> {
        let config = self.get_cached_config().await?;
        let possible_paths = config.resolve_component_path(&comp.name);

        for path in possible_paths {
            if self.file_exists_cached(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    let origin_selection_range = Range {
                        start: Position { line: comp.line, character: comp.column },
                        end: Position { line: comp.line, character: comp.end_column },
                    };
                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: Range::default(),
                        target_selection_range: Range::default(),
                    }]));
                }
            }
        }
        None
    }

    /// Create LocationLink for a Livewire reference from Salsa data
    async fn create_livewire_location_from_salsa(&self, lw: &LivewireReferenceData) -> Option<GotoDefinitionResponse> {
        let config = self.get_cached_config().await?;
        let path = config.resolve_livewire_path(&lw.name)?;

        if self.file_exists_cached(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                let origin_selection_range = Range {
                    start: Position { line: lw.line, character: lw.column },
                    end: Position { line: lw.line, character: lw.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a directive reference from Salsa data
    async fn create_directive_location_from_salsa(&self, dir: &DirectiveReferenceData) -> Option<GotoDefinitionResponse> {
        let arguments = dir.arguments.as_ref()?;
        let config = self.get_cached_config().await?;

        // Directives where first argument is a view name
        let view_directives_first_arg = [
            "extends", "include", "includeIf", "includeUnless", "each"
        ];

        // Directives where second argument is a view name (after a condition)
        let view_directives_second_arg = ["includeWhen"];

        // @component directive - resolves to component file
        if dir.name == "component" {
            if let Some(component_name) = Self::extract_view_from_directive_args(arguments) {
                // Try as component path (resources/views/components/...)
                let component_path = format!("components.{}", component_name);
                let possible_paths = config.resolve_view_path(&component_path);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }

                // Also try direct view path
                let possible_paths = config.resolve_view_path(&component_name);
                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle view directives (first argument is view name)
        if view_directives_first_arg.contains(&dir.name.as_str()) {
            if let Some(view_name) = Self::extract_view_from_directive_args(arguments) {
                let possible_paths = config.resolve_view_path(&view_name);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle @includeWhen($condition, 'view') - second arg is view
        if view_directives_second_arg.contains(&dir.name.as_str()) {
            if let Some(view_name) = Self::extract_second_string_arg(arguments) {
                let possible_paths = config.resolve_view_path(&view_name);

                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Handle @includeFirst(['view1', 'view2']) - array of views
        if dir.name == "includeFirst" {
            let view_names = Self::extract_array_string_args(arguments);
            for view_name in view_names {
                let possible_paths = config.resolve_view_path(&view_name);
                for path in possible_paths {
                    if self.file_exists_cached(&path).await {
                        return self.create_location_link(dir, &path);
                    }
                }
            }
        }

        // Note: @lang is now handled as Translation patterns (see parse_file_patterns in salsa_impl.rs)
        // Note: @vite is handled as Asset patterns, not Directive patterns
        // See parse_file_patterns in salsa_impl.rs

        None
    }

    /// Helper to create a LocationLink for a directive
    fn create_location_link(&self, dir: &DirectiveReferenceData, path: &std::path::Path) -> Option<GotoDefinitionResponse> {
        let target_uri = Url::from_file_path(path).ok()?;
        let origin_selection_range = Range {
            start: Position { line: dir.line, character: dir.column },
            end: Position { line: dir.line, character: dir.end_column },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range::default(),
            target_selection_range: Range::default(),
        }]))
    }

    /// Extract the second string argument from directive args
    /// For @includeWhen($condition, 'view.name', $data)
    fn extract_second_string_arg(arguments: &str) -> Option<String> {
        // Find second quoted string after a comma
        let mut in_string = false;
        let mut quote_char = ' ';
        let mut found_first = false;
        let mut result = String::new();
        let mut capturing = false;

        for ch in arguments.chars() {
            if !in_string {
                if ch == '\'' || ch == '"' {
                    if found_first {
                        // Start capturing second string
                        in_string = true;
                        quote_char = ch;
                        capturing = true;
                    } else {
                        // Skip first string
                        in_string = true;
                        quote_char = ch;
                    }
                }
            } else {
                if ch == quote_char {
                    in_string = false;
                    if capturing {
                        return Some(result);
                    }
                    found_first = true;
                } else if capturing {
                    result.push(ch);
                }
            }
        }
        None
    }

    /// Extract array of string arguments from directive args
    /// For @includeFirst(['view1', 'view2'])
    fn extract_array_string_args(arguments: &str) -> Vec<String> {
        let mut results = Vec::new();
        let mut current = String::new();
        let mut in_string = false;
        let mut quote_char = ' ';

        for ch in arguments.chars() {
            if !in_string {
                if ch == '\'' || ch == '"' {
                    in_string = true;
                    quote_char = ch;
                    current.clear();
                }
            } else {
                if ch == quote_char {
                    in_string = false;
                    if !current.is_empty() {
                        results.push(current.clone());
                    }
                } else {
                    current.push(ch);
                }
            }
        }
        results
    }

    /// Create LocationLink for an env reference using Salsa
    async fn create_env_location_from_salsa(&self, env: &EnvReferenceData) -> Option<GotoDefinitionResponse> {
        let env_var = self.salsa.get_parsed_env_var(env.name.clone()).await.ok()??;
        let target_uri = Url::from_file_path(&env_var.source_file).ok()?;
        let origin_selection_range = Range {
            start: Position { line: env.line, character: env.column },
            end: Position { line: env.line, character: env.end_column },
        };
        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range {
                start: Position { line: env_var.line, character: env_var.column },
                end: Position { line: env_var.line, character: env_var.column + env_var.name.len() as u32 },
            },
            target_selection_range: Range {
                start: Position { line: env_var.line, character: env_var.column },
                end: Position { line: env_var.line, character: env_var.column + env_var.name.len() as u32 },
            },
        }]))
    }

    /// Create LocationLink for a config reference from Salsa data
    async fn create_config_location_from_salsa(&self, config_ref: &ConfigReferenceData) -> Option<GotoDefinitionResponse> {
        let project_config = self.get_cached_config().await?;

        // Parse config key like "app.name" -> file: config/app.php
        let parts: Vec<&str> = config_ref.key.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let config_file = parts[0];
        let config_path = project_config.root.join("config").join(format!("{}.php", config_file));

        if self.file_exists_cached(&config_path).await {
            if let Ok(target_uri) = Url::from_file_path(&config_path) {
                let origin_selection_range = Range {
                    start: Position { line: config_ref.line, character: config_ref.column },
                    end: Position { line: config_ref.line, character: config_ref.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a middleware reference
    /// Navigates to the alias declaration (e.g., in bootstrap/app.php)
    /// Uses cache-first lookup (disk cache → Salsa fallback)
    async fn create_middleware_location_from_salsa(&self, mw: &MiddlewareReferenceData) -> Option<GotoDefinitionResponse> {
        // Use unified cache-first lookup (same as diagnostics)
        // Returns (class_name, class_file, source_file, source_line) - we navigate to source_file
        let cached = self.get_cached_middleware(&mw.name).await;
        info!("🔍 get_cached_middleware('{}') = {:?}", mw.name, cached.as_ref().map(|(c, cf, sf, sl)| (c, cf.is_some(), sf.is_some(), sl)));

        let (_class_name, _class_file, source_file, source_line) = cached?;

        let source_path = match source_file {
            Some(p) => p,
            None => {
                info!("❌ source_file is None for middleware '{}'", mw.name);
                return None;
            }
        };

        if !self.file_exists_cached(&source_path).await {
            info!("❌ source_file does not exist: {:?}", source_path);
            return None;
        }

        let target_uri = Url::from_file_path(&source_path).ok()?;
        // LSP uses 0-based line numbers, but we store 1-based
        let target_line = source_line.unwrap_or(1).saturating_sub(1);

        let origin_selection_range = Range {
            start: Position { line: mw.line, character: mw.column },
            end: Position { line: mw.line, character: mw.end_column },
        };

        // Navigate to the specific line where the alias is declared
        let target_range = Range {
            start: Position { line: target_line, character: 0 },
            end: Position { line: target_line, character: 0 },
        };

        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range,
            target_selection_range: target_range,
        }]))
    }

    /// Create LocationLink for a translation reference from Salsa data
    async fn create_translation_location_from_salsa(&self, trans: &TranslationReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Determine if this is a dotted key (PHP file) or text key (JSON file)
        let is_dotted_key = trans.key.contains('.') && !trans.key.contains(' ');

        let translation_path = if is_dotted_key {
            // Dotted key: "validation.required" -> lang/en/validation.php
            let parts: Vec<&str> = trans.key.split('.').collect();
            if parts.is_empty() {
                return None;
            }
            root.join("lang").join("en").join(format!("{}.php", parts[0]))
        } else {
            // Text key: "Welcome to our app" -> lang/en.json
            root.join("lang").join("en.json")
        };

        if self.file_exists_cached(&translation_path).await {
            if let Ok(target_uri) = Url::from_file_path(&translation_path) {
                let origin_selection_range = Range {
                    start: Position { line: trans.line, character: trans.column },
                    end: Position { line: trans.line, character: trans.end_column },
                };

                // Find the line number of the key in the file
                let target_range = if !is_dotted_key {
                    // For JSON files, find the line where the key is defined
                    Self::find_json_key_location(&translation_path, &trans.key)
                        .unwrap_or_default()
                } else {
                    // For PHP files, default to start (could be enhanced later)
                    Range::default()
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range,
                    target_selection_range: target_range,
                }]));
            }
        }
        None
    }

    /// Find the line and column of a key in a JSON translation file
    fn find_json_key_location(json_path: &Path, key: &str) -> Option<Range> {
        let content = std::fs::read_to_string(json_path).ok()?;

        // Search for the key pattern: "key": or "key" :
        // We look for the key surrounded by quotes at the start of a JSON property
        let search_pattern = format!("\"{}\"", key);

        for (line_num, line) in content.lines().enumerate() {
            if let Some(col) = line.find(&search_pattern) {
                // Found the key, position cursor at the start of the key (after the opening quote)
                let start_col = col + 1; // Skip the opening quote
                let end_col = start_col + key.len();

                return Some(Range {
                    start: Position {
                        line: line_num as u32,
                        character: start_col as u32,
                    },
                    end: Position {
                        line: line_num as u32,
                        character: end_col as u32,
                    },
                });
            }
        }

        None
    }

    /// Create LocationLink for an asset reference from Salsa data
    async fn create_asset_location_from_salsa(&self, asset: &AssetReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Determine the base path based on helper type
        use laravel_lsp::salsa_impl::AssetHelperType;
        let base_path = match asset.helper_type {
            AssetHelperType::Asset | AssetHelperType::PublicPath | AssetHelperType::Mix => root.join("public"),
            AssetHelperType::BasePath => root.clone(),
            AssetHelperType::AppPath => root.join("app"),
            AssetHelperType::StoragePath => root.join("storage"),
            AssetHelperType::DatabasePath => root.join("database"),
            AssetHelperType::LangPath => root.join("lang"),
            AssetHelperType::ConfigPath => root.join("config"),
            AssetHelperType::ResourcePath | AssetHelperType::ViteAsset => root.join("resources"),
        };

        let asset_path = base_path.join(&asset.path);

        if self.file_exists_cached(&asset_path).await {
            if let Ok(target_uri) = Url::from_file_path(&asset_path) {
                let origin_selection_range = Range {
                    start: Position { line: asset.line, character: asset.column },
                    end: Position { line: asset.line, character: asset.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a binding reference
    /// Navigates to the binding declaration (e.g., in AppServiceProvider.php)
    /// Uses cache-first lookup (disk cache → Salsa fallback)
    async fn create_binding_location_from_salsa(&self, binding: &BindingReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // If it's a class reference (e.g., User::class), navigate directly to the class file
        if binding.is_class_reference {
            if let Some(path) = resolve_class_to_file(&binding.name, root) {
                if self.file_exists_cached(&path).await {
                    if let Ok(target_uri) = Url::from_file_path(&path) {
                        let origin_selection_range = Range {
                            start: Position { line: binding.line, character: binding.column },
                            end: Position { line: binding.line, character: binding.end_column },
                        };
                        return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                            origin_selection_range: Some(origin_selection_range),
                            target_uri,
                            target_range: Range::default(),
                            target_selection_range: Range::default(),
                        }]));
                    }
                }
            }
        }

        // For string bindings, navigate to the binding declaration
        if let Some((_class_name, _class_file, source_file, source_line)) = self.get_cached_binding(&binding.name).await {
            if let Some(path) = source_file {
                if self.file_exists_cached(&path).await {
                    if let Ok(target_uri) = Url::from_file_path(&path) {
                        // LSP uses 0-based line numbers, but we store 1-based
                        let target_line = source_line.unwrap_or(1).saturating_sub(1);
                        let origin_selection_range = Range {
                            start: Position { line: binding.line, character: binding.column },
                            end: Position { line: binding.line, character: binding.end_column },
                        };
                        let target_range = Range {
                            start: Position { line: target_line, character: 0 },
                            end: Position { line: target_line, character: 0 },
                        };
                        return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                            origin_selection_range: Some(origin_selection_range),
                            target_uri,
                            target_range,
                            target_selection_range: target_range,
                        }]));
                    }
                }
            }
        }

        None
    }

    /// Create a goto location for a route('name') call
    /// Navigates to the route definition in routes/*.php files
    async fn create_route_location_from_salsa(&self, route: &RouteReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Search for route definition in routes directory
        // Route definitions look like: ->name('route.name') or Route::...->name('route.name')
        let routes_dir = root.join("routes");
        if !routes_dir.exists() {
            return None;
        }

        // Search common route files
        let route_files = vec!["web.php", "api.php", "channels.php", "console.php"];

        for file_name in route_files {
            let route_file = routes_dir.join(file_name);
            if route_file.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&route_file).await {
                    // Look for ->name('route_name') pattern
                    let search_patterns = vec![
                        format!("->name('{}')", route.name),
                        format!("->name(\"{}\")", route.name),
                        format!("'{}' =>", route.name), // Route::resource patterns
                    ];

                    for pattern in &search_patterns {
                        if let Some(pos) = content.find(pattern) {
                            // Calculate line and column from byte position
                            let before = &content[..pos];
                            let line = before.matches('\n').count() as u32;
                            let last_newline = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
                            let column = (pos - last_newline) as u32;

                            if let Ok(target_uri) = Url::from_file_path(&route_file) {
                                let origin_selection_range = Range {
                                    start: Position { line: route.line, character: route.column },
                                    end: Position { line: route.line, character: route.end_column },
                                };
                                let target_range = Range {
                                    start: Position { line, character: column },
                                    end: Position { line, character: column + pattern.len() as u32 },
                                };
                                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                                    origin_selection_range: Some(origin_selection_range),
                                    target_uri,
                                    target_range,
                                    target_selection_range: target_range,
                                }]));
                            }
                        }
                    }
                }
            }
        }

        None
    }

    /// Create a goto location for a url('path') call
    /// Navigates to the file in public directory if it exists
    async fn create_url_location_from_salsa(&self, url: &UrlReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // url() generates URLs relative to public directory
        let path = url.path.trim_start_matches('/');
        let public_path = root.join("public").join(path);

        if self.file_exists_cached(&public_path).await {
            if let Ok(target_uri) = Url::from_file_path(&public_path) {
                let origin_selection_range = Range {
                    start: Position { line: url.line, character: url.column },
                    end: Position { line: url.line, character: url.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }

        None
    }

    /// Create a goto location for an action('Controller@method') call
    /// Navigates to the controller file
    async fn create_action_location_from_salsa(&self, action: &ActionReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Parse action string: "Controller@method" or "App\Http\Controllers\Controller@method"
        let parts: Vec<&str> = action.action.split('@').collect();
        let controller_class = parts.first()?;

        // Resolve controller to file path
        let path = resolve_class_to_file(controller_class, root)?;

        if self.file_exists_cached(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                let origin_selection_range = Range {
                    start: Position { line: action.line, character: action.column },
                    end: Position { line: action.line, character: action.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]));
            }
        }

        None
    }

    /// Schedule debounced diagnostics for a file
    ///
    /// This method cancels any pending diagnostics for the file and schedules
    /// a new task to run diagnostics after the debounce delay.
    /// This updates diagnostics as you type (after a pause) and on save.
    async fn schedule_debounced_diagnostics(&self, uri: &Url, source: &str) {
        let debounce_delay = Duration::from_millis(self.debounce_delay_ms);
        
        // Cancel any existing pending diagnostic task for this file
        if let Some(handle) = self.pending_diagnostics.write().await.remove(uri) {
            handle.abort();
        }
        
        // Clone values needed for the async task
        let uri_for_spawn = uri.clone();
        let source_for_spawn = source.to_string();
        let server = self.clone_for_spawn();
        
        // Spawn a task that runs diagnostics after debounce delay
        let handle = tokio::spawn(async move {
            // Wait for the debounce delay
            sleep(debounce_delay).await;
            
            info!("⏰ Debounce expired for {} - running diagnostics", uri_for_spawn);
            
            // Run diagnostics on the debounced content
            server.validate_and_publish_diagnostics(&uri_for_spawn, &source_for_spawn).await;
        });
        
        // Store the task handle so we can cancel it if needed
        self.pending_diagnostics.write().await.insert(uri.clone(), handle);
    }
    
    /// Clone server for spawning async tasks
    fn clone_for_spawn(&self) -> Self {
        LaravelLanguageServer {
            client: self.client.clone(),
            documents: self.documents.clone(),
            root_path: self.root_path.clone(),
            diagnostics: self.diagnostics.clone(),
            pending_diagnostics: self.pending_diagnostics.clone(),
            debounce_delay_ms: self.debounce_delay_ms,
            salsa: self.salsa.clone(),
            cache: self.cache.clone(),
            pending_rescans: self.pending_rescans.clone(),
            rescan_debounce_handle: self.rescan_debounce_handle.clone(),
            file_exists_cache: self.file_exists_cache.clone(),
            cached_config: self.cached_config.clone(),
            last_goto_request: self.last_goto_request.clone(),
            initialized_root: self.initialized_root.clone(),
            pending_salsa_updates: self.pending_salsa_updates.clone(),
            salsa_debounce_ms: self.salsa_debounce_ms.clone(),
        }
    }

    /// Validate a document (Blade or PHP) and publish diagnostics
    ///
    /// This function uses Salsa-cached patterns for efficient incremental validation:
    /// 1. Gets pre-parsed patterns from Salsa (memoized, only re-parses on content change)
    /// 2. Validates patterns against config, env cache, and service registry
    /// 3. Creates diagnostics for missing files/undefined references
    /// 4. Publishes diagnostics to the editor
    async fn validate_and_publish_diagnostics(&self, uri: &Url, source: &str) {
        info!("🔍 validate_and_publish_diagnostics called for {}", uri);
        let mut diagnostics = Vec::new();

        // Get the Laravel config (checks memory cache first, then Salsa)
        let t_config = std::time::Instant::now();
        let config = match self.get_cached_config().await {
            Some(c) => c,
            None => {
                info!("   ⚠️  Cannot validate: config not set");
                return;
            }
        };
        info!("   ⏱️  get_cached_config: {:?}", t_config.elapsed());

        // Convert URI to file path for Salsa lookup
        let file_path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => {
                info!("   ⚠️  Cannot convert URI to file path");
                return;
            }
        };

        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;

        // Get patterns from Salsa (cached, incremental)
        let t_patterns = std::time::Instant::now();
        let patterns = match self.salsa.get_patterns(file_path.clone()).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                info!("   ⚠️  No patterns found in Salsa for {}", uri);
                // Fall back to empty patterns - file might not be in Salsa yet
                // Ensure Salsa has the file before proceeding
                let _ = self.salsa.update_file(file_path.clone(), 0, source.to_string()).await;
                match self.salsa.get_patterns(file_path).await {
                    Ok(Some(p)) => p,
                    _ => return,
                }
            }
            Err(e) => {
                info!("   ⚠️  Error getting patterns from Salsa: {}", e);
                return;
            }
        };
        info!("   ⏱️  salsa.get_patterns: {:?}", t_patterns.elapsed());

        // Validate PHP files with view() calls and env() calls
        if is_php {
            // Check view() calls using Salsa patterns
            for view_ref in &patterns.views {
                let possible_paths = config.resolve_view_path(&view_ref.name);
                let exists = possible_paths.iter().any(|p| p.exists());

                if !exists {
                    let expected_path = possible_paths.first()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    // Route::view() and Volt::route() should be ERROR
                    // Regular view() calls should be WARNING
                    let severity = if view_ref.is_route_view {
                        DiagnosticSeverity::ERROR
                    } else {
                        DiagnosticSeverity::WARNING
                    };

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: view_ref.line,
                                character: view_ref.column,
                            },
                            end: Position {
                                line: view_ref.line,
                                character: view_ref.end_column,
                            },
                        },
                        severity: Some(severity),
                        code: None,
                        source: Some("laravel-lsp".to_string()),
                        message: format!(
                            "View file not found: '{}'\nExpected at: {}",
                            view_ref.name,
                            expected_path
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }

            // Check env() calls using Salsa patterns - warn if variable not defined
            for env_ref in &patterns.env_refs {
                let env_exists = self.salsa.get_parsed_env_var(env_ref.name.clone()).await
                    .ok()
                    .flatten()
                    .is_some();

                if !env_exists {
                    // Show WARNING if no fallback (likely to break)
                    // Show INFO if there's a fallback (safe default)
                    let (severity, message) = if env_ref.has_fallback {
                        (
                            DiagnosticSeverity::INFORMATION,
                            format!(
                                "Environment variable '{}' not found in .env files (using fallback value)",
                                env_ref.name
                            )
                        )
                    } else {
                        (
                            DiagnosticSeverity::WARNING,
                            format!(
                                "Environment variable '{}' not found in .env files and has no fallback\nDefine it in .env, .env.example, or .env.local",
                                env_ref.name
                            )
                        )
                    };

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: env_ref.line,
                                character: env_ref.column,
                            },
                            end: Position {
                                line: env_ref.line,
                                character: env_ref.end_column,
                            },
                        },
                        severity: Some(severity),
                        code: None,
                        source: Some("laravel-lsp".to_string()),
                        message,
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }

            // Check middleware calls using Salsa patterns - warn about undefined middleware or missing class files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for mw_ref in &patterns.middleware_refs {
                    let middleware_name = &mw_ref.name;

                    // Check if middleware exists in cache or Salsa registry
                    debug!("Checking middleware '{}' in cache/registry", middleware_name);
                    if let Some((class_name, class_file, _source_file, _source_line)) = self.get_cached_middleware(middleware_name).await {
                        debug!("Middleware '{}' found, class: {}", middleware_name, class_name);
                        // Middleware is in registry - check if class file exists
                        if let Some(ref mw_class_path) = class_file {
                            debug!("Checking class file: {:?}, exists: {}", mw_class_path, mw_class_path.exists());
                            if !mw_class_path.exists() {
                                // ERROR - middleware defined but class file missing (will crash at runtime)
                                debug!("Creating ERROR diagnostic for missing middleware class file: {}", middleware_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.column,
                                        },
                                        end: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel-lsp".to_string()),
                                    message: format!(
                                        "Middleware '{}' not found\nClass: {}\nExpected at: {}\n\nThe middleware alias is registered but the class file doesn't exist.\n💡 Click to view where the alias is defined.",
                                        middleware_name,
                                        class_name,
                                        mw_class_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            } else {
                                debug!("Middleware '{}' class file exists at {:?}", middleware_name, mw_class_path);
                            }
                        } else {
                            debug!("Middleware '{}' in registry but no class_file resolved - skipping diagnostic", middleware_name);
                            // Skip diagnostic - can't verify file existence without a path
                            // This handles some framework middleware
                        }
                    } else {
                        // Middleware not in registry - try to resolve it by convention
                        info!("Laravel LSP: Middleware '{}' NOT found in registry, attempting resolution by convention", middleware_name);

                        // Convert kebab-case to PascalCase (e.g., 'undefined-middleware' -> 'UndefinedMiddleware')
                        let class_name = Self::kebab_to_pascal_case(middleware_name);
                        let app_class = format!("App\\Http\\Middleware\\{}", class_name);

                        // Try to resolve as App\Http\Middleware\{ClassName}
                        if let Some(mw_file_path) = resolve_class_to_file(&app_class, root) {
                            info!("Laravel LSP: Attempting to resolve middleware '{}' as class '{}' at {:?}", middleware_name, app_class, mw_file_path);

                            if !mw_file_path.exists() {
                                // ERROR - middleware not in config and class file doesn't exist
                                info!("Laravel LSP: Creating ERROR diagnostic for unresolved middleware: {}", middleware_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.column,
                                        },
                                        end: Position {
                                            line: mw_ref.line,
                                            character: mw_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel-lsp".to_string()),
                                    message: format!(
                                        "Middleware '{}' not found\nExpected at: {}\n\nCreate the middleware or add an alias in bootstrap/app.php",
                                        middleware_name,
                                        mw_file_path.to_string_lossy()
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            } else {
                                info!("Laravel LSP: Middleware '{}' resolved by convention, file exists at {:?}", middleware_name, mw_file_path);
                            }
                        } else {
                            // Can't resolve - show INFO as we don't know where to check
                            info!("Laravel LSP: Middleware '{}' NOT found in registry and can't resolve file path, creating INFO diagnostic", middleware_name);
                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: mw_ref.line,
                                        character: mw_ref.column,
                                    },
                                    end: Position {
                                        line: mw_ref.line,
                                        character: mw_ref.end_column,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::INFORMATION),
                                code: None,
                                source: Some("laravel-lsp".to_string()),
                                message: format!(
                                    "Middleware '{}' not found\n\nIf this middleware exists, add an alias in bootstrap/app.php",
                                    middleware_name
                                ),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            };
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            drop(root_guard);

            // Check translation calls using Salsa patterns - warn about missing translation files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for trans_ref in &patterns.translation_refs {
                    let check = Self::check_translation_file(root, &trans_ref.key);
                    if !check.exists {
                        diagnostics.push(Self::create_translation_diagnostic(
                            &trans_ref.key,
                            &check,
                            trans_ref.line,
                            trans_ref.column,
                            trans_ref.end_column,
                            DiagnosticSeverity::ERROR, // ERROR for dotted keys in PHP
                        ));
                    }
                }
            }
            drop(root_guard);

            // Check container binding calls using Salsa patterns - error for undefined bindings or missing class files
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for binding_ref in &patterns.binding_refs {
                    // Only validate string bindings (not Class::class references)
                    // Class::class references might be auto-resolved by Laravel
                    if !binding_ref.is_class_reference {
                        let binding_name = &binding_ref.name;

                        // Check if binding exists in Salsa registry
                        if let Ok(Some(binding_data)) = self.salsa.get_parsed_binding(binding_name.clone()).await {
                            // Binding exists - check if the concrete class file exists
                            if let Some(ref bind_file_path) = binding_data.file_path {
                                if !bind_file_path.exists() {
                                    // ERROR - binding exists but class file is missing
                                    info!("Laravel LSP: Creating ERROR diagnostic for binding with missing class: {}", binding_name);

                                    // Build the diagnostic message with registration location
                                    let mut message = format!(
                                        "Binding '{}' registered but class file not found\nExpected class at: {}",
                                        binding_name,
                                        bind_file_path.to_string_lossy()
                                    );

                                    // Add registration location
                                    let registered_in = binding_data.source_file.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("service provider");
                                    message.push_str(&format!("\n\nBound in: {}:{}", registered_in, binding_data.source_line + 1));
                                    message.push_str(&format!("\nConcrete class: {}", binding_data.concrete_class));

                                    let diagnostic = Diagnostic {
                                        range: Range {
                                            start: Position {
                                                line: binding_ref.line,
                                                character: binding_ref.column,
                                            },
                                            end: Position {
                                                line: binding_ref.line,
                                                character: binding_ref.end_column,
                                            },
                                        },
                                        severity: Some(DiagnosticSeverity::ERROR),
                                        code: None,
                                        source: Some("laravel-lsp".to_string()),
                                        message,
                                        related_information: None,
                                        tags: None,
                                        code_description: None,
                                        data: None,
                                    };
                                    diagnostics.push(diagnostic);
                                }
                            }
                        } else {
                            // Binding not found - check if it's a known framework binding
                            let framework_bindings = [
                                "app", "auth", "auth.driver", "blade.compiler", "cache", "cache.store",
                                "config", "cookie", "db", "db.connection", "encrypter", "events",
                                "files", "filesystem", "filesystem.disk", "hash", "log", "mailer",
                                "queue", "queue.connection", "redirect", "redis", "request", "router",
                                "session", "session.store", "url", "validator", "view",
                            ];

                            if !framework_bindings.contains(&binding_name.as_str()) {
                                // Check if we can resolve the class by convention
                                if let Some(class_path) = resolve_class_to_file(binding_name, root) {
                                    if class_path.exists() {
                                        // Class exists via convention - skip diagnostic
                                        continue;
                                    }
                                }

                                // ERROR - binding not found and not a known framework binding
                                info!("Laravel LSP: Creating ERROR diagnostic for undefined binding: {}", binding_name);
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: binding_ref.line,
                                            character: binding_ref.column,
                                        },
                                        end: Position {
                                            line: binding_ref.line,
                                            character: binding_ref.end_column,
                                        },
                                    },
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    source: Some("laravel-lsp".to_string()),
                                    message: format!(
                                        "Container binding '{}' not found\n\nDefine this binding in a service provider's register() method",
                                        binding_name
                                    ),
                                    related_information: None,
                                    tags: None,
                                    code_description: None,
                                    data: None,
                                };
                                diagnostics.push(diagnostic);
                            }
                        }
                    }
                }
            }
            drop(root_guard);

            // Check asset() and related helper calls - error if file not found
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for asset_ref in &patterns.asset_refs {
                    use laravel_lsp::salsa_impl::AssetHelperType;

                    // Determine base path based on helper type
                    let (base_path, helper_name) = match asset_ref.helper_type {
                        AssetHelperType::Asset => (root.join("public"), "asset"),
                        AssetHelperType::PublicPath => (root.join("public"), "public_path"),
                        AssetHelperType::Mix => (root.join("public"), "mix"),
                        AssetHelperType::BasePath => (root.clone(), "base_path"),
                        AssetHelperType::AppPath => (root.join("app"), "app_path"),
                        AssetHelperType::StoragePath => (root.join("storage"), "storage_path"),
                        AssetHelperType::DatabasePath => (root.join("database"), "database_path"),
                        AssetHelperType::LangPath => (root.join("lang"), "lang_path"),
                        AssetHelperType::ConfigPath => (root.join("config"), "config_path"),
                        AssetHelperType::ResourcePath => (root.join("resources"), "resource_path"),
                        AssetHelperType::ViteAsset => (root.join("resources"), "@vite"),
                    };

                    let asset_path = base_path.join(&asset_ref.path);

                    if !asset_path.exists() {
                        let diagnostic = Diagnostic {
                            range: Range {
                                start: Position {
                                    line: asset_ref.line,
                                    character: asset_ref.column,
                                },
                                end: Position {
                                    line: asset_ref.line,
                                    character: asset_ref.end_column,
                                },
                            },
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: None,
                            source: Some("laravel-lsp".to_string()),
                            message: format!(
                                "Asset file not found: '{}'\nExpected at: {}\nHelper: {}()",
                                asset_ref.path,
                                asset_path.to_string_lossy(),
                                helper_name
                            ),
                            related_information: None,
                            tags: None,
                            code_description: None,
                            data: None,
                        };
                        diagnostics.push(diagnostic);
                    }
                }
            }
            drop(root_guard);

            // Store and publish diagnostics for PHP files
            self.diagnostics.write().await.insert(uri.clone(), diagnostics.clone());
            self.client.publish_diagnostics(uri.clone(), diagnostics, None).await;
            return;
        }

        // =====================================================================
        // Blade file validation - uses Salsa patterns (already parsed above)
        // =====================================================================
        if !is_blade {
            return;
        }

        // Translation calls are already extracted by Salsa (patterns.translation_refs)
        // Check translation calls in Blade files (includes {{ __() }} syntax)
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for trans_ref in &patterns.translation_refs {
                let check = Self::check_translation_file(root, &trans_ref.key);
                if !check.exists {
                    diagnostics.push(Self::create_translation_diagnostic(
                        &trans_ref.key,
                        &check,
                        trans_ref.line,
                        trans_ref.column,
                        trans_ref.end_column,
                        DiagnosticSeverity::ERROR, // ERROR for dotted keys in Blade __()
                    ));
                }
            }
        }
        drop(root_guard);

        // Check @extends and @include directives using Salsa patterns
        for dir_ref in &patterns.directives {
            // Only validate @extends and @include
            if dir_ref.name == "extends" || dir_ref.name == "include" {
                if let Some(ref args) = dir_ref.arguments {
                    if let Some(view_name) = Self::extract_view_from_directive_args(args) {
                        let possible_paths = config.resolve_view_path(&view_name);

                        // Check if ANY of the possible paths exist
                        let exists = possible_paths.iter().any(|p| p.exists());

                        if !exists {
                            // Use the first path for the diagnostic message
                            let expected_path = possible_paths.first()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| "unknown".to_string());

                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: dir_ref.line,
                                        character: dir_ref.column,
                                    },
                                    end: Position {
                                        line: dir_ref.line,
                                        character: dir_ref.end_column,
                                    },
                                },
                                severity: Some(DiagnosticSeverity::WARNING),
                                code: None,
                                source: Some("laravel-lsp".to_string()),
                                message: format!(
                                    "View file not found: '{}'\nExpected at: {}",
                                    view_name,
                                    expected_path
                                ),
                                related_information: None,
                                tags: None,
                                code_description: None,
                                data: None,
                            };
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
        }

        // Check Blade components (<x-button>) using Salsa patterns
        for comp_ref in &patterns.components {
            let possible_paths = config.resolve_component_path(&comp_ref.name);
            let exists = possible_paths.iter().any(|p| p.exists());

            if !exists {
                let expected_path = possible_paths.first()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let diagnostic = Diagnostic {
                    range: Range {
                        start: Position {
                            line: comp_ref.line,
                            character: comp_ref.column,
                        },
                        end: Position {
                            line: comp_ref.line,
                            character: comp_ref.end_column,
                        },
                    },
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: None,
                    source: Some("laravel-lsp".to_string()),
                    message: format!(
                        "Blade component not found: '{}'\nExpected at: {}",
                        comp_ref.name,
                        expected_path
                    ),
                    related_information: None,
                    tags: None,
                    code_description: None,
                    data: None,
                };
                diagnostics.push(diagnostic);
            }
        }

        // Check Livewire components using Salsa patterns
        for lw_ref in &patterns.livewire_refs {
            if let Some(livewire_path) = config.resolve_livewire_path(&lw_ref.name) {
                if !livewire_path.exists() {
                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: lw_ref.line,
                                character: lw_ref.column,
                            },
                            end: Position {
                                line: lw_ref.line,
                                character: lw_ref.end_column,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: None,
                        source: Some("laravel-lsp".to_string()),
                        message: format!(
                            "Livewire component not found: '{}'\nExpected at: {}",
                            lw_ref.name,
                            livewire_path.to_string_lossy()
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }
        }

        // Check @lang directives for translation files using Salsa patterns
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for dir_ref in &patterns.directives {
                // Only validate @lang directives
                if dir_ref.name == "lang" {
                    if let Some(ref args) = dir_ref.arguments {
                        if let Some(translation_key) = Self::extract_view_from_directive_args(args) {
                            let check = Self::check_translation_file(root, &translation_key);
                            if !check.exists {
                                diagnostics.push(Self::create_translation_diagnostic(
                                    &translation_key,
                                    &check,
                                    dir_ref.line,
                                    dir_ref.column,
                                    dir_ref.end_column,
                                    DiagnosticSeverity::WARNING, // WARNING for dotted keys in @lang
                                ));
                            }
                        }
                    }
                }
            }
        }
        drop(root_guard);

        // Check @vite and asset() calls in Blade files - error if file not found
        let root_guard = self.root_path.read().await;
        if let Some(root) = root_guard.as_ref() {
            for asset_ref in &patterns.asset_refs {
                use laravel_lsp::salsa_impl::AssetHelperType;

                // Determine base path based on helper type
                let (base_path, helper_name) = match asset_ref.helper_type {
                    AssetHelperType::Asset => (root.join("public"), "asset"),
                    AssetHelperType::PublicPath => (root.join("public"), "public_path"),
                    AssetHelperType::Mix => (root.join("public"), "mix"),
                    AssetHelperType::BasePath => (root.clone(), "base_path"),
                    AssetHelperType::AppPath => (root.join("app"), "app_path"),
                    AssetHelperType::StoragePath => (root.join("storage"), "storage_path"),
                    AssetHelperType::DatabasePath => (root.join("database"), "database_path"),
                    AssetHelperType::LangPath => (root.join("lang"), "lang_path"),
                    AssetHelperType::ConfigPath => (root.join("config"), "config_path"),
                    AssetHelperType::ResourcePath => (root.join("resources"), "resource_path"),
                    AssetHelperType::ViteAsset => (root.join("resources"), "@vite"),
                };

                let asset_path = base_path.join(&asset_ref.path);

                if !asset_path.exists() {
                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: asset_ref.line,
                                character: asset_ref.column,
                            },
                            end: Position {
                                line: asset_ref.line,
                                character: asset_ref.end_column,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: None,
                        source: Some("laravel-lsp".to_string()),
                        message: format!(
                            "Asset file not found: '{}'\nExpected at: {}\nHelper: {}()",
                            asset_ref.path,
                            asset_path.to_string_lossy(),
                            helper_name
                        ),
                        related_information: None,
                        tags: None,
                        code_description: None,
                        data: None,
                    };
                    diagnostics.push(diagnostic);
                }
            }
        }
        drop(root_guard);

        // Store diagnostics for hover filtering
        self.diagnostics.write().await.insert(uri.clone(), diagnostics.clone());

        // Publish diagnostics
        info!("   📤 Publishing {} diagnostics to client", diagnostics.len());
        self.client.publish_diagnostics(uri.clone(), diagnostics, None).await;
        info!("   ✅ Diagnostics published successfully");
    }
}



#[tower_lsp::async_trait]
impl LanguageServer for LaravelLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        let init_start = std::time::Instant::now();
        info!("Laravel LSP: INITIALIZE");

        // Read initial settings from initialization_options (if provided)
        // These can be overridden at runtime via did_change_configuration
        if let Some(init_options) = params.initialization_options {
            match serde_json::from_value::<LspSettings>(init_options) {
                Ok(settings) => {
                    info!("⚙️  Initial settings: debounceMs={}ms", settings.laravel.debounce_ms);
                    self.update_settings(&settings).await;
                }
                Err(e) => {
                    debug!("Could not parse initialization_options: {}", e);
                }
            }
        }

        // Store the root path - lightweight operation
        if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                *self.root_path.write().await = Some(path.clone());
                info!("✅ Laravel LSP: Root path set to {:?}", path);

                // Load ALL cached data (config, middleware, bindings, env) using batch registration (fast)
                // This uses 2 round-trips instead of N round-trips for N entries
                let t_cache = std::time::Instant::now();
                info!("📦 Loading cached data...");
                let needs_rescans = self.load_cache_data(&path).await;
                info!("⏱️  load_cache_data: {:?}", t_cache.elapsed());

                // Store needs_rescans for initialized() to pick up
                self.pending_rescans.write().await.extend(needs_rescans);
            }
        }
        info!("⏱️  INITIALIZE TOTAL: {:?}", init_start.elapsed());

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // We support go-to-definition
                definition_provider: Some(OneOf::Left(true)),
                
                // We need to sync document content and receive save notifications
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false), // We get text from did_change
                        })),
                    }
                )),
                
                // ❌ REMOVED: hover_provider
                // We only support goto_definition (Option+click navigation).
                // Hover popups are redundant - the underline already indicates navigability.

                // ❌ REMOVED: completion_provider
                // We don't implement autocomplete, so don't advertise it.
                // This prevents Zed from calling us for every completion request.
                
                // ❌ REMOVED: Preparsing on every keystroke in did_change
                // This was causing autocomplete slowness due to heavy tree-sitter queries.
                
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("========================================");
        info!("🚀 Laravel LSP: INITIALIZED - spawning background work");
        info!("========================================");

        // Get root path
        let root = match self.root_path.read().await.clone() {
            Some(r) => r,
            None => {
                info!("No root path set, skipping background initialization");
                return;
            }
        };

        // Spawn background task for heavy initialization work
        // This doesn't block the LSP - Zed can start sending requests immediately
        // Note: If cache exists, config/middleware/env are already loaded in initialize()
        let server = self.clone_for_spawn();
        tokio::spawn(async move {
            // Register config if not loaded from cache
            if server.get_cached_config().await.is_none() {
                info!("📋 No cached config, registering from files...");
                server.register_config_with_salsa(&root).await;
            }

            // Register project files with Salsa for reference finding (if config available)
            if let Some(config) = server.get_cached_config().await {
                info!("Laravel config available: {} view paths", config.view_paths.len());
                server.register_project_files_with_salsa(&root).await;
            } else {
                info!("Config not available for project file registration");
            }

            // Register env files with Salsa (if not loaded from cache)
            server.register_env_files_with_salsa(&root).await;

            // Execute pending rescans (vendor, app, node_modules)
            server.execute_pending_rescans().await;
        });
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        info!("Laravel LSP: Shutting down - cleaning up resources");

        // Cancel all pending diagnostic tasks
        {
            let mut pending = self.pending_diagnostics.write().await;
            for (uri, handle) in pending.drain() {
                debug!("Cancelling pending diagnostics for: {}", uri);
                handle.abort();
            }
        }

        // Clear document cache
        self.documents.write().await.clear();

        // Clear diagnostics cache
        self.diagnostics.write().await.clear();

        // Shutdown Salsa actor
        if let Err(e) = self.salsa.shutdown().await {
            debug!("Salsa actor shutdown: {}", e);
        }

        info!("Laravel LSP: Shutdown complete");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let total_start = std::time::Instant::now();
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        info!("📂 did_open: {}", uri.path().split('/').last().unwrap_or(""));
        self.documents.write().await.insert(uri.clone(), (text.clone(), version));

        // Try to discover Laravel config from this file if we don't have one yet
        if let Ok(file_path) = uri.to_file_path() {
            let t1 = std::time::Instant::now();
            self.try_discover_from_file(&file_path).await;
            info!("   ⏱️  try_discover_from_file: {:?}", t1.elapsed());

            // Update Salsa database with new file content
            let t2 = std::time::Instant::now();
            if let Err(e) = self.salsa.update_file(file_path.clone(), version, text.clone()).await {
                debug!("Failed to update Salsa database: {}", e);
            }
            info!("   ⏱️  salsa.update_file: {:?}", t2.elapsed());
        }

        // Validate and publish diagnostics for Blade files
        let t3 = std::time::Instant::now();
        self.validate_and_publish_diagnostics(&uri, &text).await;
        info!("   ⏱️  validate_and_publish_diagnostics: {:?}", t3.elapsed());
        info!("   ✅ did_open total: {:?}", total_start.elapsed());
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            debug!("Laravel LSP: Document changed: {} (version: {})", uri, version);

            // Store in documents buffer immediately (for goto_definition during debounce)
            self.documents.write().await.insert(uri.clone(), (change.text.clone(), version));

            // Queue debounced Salsa update (250ms)
            // This handles all file types: SourceFile, ConfigFile, EnvFile, ServiceProviderFile
            // After debounce, execute_salsa_update will:
            // 1. Determine file type and update appropriate Salsa input
            // 2. Re-run diagnostics for this file
            self.queue_salsa_update(uri, change.text, version).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("🔔 Laravel LSP: did_save called for {}", uri);

        // Check for lock file changes that trigger rescans
        if let Ok(path) = uri.to_file_path() {
            let file_name = path.file_name().and_then(|n| n.to_str());
            let path_str = path.to_string_lossy();

            // Invalidate config cache if config-related files change
            let is_config_file = matches!(file_name, Some("composer.json"))
                || path_str.contains("/config/")
                || matches!(file_name, Some("view.php" | "livewire.php"));

            if is_config_file {
                info!("📦 Config file changed, invalidating config cache");
                self.invalidate_config_cache().await;
            }

            match file_name {
                Some("composer.lock") => {
                    info!("📦 composer.lock changed, queuing vendor rescan");
                    self.queue_background_rescan(RescanType::Vendor).await;
                }
                Some("package-lock.json") | Some("yarn.lock") | Some("pnpm-lock.yaml") => {
                    info!("📦 Package lock changed, queuing node_modules rescan");
                    self.queue_background_rescan(RescanType::NodeModules).await;
                }
                Some(name) if name.ends_with(".php") => {
                    // Check if it's in app/Providers/
                    if path_str.contains("app/Providers/") {
                        info!("📦 App provider changed, queuing app rescan");
                        self.queue_background_rescan(RescanType::App).await;
                    }
                }
                Some("app.php") => {
                    // Check if it's bootstrap/app.php
                    if path.parent().map(|p| p.ends_with("bootstrap")).unwrap_or(false) {
                        info!("📦 bootstrap/app.php changed, queuing app rescan");
                        self.queue_background_rescan(RescanType::App).await;
                    }
                }
                _ => {}
            }
        }

        // Cancel any pending debounced diagnostics for this file
        // We'll run diagnostics immediately on save instead
        if let Some(handle) = self.pending_diagnostics.write().await.remove(&uri) {
            handle.abort();
            info!("   ✅ Cancelled pending diagnostic task");
        }

        // Run cache update AND diagnostics on save
        if let Some((text, _version)) = self.documents.read().await.get(&uri).cloned() {
            info!("   ✅ Found document in cache, updating cache and running diagnostics...");
            let is_blade = uri.path().ends_with(".blade.php");
            let is_php = uri.path().ends_with(".php");

            if is_blade || is_php {
                // Removed: parse_and_cache_patterns - performance_cache handles this automatically
            }

            // Run diagnostics immediately on save
            info!("   📊 Running diagnostics immediately on save for {}", uri);
            self.validate_and_publish_diagnostics(&uri, &text).await;
            info!("   ✅ Diagnostics published for {}", uri);
        } else {
            info!("   ⚠️  Document not found in cache for {}", uri);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        debug!("Laravel LSP: Document closed: {}", uri);

        // Cancel any pending debounced diagnostics
        if let Some(handle) = self.pending_diagnostics.write().await.remove(&uri) {
            handle.abort();
        }

        self.documents.write().await.remove(&uri);

        // Clear diagnostics from our cache
        self.diagnostics.write().await.remove(&uri);

        // Remove from Salsa database
        if let Ok(file_path) = uri.to_file_path() {
            if let Err(e) = self.salsa.remove_file(file_path).await {
                debug!("Failed to remove from Salsa database: {}", e);
            }
        }

        // Publish empty diagnostics to clear them from the client
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        // Handle runtime configuration changes without requiring LSP restart
        // Settings are configured via: { "lsp": { "laravel-lsp": { "settings": { "laravel": { ... } } } } }
        debug!("🔧 Configuration changed: {:?}", params.settings);

        match serde_json::from_value::<LspSettings>(params.settings) {
            Ok(settings) => {
                info!("⚙️  Configuration updated: debounceMs={}ms", settings.laravel.debounce_ms);
                self.update_settings(&settings).await;
            }
            Err(e) => {
                debug!("Could not parse configuration settings: {}", e);
            }
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        info!("🎯 goto_definition called: {}:{}:{}", uri, position.line, position.character);

        // Coalescing window: skip duplicate requests within ~16ms (~60fps)
        const COALESCE_MS: u64 = 16;

        // Early return: only process PHP files
        let is_php = uri.path().ends_with(".php");
        if !is_php {
            return Ok(None);
        }

        // Request coalescing: skip rapid duplicate requests at same position
        // This handles the case where the editor rapidly fires requests while moving cursor
        {
            let last_requests = self.last_goto_request.read().await;
            if let Some((last_pos, last_time)) = last_requests.get(&uri) {
                if *last_pos == position && last_time.elapsed() < Duration::from_millis(COALESCE_MS) {
                    // Same position, very recent request - skip to avoid redundant work
                    return Ok(None);
                }
            }
        }

        // Update last request tracking
        self.last_goto_request.write().await.insert(uri.clone(), (position, Instant::now()));

        // Early return: check if document exists in our cache
        // This avoids expensive Salsa lookups for files we haven't seen
        if !self.documents.read().await.contains_key(&uri) {
            return Ok(None);
        }

        // Convert URI to file path
        let file_path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };

        // Get patterns from Salsa (cached, O(1) lookup)
        let patterns = match self.salsa.get_patterns(file_path).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                debug!("Laravel LSP: No patterns cached for file");
                return Ok(None);
            }
            Err(e) => {
                debug!("Laravel LSP: Error getting patterns: {:?}", e);
                return Ok(None);
            }
        };

        // Find pattern at cursor position
        let pattern = match patterns.find_at_position(position.line, position.character) {
            Some(p) => p,
            None => {
                // Debug: show what middleware patterns exist on this line
                let mw_on_line: Vec<_> = patterns.middleware_refs.iter()
                    .filter(|m| m.line == position.line)
                    .map(|m| format!("'{}' col {}-{}", m.name, m.column, m.end_column))
                    .collect();
                info!("🔍 No pattern at line {} col {} (middleware on line: {:?})",
                    position.line, position.character, mw_on_line);
                return Ok(None);
            }
        };

        // Create location based on pattern type
        let location = match pattern {
            PatternAtPosition::View(view) => {
                debug!("Laravel LSP: Found view: {}", view.name);
                self.create_view_location_from_salsa(&view).await
            }
            PatternAtPosition::Component(comp) => {
                debug!("Laravel LSP: Found component: {}", comp.name);
                self.create_component_location_from_salsa(&comp).await
            }
            PatternAtPosition::Livewire(lw) => {
                debug!("Laravel LSP: Found livewire: {}", lw.name);
                self.create_livewire_location_from_salsa(&lw).await
            }
            PatternAtPosition::Directive(dir) => {
                info!("🎯 Laravel LSP: Found directive: {} with args {:?} at {}:{}-{}",
                    dir.name, dir.arguments, dir.line, dir.column, dir.end_column);
                self.create_directive_location_from_salsa(&dir).await
            }
            PatternAtPosition::EnvRef(env) => {
                debug!("Laravel LSP: Found env: {}", env.name);
                self.create_env_location_from_salsa(&env).await
            }
            PatternAtPosition::ConfigRef(config) => {
                debug!("Laravel LSP: Found config: {}", config.key);
                self.create_config_location_from_salsa(&config).await
            }
            PatternAtPosition::Middleware(mw) => {
                info!("🎯 Found middleware pattern: '{}' at {}:{}-{}", mw.name, mw.line, mw.column, mw.end_column);
                let result = self.create_middleware_location_from_salsa(&mw).await;
                if result.is_none() {
                    info!("❌ Middleware location lookup returned None for '{}'", mw.name);
                }
                result
            }
            PatternAtPosition::Translation(trans) => {
                info!("🎯 Laravel LSP: Found translation pattern: '{}' at {}:{}-{}",
                    trans.key, trans.line, trans.column, trans.end_column);
                self.create_translation_location_from_salsa(&trans).await
            }
            PatternAtPosition::Asset(asset) => {
                debug!("Laravel LSP: Found asset: {}", asset.path);
                self.create_asset_location_from_salsa(&asset).await
            }
            PatternAtPosition::Binding(binding) => {
                debug!("Laravel LSP: Found binding: {}", binding.name);
                self.create_binding_location_from_salsa(&binding).await
            }
            PatternAtPosition::Route(route) => {
                debug!("Laravel LSP: Found route: {}", route.name);
                self.create_route_location_from_salsa(&route).await
            }
            PatternAtPosition::Url(url) => {
                debug!("Laravel LSP: Found url: {}", url.path);
                self.create_url_location_from_salsa(&url).await
            }
            PatternAtPosition::Action(action) => {
                debug!("Laravel LSP: Found action: {}", action.action);
                self.create_action_location_from_salsa(&action).await
            }
        };

        if location.is_none() {
            debug!("Laravel LSP: Could not resolve location for pattern");
        }

        Ok(location)
    }

    // ❌ REMOVED: hover handler
    // We don't advertise hover capability, so this method is not needed.
    // Navigation is handled by goto_definition (Option+click).



    // NOTE: completion handler removed - capability not advertised in ServerCapabilities

    // NOTE: code_lens handler removed - Zed doesn't support custom LSP commands
}

// ❌ REMOVED: code_lens helper methods (extract_view_name_from_path, find_all_references_to_view)
// Zed doesn't support custom LSP commands, so code lens was not functional.




#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with environment-based filtering
    // Default to INFO level, can be overridden with RUST_LOG env var
    // e.g., RUST_LOG=debug for verbose output during development
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    info!("========================================");
    info!("🚀 Laravel Language Server STARTING 🚀");
    info!("========================================");
    
    // Create the LSP service
    let (service, socket) = LspService::new(LaravelLanguageServer::new);
    
    // Read from stdin and write to stdout (standard LSP communication)
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    
    // Run the server
    Server::new(stdin, stdout, socket)
        .serve(service)
        .await;
    
    info!("Laravel Language Server stopped");
    Ok(())
}