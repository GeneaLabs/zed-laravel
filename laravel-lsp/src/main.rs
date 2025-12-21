use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tracing::{debug, info};

// Our tree-sitter modules
mod parser;
mod queries;
mod config;
mod env_parser;

use parser::{language_blade, language_php, parse_blade, parse_php};
use queries::{
    find_blade_components, find_config_calls, find_directives, find_env_calls,
    find_livewire_components, find_view_calls, ComponentMatch, ConfigMatch, DirectiveMatch,
    EnvMatch, LivewireMatch, ViewMatch,
};
use config::LaravelConfig;
use env_parser::EnvFileCache;

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
    /// Reference from a Blade component
    BladeComponent,
    /// Reference from a Livewire component
    LivewireComponent,
    /// Reference from a route definition
    Route,
    /// Reference from another Blade template (@extends, @include)
    BladeTemplate,
}

/// Cached references for a single file
#[derive(Debug, Clone)]
struct FileReferences {
    /// When this file was last parsed for references
    last_parsed: SystemTime,
    /// The document version when this was last parsed
    document_version: Option<i32>,
    /// All view references found in this file
    view_references: Vec<(String, ReferenceLocation)>,
    /// All component references found in this file
    component_references: Vec<ReferenceLocation>,
    /// All Livewire references found in this file
    livewire_references: Vec<ReferenceLocation>,
}

/// Cached parsed matches for goto-definition (env, config, view calls)
#[derive(Debug, Clone)]
struct ParsedMatches {
    /// Document version when this was parsed
    version: Option<i32>,
    /// Cached env() matches
    env_matches: Vec<EnvMatch<'static>>,
    /// Cached config() matches
    config_matches: Vec<ConfigMatch<'static>>,
    /// Cached view() matches
    view_matches: Vec<ViewMatch<'static>>,
    /// Cached Blade component matches
    component_matches: Vec<ComponentMatch<'static>>,
    /// Cached Livewire component matches
    livewire_matches: Vec<LivewireMatch<'static>>,
    /// Cached directive matches
    directive_matches: Vec<DirectiveMatch<'static>>,
}

/// The reference cache with intelligent invalidation
#[derive(Debug, Default)]
struct ReferenceCache {
    /// Per-file parsed references (invalidated on file change)
    file_references: HashMap<Url, FileReferences>,
    
    /// Global view name -> reference locations mapping
    view_references: HashMap<String, Vec<ReferenceLocation>>,
    
    /// Cached component file paths
    component_files: Option<(SystemTime, Vec<PathBuf>)>,
    
    /// Cached Livewire file paths
    livewire_files: Option<(SystemTime, Vec<PathBuf>)>,
    
    /// Track document versions for change detection
    document_versions: HashMap<Url, i32>,
    
    /// Cached parsed matches for goto-definition (per file)
    parsed_matches: HashMap<Url, ParsedMatches>,
}

/// The main Laravel Language Server struct
/// This holds all the state for our LSP
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
    /// Reference cache with intelligent invalidation
    reference_cache: Arc<RwLock<ReferenceCache>>,
    /// Environment variable cache (.env, .env.example, .env.local)
    env_cache: Arc<RwLock<Option<EnvFileCache>>>,
}

