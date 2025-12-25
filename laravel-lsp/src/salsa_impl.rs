//! Salsa 0.25 incremental computation database for Laravel LSP
//!
//! This module provides proper incremental computation using the Salsa framework.
//! It replaces the custom "Salsa-inspired" implementation in salsa_db.rs.
//!
//! # Actor Pattern for Async Integration
//!
//! Since Salsa's `Storage` type is not `Send+Sync`, we use an actor pattern to
//! run Salsa operations on a dedicated thread. The `SalsaActor` owns the database
//! and processes requests via channels.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use salsa::Setter;
use tokio::sync::{mpsc, oneshot};

// ============================================================================
// Database Definition
// ============================================================================

/// The Salsa database trait for Laravel LSP
#[salsa::db]
pub trait Db: salsa::Database {}

/// The concrete database implementation
#[salsa::db]
#[derive(Default, Clone)]
pub struct LaravelDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for LaravelDatabase {}

#[salsa::db]
impl Db for LaravelDatabase {}

// ============================================================================
// Input Types - Source data provided to the system
// ============================================================================

/// Represents a source file in the workspace
#[salsa::input]
pub struct SourceFile {
    /// The file path
    #[returns(ref)]
    pub path: PathBuf,

    /// The document version from LSP
    pub version: i32,

    /// The file content
    #[returns(ref)]
    pub text: String,
}

// ============================================================================
// Interned Types - Deduplicated strings
// ============================================================================

/// Interned string for view names (e.g., "users.profile")
#[salsa::interned]
pub struct ViewName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for component names (e.g., "button")
#[salsa::interned]
pub struct ComponentName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for directive names (e.g., "extends")
#[salsa::interned]
pub struct DirectiveName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for env variable names (e.g., "APP_DEBUG")
#[salsa::interned]
pub struct EnvVarName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for config keys (e.g., "app.name")
#[salsa::interned]
pub struct ConfigKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for middleware names (e.g., "auth", "throttle:60,1")
#[salsa::interned]
pub struct MiddlewareName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// Interned string for translation keys (e.g., "messages.welcome")
#[salsa::interned]
pub struct TranslationKey<'db> {
    #[returns(ref)]
    pub key: String,
}

/// Interned string for asset paths (e.g., "css/app.css")
#[salsa::interned]
pub struct AssetPath<'db> {
    #[returns(ref)]
    pub path: String,
}

/// Interned string for binding names (e.g., "auth", "App\\Contracts\\PaymentGateway")
#[salsa::interned]
pub struct BindingName<'db> {
    #[returns(ref)]
    pub name: String,
}

// ============================================================================
// Tracked Types - Computed/derived values
// ============================================================================

/// A parsed view reference found in code
#[salsa::tracked]
pub struct ViewReference<'db> {
    pub name: ViewName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
}

/// A parsed component reference found in code
#[salsa::tracked]
pub struct ComponentReference<'db> {
    pub name: ComponentName<'db>,
    pub tag_name: ComponentName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed directive reference found in code
#[salsa::tracked]
pub struct DirectiveReference<'db> {
    pub name: DirectiveName<'db>,
    #[returns(ref)]
    pub arguments: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed env reference found in code
#[salsa::tracked]
pub struct EnvReference<'db> {
    pub name: EnvVarName<'db>,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed config reference found in code
#[salsa::tracked]
pub struct ConfigReference<'db> {
    pub key: ConfigKey<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Interned string for Livewire component names
#[salsa::interned]
pub struct LivewireName<'db> {
    #[returns(ref)]
    pub name: String,
}

/// A parsed Livewire component reference found in code
#[salsa::tracked]
pub struct LivewireReference<'db> {
    pub name: LivewireName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed middleware reference found in code
#[salsa::tracked]
pub struct MiddlewareReference<'db> {
    pub name: MiddlewareName<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed translation reference found in code
#[salsa::tracked]
pub struct TranslationReference<'db> {
    pub key: TranslationKey<'db>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset helper type - mirrors queries::AssetHelperType
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetHelperType {
    Asset,
    PublicPath,
    BasePath,
    AppPath,
    StoragePath,
    DatabasePath,
    LangPath,
    ConfigPath,
    ResourcePath,
    Mix,
    ViteAsset,
}

