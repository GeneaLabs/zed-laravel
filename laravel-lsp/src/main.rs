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
mod database;

// Salsa modules
mod salsa_db;

use parser::{language_blade, language_php, parse_blade, parse_php};
use queries::{
    find_asset_calls, find_blade_components, find_directives, find_env_calls,
    find_livewire_components, find_middleware_calls, find_translation_calls, find_view_calls,
    find_binding_calls,
    AssetMatch, AssetHelperType, BindingMatch, ComponentMatch, ConfigMatch, DirectiveMatch, 
    EnvMatch, LivewireMatch, MiddlewareMatch, TranslationMatch, ViewMatch,
};
use config::LaravelConfig;
use env_parser::EnvFileCache;
use middleware_parser::resolve_class_to_file;
use service_provider_analyzer::{analyze_service_providers, ServiceProviderRegistry};

// Incremental database imports
use salsa_db::IncrementalDatabase;

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
    /// Store document contents for analysis
    documents: Arc<RwLock<HashMap<Url, String>>>,
    /// The root path of the Laravel project
    root_path: Arc<RwLock<Option<PathBuf>>>,
    /// Laravel project configuration (paths for views, components, Livewire, etc.)
    config: Arc<RwLock<Option<LaravelConfig>>>,
    /// Store diagnostics per file (for hover filtering)
    diagnostics: Arc<RwLock<HashMap<Url, Vec<Diagnostic>>>>,
    /// Environment variable cache (.env, .env.example, .env.local)
    env_cache: Arc<RwLock<Option<EnvFileCache>>>,
    /// Service provider registry (middleware, bindings, aliases, etc.)
    service_provider_registry: Arc<RwLock<Option<ServiceProviderRegistry>>>,
    /// Pending debounced diagnostic tasks (uri -> task handle)
    pending_diagnostics: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Pending debounced cache update tasks (uri -> task handle)
    pending_cache_updates: Arc<RwLock<HashMap<Url, tokio::task::JoinHandle<()>>>>,
    /// Debounce delay for diagnostics in milliseconds (default: 200ms)
    debounce_delay_ms: u64,
    /// Debounce delay for cache updates in milliseconds (default: 50ms)
    cache_debounce_ms: u64,
    /// High-performance LRU cache for Laravel patterns
    performance_cache: database::ThreadSafeCache,
    /// Incremental computation database
    incremental_db: Arc<IncrementalDatabase>,
}