impl LaravelLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            root_path: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(None)),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            reference_cache: Arc::new(RwLock::new(ReferenceCache::default())),
            env_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if a position has a diagnostic (yellow squiggle)
    /// Returns true if there's a diagnostic at this position
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

    /// Pre-parse a PHP file and cache the results for instant goto-definition
    async fn preparse_php_file(&self, uri: &Url, text: &str, version: i32) {
        if let Ok(tree) = parse_php(text) {
            let lang = language_php();
            
            if let (Ok(env), Ok(config), Ok(view)) = (
                find_env_calls(&tree, text, &lang),
                find_config_calls(&tree, text, &lang),
                find_view_calls(&tree, text, &lang)
            ) {
                // Convert to 'static lifetime by cloning strings
                let env_static: Vec<EnvMatch<'static>> = env.iter().map(|m| EnvMatch {
                    var_name: m.var_name.to_string().leak(),
                    has_fallback: m.has_fallback,
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                }).collect();
                
                let config_static: Vec<ConfigMatch<'static>> = config.iter().map(|m| ConfigMatch {
                    config_key: m.config_key.to_string().leak(),
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                }).collect();
                
                let view_static: Vec<ViewMatch<'static>> = view.iter().map(|m| ViewMatch {
                    view_name: m.view_name.to_string().leak(),
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                }).collect();
                
                // Store/update cache
                let mut cache_guard = self.reference_cache.write().await;
                cache_guard.parsed_matches.insert(uri.clone(), ParsedMatches {
                    version: Some(version),
                    env_matches: env_static,
                    config_matches: config_static,
                    view_matches: view_static,
                    component_matches: Vec::new(),
                    livewire_matches: Vec::new(),
                    directive_matches: Vec::new(),
                });
                cache_guard.document_versions.insert(uri.clone(), version);
            }
        }
    }

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
    async fn preparse_blade_file(&self, uri: &Url, text: &str, version: i32) {
        if let Ok(tree) = parse_blade(text) {
            let lang = language_blade();
            
            if let (Ok(components), Ok(livewire), Ok(directives)) = (
                find_blade_components(&tree, text, &lang),
                find_livewire_components(&tree, text, &lang),
                find_directives(&tree, text, &lang)
            ) {
                // Convert to 'static lifetime by cloning strings
                let components_static: Vec<ComponentMatch<'static>> = components.iter().map(|m| ComponentMatch {
                    component_name: m.component_name.to_string().leak(),
                    tag_name: m.tag_name.to_string().leak(),
                    byte_start: m.byte_start,
                    byte_end: m.byte_end,
                    row: m.row,
                    column: m.column,
                    end_column: m.end_column,
                }).collect();
                
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
                
                // Store/update cache
                let mut cache_guard = self.reference_cache.write().await;
                cache_guard.parsed_matches.insert(uri.clone(), ParsedMatches {
                    version: Some(version),
                    env_matches: Vec::new(),
                    config_matches: Vec::new(),
                    view_matches: Vec::new(),
                    component_matches: components_static,
                    livewire_matches: livewire_static,
                    directive_matches: directives_static,
                });
                cache_guard.document_versions.insert(uri.clone(), version);
            }
        }
    }

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

    /// Find a match at a specific cursor position
    /// Generic over any match type that has byte_start, byte_end, row, column
    fn find_match_at_position<'a, T>(
        matches: &'a [T],
        position: Position,
    ) -> Option<&'a T>
    where
        T: HasPosition,
    {
        matches.iter().find(|m| {
            m.row() == position.line as usize
                && position.character as usize >= m.column()
                && position.character as usize <= m.end_column()
        })
    }

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
                    // Calculate origin selection range (include quotes around the string)
                    // The match gives us the string content position, expand by 1 to include quotes
                    let origin_selection_range = Range {
                        start: Position {
                            line: view_match.row as u32,
                            character: view_match.column.saturating_sub(1) as u32,
                        },
                        end: Position {
                            line: view_match.row as u32,
                            character: (view_match.end_column + 1) as u32,
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

    /// Create an LSP LocationLink for a Blade component using config
    ///
    /// The origin_selection_range tells the editor what text to highlight when hovering
    async fn create_component_location(&self, component_match: &ComponentMatch<'_>) -> Option<GotoDefinitionResponse> {
        let config_guard = self.config.read().await;
        let config = config_guard.as_ref()?;

        let possible_paths = config.resolve_component_path(component_match.component_name);

        // Find first existing path (in buffer or on disk)
        for path in possible_paths {
            if self.file_exists(&path).await {
                if let Ok(target_uri) = Url::from_file_path(&path) {
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

        debug!("Component file does not exist: {}", component_match.component_name);
        None
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

    /// Validate a document (Blade or PHP) and publish diagnostics
    ///
    /// This function:
    /// 1. Parses the file for view references, directives, and components
    /// 2. Checks if referenced files exist
    /// 3. Creates yellow squiggle warnings for missing files
    /// 4. Publishes diagnostics to the editor
    async fn validate_and_publish_diagnostics(&self, uri: &Url, source: &str) {
        let mut diagnostics = Vec::new();

        // Get the Laravel config
        let config_guard = self.config.read().await;
        let Some(config) = config_guard.as_ref() else {
            debug!("Cannot validate: config not set");
            return;
        };

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
                                severity: Some(DiagnosticSeverity::WARNING),
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

        // Store diagnostics for hover filtering
        self.diagnostics.write().await.insert(uri.clone(), diagnostics.clone());

        // Publish diagnostics
        self.client.publish_diagnostics(uri.clone(), diagnostics, None).await;
    }
}

/// Trait for types that have position information
trait HasPosition {
    fn row(&self) -> usize;
    fn column(&self) -> usize;
    fn end_column(&self) -> usize;
    fn byte_start(&self) -> usize;
    fn byte_end(&self) -> usize;
}

impl<'a> HasPosition for ViewMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.column }
    fn end_column(&self) -> usize { self.end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
}

impl<'a> HasPosition for ComponentMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.column }
    fn end_column(&self) -> usize { self.end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
}

impl<'a> HasPosition for LivewireMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.column }
    fn end_column(&self) -> usize { self.end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
}

impl<'a> HasPosition for DirectiveMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.string_column }
    fn end_column(&self) -> usize { self.string_end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
}

impl<'a> HasPosition for EnvMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.column }
    fn end_column(&self) -> usize { self.end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
}

impl<'a> HasPosition for ConfigMatch<'a> {
    fn row(&self) -> usize { self.row }
    fn column(&self) -> usize { self.column }
    fn end_column(&self) -> usize { self.end_column }
    fn byte_start(&self) -> usize { self.byte_start }
    fn byte_end(&self) -> usize { self.byte_end }
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
                
                // We need to sync document content
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                
                // Future capabilities we'll add
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["'".to_string(), "\"".to_string()]),
                    ..Default::default()
                }),
                
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

        // Pre-parse files for instant goto-definition
        if uri.path().ends_with(".blade.php") {
            self.preparse_blade_file(&uri, &text, version).await;
        } else if uri.path().ends_with(".php") {
            self.preparse_php_file(&uri, &text, version).await;
        }

        // Validate and publish diagnostics for Blade files
        self.validate_and_publish_diagnostics(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            debug!("Laravel LSP: Document changed: {} (version: {})", uri, version);
            self.documents.write().await.insert(uri.clone(), change.text.clone());
            
            // Invalidate reference cache for this file
            self.invalidate_file_cache(&uri, version).await;
            
            // Check if this is an .env file and refresh env cache if needed
            if let Ok(file_path) = uri.to_file_path() {
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
            
            // Re-parse files to keep cache updated
            if uri.path().ends_with(".blade.php") {
                self.preparse_blade_file(&uri, &change.text, version).await;
            } else if uri.path().ends_with(".php") {
                self.preparse_php_file(&uri, &change.text, version).await;
            }
            
            // Re-validate the document
            self.validate_and_publish_diagnostics(&uri, &change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        debug!("Laravel LSP: Document closed: {}", uri);
        
        self.documents.write().await.remove(&uri);
        
        // Remove from reference cache
        self.reference_cache.write().await.file_references.remove(&uri);
        self.reference_cache.write().await.document_versions.remove(&uri);
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        // Get document content and version
        let (source, current_version) = {
            let documents = self.documents.read().await;
            let Some(source) = documents.get(&uri) else {
                return Ok(None);
            };
            let versions = self.reference_cache.read().await;
            let version = versions.document_versions.get(&uri).copied();
            (source.clone(), version)
        };

        // Determine file type from extension
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;

        // Try to find a pattern at the cursor position using tree-sitter
        if is_php {
            // Check cache first
            let cache_guard = self.reference_cache.read().await;
            let cached = cache_guard.parsed_matches.get(&uri);
            let use_cache = cached.map_or(false, |c| c.version == current_version);
            drop(cache_guard);

            let (env_matches, config_matches, view_matches) = if use_cache {
                // Use cached results
                let cache_guard = self.reference_cache.read().await;
                let cached = cache_guard.parsed_matches.get(&uri).unwrap();
                (cached.env_matches.clone(), cached.config_matches.clone(), cached.view_matches.clone())
            } else {
                // Parse and cache
                if let Ok(tree) = parse_php(&source) {
                    let lang = language_php();
                    
                    let env = find_env_calls(&tree, &source, &lang).unwrap_or_default();
                    let config = find_config_calls(&tree, &source, &lang).unwrap_or_default();
                    let view = find_view_calls(&tree, &source, &lang).unwrap_or_default();
                    
                    // Convert to 'static lifetime by cloning strings
                    let env_static: Vec<EnvMatch<'static>> = env.iter().map(|m| EnvMatch {
                        var_name: m.var_name.to_string().leak(),
                        has_fallback: m.has_fallback,
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                    }).collect();
                    
                    let config_static: Vec<ConfigMatch<'static>> = config.iter().map(|m| ConfigMatch {
                        config_key: m.config_key.to_string().leak(),
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                    }).collect();
                    
                    let view_static: Vec<ViewMatch<'static>> = view.iter().map(|m| ViewMatch {
                        view_name: m.view_name.to_string().leak(),
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                    }).collect();
                    
                    // Store in cache
                    let mut cache_guard = self.reference_cache.write().await;
                    cache_guard.parsed_matches.insert(uri.clone(), ParsedMatches {
                        version: current_version,
                        env_matches: env_static.clone(),
                        config_matches: config_static.clone(),
                        view_matches: view_static.clone(),
                        component_matches: Vec::new(),
                        livewire_matches: Vec::new(),
                        directive_matches: Vec::new(),
                    });
                    
                    (env_static, config_static, view_static)
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                }
            };
            
            // Try view() calls
            if let Some(view_match) = Self::find_match_at_position(&view_matches, position) {
                info!("Laravel LSP: Found view() call: {}", view_match.view_name);
                return Ok(self.create_view_location(&view_match).await);
            }
            
            // Try env() calls
            if let Some(env_match) = Self::find_match_at_position(&env_matches, position) {
                info!("Laravel LSP: Found env() call: {}", env_match.var_name);
                return Ok(self.create_env_location(&env_match).await);
            }
            
            // Try config() calls
            if let Some(config_match) = Self::find_match_at_position(&config_matches, position) {
                info!("Laravel LSP: Found config() call: {}", config_match.config_key);
                return Ok(self.create_config_location(&config_match).await);
            }
        } else if is_blade {
            // Check cache first
            let cache_guard = self.reference_cache.read().await;
            let cached = cache_guard.parsed_matches.get(&uri);
            let use_cache = cached.map_or(false, |c| c.version == current_version);
            drop(cache_guard);

            let (components, livewire, directives) = if use_cache {
                // Use cached results
                let cache_guard = self.reference_cache.read().await;
                let cached = cache_guard.parsed_matches.get(&uri).unwrap();
                (cached.component_matches.clone(), cached.livewire_matches.clone(), cached.directive_matches.clone())
            } else {
                // Parse and cache (this shouldn't happen often since we pre-parse on open/change)
                if let Ok(tree) = parse_blade(&source) {
                    let lang = language_blade();
                    
                    let comp = find_blade_components(&tree, &source, &lang).unwrap_or_default();
                    let lw = find_livewire_components(&tree, &source, &lang).unwrap_or_default();
                    let dir = find_directives(&tree, &source, &lang).unwrap_or_default();
                    
                    // Convert to 'static lifetime
                    let comp_static: Vec<ComponentMatch<'static>> = comp.iter().map(|m| ComponentMatch {
                        component_name: m.component_name.to_string().leak(),
                        tag_name: m.tag_name.to_string().leak(),
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                    }).collect();
                    
                    let lw_static: Vec<LivewireMatch<'static>> = lw.iter().map(|m| LivewireMatch {
                        component_name: m.component_name.to_string().leak(),
                        byte_start: m.byte_start,
                        byte_end: m.byte_end,
                        row: m.row,
                        column: m.column,
                        end_column: m.end_column,
                    }).collect();
                    
                    let dir_static: Vec<DirectiveMatch<'static>> = dir.iter().map(|m| DirectiveMatch {
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
                    
                    // Store in cache
                    let mut cache_guard = self.reference_cache.write().await;
                    cache_guard.parsed_matches.insert(uri.clone(), ParsedMatches {
                        version: current_version,
                        env_matches: Vec::new(),
                        config_matches: Vec::new(),
                        view_matches: Vec::new(),
                        component_matches: comp_static.clone(),
                        livewire_matches: lw_static.clone(),
                        directive_matches: dir_static.clone(),
                    });
                    
                    (comp_static, lw_static, dir_static)
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                }
            };

            // Try Blade components (<x-button>)
            if let Some(comp_match) = Self::find_match_at_position(&components, position) {
                info!("Laravel LSP: Found Blade component: {}", comp_match.component_name);
                return Ok(self.create_component_location(&comp_match).await);
            }

            // Try Livewire components (<livewire:user-profile>)
            if let Some(lw_match) = Self::find_match_at_position(&livewire, position) {
                info!("Laravel LSP: Found Livewire component: {}", lw_match.component_name);
                return Ok(self.create_livewire_location(&lw_match).await);
            }

            // Try directives (@extends, @section, etc.)
            if let Some(dir_match) = Self::find_match_at_position(&directives, position) {
                info!("Laravel LSP: Found directive: @{}", dir_match.directive_name);
                return Ok(self.create_directive_location(&dir_match).await);
            }
        }

        debug!("Laravel LSP: No definition found");
        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let start = std::time::Instant::now();
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        // Get document content (clone to avoid holding lock)
        let source = {
            let documents = self.documents.read().await;
            documents.get(&uri).cloned()
        };
        
        let Some(source) = source else {
            return Ok(None);
        };

        // Only process PHP files (including Blade files that end in .php)
        let is_php = uri.path().ends_with(".php");
        if !is_php {
            return Ok(None);
        }

        // Parse the file and look for env() or config() calls at cursor position
        let parse_start = std::time::Instant::now();
        let Ok(tree) = parse_php(&source) else {
            return Ok(None);
        };
        debug!("Hover: Parse took {:?}", parse_start.elapsed());
        
        let lang = language_php();

        // Try env() calls first - bail early if found
        let query_start = std::time::Instant::now();
        if let Ok(env_matches) = find_env_calls(&tree, &source, &lang) {
            debug!("Hover: find_env_calls took {:?}", query_start.elapsed());
            if let Some(env_match) = Self::find_match_at_position(&env_matches, position) {
                // Look up the variable in cache
                let env_cache_guard = self.env_cache.read().await;
                if let Some(env_cache) = env_cache_guard.as_ref() {
                    if let Some(env_var) = env_cache.get(env_match.var_name) {
                        let value_display = if env_var.value.is_empty() {
                            "(empty string)".to_string()
                        } else {
                            format!("`{}`", env_var.value)
                        };

                        let file_name = env_var.file_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");

                        let markdown = format!(
                            "**Environment Variable**: `{}`\n\n**Value**: {}\n\n**Defined in**: `{}`",
                            env_var.name,
                            value_display,
                            file_name
                        );

                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: markdown,
                            }),
                            range: Some(Range {
                                start: Position {
                                    line: env_match.row as u32,
                                    character: env_match.column as u32,
                                },
                                end: Position {
                                    line: env_match.row as u32,
                                    character: env_match.end_column as u32,
                                },
                            }),
                        }));
                    }
                }
                // Found env match but not in cache - return early, no need to check config
                return Ok(None);
            }
        }

        // Try config() calls only if env didn't match
        let config_query_start = std::time::Instant::now();
        if let Ok(config_matches) = find_config_calls(&tree, &source, &lang) {
            debug!("Hover: find_config_calls took {:?}", config_query_start.elapsed());
            if let Some(config_match) = Self::find_match_at_position(&config_matches, position) {
                let root_guard = self.root_path.read().await;
                if let Some(root) = root_guard.as_ref() {
                    // Parse config key: "app.name" -> file: "app.php"
                    let parts: Vec<&str> = config_match.config_key.split('.').collect();
                    if !parts.is_empty() {
                        let config_file = parts[0];
                        let config_path = root.join("config").join(format!("{}.php", config_file));

                        let file_exists = if self.file_exists(&config_path).await {
                            "✓ File exists"
                        } else {
                            "⚠ File not found"
                        };

                        let markdown = format!(
                            "**Config Key**: `{}`\n\n**File**: `config/{}.php`\n\n{}",
                            config_match.config_key,
                            config_file,
                            file_exists
                        );

                        return Ok(Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: markdown,
                            }),
                            range: Some(Range {
                                start: Position {
                                    line: config_match.row as u32,
                                    character: config_match.column as u32,
                                },
                                end: Position {
                                    line: config_match.row as u32,
                                    character: config_match.end_column as u32,
                                },
                            }),
                        }));
                    }
                }
            }
        }

        debug!("Hover: Total time {:?}", start.elapsed());
        Ok(None)
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
    /// Invalidate cache for a specific file when it changes
    async fn invalidate_file_cache(&self, uri: &Url, version: i32) {
        let mut cache = self.reference_cache.write().await;
        
        // Update document version
        cache.document_versions.insert(uri.clone(), version);
        
        // Remove cached references for this file
        if let Some(old_refs) = cache.file_references.remove(uri) {
            debug!("Invalidated cache for file: {} (had {} view references)", 
                   uri, old_refs.view_references.len());
        }
        
        // Rebuild global view references map since we removed references from one file
        self.rebuild_view_references_map(&mut cache).await;
    }

    /// Rebuild the global view -> references mapping from all cached files
    async fn rebuild_view_references_map(&self, cache: &mut ReferenceCache) {
        cache.view_references.clear();
        
        for file_refs in cache.file_references.values() {
            for (view_name, reference) in &file_refs.view_references {
                cache.view_references
                    .entry(view_name.clone())
                    .or_insert_with(Vec::new)
                    .push(reference.clone());
            }
        }
    }

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
        
        // Check if we have cached references for this view
        {
            let cache = self.reference_cache.read().await;
            if let Some(cached_refs) = cache.view_references.get(view_name) {
                debug!("Found {} cached references for view: {}", cached_refs.len(), view_name);
                return cached_refs.clone();
            }
        }

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

        // Cache the results
        {
            let mut cache = self.reference_cache.write().await;
            cache.view_references.insert(view_name.to_string(), all_references.clone());
        }

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