/// A parsed asset reference found in code
#[salsa::tracked]
pub struct AssetReference<'db> {
    pub path: AssetPath<'db>,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// A parsed binding reference found in code
#[salsa::tracked]
pub struct BindingReference<'db> {
    pub name: BindingName<'db>,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// All patterns found in a file
#[salsa::tracked]
pub struct ParsedPatterns<'db> {
    pub file: SourceFile,
    #[returns(ref)]
    pub views: Vec<ViewReference<'db>>,
    #[returns(ref)]
    pub components: Vec<ComponentReference<'db>>,
    #[returns(ref)]
    pub directives: Vec<DirectiveReference<'db>>,
    #[returns(ref)]
    pub env_refs: Vec<EnvReference<'db>>,
    #[returns(ref)]
    pub config_refs: Vec<ConfigReference<'db>>,
    #[returns(ref)]
    pub livewire_refs: Vec<LivewireReference<'db>>,
    #[returns(ref)]
    pub middleware_refs: Vec<MiddlewareReference<'db>>,
    #[returns(ref)]
    pub translation_refs: Vec<TranslationReference<'db>>,
    #[returns(ref)]
    pub asset_refs: Vec<AssetReference<'db>>,
    #[returns(ref)]
    pub binding_refs: Vec<BindingReference<'db>>,
}

// ============================================================================
// Query Functions - The actual computation
// ============================================================================

