use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use tokio::time::sleep;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};



// Our tree-sitter modules
mod parser;
mod queries;
mod config;
mod env_parser;
mod middleware_parser;
mod service_provider_analyzer;

// Salsa 0.25 implementation (incremental computation)
mod salsa_impl;

use parser::{language_blade, language_php, parse_blade, parse_php};
use queries::{
    find_binding_calls, find_blade_components, find_directives,
    find_env_calls, find_livewire_components, find_middleware_calls,
    find_translation_calls, find_view_calls,
};
use config::LaravelConfig;
use env_parser::EnvFileCache;
use middleware_parser::resolve_class_to_file;
use service_provider_analyzer::{analyze_service_providers, ServiceProviderRegistry};

// Salsa 0.25 database - integrated via actor pattern for async compatibility
use salsa_impl::{
    SalsaActor, SalsaHandle, PatternAtPosition,
    ViewReferenceData, ComponentReferenceData, DirectiveReferenceData,
    EnvReferenceData, ConfigReferenceData, LivewireReferenceData,
    MiddlewareReferenceData, TranslationReferenceData, AssetReferenceData, BindingReferenceData,
};

// ============================================================================
// PART 1: Core Language Server Implementation
// ============================================================================


/// A reference to a Laravel view from another file
#[derive(Debug, Clone, serde::Serialize)]
struct ReferenceLocation {
    /// The file that contains the reference
    file_path: PathBuf,
    /// The URI of the file (for LSP operations)
    uri: Url,
    /// The line number where the reference occurs (0-based)
    line: u32,
    /// The character position where the reference starts (0-based)
    character: u32,
    /// The type of reference (controller, component, livewire, route, etc.)
    reference_type: ReferenceType,
    /// The actual text that was matched
    matched_text: String,
}

/// Types of references we can find to Laravel views
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
enum ReferenceType {
    /// Reference from a controller method (view() call)
    Controller,
    /// Reference from a Livewire component
    LivewireComponent,
    /// Reference from a route definition
    Route,
    /// Reference from another Blade template (@extends, @include)
    BladeTemplate,
}

// Removed: Old cache structures (FileReferences, ParsedMatches, ReferenceCache)
// These have been replaced by the high-performance PerformanceCache system

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
    /// Laravel project configuration (paths for views, components, Livewire, etc.)
    config: Arc<RwLock<Option<LaravelConfig>>>,
    /// Track when we last attempted to initialize config (for retry logic)
    config_last_attempt: Arc<RwLock<Option<std::time::Instant>>>,
    /// Store diagnostics per file (for hover filtering)
    diagnostics: Arc<RwLock<HashMap<Url, Vec<Diagnostic>>>>,
    /// Environment variable cache (.env, .env.example, .env.local)
    env_cache: Arc<RwLock<Option<EnvFileCache>>>,
    /// Track when we last attempted to initialize env_cache (for retry logic)
    env_cache_last_attempt: Arc<RwLock<Option<std::time::Instant>>>,
    /// Service provider registry (middleware, bindings, aliases, etc.)
    service_provider_registry: Arc<RwLock<Option<ServiceProviderRegistry>>>,
    /// Track when we last attempted to initialize service_provider_registry
    service_provider_registry_last_attempt: Arc<RwLock<Option<std::time::Instant>>>,
    /// Pending debounced diagnostic tasks (uri -> task handle)
    pending_diagnostics: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Debounce delay for diagnostics in milliseconds (default: 200ms)
    debounce_delay_ms: u64,
    /// Salsa 0.25 database handle (runs on dedicated thread via actor pattern)
    salsa: SalsaHandle,
}