impl LaravelLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            root_path: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(None)),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            env_cache: Arc::new(RwLock::new(None)),
            service_provider_registry: Arc::new(RwLock::new(None)),
            pending_diagnostics: Arc::new(RwLock::new(HashMap::new())),
            pending_cache_updates: Arc::new(RwLock::new(HashMap::new())),
            debounce_delay_ms: 200,  // 200ms for diagnostics
            cache_debounce_ms: 50,   // 50ms for cache updates
            performance_cache: database::create_performance_cache(),
            incremental_db: Arc::new(IncrementalDatabase::new()),
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
                for (doc_uri, doc_text) in documents.iter() {
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
                if let Some(buffer_content) = documents.get(&env_uri) {
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
        for (doc_uri, doc_text) in documents.iter() {
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

    /// Pre-parse a Blade file and cache the results for instant goto-definition
    /// Blade files are parsed with BOTH parsers:
    /// - Blade parser: for Blade-specific syntax (components, directives)
    /// - PHP parser: for PHP expressions (config, env, view, etc.)
    async fn preparse_blade_file(&self, _uri: &Url, text: &str, _version: i32) {
        // Parse Blade-specific patterns
        let (_components_static, _livewire_static, _directives_static) = if let Ok(tree) = parse_blade(text) {
            let lang = language_blade();
            
            if let (Ok(components), Ok(livewire), Ok(directives)) = (
                find_blade_components(&tree, text, &lang),
                find_livewire_components(&tree, text, &lang),
                find_directives(&tree, text, &lang)
            ) {
                // Convert to 'static lifetime by cloning strings
                // Also pre-resolve component paths for performance
                let config_guard = self.config.read().await;
                let components_static: Vec<ComponentMatch<'static>> = components.iter().map(|m| {
                    // Pre-resolve the component path during parsing
                    let resolved_path = if let Some(config) = config_guard.as_ref() {
                        let possible_paths = config.resolve_component_path(m.component_name);
                        // Find first existing path
                        possible_paths.into_iter().find(|p| p.exists())
                    } else {
                        None
                    };
                    
                    ComponentMatch {
                        component_name: m.component_name.to_string().leak(),
                        tag_name: m.tag_name.to_string().leak(),
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                        resolved_path,
                    }
                }).collect();
                drop(config_guard);
                
                let livewire_static: Vec<LivewireMatch<'static>> = livewire.iter().map(|m| LivewireMatch {
                    component_name: m.component_name.to_string().leak(),
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                }).collect();
                
                let directives_static: Vec<DirectiveMatch<'static>> = directives.iter().map(|m| DirectiveMatch {
                    directive_name: m.directive_name.to_string().leak(),
                    full_text: m.full_text.clone(),
                    arguments: m.arguments.map(|s| s.to_string().leak() as &str),
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                    string_column: m.string_column,
                    string_end_column: m.string_end_column,
                }).collect();
                
                (components_static, livewire_static, directives_static)
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            }
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
                


        // Removed: Old preparse functionality - performance_cache handles this now
    }

    // Removed: preparse_blade_file - functionality moved to performance_cache

    /// Convert a Laravel view name to possible file paths using config
    ///
    /// Returns the first existing path, or the first configured path if none exist
    async fn resolve_view_path(&self, view_name: &str) -> Option<PathBuf> {
        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;

        let possible_paths = config.resolve_view_path(view_name);

        // Return first existing path
        for path in &possible_paths {
            if path.exists() {
                return Some(path.clone());
            }
        }

        // Return first possibility even if it doesn't exist (for diagnostics)
        possible_paths.first().cloned()
    }

    /// Find view references at a specific position in the document
    async fn find_view_at_position(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<String> {
        let documents = self.documents.read().await;
        let content = documents.get(uri)?;
        
        // Convert position to byte offset
        let lines: Vec<&str> = content.lines().collect();
        if position.line >= lines.len() as u32 {
            return None;
        }
        
        let line = lines[position.line as usize];
        let char_pos = position.character as usize;
        
        // Look for view() or View::make() calls on this line
        if let Some(view_name) = self.extract_view_from_line(line, char_pos) {
            return Some(view_name);
        }
        
        None
    }

    /// Extract view name from a line at a specific character position
    fn extract_view_from_line(&self, line: &str, char_pos: usize) -> Option<String> {
        // Check for view() calls
        if let Some(start) = line.find("view(") {
            let after_view = &line[start + 5..];
            if let Some(quote_start) = after_view.find(|c| c == '\'' || c == '"') {
                let quote_char = after_view.chars().nth(quote_start)?;
                let content_start = start + 5 + quote_start + 1;
                let after_quote = &line[content_start..];
                
                if let Some(quote_end) = after_quote.find(quote_char) {
                    let content_end = content_start + quote_end;
                    
                    // Check if cursor is within the view name
                    if char_pos >= content_start && char_pos <= content_end {
                        return Some(after_quote[..quote_end].to_string());
                    }
                }
            }
        }
        
        // Check for View::make() calls
        if let Some(start) = line.find("View::make(") {
            let after_view = &line[start + 11..];
            if let Some(quote_start) = after_view.find(|c| c == '\'' || c == '"') {
                let quote_char = after_view.chars().nth(quote_start)?;
                let content_start = start + 11 + quote_start + 1;
                let after_quote = &line[content_start..];
                
                if let Some(quote_end) = after_quote.find(quote_char) {
                    let content_end = content_start + quote_end;
                    
                    // Check if cursor is within the view name
                    if char_pos >= content_start && char_pos <= content_end {
                        return Some(after_quote[..quote_end].to_string());
                    }
                }
            }
        }
        
        None
    }

    // ========================================================================
    // Tree-sitter-based helper functions
    // ========================================================================



    /// Create an LSP LocationLink for a view file using config
    ///
    /// The origin_selection_range tells the editor what text to highlight when hovering
    async fn create_view_location(&self, view_match: &ViewMatch<'_>) -> Option<GotoDefinitionResponse> {
        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;

        let possible_paths = config.resolve_view_path(view_match.view_name);

        // Find first existing path (in buffer or on disk)
        for path in possible_paths {
            if self.file_exists(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
                    // Calculate origin selection range (highlight just the view name, not quotes)
                    // The match positions already point to the content without quotes
                    let origin_selection_range = Range {
                        start: Position {
                            line: view_match.row as u32,
                            character: view_match.column as u32,
                        },
                        end: Position {
                            line: view_match.row as u32,
                            character: view_match.end_column as u32,
                        },
                    };

                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: Range {
                            start: Position { line: 0, character: 0 },
                            end: Position { line: 0, character: 0 },
                        },
                        target_selection_range: Range {
                            start: Position { line: 0, character: 0 },
                            end: Position { line: 0, character: 0 },
                        },
                    }]));
                }
            }
        }

        debug!("View file does not exist: {}", view_match.view_name);
        None
    }

    /// Create an LSP LocationLink for a Blade component using pre-resolved path
    ///
    /// The origin_selection_range tells the editor what text to highlight when hovering
    async fn create_component_location(&self, component_match: &ComponentMatch<'_>) -> Option<GotoDefinitionResponse> {
        // Use pre-resolved path from cache for instant navigation (no file I/O!)
        let path = component_match.resolved_path.as_ref()?;
        
        let target_uri = Url::from_file_path(path).ok()?;
        
        // Calculate origin selection range for the component tag
        // This highlights the entire tag name (e.g., "x-button")
        let origin_selection_range = Range {
            start: Position {
                line: component_match.row as u32,
                character: component_match.column as u32,
            },
            end: Position {
                line: component_match.row as u32,
                character: component_match.end_column as u32,
            },
        };

        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 0 },
            },
            target_selection_range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 0 },
            },
        }]))
    }

    /// Create an LSP LocationLink for a Livewire component using config
    ///
    /// The origin_selection_range tells the editor what text to highlight when hovering
    async fn create_livewire_location(&self, livewire_match: &LivewireMatch<'_>) -> Option<GotoDefinitionResponse> {
        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;

        let path = config.resolve_livewire_path(livewire_match.component_name)?;

        if self.file_exists(&path).await {
            if let Ok(target_uri) = Url::from_file_path(&path) {
                // Calculate origin selection range for the Livewire component
                // For <livewire:user-profile>, this highlights "user-profile"
                // For @livewire('user-profile'), this highlights 'user-profile' (with quotes)
                let origin_selection_range = Range {
                    start: Position {
                        line: livewire_match.row as u32,
                        character: livewire_match.column as u32,
                    },
                    end: Position {
                        line: livewire_match.row as u32,
                        character: livewire_match.end_column as u32,
                    },
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    },
                    target_selection_range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    },
                }]));
            }
        }

        debug!("Livewire component file does not exist: {:?}", path);
        None
    }

    /// For @extends and @include, navigate to the referenced view
    /// The highlighting will be on the view string only, not the entire directive
    async fn create_directive_location(&self, directive: &DirectiveMatch<'_>) -> Option<GotoDefinitionResponse> {
        // For @extends and @include, we can extract the view name from arguments
        if (directive.directive_name == "extends" || directive.directive_name == "include")
            && directive.arguments.is_some()
        {
            let arguments = directive.arguments.unwrap();

            // Extract view name from arguments like "('layouts.app')"
            if let Some(view_name) = Self::extract_view_from_directive_args(arguments) {
                // Resolve the view path
                let config_guard = self.config.read().await;
                let config = config_guard.as_ref()?;
                let possible_paths = config.resolve_view_path(&view_name);

                // Find first existing path (in buffer or on disk)
                for path in possible_paths {
                    if self.file_exists(&path).await {
                        if let Ok(target_uri) = Url::from_file_path(&path) {
                            // Use the pre-calculated string column positions from DirectiveMatch
                            // These are already set to point to the quoted string, not the full directive
                            // Adjust by 1 on each side to exclude the quotes from highlighting
                            let origin_selection_range = Range {
                                start: Position {
                                    line: directive.row as u32,
                                    character: (directive.string_column + 1) as u32,  // Skip opening quote
                                },
                                end: Position {
                                    line: directive.row as u32,
                                    character: (directive.string_end_column - 1) as u32,  // Skip closing quote
                                },
                            };

                            return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                                origin_selection_range: Some(origin_selection_range),
                                target_uri,
                                target_range: Range {
                                    start: Position { line: 0, character: 0 },
                                    end: Position { line: 0, character: 0 },
                                },
                                target_selection_range: Range {
                                    start: Position { line: 0, character: 0 },
                                    end: Position { line: 0, character: 0 },
                                },
                            }]));
                        }
                    }
                }
            }
        }

        None
    }

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

    /// Create a go-to-definition location for an env() call
    /// Jumps to the .env file where the variable is defined
    async fn create_env_location(&self, env_match: &EnvMatch<'_>) -> Option<GotoDefinitionResponse> {
        let env_cache_guard = self.env_cache.read().await;
        let env_cache = env_cache_guard.as_ref()?;

        debug!("Laravel LSP: Looking up env variable '{}' in cache ({} variables total)", 
               env_match.var_name, env_cache.variables.len());

        // Look up the variable in the cache
        let env_var = match env_cache.get(env_match.var_name) {
            Some(var) => {
                info!("Laravel LSP: Found env variable '{}' in {:?}", 
                      var.name, var.file_path.file_name());
                var
            }
            None => {
                info!("Laravel LSP: Env variable '{}' not found in cache", env_match.var_name);
                return None;
            }
        };

        // Create URI for the .env file
        let target_uri = Url::from_file_path(&env_var.file_path).ok()?;

        // Origin selection range - highlight just the variable name inside quotes
        let origin_selection_range = Range {
            start: Position {
                line: env_match.row as u32,
                character: env_match.column as u32,
            },
            end: Position {
                line: env_match.row as u32,
                character: env_match.end_column as u32,
            },
        };

        // Target selection range - highlight the variable name in .env file
        let target_selection_range = Range {
            start: Position {
                line: env_var.line as u32,
                character: env_var.column as u32,
            },
            end: Position {
                line: env_var.line as u32,
                character: (env_var.column + env_var.name.len()) as u32,
            },
        };

        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: target_selection_range,
            target_selection_range,
        }]))
    }

    /// Create a go-to-definition location for a config() call
    /// Jumps to the config file where the key is defined
    async fn create_config_location(&self, config_match: &ConfigMatch<'_>) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Parse config key: "app.name" -> file: "app.php", key: "name"
        let parts: Vec<&str> = config_match.config_key.split('.').collect();
        if parts.is_empty() {
            debug!("Laravel LSP: Config key '{}' has no parts", config_match.config_key);
            return None;
        }

        let config_file = parts[0];
        let config_path = root.join("config").join(format!("{}.php", config_file));

        if !self.file_exists(&config_path).await {
            info!("Laravel LSP: Config file not found: {:?}", config_path);
            return None;
        }

        info!("Laravel LSP: Found config file: {:?}", config_path);

        // Create URI for the config file
        let target_uri = Url::from_file_path(&config_path).ok()?;

        // Origin selection range - highlight the config key inside quotes
        let origin_selection_range = Range {
            start: Position {
                line: config_match.row as u32,
                character: config_match.column as u32,
            },
            end: Position {
                line: config_match.row as u32,
                character: config_match.end_column as u32,
            },
        };

        // Try to find the exact line in the config file
        // For now, just jump to the top of the file
        // TODO: Parse the config file and find the exact key location
        let target_selection_range = Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: 0, character: 0 },
        };

        Some(GotoDefinitionResponse::Link(vec![LocationLink {
            origin_selection_range: Some(origin_selection_range),
            target_uri,
            target_range: target_selection_range,
            target_selection_range,
        }]))
    }

    /// Create a go-to-definition location for a middleware reference
    /// Jumps to the middleware class file based on the alias
    /// If the class file doesn't exist, jumps to where the alias is defined
    async fn create_middleware_location(&self, middleware_match: &MiddlewareMatch<'_>) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        // Get service provider registry
        let registry_guard = self.service_provider_registry.read().await;
        let registry = registry_guard.as_ref()?;

        // Look up the middleware alias (handles parameters like "throttle:60,1")
        let middleware_reg = registry.get_middleware(middleware_match.middleware_name)?;
        
        info!("Laravel LSP: Found middleware alias '{}' -> {}", 
              middleware_match.middleware_name, middleware_reg.class_name);

        // Try to find the middleware class file
        let middleware_path = if let Some(file_path) = &middleware_reg.file_path {
            Some(file_path.clone())
        } else {
            // Try to resolve the class name to a file path
            let class_name = &middleware_reg.class_name;
            let path_str = class_name.replace("\\", "/");
            
            // Try App namespace first
            if path_str.starts_with("App/") {
                let relative = path_str.strip_prefix("App/").unwrap();
                let file_path = root.join("app").join(relative).with_extension("php");
                
                if self.file_exists(&file_path).await {
                    Some(file_path)
                } else {
                    info!("Laravel LSP: Middleware class file not found: {:?}", file_path);
                    None
                }
            } else if path_str.starts_with("Illuminate/") {
                // Framework middleware - try to navigate to vendor
                let relative = path_str.strip_prefix("Illuminate/").unwrap();
                let vendor_path = root.join("vendor/laravel/framework/src/Illuminate").join(relative).with_extension("php");
                
                if self.file_exists(&vendor_path).await {
                    Some(vendor_path)
                } else {
                    info!("Laravel LSP: Framework middleware file not found: {:?}", vendor_path);
                    None
                }
            } else {
                info!("Laravel LSP: Unable to resolve middleware path for: {}", class_name);
                None
            }
        };

        // If middleware class file exists, navigate to it
        if let Some(path) = middleware_path {
            if self.file_exists(&path).await {
                info!("Laravel LSP: Found middleware file: {:?}", path);

                // Create URI for the middleware file
                let target_uri = Url::from_file_path(&path).ok()?;

                // Origin selection range - highlight the middleware name inside quotes
                let origin_selection_range = Range {
                    start: Position {
                        line: middleware_match.row as u32,
                        character: middleware_match.column as u32,
                    },
                    end: Position {
                        line: middleware_match.row as u32,
                        character: middleware_match.end_column as u32,
                    },
                };

                // Jump to the top of the middleware class file
                // TODO: Parse the file and find the exact class declaration
                let target_selection_range = Range {
                    start: Position { line: 0, character: 0 },
                    end: Position { line: 0, character: 0 },
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: target_selection_range,
                    target_selection_range,
                }]));
            }
        }

        // Middleware class file doesn't exist - navigate to where the alias is defined instead
        if let (Some(source_file), Some(source_line)) = (&middleware_reg.source_file, middleware_reg.source_line) {
            if self.file_exists(source_file).await {
                info!("Laravel LSP: Middleware class not found, navigating to alias definition at {:?}:{}",
                      source_file, source_line);

                let target_uri = Url::from_file_path(source_file).ok()?;

                // Origin selection range - highlight the middleware name inside quotes
                let origin_selection_range = Range {
                    start: Position {
                        line: middleware_match.row as u32,
                        character: middleware_match.column as u32,
                    },
                    end: Position {
                        line: middleware_match.row as u32,
                        character: middleware_match.end_column as u32,
                    },
                };

                // Jump to the line where the alias is defined
                let target_selection_range = Range {
                    start: Position { 
                        line: source_line as u32, 
                        character: 0 
                    },
                    end: Position { 
                        line: source_line as u32, 
                        character: 0 
                    },
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: target_selection_range,
                    target_selection_range,
                }]));
            }
        }

        info!("Laravel LSP: Could not navigate to middleware class or alias definition");
        None
    }

    /// Create a go-to-definition location for a translation reference
    /// Jumps to the translation file (PHP or JSON) where the key is defined
    async fn create_translation_location(&self, translation_match: &TranslationMatch<'_>) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;

        let translation_key = translation_match.translation_key;
        
        info!("Laravel LSP: Looking for translation key: {}", translation_key);

        // Check if this looks like a JSON translation (no dots, or contains spaces)
        let looks_like_json = !translation_key.contains('.') || translation_key.contains(' ');

        // Try JSON first if it looks like a JSON translation
        if looks_like_json {
            info!("Laravel LSP: Checking JSON translation files for: {}", translation_key);
            
            // Try both possible locations for JSON files
            let json_paths = [
                root.join("lang/en.json"),
                root.join("resources/lang/en.json"),
            ];

            // First, try to find an existing JSON file
            for json_path in &json_paths {
                if self.file_exists(json_path).await {
                    info!("Laravel LSP: Found JSON translation file: {:?}", json_path);
                    
                    let target_uri = Url::from_file_path(json_path).ok()?;
                    
                    let origin_selection_range = Range {
                        start: Position {
                            line: translation_match.row as u32,
                            character: translation_match.column as u32,
                        },
                        end: Position {
                            line: translation_match.row as u32,
                            character: translation_match.end_column as u32,
                        },
                    };

                    let target_selection_range = Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    };

                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: target_selection_range,
                        target_selection_range,
                    }]));
                }
            }
            
            info!("Laravel LSP: JSON translation file not found");
            
            // If multi-word (contains spaces), navigate to where JSON file SHOULD be
            // This allows users to create the file if it doesn't exist
            if translation_key.contains(' ') {
                info!("Laravel LSP: Multi-word translation, pointing to expected JSON location");
                
                // Prefer Laravel 9+ location (lang/en.json)
                let preferred_json_path = root.join("lang/en.json");
                
                if let Ok(target_uri) = Url::from_file_path(&preferred_json_path) {
                    let origin_selection_range = Range {
                        start: Position {
                            line: translation_match.row as u32,
                            character: translation_match.column as u32,
                        },
                        end: Position {
                            line: translation_match.row as u32,
                            character: translation_match.end_column as u32,
                        },
                    };

                    let target_selection_range = Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    };

                    return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                        origin_selection_range: Some(origin_selection_range),
                        target_uri,
                        target_range: target_selection_range,
                        target_selection_range,
                    }]));
                }
                
                return None;
            }
            
            // If JSON file doesn't exist and this is a single word (no dots, no spaces),
            // fall back to checking common PHP translation files
            if !translation_key.contains('.') {
                info!("Laravel LSP: Falling back to common PHP files for single-word key: {}", translation_key);
                
                // Try common translation file names
                let common_files = ["messages", "common", "app"];
                
                for file_name in &common_files {
                    let php_paths = [
                        root.join("lang/en").join(format!("{}.php", file_name)),
                        root.join("resources/lang/en").join(format!("{}.php", file_name)),
                    ];
                    
                    for php_path in &php_paths {
                        if self.file_exists(php_path).await {
                            info!("Laravel LSP: Found fallback PHP translation file: {:?}", php_path);
                            
                            let target_uri = Url::from_file_path(php_path).ok()?;
                            
                            let origin_selection_range = Range {
                                start: Position {
                                    line: translation_match.row as u32,
                                    character: translation_match.column as u32,
                                },
                                end: Position {
                                    line: translation_match.row as u32,
                                    character: translation_match.end_column as u32,
                                },
                            };

                            let target_selection_range = Range {
                                start: Position { line: 0, character: 0 },
                                end: Position { line: 0, character: 0 },
                            };

                            return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                                origin_selection_range: Some(origin_selection_range),
                                target_uri,
                                target_range: target_selection_range,
                                target_selection_range,
                            }]));
                        }
                    }
                }
            }
            
            // Fall through to PHP file handling for single-word keys
        }

        // PHP translation - parse the key: "messages.welcome" -> file: "messages.php", key: "welcome"
        let parts: Vec<&str> = translation_key.split('.').collect();
        if parts.is_empty() {
            debug!("Laravel LSP: Translation key '{}' has no parts", translation_key);
            return None;
        }

        let translation_file = parts[0];
        
        // Try both possible locations for PHP translation files
        let php_paths = [
            root.join("lang/en").join(format!("{}.php", translation_file)),
            root.join("resources/lang/en").join(format!("{}.php", translation_file)),
        ];

        for translation_path in &php_paths {
            if self.file_exists(translation_path).await {
                info!("Laravel LSP: Found translation file: {:?}", translation_path);

                let target_uri = Url::from_file_path(translation_path).ok()?;

                let origin_selection_range = Range {
                    start: Position {
                        line: translation_match.row as u32,
                        character: translation_match.column as u32,
                    },
                    end: Position {
                        line: translation_match.row as u32,
                        character: translation_match.end_column as u32,
                    },
                };

                // Jump to the top of the translation file
                // TODO: Parse the PHP file and find the exact nested key location
                let target_selection_range = Range {
                    start: Position { line: 0, character: 0 },
                    end: Position { line: 0, character: 0 },
                };

                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: target_selection_range,
                    target_selection_range,
                }]));
            }
        }

        info!("Laravel LSP: Translation file not found for key: {}", translation_key);
        None
    }

    /// Create a go-to-definition location for a container binding call
    /// Handles both app('string') and app(SomeClass::class) patterns
    async fn create_binding_location(&self, binding_match: &BindingMatch<'_>) -> Option<GotoDefinitionResponse> {
        let binding_name = binding_match.binding_name;
        
        info!("Laravel LSP: Looking for container binding: {} (is_class: {})", 
              binding_name, binding_match.is_class_reference);

        // If it's a class reference (Class::class), resolve directly to the class file
        if binding_match.is_class_reference {
            return self.resolve_class_binding(binding_match).await;
        }

        // For string bindings, check the service provider registry
        let registry_guard = self.service_provider_registry.read().await;
        if let Some(registry) = registry_guard.as_ref() {
            if let Some(binding_reg) = registry.get_binding(binding_name) {
                info!("Laravel LSP: Found binding registration for '{}'", binding_name);
                
                // Always navigate to where the binding is registered (not the concrete class)
                if let (Some(source_file), Some(source_line)) = (&binding_reg.source_file, binding_reg.source_line) {
                    if let Ok(target_uri) = Url::from_file_path(source_file) {
                        let origin_selection_range = Range {
                            start: Position {
                                line: binding_match.row as u32,
                                character: binding_match.column as u32,
                            },
                            end: Position {
                                line: binding_match.row as u32,
                                character: binding_match.end_column as u32,
                            },
                        };

                        let target_selection_range = Range {
                            start: Position { line: source_line as u32, character: 0 },
                            end: Position { line: source_line as u32, character: 0 },
                        };

                        info!("Laravel LSP: Navigating to binding registration at {:?}:{}", source_file, source_line);
                        return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                            origin_selection_range: Some(origin_selection_range),
                            target_uri,
                            target_range: target_selection_range,
                            target_selection_range,
                        }]));
                    }
                } else {
                    // Binding exists but has no source file (framework binding)
                    // Return empty array to prevent Zed's fallback navigation
                    info!("Laravel LSP: Binding '{}' exists (framework) but has no source file, returning empty", binding_name);
                    return Some(GotoDefinitionResponse::Array(vec![]));
                }
            }
        }

        // No binding found - return empty array to prevent Zed's fallback navigation
        info!("Laravel LSP: No binding found for '{}', returning empty to prevent fallback", binding_name);
        Some(GotoDefinitionResponse::Array(vec![]))
    }

    /// Resolve a class reference binding (Class::class) to its file
    async fn resolve_class_binding(&self, binding_match: &BindingMatch<'_>) -> Option<GotoDefinitionResponse> {
        let class_name = binding_match.binding_name;
        
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;
        
        // Use the existing resolve_class_to_file function
        if let Some(file_path) = resolve_class_to_file(class_name, root) {
            if let Ok(target_uri) = Url::from_file_path(&file_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: binding_match.row as u32,
                        character: binding_match.column as u32,
                    },
                    end: Position {
                        line: binding_match.row as u32,
                        character: binding_match.end_column as u32,
                    },
                };

                let target_selection_range = Range {
                    start: Position { line: 0, character: 0 },
                    end: Position { line: 0, character: 0 },
                };

                info!("Laravel LSP: Resolved class reference '{}' to {:?}", class_name, file_path);
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: target_selection_range,
                    target_selection_range,
                }]));
            }
        }

        // Class file not found - return empty array to prevent Zed's fallback
        info!("Laravel LSP: Class file not found for '{}', returning empty to prevent fallback", class_name);
        Some(GotoDefinitionResponse::Array(vec![]))
    }

    /// Create a go-to-definition location for an asset path
    /// Resolves asset helpers like asset(), Vite::asset(), base_path(), etc. to actual files
    async fn create_asset_location(&self, asset_match: &AssetMatch<'_>) -> Option<GotoDefinitionResponse> {
        let root_guard = self.root_path.read().await;
        let root = root_guard.as_ref()?;
        
        // Resolve the asset path based on helper type
        let resolved_path = match asset_match.helper_type {
            AssetHelperType::Asset => root.join("public").join(asset_match.path),
            AssetHelperType::PublicPath => root.join("public").join(asset_match.path),
            AssetHelperType::BasePath => root.join(asset_match.path),
            AssetHelperType::AppPath => root.join("app").join(asset_match.path),
            AssetHelperType::StoragePath => root.join("storage").join(asset_match.path),
            AssetHelperType::DatabasePath => root.join("database").join(asset_match.path),
            AssetHelperType::LangPath => {
                // Try lang/ first (Laravel 9+), then resources/lang/ (older)
                let lang_path = root.join("lang").join(asset_match.path);
                if lang_path.exists() {
                    lang_path
                } else {
                    root.join("resources/lang").join(asset_match.path)
                }
            },
            AssetHelperType::ConfigPath => root.join("config").join(asset_match.path),
            AssetHelperType::ResourcePath => root.join("resources").join(asset_match.path),
            AssetHelperType::Mix => root.join("public").join(asset_match.path),
            AssetHelperType::ViteAsset => root.join(asset_match.path),
        };
        
        info!("Laravel LSP: Resolving asset '{}' ({:?}) to {:?}", 
            asset_match.path, asset_match.helper_type, resolved_path);
        
        // Check if file exists
        if self.file_exists(&resolved_path).await {
            if let Ok(target_uri) = Url::from_file_path(&resolved_path) {
                let origin_selection_range = Range {
                    start: Position {
                        line: asset_match.row as u32,
                        character: asset_match.column as u32,
                    },
                    end: Position {
                        line: asset_match.row as u32,
                        character: asset_match.end_column as u32,
                    },
                };
                
                return Some(GotoDefinitionResponse::Link(vec![LocationLink {
                    origin_selection_range: Some(origin_selection_range),
                    target_uri,
                    target_range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    },
                    target_selection_range: Range {
                        start: Position { line: 0, character: 0 },
                        end: Position { line: 0, character: 0 },
                    },
                }]));
            }
        }
        
        // File not found
        info!("Laravel LSP: Asset file not found: {:?}", resolved_path);
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
            config: self.config.clone(),
            diagnostics: self.diagnostics.clone(),
            env_cache: self.env_cache.clone(),
            service_provider_registry: self.service_provider_registry.clone(),
            pending_diagnostics: self.pending_diagnostics.clone(),
            pending_cache_updates: self.pending_cache_updates.clone(),
            debounce_delay_ms: self.debounce_delay_ms,
            cache_debounce_ms: self.cache_debounce_ms,
            performance_cache: self.performance_cache.clone(),
            incremental_db: self.incremental_db.clone(),
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
                
                // Initialize incremental database
                info!("========================================");
                info!("🚀 Initializing incremental computation database");
                info!("========================================");
                self.initialize_incremental_database(&path).await;
                
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
        info!("Laravel LSP: Shutting down");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        debug!("Laravel LSP: Document opened: {}", uri);
        self.documents.write().await.insert(uri.clone(), text.clone());

        // Try to discover Laravel config from this file if we don't have one yet
        if let Ok(file_path) = uri.to_file_path() {
            self.try_discover_from_file(&file_path).await;
        }

        // Update LRU cache with new file content
        self.performance_cache.update_patterns(uri.clone(), text.clone(), version).await;

        // Validate and publish diagnostics for Blade files
        self.validate_and_publish_diagnostics(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            debug!("Laravel LSP: Document changed: {} (version: {})", uri, version);
            self.documents.write().await.insert(uri.clone(), change.text.clone());
            
            // 🚀 Update LRU cache - automatic invalidation happens
            self.performance_cache.update_patterns(uri.clone(), change.text.clone(), version).await;
            
            // Update incremental database with new file content
            if let Ok(file_path) = uri.to_file_path() {
                self.incremental_db.update_file(file_path.clone(), change.text.clone(), 1).await;
                
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
        if let Some(text) = self.documents.read().await.get(&uri).cloned() {
            info!("   ✅ Found document in cache, updating cache and running diagnostics...");
            
            // Update cache immediately (now that user saved)
            let _version = 0; // Version doesn't matter for save operations
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
        
        // File cleanup handled by performance cache automatically
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        // Get document content
        let source = {
            let documents = self.documents.read().await;
            let Some(source) = documents.get(&uri) else {
                return Ok(None);
            };
            source.clone()
        };

        // 🚀 Use LRU cache to get patterns and find goto definition
        let patterns = self.performance_cache.get_patterns(&uri, 1).await.unwrap_or_default();
        
        // Find pattern at cursor position
        for (pattern_type, pattern_list) in patterns.iter() {
            for pattern_info in pattern_list {
                let line = position.line as usize;
                let character = position.character as usize;
                
                if pattern_info.row == line &&
                   character >= pattern_info.col &&
                   character <= pattern_info.col + pattern_info.text.len() {
                    
                    debug!("Goto definition: Found {} pattern at cursor", pattern_type);
                    
                    // For now, return basic location info
                    // TODO: Implement proper goto-definition for each pattern type
                    return Ok(None);
                }
            }
        }

        // Final emergency fallback for legacy tree-sitter patterns
        let is_php = uri.path().ends_with(".php") && !uri.path().ends_with(".blade.php");
        if is_php {
            debug!("Goto definition: Emergency fallback for PHP file");
            if let Ok(tree) = parse_php(&source) {
                let lang = language_php();
                if let Ok(assets) = find_asset_calls(&tree, &source, &lang) {
                    // Find asset match at cursor position
                    if let Some(asset_match) = assets.iter().find(|m| {
                        m.row == position.line as usize
                            && position.character as usize >= m.column
                            && position.character as usize <= m.end_column
                    }) {
                        info!("Laravel LSP: Found asset call (fallback): {} ({:?})", asset_match.path, asset_match.helper_type);
                        return Ok(self.create_asset_location(&asset_match).await);
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



    async fn completion(&self, params: CompletionParams) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        
        debug!("Laravel LSP: Completion requested at {:?}:{:?}", uri, position);
        
        // For now, return empty completions
        // We'll implement this in a later phase
        Ok(Some(CompletionResponse::Array(vec![])))
    }

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

    /// Get cache diagnostics for debugging
    pub async fn get_cache_diagnostics(&self) -> String {
        self.performance_cache.get_performance_report().await
    }

    // Removed: invalidate_file_cache - performance_cache handles invalidation automatically
    // Removed: rebuild_view_references_map - old cache system removed

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

    /// Initialize incremental database with project configuration
    async fn initialize_incremental_database(&self, root_path: &PathBuf) {
        self.incremental_db.set_project_root(root_path.clone()).await;
        
        info!("Laravel LSP: Incremental database initialized for project: {}", root_path.display());
    }

    /// Get hover information using incremental computation
    async fn get_incremental_hover_info(&self, uri: &Url, line: u32, character: u32) -> Option<Hover> {
        // Get current document content
        let content = {
            let documents = self.documents.read().await;
            documents.get(uri)?.clone()
        };

        // Convert to file path
        let file_path = uri.to_file_path().ok()?;
        
        // Check if this is a Blade file
        if uri.path().contains(".blade.php") {
            // Get hover info from incremental database
            if let Some(hover_text) = self.incremental_db.get_blade_hover(
                file_path, content, 1, line, character
            ).await {
                return Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(hover_text)),
                    range: None,
                });
            }
        } else {
            // For regular PHP files, fall back to the old cached system
            let version = 1;
            return self.performance_cache.get_hover(uri.clone(), 
                lsp_types::Position::new(line, character), version).await;
        }

        None
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