/// Parse a source file and extract all Laravel patterns
/// This is automatically memoized by Salsa
#[salsa::tracked]
pub fn parse_file_patterns<'db>(db: &'db dyn Db, file: SourceFile) -> ParsedPatterns<'db> {
    use crate::parser::{parse_blade, parse_php, language_blade, language_php};
    use crate::queries::{
        find_view_calls, find_blade_components, find_directives,
        find_env_calls, find_livewire_components, find_config_calls,
        find_middleware_calls, find_translation_calls, find_asset_calls, find_binding_calls,
        AssetHelperType as QueryAssetHelperType,
    };

    let text = file.text(db);
    let path = file.path(db);
    let is_blade = path.to_string_lossy().ends_with(".blade.php");

    let mut views = Vec::new();
    let mut components = Vec::new();
    let mut directives = Vec::new();
    let mut env_refs = Vec::new();
    let mut config_refs = Vec::new();
    let mut livewire_refs = Vec::new();
    let mut middleware_refs = Vec::new();
    let mut translation_refs = Vec::new();
    let mut asset_refs = Vec::new();
    let mut binding_refs = Vec::new();

    // Parse Blade files
    if is_blade {
        if let Ok(tree) = parse_blade(text) {
            let lang = language_blade();

            // Extract Blade components
            if let Ok(comps) = find_blade_components(&tree, text, &lang) {
                for comp in comps {
                    let name = ComponentName::new(db, comp.component_name.to_string());
                    let tag = ComponentName::new(db, comp.tag_name.to_string());
                    components.push(ComponentReference::new(
                        db,
                        name,
                        tag,
                        comp.row as u32,
                        comp.column as u32,
                        comp.end_column as u32,
                    ));
                }
            }

            // Extract Livewire components
            if let Ok(lw_comps) = find_livewire_components(&tree, text, &lang) {
                for lw in lw_comps {
                    let name = LivewireName::new(db, lw.component_name.to_string());
                    livewire_refs.push(LivewireReference::new(
                        db,
                        name,
                        lw.row as u32,
                        lw.column as u32,
                        lw.end_column as u32,
                    ));
                }
            }

            // Extract directives
            if let Ok(dirs) = find_directives(&tree, text, &lang) {
                for dir in dirs {
                    let name = DirectiveName::new(db, dir.directive_name.to_string());
                    let args = dir.arguments.map(|s| s.to_string());
                    directives.push(DirectiveReference::new(
                        db,
                        name,
                        args,
                        dir.row as u32,
                        dir.column as u32,
                        dir.end_column as u32,
                    ));
                }
            }
        }
    }

    // Parse PHP (including Blade files for embedded PHP)
    if let Ok(tree) = parse_php(text) {
        let lang = language_php();

        // Extract view calls
        if let Ok(view_calls) = find_view_calls(&tree, text, &lang) {
            for view in view_calls {
                let name = ViewName::new(db, view.view_name.to_string());
                views.push(ViewReference::new(
                    db,
                    name,
                    view.row as u32,
                    view.column as u32,
                    view.end_column as u32,
                    view.is_route_view,
                ));
            }
        }

        // Extract env calls
        if let Ok(env_calls) = find_env_calls(&tree, text, &lang) {
            for env in env_calls {
                let name = EnvVarName::new(db, env.var_name.to_string());
                env_refs.push(EnvReference::new(
                    db,
                    name,
                    env.has_fallback,
                    env.row as u32,
                    env.column as u32,
                    env.end_column as u32,
                ));
            }
        }

        // Extract config calls
        if let Ok(config_calls) = find_config_calls(&tree, text, &lang) {
            for config in config_calls {
                let key = ConfigKey::new(db, config.config_key.to_string());
                config_refs.push(ConfigReference::new(
                    db,
                    key,
                    config.row as u32,
                    config.column as u32,
                    config.end_column as u32,
                ));
            }
        }

        // Extract middleware calls
        if let Ok(middleware_calls) = find_middleware_calls(&tree, text, &lang) {
            for mw in middleware_calls {
                let name = MiddlewareName::new(db, mw.middleware_name.to_string());
                middleware_refs.push(MiddlewareReference::new(
                    db,
                    name,
                    mw.row as u32,
                    mw.column as u32,
                    mw.end_column as u32,
                ));
            }
        }

        // Extract translation calls
        if let Ok(translation_calls) = find_translation_calls(&tree, text, &lang) {
            for trans in translation_calls {
                let key = TranslationKey::new(db, trans.translation_key.to_string());
                translation_refs.push(TranslationReference::new(
                    db,
                    key,
                    trans.row as u32,
                    trans.column as u32,
                    trans.end_column as u32,
                ));
            }
        }

        // Extract asset calls
        if let Ok(asset_calls) = find_asset_calls(&tree, text, &lang) {
            for asset in asset_calls {
                let path = AssetPath::new(db, asset.path.to_string());
                let helper_type = match asset.helper_type {
                    QueryAssetHelperType::Asset => AssetHelperType::Asset,
                    QueryAssetHelperType::PublicPath => AssetHelperType::PublicPath,
                    QueryAssetHelperType::BasePath => AssetHelperType::BasePath,
                    QueryAssetHelperType::AppPath => AssetHelperType::AppPath,
                    QueryAssetHelperType::StoragePath => AssetHelperType::StoragePath,
                    QueryAssetHelperType::DatabasePath => AssetHelperType::DatabasePath,
                    QueryAssetHelperType::LangPath => AssetHelperType::LangPath,
                    QueryAssetHelperType::ConfigPath => AssetHelperType::ConfigPath,
                    QueryAssetHelperType::ResourcePath => AssetHelperType::ResourcePath,
                    QueryAssetHelperType::Mix => AssetHelperType::Mix,
                    QueryAssetHelperType::ViteAsset => AssetHelperType::ViteAsset,
                };
                asset_refs.push(AssetReference::new(
                    db,
                    path,
                    helper_type,
                    asset.row as u32,
                    asset.column as u32,
                    asset.end_column as u32,
                ));
            }
        }

        // Extract binding calls
        if let Ok(binding_calls) = find_binding_calls(&tree, text, &lang) {
            for binding in binding_calls {
                let name = BindingName::new(db, binding.binding_name.to_string());
                binding_refs.push(BindingReference::new(
                    db,
                    name,
                    binding.is_class_reference,
                    binding.row as u32,
                    binding.column as u32,
                    binding.end_column as u32,
                ));
            }
        }
    }

    ParsedPatterns::new(
        db, file, views, components, directives, env_refs, config_refs, livewire_refs,
        middleware_refs, translation_refs, asset_refs, binding_refs,
    )
}

// ============================================================================
// Helper Functions
// ============================================================================