impl LaravelLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            root_path: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(None)),
            config_last_attempt: Arc::new(RwLock::new(None)),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            env_cache: Arc::new(RwLock::new(None)),
            env_cache_last_attempt: Arc::new(RwLock::new(None)),
            service_provider_registry: Arc::new(RwLock::new(None)),
            service_provider_registry_last_attempt: Arc::new(RwLock::new(None)),
            pending_diagnostics: Arc::new(RwLock::new(HashMap::new())),
            debounce_delay_ms: 200,  // 200ms for diagnostics
            salsa: SalsaActor::spawn(),
        }
    }

    /// Check if a position has a diagnostic (yellow squiggle)
    /// Returns true if there's a diagnostic at this position

    // Removed: parse_and_cache_patterns - functionality moved to performance_cache



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

    /// Try to discover Laravel config from a file path
    ///
    /// This implements a hybrid discovery strategy:
    /// - Always tries to find Laravel root from the opened file
    /// - Updates config if discovered root is more specific or file is outside current root
    /// - This handles both nested Laravel projects and files outside initial workspace
    async fn try_discover_from_file(&self, file_path: &Path) {
        // Always try to find the Laravel project root from this file
        let Some(discovered_root) = LaravelConfig::find_project_root(file_path) else {
            debug!("Could not find Laravel project root from file: {:?}", file_path);
            return;
        };

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

        // Discover and store configuration
        match LaravelConfig::discover(&discovered_root) {
            Ok(config) => {
                info!("Laravel configuration discovered successfully");
                *self.config.write().await = Some(config);
                
                // Re-validate all open documents since config changed (view paths, component paths, etc.)
                info!("Laravel LSP: Re-validating all open documents due to config change");
                let documents = self.documents.read().await;
                for (doc_uri, (doc_text, _version)) in documents.iter() {
                    self.validate_and_publish_diagnostics(doc_uri, doc_text).await;
                }
            }
            Err(e) => {
                info!("Failed to discover Laravel config: {}", e);
            }
        }

        // Re-initialize service provider registry with the new root
        info!("========================================");
        info!("🛡️  Re-initializing service provider registry from new root: {:?}", discovered_root);
        info!("========================================");
        match analyze_service_providers(&discovered_root).await {
            Ok(registry) => {
                info!("Laravel LSP: Service provider registry loaded: {} middleware aliases found", registry.middleware_aliases.len());
                if !registry.middleware_aliases.is_empty() {
                    info!("Laravel LSP: Available middleware: {:?}", 
                          registry.middleware_aliases.keys().collect::<Vec<_>>());
                }
                *self.service_provider_registry.write().await = Some(registry);
            }
            Err(e) => {
                info!("Laravel LSP: Failed to analyze service providers: {}", e);
            }
        }

        // Re-initialize environment variable cache with the new root
        info!("========================================");
        info!("📁 Re-initializing env cache from new root: {:?}", discovered_root);
        info!("========================================");
        let mut env_cache = EnvFileCache::new(discovered_root.clone());
        match env_cache.parse_all() {
            Ok(_) => {
                info!("Laravel LSP: Environment variables loaded: {} variables found", env_cache.variables.len());
                if env_cache.variables.is_empty() {
                    info!("Laravel LSP: Warning - env cache is empty! Files checked: {:?}", 
                          env_cache.file_metadata.keys().collect::<Vec<_>>());
                } else {
                    info!("Laravel LSP: Loaded variables: {:?}", 
                          env_cache.variables.keys().collect::<Vec<_>>());
                }
                *self.env_cache.write().await = Some(env_cache);
            }
            Err(e) => {
                info!("Laravel LSP: Failed to parse env files (will continue without env support): {}", e);
            }
        }
    }

    // Removed: preparse_php_file - functionality moved to performance_cache

    /// Refresh the env cache by parsing all .env files from editor buffers (or disk if not open)
    async fn refresh_env_cache_from_buffers(&self, root: &PathBuf) {
        use env_parser::{parse_env_file, parse_env_content};
        
        let mut env_cache = EnvFileCache::new(root.clone());
        
        // Clear existing cache
        env_cache.variables.clear();
        env_cache.file_metadata.clear();
        
        // Define env files in reverse priority order (same as EnvFileCache::parse_all)
        let env_files = vec![
            root.join(".env.example"),
            root.join(".env.local"),
            root.join(".env"),
        ];
        
        let documents = self.documents.read().await;
        
        for env_path in env_files {
            // Check if file is open in editor
            if let Ok(env_uri) = Url::from_file_path(&env_path) {
                if let Some((buffer_content, _version)) = documents.get(&env_uri) {
                    // Parse from editor buffer (includes unsaved changes!)
                    info!("Laravel LSP: Parsing .env from buffer: {:?}", env_path);
                    if let Ok(vars) = parse_env_content(buffer_content, env_path.clone()) {
                        for var in vars {
                            env_cache.variables.insert(var.name.clone(), var);
                        }
                    }
                    continue;
                }
            }
            
            // File not open in editor, parse from disk
            if env_path.exists() {
                info!("Laravel LSP: Parsing .env from disk: {:?}", env_path);
                if let Ok(vars) = parse_env_file(&env_path) {
                    for var in vars {
                        env_cache.variables.insert(var.name.clone(), var);
                    }
                }
            }
        }
        
        info!("Environment variables loaded: {} variables found", env_cache.variables.len());
        *self.env_cache.write().await = Some(env_cache);
        
        // Re-validate all open PHP documents since env vars changed
        info!("Laravel LSP: Re-validating all open documents due to .env change");
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
    // Salsa-based helper functions (for cached pattern data)
    // ========================================================================

    /// Create LocationLink for a view reference from Salsa data
    async fn create_view_location_from_salsa(&self, view: &ViewReferenceData) -> Option<GotoDefinitionResponse> {
        self.try_init_config().await;

        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;
        let possible_paths = config.resolve_view_path(&view.name);

        for path in possible_paths {
            if self.file_exists(&path).await {
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
        self.try_init_config().await;

        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;
        let possible_paths = config.resolve_component_path(&comp.name);

        for path in possible_paths {
            if self.file_exists(&path).await {
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
        self.try_init_config().await;

        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;
        let path = config.resolve_livewire_path(&lw.name)?;

        if self.file_exists(&path).await {
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
        if (dir.name == "extends" || dir.name == "include") && dir.arguments.is_some() {
            let arguments = dir.arguments.as_ref().unwrap();
            if let Some(view_name) = Self::extract_view_from_directive_args(arguments) {
                self.try_init_config().await;

                let config_guard = self.config.read().await;
                let config = config_guard.as_ref()?;
                let possible_paths = config.resolve_view_path(&view_name);

                for path in possible_paths {
                    if self.file_exists(&path).await {
                        if let Ok(target_uri) = Url::from_file_path(&path) {
                            let origin_selection_range = Range {
                                start: Position { line: dir.line, character: dir.column },
                                end: Position { line: dir.line, character: dir.end_column },
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
        }
        None
    }

    /// Create LocationLink for an env reference from Salsa data
    async fn create_env_location_from_salsa(&self, env: &EnvReferenceData) -> Option<GotoDefinitionResponse> {
        self.try_init_env_cache().await;

        let env_cache_guard = self.env_cache.read().await;
        let env_cache = env_cache_guard.as_ref()?;

        if let Some(env_var) = env_cache.get(&env.name) {
            if let Ok(target_uri) = Url::from_file_path(&env_var.file_path) {
                let origin_selection_range = Range {
                    start: Position { line: env.line, character: env.column },
                    end: Position { line: env.line, character: env.end_column },
                };
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range {
                        start: Position { line: env_var.line as u32, character: env_var.column as u32 },
                        end: Position { line: env_var.line as u32, character: (env_var.column + env_var.name.len()) as u32 },
                    },
                    target_selection_range: Range {
                        start: Position { line: env_var.line as u32, character: env_var.column as u32 },
                        end: Position { line: env_var.line as u32, character: (env_var.column + env_var.name.len()) as u32 },
                    },
                }]));
            }
        }
        None
    }

    /// Create LocationLink for a config reference from Salsa data
    async fn create_config_location_from_salsa(&self, config_ref: &ConfigReferenceData) -> Option<GotoDefinitionResponse> {
        self.try_init_config().await;

        let config_guard = self.config.read().await;
        let project_config = config_guard.as_ref()?;

        // Parse config key like "app.name" -> file: config/app.php
        let parts: Vec<&str> = config_ref.key.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let config_file = parts[0];
        let config_path = project_config.root.join("config").join(format!("{}.php", config_file));

        if self.file_exists(&config_path).await {
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

    /// Create LocationLink for a middleware reference from Salsa data
    async fn create_middleware_location_from_salsa(&self, mw: &MiddlewareReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Try to recover service_provider_registry if needed
        if !self.try_init_service_provider_registry().await {
            return None;
        }

        let registry_guard = self.service_provider_registry.read().await;
        let registry = registry_guard.as_ref()?;

        // Look up the middleware alias
        let middleware_reg = registry.get_middleware(&mw.name)?;

        // Try to find the middleware class file
        let middleware_path = if let Some(file_path) = &middleware_reg.file_path {
            Some(file_path.clone())
        } else {
            resolve_class_to_file(&middleware_reg.class_name, root)
        };

        if let Some(path) = middleware_path {
            if self.file_exists(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    let origin_selection_range = Range {
                        start: Position { line: mw.line, character: mw.column },
                        end: Position { line: mw.line, character: mw.end_column },
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

        if self.file_exists(&translation_path).await {
            if let Ok(target_uri) = Url::from_file_path(&translation_path) {
                let origin_selection_range = Range {
                    start: Position { line: trans.line, character: trans.column },
                    end: Position { line: trans.line, character: trans.end_column },
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

    /// Create LocationLink for an asset reference from Salsa data
    async fn create_asset_location_from_salsa(&self, asset: &AssetReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Determine the base path based on helper type
        use salsa_impl::AssetHelperType;
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

        if self.file_exists(&asset_path).await {
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

    /// Create LocationLink for a binding reference from Salsa data
    async fn create_binding_location_from_salsa(&self, binding: &BindingReferenceData) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Try to recover service_provider_registry if needed
        if !self.try_init_service_provider_registry().await {
            return None;
        }

        let registry_guard = self.service_provider_registry.read().await;
        let registry = registry_guard.as_ref()?;

        // Look up the binding
        if let Some(binding_reg) = registry.get_binding(&binding.name) {
            if let Some(file_path) = &binding_reg.file_path {
                if self.file_exists(file_path).await {
                    if let Ok(target_uri) = Url::from_file_path(file_path) {
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

        // If it's a class reference, try to resolve the class to a file
        if binding.is_class_reference {
            if let Some(path) = resolve_class_to_file(&binding.name, root) {
                if self.file_exists(&path).await {
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

        None
    }

    /// Try to initialize env_cache if it's None and we haven't tried recently
    /// Returns true if cache is now available, false otherwise
    async fn try_init_env_cache(&self) -> bool {
        const RETRY_COOLDOWN_SECS: u64 = 30;

        // Check if cache already exists
        if self.env_cache.read().await.is_some() {
            return true;
        }

        // Check cooldown - don't retry too frequently
        let now = std::time::Instant::now();
        {
            let last_attempt = self.env_cache_last_attempt.read().await;
            if let Some(last) = *last_attempt {
                if now.duration_since(last).as_secs() < RETRY_COOLDOWN_SECS {
                    debug!("Laravel LSP: env_cache init skipped - cooldown active");
                    return false;
                }
            }
        }

        // Update last attempt time
        *self.env_cache_last_attempt.write().await = Some(now);

        // Try to initialize
        let root = self.root_path.read().await.clone();
        if let Some(root_path) = root {
            let mut env_cache = EnvFileCache::new(root_path.clone());
            match env_cache.parse_all() {
                Ok(()) => {
                    info!("Laravel LSP: env_cache recovered - {} variables found",
                          env_cache.variables.len());
                    *self.env_cache.write().await = Some(env_cache);
                    return true;
                }
                Err(e) => {
                    tracing::warn!("Laravel LSP: Failed to initialize env_cache: {}", e);
                }
            }
        } else {
            debug!("Laravel LSP: Cannot init env_cache - no root path");
        }

        false
    }

    /// Try to initialize or refresh service_provider_registry if needed
    /// Returns true if registry is now available, false otherwise
    async fn try_init_service_provider_registry(&self) -> bool {
        const RETRY_COOLDOWN_SECS: u64 = 30;

        // Check if registry exists and doesn't need refresh
        {
            let registry_guard = self.service_provider_registry.read().await;
            if let Some(registry) = registry_guard.as_ref() {
                // Check if we need to refresh due to file changes
                if !registry.needs_refresh() {
                    return true;
                }
                info!("Laravel LSP: Service provider files changed, refreshing registry");
            }
        }

        // Check cooldown - don't retry too frequently
        let now = std::time::Instant::now();
        {
            let last_attempt = self.service_provider_registry_last_attempt.read().await;
            if let Some(last) = *last_attempt {
                if now.duration_since(last).as_secs() < RETRY_COOLDOWN_SECS {
                    debug!("Laravel LSP: service_provider_registry init skipped - cooldown active");
                    // Return true if we have a registry (even if stale), false if none
                    return self.service_provider_registry.read().await.is_some();
                }
            }
        }

        // Update last attempt time
        *self.service_provider_registry_last_attempt.write().await = Some(now);

        // Try to initialize/refresh
        let root = self.root_path.read().await.clone();
        if let Some(root_path) = root {
            match analyze_service_providers(&root_path).await {
                Ok(registry) => {
                    info!("Laravel LSP: service_provider_registry recovered - {} middleware aliases found",
                          registry.middleware_aliases.len());
                    *self.service_provider_registry.write().await = Some(registry);
                    return true;
                }
                Err(e) => {
                    tracing::warn!("Laravel LSP: Failed to initialize service_provider_registry: {}", e);
                }
            }
        } else {
            debug!("Laravel LSP: Cannot init service_provider_registry - no root path");
        }

        // Return true if we have a stale registry, false if none
        self.service_provider_registry.read().await.is_some()
    }

    /// Try to initialize or refresh config if needed
    /// Returns true if config is now available, false otherwise
    async fn try_init_config(&self) -> bool {
        const RETRY_COOLDOWN_SECS: u64 = 30;

        // Check if config exists and doesn't need refresh
        {
            let config_guard = self.config.read().await;
            if let Some(config) = config_guard.as_ref() {
                // Check if we need to refresh due to file changes
                if !config.needs_refresh() {
                    return true;
                }
                info!("Laravel LSP: Config files changed, refreshing config");
            }
        }

        // Check cooldown - don't retry too frequently
        let now = std::time::Instant::now();
        {
            let last_attempt = self.config_last_attempt.read().await;
            if let Some(last) = *last_attempt {
                if now.duration_since(last).as_secs() < RETRY_COOLDOWN_SECS {
                    debug!("Laravel LSP: config init skipped - cooldown active");
                    // Return true if we have a config (even if stale), false if none
                    return self.config.read().await.is_some();
                }
            }
        }

        // Update last attempt time
        *self.config_last_attempt.write().await = Some(now);

        // Try to initialize/refresh
        let root = self.root_path.read().await.clone();
        if let Some(root_path) = root {
            match LaravelConfig::discover(&root_path) {
                Ok(config) => {
                    info!("Laravel LSP: config recovered - {} view paths found",
                          config.view_paths.len());
                    *self.config.write().await = Some(config);
                    return true;
                }
                Err(e) => {
                    tracing::warn!("Laravel LSP: Failed to initialize config: {}", e);
                }
            }
        } else {
            debug!("Laravel LSP: Cannot init config - no root path");
        }

        // Return true if we have a stale config, false if none
        self.config.read().await.is_some()
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
            config: self.config.clone(),
            config_last_attempt: self.config_last_attempt.clone(),
            diagnostics: self.diagnostics.clone(),
            env_cache: self.env_cache.clone(),
            env_cache_last_attempt: self.env_cache_last_attempt.clone(),
            service_provider_registry: self.service_provider_registry.clone(),
            service_provider_registry_last_attempt: self.service_provider_registry_last_attempt.clone(),
            pending_diagnostics: self.pending_diagnostics.clone(),
            debounce_delay_ms: self.debounce_delay_ms,
            salsa: self.salsa.clone(),
        }
    }

    /// Validate a document (Blade or PHP) and publish diagnostics
    ///
    /// This function:
    /// 1. Parses the Blade/PHP file with tree-sitter
    /// 2. Finds all view(), View::make(), Route::view(), etc. calls
    /// 3. Creates yellow squiggle warnings for missing files
    /// 4. Publishes diagnostics to the editor
    async fn validate_and_publish_diagnostics(&self, uri: &Url, source: &str) {
        info!("🔍 validate_and_publish_diagnostics called for {}", uri);
        let mut diagnostics = Vec::new();

        // Get the Laravel config
        let config_guard = self.config.read().await;
        let Some(config) = config_guard.as_ref() else {
            info!("   ⚠️  Cannot validate: config not set");
            return;
        };
        info!("   ✅ Config loaded");

        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;

        // Validate PHP files with view() calls and env() calls
        if is_php {
            if let Ok(tree) = parse_php(source) {
                let lang = language_php();
                
                // Check view() calls
                if let Ok(view_calls) = find_view_calls(&tree, source, &lang) {
                    for view_match in view_calls {
                        let possible_paths = config.resolve_view_path(view_match.view_name);
                        let exists = possible_paths.iter().any(|p| p.exists());

                        if !exists {
                            let expected_path = possible_paths.first()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| "unknown".to_string());

                            // Route::view() and Volt::route() should be ERROR
                            // Regular view() calls should be WARNING
                            let severity = if view_match.is_route_view {
                                DiagnosticSeverity::ERROR
                            } else {
                                DiagnosticSeverity::WARNING
                            };

                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: view_match.row as u32,
                                        character: view_match.column as u32,
                                    },
                                    end: Position {
                                        line: view_match.row as u32,
                                        character: view_match.end_column as u32,
                                    },
                                },
                                severity: Some(severity),
                                code: None,
                                source: Some("laravel-lsp".to_string()),
                                message: format!(
                                    "View file not found: '{}'\nExpected at: {}",
                                    view_match.view_name,
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
                
                // Check env() calls - warn if variable not defined
                // Try to recover env_cache if needed
                self.try_init_env_cache().await;
                let env_cache_guard = self.env_cache.read().await;
                if let Some(env_cache) = env_cache_guard.as_ref() {
                    if let Ok(env_calls) = find_env_calls(&tree, source, &lang) {
                        for env_match in env_calls {
                            if !env_cache.contains(env_match.var_name) {
                                // Show WARNING if no fallback (likely to break)
                                // Show INFO if there's a fallback (safe default)
                                let (severity, message) = if env_match.has_fallback {
                                    (
                                        DiagnosticSeverity::INFORMATION,
                                        format!(
                                            "Environment variable '{}' not found in .env files (using fallback value)",
                                            env_match.var_name
                                        )
                                    )
                                } else {
                                    (
                                        DiagnosticSeverity::WARNING,
                                        format!(
                                            "Environment variable '{}' not found in .env files and has no fallback\nDefine it in .env, .env.example, or .env.local",
                                            env_match.var_name
                                        )
                                    )
                                };

                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: env_match.row as u32,
                                            character: env_match.column as u32,
                                        },
                                        end: Position {
                                            line: env_match.row as u32,
                                            character: env_match.end_column as u32,
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
                    }
                }
                drop(env_cache_guard);
                
                // Check middleware calls - warn about undefined middleware or missing class files
                let registry_guard = self.service_provider_registry.read().await;
                let root_guard = self.root_path.read().await;
                if let (Some(registry), Some(root)) = (registry_guard.as_ref(), root_guard.as_ref()) {
                    if let Ok(middleware_calls) = find_middleware_calls(&tree, source, &lang) {
                        for middleware_match in middleware_calls {
                            let middleware_name = middleware_match.middleware_name;
                            
                            // Check if middleware exists in registry
                            info!("Laravel LSP: Checking middleware '{}' in registry", middleware_name);
                            if let Some(middleware_reg) = registry.get_middleware(middleware_name) {
                                info!("Laravel LSP: Middleware '{}' found in registry, class: {}", middleware_name, middleware_reg.class_name);
                                // Middleware is in registry - check if class file exists
                                if let Some(ref file_path) = middleware_reg.file_path {
                                    let class_path = root.join(file_path);
                                    info!("Laravel LSP: Checking file path: {:?}, exists: {}", class_path, class_path.exists());
                                    if !class_path.exists() {
                                        // ERROR - middleware defined but class file missing (will crash at runtime)
                                        info!("Laravel LSP: Creating ERROR diagnostic for missing middleware class file: {}", middleware_name);
                                        let diagnostic = Diagnostic {
                                            range: Range {
                                                start: Position {
                                                    line: middleware_match.row as u32,
                                                    character: middleware_match.column as u32,
                                                },
                                                end: Position {
                                                    line: middleware_match.row as u32,
                                                    character: middleware_match.end_column as u32,
                                                },
                                            },
                                            severity: Some(DiagnosticSeverity::ERROR),
                                            code: None,
                                            source: Some("laravel-lsp".to_string()),
                                            message: format!(
                                                "Middleware '{}' not found\nClass: {}\nExpected at: {}\n\nThe middleware alias is registered but the class file doesn't exist.\n💡 Click to view where the alias is defined.",
                                                middleware_name,
                                                middleware_reg.class_name,
                                                file_path.to_string_lossy()
                                            ),
                                            related_information: None,
                                            tags: None,
                                            code_description: None,
                                            data: None,
                                        };
                                        diagnostics.push(diagnostic);
                                    } else {
                                        info!("Laravel LSP: Middleware '{}' class file exists at {:?}", middleware_name, class_path);
                                    }
                                } else {
                                    info!("Laravel LSP: Middleware '{}' in registry but no file_path resolved - skipping diagnostic", middleware_name);
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
                                if let Some(file_path) = resolve_class_to_file(&app_class, root) {
                                    let class_path = root.join(&file_path);
                                    info!("Laravel LSP: Attempting to resolve middleware '{}' as class '{}' at {:?}", middleware_name, app_class, class_path);
                                    
                                    if !class_path.exists() {
                                        // ERROR - middleware not in config and class file doesn't exist
                                        info!("Laravel LSP: Creating ERROR diagnostic for unresolved middleware: {}", middleware_name);
                                        let diagnostic = Diagnostic {
                                            range: Range {
                                                start: Position {
                                                    line: middleware_match.row as u32,
                                                    character: middleware_match.column as u32,
                                                },
                                                end: Position {
                                                    line: middleware_match.row as u32,
                                                    character: middleware_match.end_column as u32,
                                                },
                                            },
                                            severity: Some(DiagnosticSeverity::ERROR),
                                            code: None,
                                            source: Some("laravel-lsp".to_string()),
                                            message: format!(
                                                "Middleware '{}' not found\nExpected at: {}\n\nCreate the middleware or add an alias in bootstrap/app.php",
                                                middleware_name,
                                                file_path.to_string_lossy()
                                            ),
                                            related_information: None,
                                            tags: None,
                                            code_description: None,
                                            data: None,
                                        };
                                        diagnostics.push(diagnostic);
                                    } else {
                                        info!("Laravel LSP: Middleware '{}' resolved by convention, file exists at {:?}", middleware_name, class_path);
                                    }
                                } else {
                                    // Can't resolve - show INFO as we don't know where to check
                                    info!("Laravel LSP: Middleware '{}' NOT found in registry and can't resolve file path, creating INFO diagnostic", middleware_name);
                                    let diagnostic = Diagnostic {
                                        range: Range {
                                            start: Position {
                                                line: middleware_match.row as u32,
                                                character: middleware_match.column as u32,
                                            },
                                            end: Position {
                                                line: middleware_match.row as u32,
                                                character: middleware_match.end_column as u32,
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
                }
                drop(root_guard);
                drop(registry_guard);
                
                // Check translation calls - warn about missing translation files
                if let Ok(translation_calls) = find_translation_calls(&tree, source, &lang) {
                    let root_guard = self.root_path.read().await;
                    if let Some(root) = root_guard.as_ref() {
                        for trans_match in translation_calls {
                            let translation_key = trans_match.translation_key;
                            
                            // Determine if this is a dotted key (PHP file) or text key (JSON file)
                            let is_dotted_key = translation_key.contains('.') && !translation_key.contains(' ');
                            let is_multi_word = translation_key.contains(' ');
                            
                            let mut file_exists = false;
                            let mut expected_location = String::new();
                            
                            if is_multi_word || (!is_dotted_key && !translation_key.contains('.')) {
                                // Check JSON files
                                let json_paths = [
                                    root.join("lang/en.json"),
                                    root.join("resources/lang/en.json"),
                                ];
                                
                                for json_path in &json_paths {
                                    if json_path.exists() {
                                        file_exists = true;
                                        break;
                                    }
                                }
                                
                                if !file_exists {
                                    expected_location = "lang/en.json or resources/lang/en.json".to_string();
                                }
                            } else if is_dotted_key {
                                // Check PHP file based on first segment
                                let parts: Vec<&str> = translation_key.split('.').collect();
                                if !parts.is_empty() {
                                    let file_name = parts[0];
                                    let php_paths = [
                                        root.join("lang/en").join(format!("{}.php", file_name)),
                                        root.join("resources/lang/en").join(format!("{}.php", file_name)),
                                    ];
                                    
                                    for php_path in &php_paths {
                                        if php_path.exists() {
                                            file_exists = true;
                                            break;
                                        }
                                    }
                                    
                                    if !file_exists {
                                        expected_location = format!("lang/en/{}.php or resources/lang/en/{}.php", file_name, file_name);
                                    }
                                }
                            }
                            
                            // Create diagnostic if file not found
                            if !file_exists {
                                // ERROR for dotted keys (likely to break at runtime)
                                // INFO for text keys (might be intentional)
                                let (severity, message) = if is_dotted_key {
                                    (
                                        DiagnosticSeverity::ERROR,
                                        format!(
                                            "Translation file not found for key '{}'\nExpected at: {}",
                                            translation_key,
                                            expected_location
                                        )
                                    )
                                } else {
                                    (
                                        DiagnosticSeverity::INFORMATION,
                                        format!(
                                            "Translation file not found for key '{}'\nCreate {} to add this translation",
                                            translation_key,
                                            expected_location
                                        )
                                    )
                                };
                                
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: trans_match.row as u32,
                                            character: trans_match.column as u32,
                                        },
                                        end: Position {
                                            line: trans_match.row as u32,
                                            character: trans_match.end_column as u32,
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
                    }
                    drop(root_guard);
                }
                
                // Check container binding calls - error for undefined bindings or missing class files
                let registry_guard = self.service_provider_registry.read().await;
                let root_guard = self.root_path.read().await;
                if let (Some(registry), Some(root)) = (registry_guard.as_ref(), root_guard.as_ref()) {
                    if let Ok(binding_calls) = find_binding_calls(&tree, source, &lang) {
                        for binding_match in binding_calls {
                            // Only validate string bindings (not Class::class references)
                            // Class::class references might be auto-resolved by Laravel
                            if !binding_match.is_class_reference {
                                let binding_name = binding_match.binding_name;
                                
                                // Check if binding exists in registry
                                if let Some(binding_reg) = registry.get_binding(binding_name) {
                                    // Binding exists - check if the concrete class file exists
                                    if let Some(ref file_path) = binding_reg.file_path {
                                        let class_path = root.join(file_path);
                                        if !class_path.exists() {
                                            // ERROR - binding exists but class file is missing
                                            info!("Laravel LSP: Creating ERROR diagnostic for binding with missing class: {}", binding_name);
                                            
                                            // Build the diagnostic message with registration location
                                            let mut message = format!(
                                                "Binding '{}' registered but class file not found\nExpected class at: {}",
                                                binding_name,
                                                file_path.to_string_lossy()
                                            );
                                            
                                            // Add registration location if available
                                            if let Some(ref source_file) = binding_reg.source_file {
                                                let registered_in = source_file.file_name()
                                                    .and_then(|n| n.to_str())
                                                    .unwrap_or("service provider");
                                                
                                                if let Some(line) = binding_reg.source_line {
                                                    message.push_str(&format!("\n\nBound in: {}:{}", registered_in, line + 1));
                                                } else {
                                                    message.push_str(&format!("\n\nBound in: {}", registered_in));
                                                }
                                            }
                                            
                                            message.push_str(&format!("\nConcrete class: {}", binding_reg.concrete_class));
                                            
                                            let diagnostic = Diagnostic {
                                                range: Range {
                                                    start: Position {
                                                        line: binding_match.row as u32,
                                                        character: binding_match.column as u32,
                                                    },
                                                    end: Position {
                                                        line: binding_match.row as u32,
                                                        character: binding_match.end_column as u32,
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
                                    
                                    if !framework_bindings.contains(&binding_name) {
                                        // ERROR - binding not found and not a known framework binding
                                        info!("Laravel LSP: Creating ERROR diagnostic for undefined binding: {}", binding_name);
                                        let diagnostic = Diagnostic {
                                            range: Range {
                                                start: Position {
                                                    line: binding_match.row as u32,
                                                    character: binding_match.column as u32,
                                                },
                                                end: Position {
                                                    line: binding_match.row as u32,
                                                    character: binding_match.end_column as u32,
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
                }
                drop(root_guard);
                drop(registry_guard);
            }

            // Store and publish diagnostics for PHP files
            self.diagnostics.write().await.insert(uri.clone(), diagnostics.clone());
            self.client.publish_diagnostics(uri.clone(), diagnostics, None).await;
            return;
        }

        // Only validate Blade files beyond this point
        if !is_blade {
            return;
        }

        // Parse the Blade file
        let Ok(tree) = parse_blade(source) else {
            debug!("Failed to parse Blade file for diagnostics");
            return;
        };

        let lang = language_blade();
        
        // Also parse Blade files with PHP parser to catch {{ __() }} syntax
        if let Ok(php_tree) = parse_php(source) {
            let php_lang = language_php();
            
            // Check translation calls in PHP expressions within Blade
            if let Ok(translation_calls) = find_translation_calls(&php_tree, source, &php_lang) {
                let root_guard = self.root_path.read().await;
                if let Some(root) = root_guard.as_ref() {
                    for trans_match in translation_calls {
                        let translation_key = trans_match.translation_key;
                        
                        // Determine if this is a dotted key (PHP file) or text key (JSON file)
                        let is_dotted_key = translation_key.contains('.') && !translation_key.contains(' ');
                        let is_multi_word = translation_key.contains(' ');
                        
                        let mut file_exists = false;
                        let mut expected_location = String::new();
                        
                        if is_multi_word || (!is_dotted_key && !translation_key.contains('.')) {
                            // Check JSON files
                            let json_paths = [
                                root.join("lang/en.json"),
                                root.join("resources/lang/en.json"),
                            ];
                            
                            for json_path in &json_paths {
                                if json_path.exists() {
                                    file_exists = true;
                                    break;
                                }
                            }
                            
                            if !file_exists {
                                expected_location = "lang/en.json or resources/lang/en.json".to_string();
                            }
                        } else if is_dotted_key {
                            // Check PHP file based on first segment
                            let parts: Vec<&str> = translation_key.split('.').collect();
                            if !parts.is_empty() {
                                let file_name = parts[0];
                                let php_paths = [
                                    root.join("lang/en").join(format!("{}.php", file_name)),
                                    root.join("resources/lang/en").join(format!("{}.php", file_name)),
                                ];
                                
                                for php_path in &php_paths {
                                    if php_path.exists() {
                                        file_exists = true;
                                        break;
                                    }
                                }
                                
                                if !file_exists {
                                    expected_location = format!("lang/en/{}.php or resources/lang/en/{}.php", file_name, file_name);
                                }
                            }
                        }
                        
                        // Create diagnostic if file not found
                        if !file_exists {
                            // ERROR for dotted keys (likely to break at runtime)
                            // INFO for text keys (might be intentional)
                            let (severity, message) = if is_dotted_key {
                                (
                                    DiagnosticSeverity::ERROR,
                                    format!(
                                        "Translation file not found for key '{}'\nExpected at: {}",
                                        translation_key,
                                        expected_location
                                    )
                                )
                            } else {
                                (
                                    DiagnosticSeverity::INFORMATION,
                                    format!(
                                        "Translation file not found for key '{}'\nCreate {} to add this translation",
                                        translation_key,
                                        expected_location
                                    )
                                )
                            };
                            
                            let diagnostic = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: trans_match.row as u32,
                                        character: trans_match.column as u32,
                                    },
                                    end: Position {
                                        line: trans_match.row as u32,
                                        character: trans_match.end_column as u32,
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
                }
                drop(root_guard);
            }
        }

        // Check @extends and @include directives
        if let Ok(directives) = find_directives(&tree, source, &lang) {
            for directive in directives {
                // Only validate @extends and @include
                if directive.directive_name == "extends" || directive.directive_name == "include" {
                    if let Some(view_name) = Self::extract_view_from_directive_args(
                        directive.arguments.unwrap_or("")
                    ) {
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
                                        line: directive.row as u32,
                                        character: directive.column as u32,
                                    },
                                    end: Position {
                                        line: directive.row as u32,
                                        character: directive.end_column as u32,
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

        // Check Blade components (<x-button>)
        if let Ok(components) = find_blade_components(&tree, source, &lang) {
            for component in components {
                let possible_paths = config.resolve_component_path(component.component_name);
                let exists = possible_paths.iter().any(|p| p.exists());

                if !exists {
                    let expected_path = possible_paths.first()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    let diagnostic = Diagnostic {
                        range: Range {
                            start: Position {
                                line: component.row as u32,
                                character: component.column as u32,
                            },
                            end: Position {
                                line: component.row as u32,
                                character: component.end_column as u32,
                            },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: None,
                        source: Some("laravel-lsp".to_string()),
                        message: format!(
                            "Blade component not found: '{}'\nExpected at: {}",
                            component.component_name,
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

        // Check Livewire components
        if let Ok(livewire) = find_livewire_components(&tree, source, &lang) {
            for lw in livewire {
                if let Some(livewire_path) = config.resolve_livewire_path(lw.component_name) {
                    if !livewire_path.exists() {
                        let diagnostic = Diagnostic {
                            range: Range {
                                start: Position {
                                    line: lw.row as u32,
                                    character: lw.column as u32,
                                },
                                end: Position {
                                    line: lw.row as u32,
                                    character: lw.end_column as u32,
                                },
                            },
                            severity: Some(DiagnosticSeverity::WARNING),
                            code: None,
                            source: Some("laravel-lsp".to_string()),
                            message: format!(
                                "Livewire component not found: '{}'\nExpected at: {}",
                                lw.component_name,
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
        }

        // Check @lang directives for translation files
        if let Ok(directives) = find_directives(&tree, source, &lang) {
            let root_guard = self.root_path.read().await;
            if let Some(root) = root_guard.as_ref() {
                for directive in directives {
                    // Only validate @lang directives
                    if directive.directive_name == "lang" {
                        if let Some(translation_key) = Self::extract_view_from_directive_args(
                            directive.arguments.unwrap_or("")
                        ) {
                            // Determine if this is a dotted key (PHP file) or text key (JSON file)
                            let is_dotted_key = translation_key.contains('.') && !translation_key.contains(' ');
                            let is_multi_word = translation_key.contains(' ');
                            
                            let mut file_exists = false;
                            let mut expected_location = String::new();
                            
                            if is_multi_word || (!is_dotted_key && !translation_key.contains('.')) {
                                // Check JSON files
                                let json_paths = [
                                    root.join("lang/en.json"),
                                    root.join("resources/lang/en.json"),
                                ];
                                
                                for json_path in &json_paths {
                                    if json_path.exists() {
                                        file_exists = true;
                                        break;
                                    }
                                }
                                
                                if !file_exists {
                                    expected_location = "lang/en.json or resources/lang/en.json".to_string();
                                }
                            } else if is_dotted_key {
                                // Check PHP file based on first segment
                                let parts: Vec<&str> = translation_key.split('.').collect();
                                if !parts.is_empty() {
                                    let file_name = parts[0];
                                    let php_paths = [
                                        root.join("lang/en").join(format!("{}.php", file_name)),
                                        root.join("resources/lang/en").join(format!("{}.php", file_name)),
                                    ];
                                    
                                    for php_path in &php_paths {
                                        if php_path.exists() {
                                            file_exists = true;
                                            break;
                                        }
                                    }
                                    
                                    if !file_exists {
                                        expected_location = format!("lang/en/{}.php or resources/lang/en/{}.php", file_name, file_name);
                                    }
                                }
                            }
                            
                            // Create diagnostic if file not found
                            if !file_exists {
                                // WARNING for dotted keys (more likely to be actual errors)
                                // INFO for text keys (might be intentional)
                                let (severity, message) = if is_dotted_key {
                                    (
                                        DiagnosticSeverity::WARNING,
                                        format!(
                                            "Translation file not found for key '{}'\nExpected at: {}",
                                            translation_key,
                                            expected_location
                                        )
                                    )
                                } else {
                                    (
                                        DiagnosticSeverity::INFORMATION,
                                        format!(
                                            "Translation file not found for key '{}'\nCreate {} to add this translation",
                                            translation_key,
                                            expected_location
                                        )
                                    )
                                };
                                
                                let diagnostic = Diagnostic {
                                    range: Range {
                                        start: Position {
                                            line: directive.row as u32,
                                            character: directive.string_column as u32,
                                        },
                                        end: Position {
                                            line: directive.row as u32,
                                            character: directive.string_end_column as u32,
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
                    }
                }
            }
            drop(root_guard);
        }

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
        info!("========================================");
        info!("🔧 Laravel LSP: INITIALIZE CALLED 🔧");
        info!("========================================");

        // Store the root path and discover Laravel configuration
        if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                *self.root_path.write().await = Some(path.clone());
                info!("✅ Laravel LSP: Root path set to {:?}", path);

                // Discover Laravel configuration
                match LaravelConfig::discover(&path) {
                    Ok(config) => {
                        info!("Laravel configuration discovered successfully");
                        *self.config.write().await = Some(config);
                    }
                    Err(e) => {
                        info!("Failed to discover Laravel config (will use defaults): {}", e);
                        // We'll continue with default paths
                    }
                }
                
                // Initialize environment variable cache
                info!("========================================");
                info!("📁 Initializing env cache from root: {:?}", path);
                info!("========================================");
                let mut env_cache = EnvFileCache::new(path.clone());
                match env_cache.parse_all() {
                    Ok(_) => {
                        info!("Laravel LSP: Environment variables loaded: {} variables found", env_cache.variables.len());
                        if env_cache.variables.is_empty() {
                            info!("Laravel LSP: Warning - env cache is empty! Files checked: {:?}", 
                                  env_cache.file_metadata.keys().collect::<Vec<_>>());
                        } else {
                            info!("Laravel LSP: Loaded variables: {:?}", 
                                  env_cache.variables.keys().collect::<Vec<_>>());
                        }
                        *self.env_cache.write().await = Some(env_cache);
                    }
                    Err(e) => {
                        info!("Laravel LSP: Failed to parse env files (will continue without env support): {}", e);
                    }
                }
                
                // Initialize service provider registry
                info!("========================================");
                info!("🛡️  Initializing service provider registry from root: {:?}", path);
                info!("🚀 LARAVEL LSP v2024-12-21-OPTION3-v2 - NO PREPARSE ON CHANGE");
                info!("========================================");
                match analyze_service_providers(&path).await {
                    Ok(registry) => {
                        info!("Laravel LSP: Service provider registry loaded: {} middleware aliases, {} bindings, {} singletons", 
                              registry.middleware_aliases.len(), 
                              registry.bindings.len(),
                              registry.singletons.len());
                        
                        // Debug: Show some binding examples
                        if let Some(cache_binding) = registry.bindings.get("cache").or_else(|| registry.singletons.get("cache")) {
                            info!("Laravel LSP: DEBUG - 'cache' binding found: concrete={}, source_file={:?}, source_line={:?}", 
                                  cache_binding.concrete_class,
                                  cache_binding.source_file,
                                  cache_binding.source_line);
                        } else {
                            info!("Laravel LSP: DEBUG - 'cache' binding NOT found in registry!");
                        }
                        if !registry.middleware_aliases.is_empty() {
                            info!("Laravel LSP: Available middleware: {:?}", 
                                  registry.middleware_aliases.keys().collect::<Vec<_>>());
                        }
                        *self.service_provider_registry.write().await = Some(registry);
                    }
                    Err(e) => {
                        info!("Laravel LSP: Failed to analyze service providers: {}", e);
                    }
                }
            }
        }
        
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // We support go-to-definition
                definition_provider: Some(OneOf::Left(true)),
                
                // We support code lenses for showing references
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                
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
                
                // Hover support for documentation
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                
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
        info!("Laravel LSP: Server initialized");
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
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        debug!("Laravel LSP: Document opened: {}", uri);
        self.documents.write().await.insert(uri.clone(), (text.clone(), version));

        // Try to discover Laravel config from this file if we don't have one yet
        if let Ok(file_path) = uri.to_file_path() {
            self.try_discover_from_file(&file_path).await;

            // Update Salsa database with new file content
            if let Err(e) = self.salsa.update_file(file_path, version, text.clone()).await {
                debug!("Failed to update Salsa database: {}", e);
            }
        }

        // Validate and publish diagnostics for Blade files
        self.validate_and_publish_diagnostics(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            debug!("Laravel LSP: Document changed: {} (version: {})", uri, version);
            self.documents.write().await.insert(uri.clone(), (change.text.clone(), version));

            // Update Salsa database with new file content
            if let Ok(file_path) = uri.to_file_path() {
                if let Err(e) = self.salsa.update_file(file_path.clone(), version, change.text.clone()).await {
                    debug!("Failed to update Salsa database: {}", e);
                }

                // Check if this is an .env file and refresh env cache if needed
                if let Some(file_name) = file_path.file_name().and_then(|n| n.to_str()) {
                    if file_name == ".env" || file_name == ".env.example" || file_name == ".env.local" {
                        info!("Laravel LSP: .env file changed in buffer, refreshing environment cache");
                        if let Some(root) = self.root_path.read().await.as_ref() {
                            // Parse from buffer (unsaved changes) instead of disk
                            self.refresh_env_cache_from_buffers(root).await;
                        }
                    }
                }
            }
            
            // ONLY debounce diagnostics (200ms) - Salsa handles hover/goto automatically
            self.schedule_debounced_diagnostics(&uri, &change.text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("🔔 Laravel LSP: did_save called for {}", uri);
        
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

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let is_php = uri.path().ends_with(".php");
        if !is_php {
            return Ok(None);
        }

        // Convert URI to file path for Salsa lookup
        let file_path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };

        // First, try to get patterns from Salsa (cached/incremental computation)
        if let Ok(Some(patterns)) = self.salsa.get_patterns(file_path.clone()).await {
            // Use find_at_position for efficient lookup
            if let Some(pattern) = patterns.find_at_position(position.line, position.character) {
                match pattern {
                    PatternAtPosition::Component(comp) => {
                        debug!("Laravel LSP: [Salsa] Found Blade component: {}", comp.name);
                        return Ok(self.create_component_location_from_salsa(&comp).await);
                    }
                    PatternAtPosition::Livewire(lw) => {
                        debug!("Laravel LSP: [Salsa] Found Livewire component: {}", lw.name);
                        return Ok(self.create_livewire_location_from_salsa(&lw).await);
                    }
                    PatternAtPosition::Directive(dir) => {
                        debug!("Laravel LSP: [Salsa] Found directive: @{}", dir.name);
                        return Ok(self.create_directive_location_from_salsa(&dir).await);
                    }
                    PatternAtPosition::View(view) => {
                        debug!("Laravel LSP: [Salsa] Found view call: {}", view.name);
                        return Ok(self.create_view_location_from_salsa(&view).await);
                    }
                    PatternAtPosition::EnvRef(env) => {
                        debug!("Laravel LSP: [Salsa] Found env call: {}", env.name);
                        return Ok(self.create_env_location_from_salsa(&env).await);
                    }
                    PatternAtPosition::ConfigRef(config) => {
                        debug!("Laravel LSP: [Salsa] Found config call: {}", config.key);
                        return Ok(self.create_config_location_from_salsa(&config).await);
                    }
                    PatternAtPosition::Middleware(mw) => {
                        debug!("Laravel LSP: [Salsa] Found middleware call: {}", mw.name);
                        return Ok(self.create_middleware_location_from_salsa(&mw).await);
                    }
                    PatternAtPosition::Translation(trans) => {
                        debug!("Laravel LSP: [Salsa] Found translation call: {}", trans.key);
                        return Ok(self.create_translation_location_from_salsa(&trans).await);
                    }
                    PatternAtPosition::Asset(asset) => {
                        debug!("Laravel LSP: [Salsa] Found asset call: {}", asset.path);
                        return Ok(self.create_asset_location_from_salsa(&asset).await);
                    }
                    PatternAtPosition::Binding(binding) => {
                        debug!("Laravel LSP: [Salsa] Found binding call: {}", binding.name);
                        return Ok(self.create_binding_location_from_salsa(&binding).await);
                    }
                }
            }
        }

        debug!("Laravel LSP: No definition found");
        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let start_time = std::time::Instant::now();
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        // Don't show hover if there's already a diagnostic at this position
        if self.has_diagnostic_at_position(&uri, position).await {
            return Ok(None);
        }

        // Only process PHP files (including Blade files that end in .php)
        let is_php = uri.path().ends_with(".php");
        if !is_php {
            return Ok(None);
        }

        // 🚀 Use incremental computation for hover info
        let hover_info = self.get_incremental_hover_info(&uri, position.line, position.character).await;
        
        let duration = start_time.elapsed();
        if duration > std::time::Duration::from_millis(50) {
            warn!("Laravel LSP: Slow hover response: {}ms for {}", duration.as_millis(), uri);
        } else {
            debug!("Laravel LSP: Incremental hover response: {}ms", duration.as_millis());
        }

        Ok(hover_info)
    }



    // NOTE: completion handler removed - capability not advertised in ServerCapabilities

    async fn code_lens(&self, params: CodeLensParams) -> jsonrpc::Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;

        // Only provide code lenses for Blade files
        if let Ok(file_path) = uri.to_file_path() {
            if let Some(extension) = file_path.extension() {
                if extension != "php" || !file_path.to_string_lossy().contains(".blade.") {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }

        debug!("Laravel LSP: Providing code lenses for: {}", uri);

        // Extract view name from file path
        let view_name = match self.extract_view_name_from_path(&uri).await {
            Some(name) => name,
            None => {
                debug!("Could not extract view name from path: {}", uri);
                return Ok(None);
            }
        };

        // Find all references to this view
        let references = self.find_all_references_to_view(&view_name).await;

        if references.is_empty() {
            return Ok(None);
        }

        // Create a code lens at the top of the file
        let code_lens = CodeLens {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 0 },
            },
            command: Some(Command {
                title: format!("{} reference{}", references.len(), if references.len() == 1 { "" } else { "s" }),
                command: "laravel.showReferences".to_string(),
                arguments: Some(vec![
                    serde_json::to_value(&uri).unwrap(),
                    serde_json::to_value(&Position { line: 0, character: 0 }).unwrap(),
                    serde_json::to_value(&references).unwrap(),
                ]),
            }),
            data: None,
        };

        Ok(Some(vec![code_lens]))
    }
}

impl LaravelLanguageServer {
    /// Extract view name from a Blade file path
    async fn extract_view_name_from_path(&self, uri: &Url) -> Option<String> {
        let file_path = uri.to_file_path().ok()?;
        debug!("Extracting view name from file path: {:?}", file_path);
        
        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;
        debug!("Laravel config root: {:?}", config.root);
        debug!("View paths: {:?}", config.view_paths);

        for views_path in &config.view_paths {
            // Convert relative view path to absolute path
            let absolute_views_path = config.root.join(views_path);
            debug!("Checking against absolute view path: {:?}", absolute_views_path);
            
            if let Ok(relative_path) = file_path.strip_prefix(&absolute_views_path) {
                debug!("File is within view path, relative path: {:?}", relative_path);
                let mut view_name = relative_path.to_string_lossy().to_string();
                debug!("Initial view name: {}", view_name);
                
                // Remove .blade.php extension
                if view_name.ends_with(".blade.php") {
                    view_name = view_name[..view_name.len() - 10].to_string();
                    debug!("After removing .blade.php extension: {}", view_name);
                } else {
                    debug!("Warning: View name doesn't end with .blade.php: {}", view_name);
                }
                
                // Convert path separators to dots
                view_name = view_name.replace(std::path::MAIN_SEPARATOR, ".");
                view_name = view_name.replace('/', ".");
                debug!("Final view name after path conversion: {}", view_name);
                
                return Some(view_name);
            } else {
                debug!("File path {:?} is not within view path {:?}", file_path, absolute_views_path);
            }
        }
        
        debug!("Could not extract view name - file is not in any configured view path");
        None
    }

    /// Find all references to a specific view across the project
    async fn find_all_references_to_view(&self, view_name: &str) -> Vec<ReferenceLocation> {
        let mut all_references = Vec::new();
        
        // TODO: Add caching back when needed - for now search directly

        // No cached references, need to search the project
        debug!("Searching for references to view: {}", view_name);
        
        let config_guard = self.config.read().await;
        let root_guard = self.root_path.read().await;
        
        if let (Some(config), Some(root_path)) = (config_guard.as_ref(), root_guard.as_ref()) {
            // Search in controllers
            all_references.extend(self.find_controller_references(view_name, root_path, config).await);
            
            // Search in Blade templates
            all_references.extend(self.find_blade_references(view_name, config).await);
            
            // Search in Livewire components
            all_references.extend(self.find_livewire_references(view_name, root_path).await);
            
            // Search in routes
            all_references.extend(self.find_route_references(view_name, root_path).await);
        }

        // TODO: Cache results in performance_cache when needed

        debug!("Found {} total references for view: {}", all_references.len(), view_name);
        all_references
    }

    /// Search for view references in controller files
    async fn find_controller_references(
        &self,
        view_name: &str,
        root_path: &Path,
        _config: &LaravelConfig,
    ) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();
        let controllers_path = root_path.join("app/Http/Controllers");
        
        if !controllers_path.exists() {
            return references;
        }

        // Search for view() calls with this view name
        let view_patterns = [
            format!("view('{}'", view_name),
            format!("view(\"{}\"", view_name),
            format!("View::make('{}'", view_name),
            format!("View::make(\"{}\"", view_name),
        ];

        if let Ok(entries) = std::fs::read_dir(&controllers_path) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_file() {
                        let file_path = entry.path();
                        if let Some(extension) = file_path.extension() {
                            if extension == "php" {
                                references.extend(
                                    self.search_file_for_patterns(&file_path, &view_patterns, ReferenceType::Controller)
                                );
                            }
                        }
                    } else if file_type.is_dir() {
                        // Recursively search subdirectories
                        references.extend(
                            self.search_directory_for_view(&entry.path(), view_name, ReferenceType::Controller)
                        );
                    }
                }
            }
        }

        references
    }

    /// Search for view references in Blade template files
    async fn find_blade_references(&self, view_name: &str, config: &LaravelConfig) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();

        // Search for @extends and @include directives
        let blade_patterns = [
            format!("@extends('{}')", view_name),
            format!("@extends(\"{}\")", view_name),
            format!("@include('{}')", view_name),
            format!("@include(\"{}\")", view_name),
        ];

        for views_path in &config.view_paths {
            if views_path.exists() {
                references.extend(
                    self.search_directory_for_blade_patterns(views_path, &blade_patterns)
                );
            }
        }

        references
    }

    /// Search for view references in Livewire components
    async fn find_livewire_references(&self, view_name: &str, root_path: &Path) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();
        let livewire_path = root_path.join("app/Livewire");
        
        if !livewire_path.exists() {
            return references;
        }

        // Search for render() methods that return this view
        let _livewire_patterns = [
            format!("return view('{}'", view_name),
            format!("return view(\"{}\"", view_name),
        ];

        references.extend(
            self.search_directory_for_view(&livewire_path, view_name, ReferenceType::LivewireComponent)
        );

        references
    }

    /// Search for view references in route files
    async fn find_route_references(&self, view_name: &str, root_path: &Path) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();
        let routes_path = root_path.join("routes");
        
        if !routes_path.exists() {
            return references;
        }

        let route_patterns = [
            format!("return view('{}'", view_name),
            format!("return view(\"{}\"", view_name),
        ];

        if let Ok(entries) = std::fs::read_dir(&routes_path) {
            for entry in entries.flatten() {
                let file_path = entry.path();
                if let Some(extension) = file_path.extension() {
                    if extension == "php" {
                        references.extend(
                            self.search_file_for_patterns(&file_path, &route_patterns, ReferenceType::Route)
                        );
                    }
                }
            }
        }

        references
    }

    /// Search a single file for view reference patterns
    fn search_file_for_patterns(
        &self,
        file_path: &Path,
        patterns: &[String],
        reference_type: ReferenceType,
    ) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();

        if let Ok(content) = std::fs::read_to_string(file_path) {
            for (line_num, line) in content.lines().enumerate() {
                for pattern in patterns {
                    if let Some(char_pos) = line.find(pattern) {
                        if let Ok(uri) = Url::from_file_path(file_path) {
                            references.push(ReferenceLocation {
                                file_path: file_path.to_path_buf(),
                                uri,
                                line: line_num as u32,
                                character: char_pos as u32,
                                reference_type: reference_type.clone(),
                                matched_text: pattern.clone(),
                            });
                        }
                    }
                }
            }
        }

        references
    }

    /// Recursively search a directory for view references
    fn search_directory_for_view(
        &self,
        dir_path: &Path,
        view_name: &str,
        reference_type: ReferenceType,
    ) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_file() {
                        if let Some(extension) = path.extension() {
                            if extension == "php" {
                                let patterns = match reference_type {
                                    ReferenceType::Controller => vec![
                                        format!("view('{}'", view_name),
                                        format!("view(\"{}\"", view_name),
                                        format!("View::make('{}'", view_name),
                                        format!("View::make(\"{}\"", view_name),
                                    ],
                                    ReferenceType::LivewireComponent => vec![
                                        format!("return view('{}'", view_name),
                                        format!("return view(\"{}\"", view_name),
                                    ],
                                    ReferenceType::Route => vec![
                                        format!("return view('{}'", view_name),
                                        format!("return view(\"{}\"", view_name),
                                    ],
                                    _ => vec![],
                                };
                                
                                references.extend(
                                    self.search_file_for_patterns(&path, &patterns, reference_type.clone())
                                );
                            }
                        }
                    } else if file_type.is_dir() {
                        references.extend(
                            self.search_directory_for_view(&path, view_name, reference_type.clone())
                        );
                    }
                }
            }
        }

        references
    }

    /// Search directory for Blade template patterns
    fn search_directory_for_blade_patterns(
        &self,
        dir_path: &Path,
        patterns: &[String],
    ) -> Vec<ReferenceLocation> {
        let mut references = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_file() {
                        if let Some(file_name) = path.file_name() {
                            if file_name.to_string_lossy().contains(".blade.php") {
                                references.extend(
                                    self.search_file_for_patterns(&path, patterns, ReferenceType::BladeTemplate)
                                );
                            }
                        }
                    } else if file_type.is_dir() {
                        references.extend(
                            self.search_directory_for_blade_patterns(&path, patterns)
                        );
                    }
                }
            }
        }

        references
    }

    /// Get hover information using Salsa incremental computation
    async fn get_incremental_hover_info(&self, uri: &Url, line: u32, character: u32) -> Option<Hover> {
        // Convert to file path
        let file_path = uri.to_file_path().ok()?;

        // Get patterns from Salsa
        let patterns = self.salsa.get_patterns(file_path).await.ok()??;

        // Find pattern at cursor position
        let pattern = patterns.find_at_position(line, character)?;

        // Generate hover text based on pattern type
        let hover_text = match pattern {
            PatternAtPosition::Component(comp) => {
                format!("**Blade Component**: `<x-{}>`\n\nComponent: `{}`", comp.tag_name, comp.name)
            }
            PatternAtPosition::Livewire(lw) => {
                format!("**Livewire Component**: `<livewire:{}>`\n\nClass: `App\\Livewire\\{}`",
                    lw.name,
                    Self::kebab_to_pascal_case(&lw.name))
            }
            PatternAtPosition::Directive(dir) => {
                if dir.name == "extends" || dir.name == "include" {
                    let view_name = dir.arguments.as_ref()
                        .and_then(|args| Self::extract_view_from_directive_args(args))
                        .unwrap_or_else(|| "unknown".to_string());
                    format!("**Blade Directive**: `@{}`\n\nView: `{}`", dir.name, view_name)
                } else {
                    format!("**Blade Directive**: `@{}`", dir.name)
                }
            }
            PatternAtPosition::View(view) => {
                format!("**View**: `{}`\n\nPath: `resources/views/{}.blade.php`",
                    view.name,
                    view.name.replace('.', "/"))
            }
            PatternAtPosition::EnvRef(env) => {
                let fallback_info = if env.has_fallback { " (has fallback)" } else { "" };
                format!("**Environment Variable**: `{}`{}", env.name, fallback_info)
            }
            PatternAtPosition::ConfigRef(config) => {
                let parts: Vec<&str> = config.key.split('.').collect();
                let file = parts.first().unwrap_or(&"config");
                format!("**Config**: `{}`\n\nFile: `config/{}.php`", config.key, file)
            }
            PatternAtPosition::Middleware(mw) => {
                format!("**Middleware**: `{}`", mw.name)
            }
            PatternAtPosition::Translation(trans) => {
                format!("**Translation**: `{}`", trans.key)
            }
            PatternAtPosition::Asset(asset) => {
                use salsa_impl::AssetHelperType;
                let helper_name = match asset.helper_type {
                    AssetHelperType::Asset => "asset()",
                    AssetHelperType::PublicPath => "public_path()",
                    AssetHelperType::BasePath => "base_path()",
                    AssetHelperType::AppPath => "app_path()",
                    AssetHelperType::StoragePath => "storage_path()",
                    AssetHelperType::DatabasePath => "database_path()",
                    AssetHelperType::LangPath => "lang_path()",
                    AssetHelperType::ConfigPath => "config_path()",
                    AssetHelperType::ResourcePath => "resource_path()",
                    AssetHelperType::Mix => "mix()",
                    AssetHelperType::ViteAsset => "Vite::asset()",
                };
                format!("**Asset Helper**: `{}`\n\nPath: `{}`", helper_name, asset.path)
            }
            PatternAtPosition::Binding(binding) => {
                if binding.is_class_reference {
                    format!("**Container Binding**: `{}`\n\n(Class reference)", binding.name)
                } else {
                    format!("**Container Binding**: `{}`", binding.name)
                }
            }
        };

        Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(hover_text)),
            range: None,
        })
    }
}




#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
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