impl LaravelDatabase {
    /// Create a new database instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a source file
    pub fn get_or_create_file(&self, path: PathBuf, version: i32, text: String) -> SourceFile {
        SourceFile::new(self, path, version, text)
    }

    /// Update a source file's content
    pub fn update_file(&mut self, file: SourceFile, version: i32, text: String) {
        file.set_version(self).to(version);
        file.set_text(self).to(text);
    }
}

// ============================================================================
// Data Transfer Types - Plain structs for sending data across threads
// ============================================================================

/// View reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ViewReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_route_view: bool,
}

/// Component reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ComponentReferenceData {
    pub name: String,
    pub tag_name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Directive reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct DirectiveReferenceData {
    pub name: String,
    pub arguments: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Env reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct EnvReferenceData {
    pub name: String,
    pub has_fallback: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Config reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct ConfigReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Livewire reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct LivewireReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Middleware reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct MiddlewareReferenceData {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Translation reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct TranslationReferenceData {
    pub key: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Asset reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct AssetReferenceData {
    pub path: String,
    pub helper_type: AssetHelperType,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Binding reference data for transfer across async boundaries
#[derive(Debug, Clone)]
pub struct BindingReferenceData {
    pub name: String,
    pub is_class_reference: bool,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// All parsed patterns for a file - plain data for transfer
#[derive(Debug, Clone, Default)]
pub struct ParsedPatternsData {
    pub views: Vec<ViewReferenceData>,
    pub components: Vec<ComponentReferenceData>,
    pub directives: Vec<DirectiveReferenceData>,
    pub env_refs: Vec<EnvReferenceData>,
    pub config_refs: Vec<ConfigReferenceData>,
    pub livewire_refs: Vec<LivewireReferenceData>,
    pub middleware_refs: Vec<MiddlewareReferenceData>,
    pub translation_refs: Vec<TranslationReferenceData>,
    pub asset_refs: Vec<AssetReferenceData>,
    pub binding_refs: Vec<BindingReferenceData>,
}

/// A pattern found at a specific cursor position
#[derive(Debug, Clone)]
pub enum PatternAtPosition {
    View(ViewReferenceData),
    Component(ComponentReferenceData),
    Directive(DirectiveReferenceData),
    EnvRef(EnvReferenceData),
    ConfigRef(ConfigReferenceData),
    Livewire(LivewireReferenceData),
    Middleware(MiddlewareReferenceData),
    Translation(TranslationReferenceData),
    Asset(AssetReferenceData),
    Binding(BindingReferenceData),
}

impl ParsedPatternsData {
    /// Find a pattern at the given cursor position (line, column)
    /// Returns the first matching pattern, or None if no pattern at that position
    pub fn find_at_position(&self, line: u32, column: u32) -> Option<PatternAtPosition> {
        // Check components first (most common in Blade files)
        for comp in &self.components {
            if comp.line == line && column >= comp.column && column <= comp.end_column {
                return Some(PatternAtPosition::Component(comp.clone()));
            }
        }

        // Check Livewire components
        for lw in &self.livewire_refs {
            if lw.line == line && column >= lw.column && column <= lw.end_column {
                return Some(PatternAtPosition::Livewire(lw.clone()));
            }
        }

        // Check directives (@extends, @include, etc.)
        for dir in &self.directives {
            if dir.line == line && column >= dir.column && column <= dir.end_column {
                return Some(PatternAtPosition::Directive(dir.clone()));
            }
        }

        // Check view calls
        for view in &self.views {
            if view.line == line && column >= view.column && column <= view.end_column {
                return Some(PatternAtPosition::View(view.clone()));
            }
        }

        // Check env calls
        for env in &self.env_refs {
            if env.line == line && column >= env.column && column <= env.end_column {
                return Some(PatternAtPosition::EnvRef(env.clone()));
            }
        }

        // Check config calls
        for config in &self.config_refs {
            if config.line == line && column >= config.column && column <= config.end_column {
                return Some(PatternAtPosition::ConfigRef(config.clone()));
            }
        }

        // Check middleware calls
        for mw in &self.middleware_refs {
            if mw.line == line && column >= mw.column && column <= mw.end_column {
                return Some(PatternAtPosition::Middleware(mw.clone()));
            }
        }

        // Check translation calls
        for trans in &self.translation_refs {
            if trans.line == line && column >= trans.column && column <= trans.end_column {
                return Some(PatternAtPosition::Translation(trans.clone()));
            }
        }

        // Check asset calls
        for asset in &self.asset_refs {
            if asset.line == line && column >= asset.column && column <= asset.end_column {
                return Some(PatternAtPosition::Asset(asset.clone()));
            }
        }

        // Check binding calls
        for binding in &self.binding_refs {
            if binding.line == line && column >= binding.column && column <= binding.end_column {
                return Some(PatternAtPosition::Binding(binding.clone()));
            }
        }

        None
    }
}

// ============================================================================
// Actor Pattern - For async integration
// ============================================================================

/// Requests that can be sent to the Salsa actor
pub enum SalsaRequest {
    /// Update or create a file in the database
    UpdateFile {
        path: PathBuf,
        version: i32,
        text: String,
        reply: oneshot::Sender<()>,
    },
    /// Get parsed patterns for a file
    GetPatterns {
        path: PathBuf,
        reply: oneshot::Sender<Option<ParsedPatternsData>>,
    },
    /// Remove a file from the database
    RemoveFile {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    /// Shutdown the actor
    Shutdown,
}

/// Handle to communicate with the Salsa actor
#[derive(Clone)]
pub struct SalsaHandle {
    sender: mpsc::Sender<SalsaRequest>,
}

impl SalsaHandle {
    /// Update or create a file in the database
    pub async fn update_file(&self, path: PathBuf, version: i32, text: String) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::UpdateFile { path, version, text, reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx.await.map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Get parsed patterns for a file
    pub async fn get_patterns(&self, path: PathBuf) -> Result<Option<ParsedPatternsData>, &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::GetPatterns { path, reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx.await.map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Remove a file from the database
    pub async fn remove_file(&self, path: PathBuf) -> Result<(), &'static str> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SalsaRequest::RemoveFile { path, reply: reply_tx })
            .await
            .map_err(|_| "Salsa actor disconnected")?;
        reply_rx.await.map_err(|_| "Salsa actor dropped reply channel")
    }

    /// Shutdown the actor gracefully
    pub async fn shutdown(&self) -> Result<(), &'static str> {
        self.sender
            .send(SalsaRequest::Shutdown)
            .await
            .map_err(|_| "Salsa actor already disconnected")
    }
}

/// The Salsa actor that owns the database and runs on a dedicated thread
pub struct SalsaActor {
    db: LaravelDatabase,
    receiver: mpsc::Receiver<SalsaRequest>,
    /// Map from path to SourceFile for efficient lookups and updates
    files: HashMap<PathBuf, SourceFile>,
}

impl SalsaActor {
    /// Spawn the actor on a dedicated thread and return a handle for communication
    pub fn spawn() -> SalsaHandle {
        let (tx, rx) = mpsc::channel(256);

        std::thread::spawn(move || {
            let mut actor = SalsaActor {
                db: LaravelDatabase::new(),
                receiver: rx,
                files: HashMap::new(),
            };
            actor.run();
        });

        SalsaHandle { sender: tx }
    }

    /// Main event loop - process requests until shutdown
    fn run(&mut self) {
        while let Some(request) = self.receiver.blocking_recv() {
            match request {
                SalsaRequest::UpdateFile { path, version, text, reply } => {
                    self.handle_update_file(path, version, text);
                    let _ = reply.send(());
                }
                SalsaRequest::GetPatterns { path, reply } => {
                    let result = self.handle_get_patterns(&path);
                    let _ = reply.send(result);
                }
                SalsaRequest::RemoveFile { path, reply } => {
                    self.files.remove(&path);
                    let _ = reply.send(());
                }
                SalsaRequest::Shutdown => {
                    break;
                }
            }
        }
    }

    /// Handle file update - create or update the SourceFile
    fn handle_update_file(&mut self, path: PathBuf, version: i32, text: String) {
        if let Some(file) = self.files.get(&path) {
            // Update existing file
            file.set_version(&mut self.db).to(version);
            file.set_text(&mut self.db).to(text);
        } else {
            // Create new file
            let file = SourceFile::new(&self.db, path.clone(), version, text);
            self.files.insert(path, file);
        }
    }

    /// Handle pattern query - parse file and extract patterns
    fn handle_get_patterns(&self, path: &PathBuf) -> Option<ParsedPatternsData> {
        let file = self.files.get(path)?;

        // This call is memoized by Salsa - it only re-parses if the file content changed
        let patterns = parse_file_patterns(&self.db, *file);

        // Convert Salsa types to plain data types for transfer
        let views = patterns.views(&self.db)
            .iter()
            .map(|v| ViewReferenceData {
                name: v.name(&self.db).name(&self.db).clone(),
                line: v.line(&self.db),
                column: v.column(&self.db),
                end_column: v.end_column(&self.db),
                is_route_view: v.is_route_view(&self.db),
            })
            .collect();

        let components = patterns.components(&self.db)
            .iter()
            .map(|c| ComponentReferenceData {
                name: c.name(&self.db).name(&self.db).clone(),
                tag_name: c.tag_name(&self.db).name(&self.db).clone(),
                line: c.line(&self.db),
                column: c.column(&self.db),
                end_column: c.end_column(&self.db),
            })
            .collect();

        let directives = patterns.directives(&self.db)
            .iter()
            .map(|d| DirectiveReferenceData {
                name: d.name(&self.db).name(&self.db).clone(),
                arguments: d.arguments(&self.db).clone(),
                line: d.line(&self.db),
                column: d.column(&self.db),
                end_column: d.end_column(&self.db),
            })
            .collect();

        let env_refs = patterns.env_refs(&self.db)
            .iter()
            .map(|e| EnvReferenceData {
                name: e.name(&self.db).name(&self.db).clone(),
                has_fallback: e.has_fallback(&self.db),
                line: e.line(&self.db),
                column: e.column(&self.db),
                end_column: e.end_column(&self.db),
            })
            .collect();

        let config_refs = patterns.config_refs(&self.db)
            .iter()
            .map(|c| ConfigReferenceData {
                key: c.key(&self.db).key(&self.db).clone(),
                line: c.line(&self.db),
                column: c.column(&self.db),
                end_column: c.end_column(&self.db),
            })
            .collect();

        let livewire_refs = patterns.livewire_refs(&self.db)
            .iter()
            .map(|lw| LivewireReferenceData {
                name: lw.name(&self.db).name(&self.db).clone(),
                line: lw.line(&self.db),
                column: lw.column(&self.db),
                end_column: lw.end_column(&self.db),
            })
            .collect();

        let middleware_refs = patterns.middleware_refs(&self.db)
            .iter()
            .map(|mw| MiddlewareReferenceData {
                name: mw.name(&self.db).name(&self.db).clone(),
                line: mw.line(&self.db),
                column: mw.column(&self.db),
                end_column: mw.end_column(&self.db),
            })
            .collect();

        let translation_refs = patterns.translation_refs(&self.db)
            .iter()
            .map(|t| TranslationReferenceData {
                key: t.key(&self.db).key(&self.db).clone(),
                line: t.line(&self.db),
                column: t.column(&self.db),
                end_column: t.end_column(&self.db),
            })
            .collect();

        let asset_refs = patterns.asset_refs(&self.db)
            .iter()
            .map(|a| AssetReferenceData {
                path: a.path(&self.db).path(&self.db).clone(),
                helper_type: a.helper_type(&self.db),
                line: a.line(&self.db),
                column: a.column(&self.db),
                end_column: a.end_column(&self.db),
            })
            .collect();

        let binding_refs = patterns.binding_refs(&self.db)
            .iter()
            .map(|b| BindingReferenceData {
                name: b.name(&self.db).name(&self.db).clone(),
                is_class_reference: b.is_class_reference(&self.db),
                line: b.line(&self.db),
                column: b.column(&self.db),
                end_column: b.end_column(&self.db),
            })
            .collect();

        Some(ParsedPatternsData {
            views,
            components,
            directives,
            env_refs,
            config_refs,
            livewire_refs,
            middleware_refs,
            translation_refs,
            asset_refs,
            binding_refs,
        })
    }
}
