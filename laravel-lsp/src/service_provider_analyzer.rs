//! Service Provider Analyzer
//!
//! This module analyzes Laravel service providers to extract registered components:
//! - Middleware aliases and groups
//! - Route bindings
//! - Singletons and bindings
//! - Aliases
//! - Event listeners
//! - Commands
//!
//! This is the "Laravel way" of discovering what's available in the application,
//! as opposed to just file system scanning or autoload parsing.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

use crate::parser::{language_php, parse_php};

/// Complete registry of all components registered through service providers
#[derive(Debug, Clone)]
pub struct ServiceProviderRegistry {
    /// Middleware aliases: 'auth' -> 'App\Http\Middleware\Authenticate'
    pub middleware_aliases: HashMap<String, MiddlewareRegistration>,
    
    /// Middleware groups: 'web' -> ['App\Http\Middleware\...', ...]
    pub middleware_groups: HashMap<String, Vec<String>>,
    
    /// Service bindings: abstract -> BindingRegistration
    pub bindings: HashMap<String, BindingRegistration>,
    
    /// Singleton bindings: abstract -> BindingRegistration
    pub singletons: HashMap<String, BindingRegistration>,
    
    /// Class aliases: 'Route' -> 'Illuminate\Support\Facades\Route'
    pub aliases: HashMap<String, String>,
    
    /// Registered commands
    pub commands: Vec<String>,
    
    /// Last time this was parsed
    pub last_parsed: SystemTime,
    
    /// Root path of the project
    pub root_path: PathBuf,
}

/// Information about a registered middleware
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareRegistration {
    /// The middleware alias (e.g., 'auth')
    pub alias: String,
    
    /// Fully qualified class name
    pub class_name: String,
    
    /// Resolved file path of the middleware class (if resolvable)
    pub file_path: Option<PathBuf>,
    
    /// Where this was registered (for debugging, e.g., "bootstrap/app.php")
    pub registered_in: String,
    
    /// Source file where the alias is defined (for goto-definition to alias)
    pub source_file: Option<PathBuf>,
    
    /// Line number in source file where alias is defined (0-based)
    pub source_line: Option<usize>,
    
    /// Priority (framework = 0, package = 1, app = 2)
    pub priority: u8,
}

/// Information about a registered container binding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingRegistration {
    /// The abstract/alias (e.g., 'auth', 'App\Contracts\PaymentGateway')
    pub abstract_name: String,
    
    /// Fully qualified concrete class name
    pub concrete_class: String,
    
    /// Resolved file path of the concrete class (if resolvable)
    pub file_path: Option<PathBuf>,
    
    /// Binding type (bind, singleton, scoped, alias)
    pub binding_type: BindingType,
    
    /// Where this was registered (e.g., "app/Providers/AppServiceProvider.php")
    pub registered_in: String,
    
    /// Source file where the binding is defined (for goto-definition)
    pub source_file: Option<PathBuf>,
    
    /// Line number in source file where binding is defined (0-based)
    pub source_line: Option<usize>,
    
    /// Priority (framework = 0, package = 1, app = 2)
    pub priority: u8,
}

/// Types of container bindings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingType {
    /// $this->app->bind()
    Bind,
    /// $this->app->singleton()
    Singleton,
    /// $this->app->scoped()
    Scoped,
    /// $this->app->alias()
    Alias,
}

impl ServiceProviderRegistry {
    /// Create a new empty registry
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            middleware_aliases: HashMap::new(),
            middleware_groups: HashMap::new(),
            bindings: HashMap::new(),
            singletons: HashMap::new(),
            aliases: HashMap::new(),
            commands: Vec::new(),
            last_parsed: SystemTime::now(),
            root_path,
        }
    }

    /// Create a default empty registry (for compatibility)
    pub fn default_with_root(root_path: PathBuf) -> Self {
        Self::new(root_path)
    }

    /// Get middleware by alias
    pub fn get_middleware(&self, alias: &str) -> Option<&MiddlewareRegistration> {
        self.middleware_aliases.get(alias)
    }

    /// Add a middleware registration
    pub fn add_middleware(&mut self, registration: MiddlewareRegistration) {
        // Only add if not already present, or if higher priority
        if let Some(existing) = self.middleware_aliases.get(&registration.alias) {
            if registration.priority >= existing.priority {
                self.middleware_aliases.insert(registration.alias.clone(), registration);
            }
        } else {
            self.middleware_aliases.insert(registration.alias.clone(), registration);
        }
    }

    /// Get binding by abstract name
    pub fn get_binding(&self, abstract_name: &str) -> Option<&BindingRegistration> {
        // Check bindings first, then singletons
        self.bindings.get(abstract_name)
            .or_else(|| self.singletons.get(abstract_name))
    }

    /// Add a binding registration
    pub fn add_binding(&mut self, registration: BindingRegistration) {
        // Only add if not already present, or if higher priority
        let target_map = match registration.binding_type {
            BindingType::Singleton | BindingType::Scoped => &mut self.singletons,
            BindingType::Bind | BindingType::Alias => &mut self.bindings,
        };
        
        if let Some(existing) = target_map.get(&registration.abstract_name) {
            if registration.priority >= existing.priority {
                target_map.insert(registration.abstract_name.clone(), registration);
            }
        } else {
            target_map.insert(registration.abstract_name.clone(), registration);
        }
    }
}

/// Analyzes all service providers and builds a complete registry
pub async fn analyze_service_providers(root_path: &Path) -> Result<ServiceProviderRegistry> {
    let mut registry = ServiceProviderRegistry::new(root_path.to_path_buf());

    eprintln!("DEBUG: Starting service provider analysis for root: {:?}", root_path);

    // Priority 0: Framework providers (highest priority)
    analyze_framework_providers(&mut registry, root_path).await?;

    // Priority 1: Package providers
    analyze_package_providers(&mut registry, root_path).await?;

    // Priority 2: Application providers (can override framework/packages)
    analyze_app_providers(&mut registry, root_path).await?;

    eprintln!("DEBUG: Service provider analysis complete. Found {} middleware aliases", registry.middleware_aliases.len());

    Ok(registry)
}

/// Analyze Laravel framework service providers
async fn analyze_framework_providers(registry: &mut ServiceProviderRegistry, root_path: &Path) -> Result<()> {
    eprintln!("DEBUG: Analyzing framework providers...");
    
    let framework_path = root_path.join("vendor/laravel/framework/src/Illuminate");
    
    if !framework_path.exists() {
        eprintln!("DEBUG: Framework path not found: {:?}", framework_path);
        return Ok(());
    }

    // Key framework providers that register middleware
    let _provider_paths = [
        "Auth/AuthServiceProvider.php",
        "Auth/Middleware/AuthenticateServiceProvider.php", // Not real, example
        "Routing/RoutingServiceProvider.php",
        "Foundation/Support/Providers/RouteServiceProvider.php",
        "Session/SessionServiceProvider.php",
    ];

    // Also scan for all *ServiceProvider.php files in framework
    let framework_providers = find_service_provider_files(&framework_path)?;
    
    eprintln!("DEBUG: Found {} framework provider files", framework_providers.len());

    for provider_file in framework_providers {
        if let Ok(content) = tokio::fs::read_to_string(&provider_file).await {
            parse_service_provider(
                &content,
                &provider_file,
                registry,
                0, // Priority 0 = framework
                root_path,
            )?;
        }
    }

    // Hardcoded framework middleware (as fallback if we can't parse providers)
    add_known_framework_middleware(registry, root_path);
    
    // Hardcoded framework bindings (as fallback if we can't parse providers)
    add_known_framework_bindings(registry, root_path);

    Ok(())
}

/// Add known Laravel framework middleware as fallback
/// Only adds fallbacks for middleware that weren't discovered during provider scanning
fn add_known_framework_middleware(registry: &mut ServiceProviderRegistry, root_path: &Path) {
    eprintln!("DEBUG: Adding known framework middleware fallbacks (only for undiscovered middleware)...");
    
    let known_middleware = [
        ("auth", "Illuminate\\Auth\\Middleware\\Authenticate"),
        ("auth.basic", "Illuminate\\Auth\\Middleware\\AuthenticateWithBasicAuth"),
        ("auth.session", "Illuminate\\Session\\Middleware\\AuthenticateSession"),
        ("cache.headers", "Illuminate\\Http\\Middleware\\SetCacheHeaders"),
        ("can", "Illuminate\\Auth\\Middleware\\Authorize"),
        ("guest", "Illuminate\\Auth\\Middleware\\RedirectIfAuthenticated"),
        ("password.confirm", "Illuminate\\Auth\\Middleware\\RequirePassword"),
        ("precognition", "Illuminate\\Foundation\\Http\\Middleware\\HandlePrecognitiveRequests"),
        ("signed", "Illuminate\\Routing\\Middleware\\ValidateSignature"),
        ("throttle", "Illuminate\\Routing\\Middleware\\ThrottleRequests"),
        ("verified", "Illuminate\\Auth\\Middleware\\EnsureEmailIsVerified"),
    ];

    for (alias, class_name) in &known_middleware {
        // Only add fallback if this middleware wasn't already discovered
        if registry.get_middleware(alias).is_some() {
            eprintln!("DEBUG: Skipping fallback for '{}' middleware - already discovered in service provider", alias);
            continue;
        }
        
        let file_path = resolve_illuminate_class_to_file(class_name, root_path);
        
        eprintln!("DEBUG: Adding fallback middleware: '{}' -> '{}'", alias, class_name);
        
        registry.add_middleware(MiddlewareRegistration {
            alias: alias.to_string(),
            class_name: class_name.to_string(),
            file_path,
            registered_in: "Framework (fallback middleware)".to_string(),
            source_file: None,
            source_line: None,
            priority: 0,
        });
    }
}

/// Analyze package service providers from vendor
async fn analyze_package_providers(registry: &mut ServiceProviderRegistry, root_path: &Path) -> Result<()> {
    eprintln!("DEBUG: Analyzing package providers...");
    
    let vendor_path = root_path.join("vendor");
    
    if !vendor_path.exists() {
        eprintln!("DEBUG: Vendor path not found: {:?}", vendor_path);
        return Ok(());
    }

    // Scan vendor for service providers
    // Look in common package locations
    let package_providers = find_package_service_providers(&vendor_path)?;
    
    eprintln!("DEBUG: Found {} package provider files", package_providers.len());

    for provider_file in package_providers {
        if let Ok(content) = tokio::fs::read_to_string(&provider_file).await {
            parse_service_provider(
                &content,
                &provider_file,
                registry,
                1, // Priority 1 = packages
                root_path,
            )?;
        }
    }

    Ok(())
}

/// Analyze application service providers
async fn analyze_app_providers(registry: &mut ServiceProviderRegistry, root_path: &Path) -> Result<()> {
    eprintln!("DEBUG: Analyzing app providers...");
    
    // Parse bootstrap/app.php for Laravel 11+
    let bootstrap_app = root_path.join("bootstrap/app.php");
    if bootstrap_app.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&bootstrap_app).await {
            parse_bootstrap_app(&content, registry, root_path)?;
        }
    }

    // Parse app/Http/Kernel.php for Laravel 10 and below
    let kernel_path = root_path.join("app/Http/Kernel.php");
    if kernel_path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&kernel_path).await {
            parse_kernel_php(&content, registry, root_path)?;
        }
    }

    // Parse app/Providers/* service providers
    let providers_path = root_path.join("app/Providers");
    if providers_path.exists() {
        let app_providers = find_service_provider_files(&providers_path)?;
        
        eprintln!("DEBUG: Found {} app provider files", app_providers.len());
        
        for provider_file in app_providers {
            if let Ok(content) = tokio::fs::read_to_string(&provider_file).await {
                parse_service_provider(
                    &content,
                    &provider_file,
                    registry,
                    2, // Priority 2 = app (highest)
                    root_path,
                )?;
            }
        }
    }

    Ok(())
}

/// Parse Laravel 11+ bootstrap/app.php for middleware registrations
fn parse_bootstrap_app(content: &str, registry: &mut ServiceProviderRegistry, root_path: &Path) -> Result<()> {
    eprintln!("DEBUG: Parsing bootstrap/app.php for middleware...");
    
    // Use regex to find $middleware->alias([...]) calls
    let re = regex::Regex::new(r#"['"]([^'"]+)['"]\s*=>\s*\\?([A-Za-z0-9_\\]+)::class"#)?;
    
    let bootstrap_path = root_path.join("bootstrap/app.php");
    
    let mut count = 0;
    for cap in re.captures_iter(content) {
        if let (Some(alias), Some(class)) = (cap.get(1), cap.get(2)) {
            let alias_str = alias.as_str();
            let class_str = class.as_str().trim_start_matches('\\');
            
            // Calculate line number where this alias appears
            let match_start = alias.start();
            let line_number = content[..match_start].lines().count();
            
            eprintln!("DEBUG: Found middleware alias in bootstrap/app.php: '{}' -> '{}' at line {}", 
                     alias_str, class_str, line_number);
            
            let file_path = resolve_class_to_file(class_str, root_path);
            
            registry.add_middleware(MiddlewareRegistration {
                alias: alias_str.to_string(),
                class_name: class_str.to_string(),
                file_path,
                registered_in: "bootstrap/app.php".to_string(),
                source_file: Some(bootstrap_path.clone()),
                source_line: Some(line_number),
                priority: 2,
            });
            count += 1;
        }
    }
    
    eprintln!("DEBUG: Parsed {} middleware from bootstrap/app.php", count);
    
    Ok(())
}

/// Parse Laravel 10 app/Http/Kernel.php for middleware
fn parse_kernel_php(content: &str, registry: &mut ServiceProviderRegistry, root_path: &Path) -> Result<()> {
    eprintln!("DEBUG: Parsing app/Http/Kernel.php for middleware...");
    
    let kernel_path = root_path.join("app/Http/Kernel.php");
    
    // Parse with tree-sitter to find $middlewareAliases or $routeMiddleware arrays
    let tree = parse_php(content)?;
    let lang = language_php();
    
    // Tree-sitter query for middleware arrays
    let query_str = r#"
        (property_declaration
            (variable_name) @prop_name
            (array_creation_expression) @array)
    "#;
    
    let query = Query::new(&lang, query_str)
        .map_err(|e| anyhow!("Failed to compile query: {:?}", e))?;
    
    let mut cursor = QueryCursor::new();
    let root_node = tree.root_node();
    let source_bytes = content.as_bytes();
    
    let mut captures = cursor.captures(&query, root_node, source_bytes);
    
    while let Some((query_match, _)) = captures.next() {
        let mut prop_name = None;
        let mut array_node = None;
        
        for capture in query_match.captures {
            let name = query.capture_names()[capture.index as usize];
            match name {
                "prop_name" => {
                    prop_name = capture.node.utf8_text(source_bytes).ok();
                }
                "array" => {
                    array_node = Some(capture.node);
                }
                _ => {}
            }
        }
        
        if let (Some(name), Some(array)) = (prop_name, array_node) {
            if name.contains("middleware") {
                parse_middleware_array(array, source_bytes, registry, root_path, &kernel_path, 2)?;
            }
        }
    }
    
    Ok(())
}

/// Parse a middleware array from tree-sitter node
fn parse_middleware_array(
    array_node: tree_sitter::Node,
    source_bytes: &[u8],
    registry: &mut ServiceProviderRegistry,
    root_path: &Path,
    source_file_path: &Path,
    priority: u8,
) -> Result<()> {
    let mut cursor = array_node.walk();
    
    for child in array_node.children(&mut cursor) {
        if child.kind() == "array_element_initializer" {
            let mut key = None;
            let mut value = None;
            
            let mut child_cursor = child.walk();
            for element_child in child.children(&mut child_cursor) {
                if element_child.kind() == "string" {
                    let text = element_child.utf8_text(source_bytes).ok();
                    if key.is_none() {
                        key = text.map(|s| s.trim_matches(|c| c == '\'' || c == '"').to_string());
                    } else if value.is_none() {
                        value = text.map(|s| s.trim_matches(|c| c == '\'' || c == '"').to_string());
                    }
                } else if element_child.kind() == "class_constant_access_expression" {
                    let text = element_child.utf8_text(source_bytes).ok();
                    if let Some(class_ref) = text {
                        // Extract "SomeClass::class" -> "SomeClass"
                        let class_name = class_ref.trim_end_matches("::class").trim_start_matches('\\').to_string();
                        value = Some(class_name);
                    }
                }
            }
            
            if let (Some(alias), Some(class)) = (key, value) {
                // Calculate line number for this middleware entry
                let line_number = {
                    let start_byte = child.start_byte();
                    let preceding_text = std::str::from_utf8(&source_bytes[..start_byte]).unwrap_or("");
                    preceding_text.lines().count()
                };
                
                eprintln!("DEBUG: Found middleware in {:?}: '{}' -> '{}' at line {}", 
                         source_file_path, alias, class, line_number);
                
                let file_path = resolve_class_to_file(&class, root_path);
                
                registry.add_middleware(MiddlewareRegistration {
                    alias,
                    class_name: class,
                    file_path,
                    registered_in: source_file_path.to_string_lossy().to_string(),
                    source_file: Some(source_file_path.to_path_buf()),
                    source_line: Some(line_number),
                    priority,
                });
            }
        }
    }
    
    Ok(())
}

/// Parse a service provider file for registrations
fn parse_service_provider(
    content: &str,
    provider_file: &Path,
    registry: &mut ServiceProviderRegistry,
    priority: u8,
    root_path: &Path,
) -> Result<()> {
    // Parse with tree-sitter
    let _tree = parse_php(content)?;
    
    // Look for $router->aliasMiddleware() calls, middleware registrations, etc.
    // This is complex and would need sophisticated tree-sitter queries
    
    // For now, use regex as a fallback (we can enhance with tree-sitter later)
    parse_service_provider_regex(content, provider_file, registry, priority, root_path)?;
    
    Ok(())
}

/// Parse service provider using regex (fallback)
fn parse_service_provider_regex(
    content: &str,
    provider_file: &Path,
    registry: &mut ServiceProviderRegistry,
    priority: u8,
    root_path: &Path,
) -> Result<()> {
    // Parse container bindings
    parse_container_bindings(content, provider_file, registry, priority, root_path)?;
    
    // Look for $router->aliasMiddleware('name', Class::class)
    let alias_re = regex::Regex::new(r#"aliasMiddleware\s*\(\s*['"]([^'"]+)['"]\s*,\s*([A-Za-z0-9_\\]+)::class"#)?;
    
    for cap in alias_re.captures_iter(content) {
        if let (Some(alias), Some(class)) = (cap.get(1), cap.get(2)) {
            let alias_str = alias.as_str();
            let class_str = class.as_str().trim_start_matches('\\');
            
            let file_path = resolve_class_to_file(class_str, root_path);
            let source_name = provider_file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
            
            eprintln!("DEBUG: Found middleware in {}: '{}' -> '{}'", source_name, alias_str, class_str);
            
            registry.add_middleware(MiddlewareRegistration {
                alias: alias_str.to_string(),
                class_name: class_str.to_string(),
                file_path,
                registered_in: format!("{}", provider_file.display()),
                source_file: Some(provider_file.to_path_buf()),
                source_line: None, // TODO: Extract line number from tree-sitter node
                priority,
            });
        }
    }
    
    Ok(())
}

/// Find all *ServiceProvider.php files in a directory (recursive)
fn find_service_provider_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut providers = Vec::new();
    
    if !dir.exists() || !dir.is_dir() {
        return Ok(providers);
    }
    
    // Recursively find *ServiceProvider.php files
    find_providers_recursive(dir, &mut providers, 0)?;
    
    Ok(providers)
}

fn find_providers_recursive(dir: &Path, providers: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    // Limit recursion depth to avoid infinite loops
    if depth > 10 {
        return Ok(());
    }
    
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_dir() {
            // Skip common non-provider directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "tests" || name == "Tests" || name == "node_modules" {
                    continue;
                }
            }
            find_providers_recursive(&path, providers, depth + 1)?;
        } else if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.ends_with("ServiceProvider.php") {
                    providers.push(path);
                }
            }
        }
    }
    
    Ok(())
}

/// Find service providers in vendor packages
fn find_package_service_providers(vendor_path: &Path) -> Result<Vec<PathBuf>> {
    let mut providers = Vec::new();
    
    // Limit scanning to avoid performance issues
    // Only scan first 2 levels: vendor/{org}/{package}/src/*ServiceProvider.php
    
    if !vendor_path.exists() {
        return Ok(providers);
    }
    
    for org_entry in std::fs::read_dir(vendor_path)? {
        let org_entry = org_entry?;
        let org_path = org_entry.path();
        
        if !org_path.is_dir() {
            continue;
        }
        
        // Skip special directories
        if let Some(name) = org_path.file_name().and_then(|n| n.to_str()) {
            if name == "bin" || name == "composer" {
                continue;
            }
        }
        
        for package_entry in std::fs::read_dir(&org_path)? {
            let package_entry = package_entry?;
            let package_path = package_entry.path();
            
            if !package_path.is_dir() {
                continue;
            }
            
            // Look for src/*ServiceProvider.php
            let src_path = package_path.join("src");
            if src_path.exists() {
                let package_providers = find_service_provider_files(&src_path)?;
                providers.extend(package_providers);
            }
        }
    }
    
    Ok(providers)
}

/// Parse container bindings from service provider content
fn parse_container_bindings(
    content: &str,
    provider_file: &Path,
    registry: &mut ServiceProviderRegistry,
    priority: u8,
    root_path: &Path,
) -> Result<()> {
    // Pattern 1: $this->app->bind('abstract', 'concrete')
    // Pattern 2: $this->app->bind('abstract', Concrete::class)
    // Pattern 3: $this->app->bind(Abstract::class, Concrete::class)
    let bind_re = regex::Regex::new(
        r#"(?:->|\$this->app->|\$app->)(bind|singleton|scoped)\s*\(\s*(?:['"]([^'"]+)['"]|([A-Za-z0-9_\\]+)::class)\s*,\s*(?:['"]([^'"]+)['"]|([A-Za-z0-9_\\]+)::class|function|fn)"#
    )?;
    
    let content_bytes = content.as_bytes();
    
    for cap in bind_re.captures_iter(content) {
        let binding_method = cap.get(1).map(|m| m.as_str()).unwrap_or("bind");
        let binding_type = match binding_method {
            "singleton" => BindingType::Singleton,
            "scoped" => BindingType::Scoped,
            _ => BindingType::Bind,
        };
        
        // Extract abstract (either string or Class::class)
        let abstract_name = if let Some(string_abstract) = cap.get(2) {
            string_abstract.as_str().to_string()
        } else if let Some(class_abstract) = cap.get(3) {
            class_abstract.as_str().trim_start_matches('\\').to_string()
        } else {
            continue;
        };
        
        // Extract concrete (either string or Class::class)
        let concrete_class = if let Some(string_concrete) = cap.get(4) {
            string_concrete.as_str().to_string()
        } else if let Some(class_concrete) = cap.get(5) {
            class_concrete.as_str().trim_start_matches('\\').to_string()
        } else {
            // If no concrete class (closure binding), use abstract as concrete
            abstract_name.clone()
        };
        
        let file_path = resolve_class_to_file(&concrete_class, root_path);
        let source_name = provider_file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
        
        // Calculate line number for this binding
        let line_number = if let Some(match_obj) = cap.get(0) {
            let start_byte = match_obj.start();
            let preceding_text = std::str::from_utf8(&content_bytes[..start_byte]).unwrap_or("");
            preceding_text.lines().count().saturating_sub(1)
        } else {
            0
        };
        
        eprintln!("DEBUG: Found {} binding in {}:{}: '{}' -> '{}'", 
                 binding_method, source_name, line_number + 1, abstract_name, concrete_class);
        
        registry.add_binding(BindingRegistration {
            abstract_name: abstract_name.clone(),
            concrete_class,
            file_path,
            binding_type,
            registered_in: provider_file.to_string_lossy().to_string(),
            source_file: Some(provider_file.to_path_buf()),
            source_line: Some(line_number),
            priority,
        });
    }
    
    // Pattern 4: $this->app->alias('concrete', 'alias')
    // Pattern 5: $this->app->alias(Concrete::class, 'alias')
    let alias_re = regex::Regex::new(
        r#"(?:->|\$this->app->|\$app->)alias\s*\(\s*(?:['"]([^'"]+)['"]|([A-Za-z0-9_\\]+)::class)\s*,\s*['"]([^'"]+)['"]\s*\)"#
    )?;
    
    for cap in alias_re.captures_iter(content) {
        // Extract concrete (either string or Class::class)
        let concrete_class = if let Some(string_concrete) = cap.get(1) {
            string_concrete.as_str().to_string()
        } else if let Some(class_concrete) = cap.get(2) {
            class_concrete.as_str().trim_start_matches('\\').to_string()
        } else {
            continue;
        };
        
        let alias_name = cap.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
        
        let file_path = resolve_class_to_file(&concrete_class, root_path);
        let source_name = provider_file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
        
        // Calculate line number for this alias
        let line_number = if let Some(match_obj) = cap.get(0) {
            let start_byte = match_obj.start();
            let preceding_text = std::str::from_utf8(&content_bytes[..start_byte]).unwrap_or("");
            preceding_text.lines().count().saturating_sub(1)
        } else {
            0
        };
        
        eprintln!("DEBUG: Found alias binding in {}:{}: '{}' -> '{}'", 
                 source_name, line_number + 1, alias_name, concrete_class);
        
        registry.add_binding(BindingRegistration {
            abstract_name: alias_name.clone(),
            concrete_class,
            file_path,
            binding_type: BindingType::Alias,
            registered_in: provider_file.to_string_lossy().to_string(),
            source_file: Some(provider_file.to_path_buf()),
            source_line: Some(line_number),
            priority,
        });
    }
    
    Ok(())
}

/// Resolve a fully qualified class name to a file path
/// Uses PSR-4 autoloading conventions
pub fn resolve_class_to_file(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    let path_str = class_name.replace("\\", "/");
    
    // Try common namespace mappings
    let mappings = [
        ("App/", "app/"),
        ("Database/Seeders/", "database/seeders/"),
        ("Database/Factories/", "database/factories/"),
        ("Illuminate/", "vendor/laravel/framework/src/Illuminate/"),
    ];
    
    for (namespace_prefix, path_prefix) in &mappings {
        if path_str.starts_with(namespace_prefix) {
            let relative = path_str.strip_prefix(namespace_prefix).unwrap();
            let file_path = root_path.join(path_prefix).join(relative).with_extension("php");
            return Some(file_path);
        }
    }
    
    None
}

/// Add known Laravel framework container bindings as fallback
/// Only adds fallbacks for bindings that weren't discovered during provider scanning
fn add_known_framework_bindings(registry: &mut ServiceProviderRegistry, root_path: &Path) {
    eprintln!("DEBUG: Adding known framework bindings fallbacks (only for undiscovered bindings)...");
    
    let known_bindings = [
        // Core application bindings
        ("app", "Illuminate\\Foundation\\Application"),
        ("auth", "Illuminate\\Auth\\AuthManager"),
        ("auth.driver", "Illuminate\\Contracts\\Auth\\Guard"),
        ("blade.compiler", "Illuminate\\View\\Compilers\\BladeCompiler"),
        ("cache", "Illuminate\\Cache\\CacheManager"),
        ("cache.store", "Illuminate\\Cache\\Repository"),
        ("config", "Illuminate\\Config\\Repository"),
        ("cookie", "Illuminate\\Cookie\\CookieJar"),
        ("db", "Illuminate\\Database\\DatabaseManager"),
        ("db.connection", "Illuminate\\Database\\Connection"),
        ("encrypter", "Illuminate\\Encryption\\Encrypter"),
        ("events", "Illuminate\\Events\\Dispatcher"),
        ("files", "Illuminate\\Filesystem\\Filesystem"),
        ("filesystem", "Illuminate\\Filesystem\\FilesystemManager"),
        ("filesystem.disk", "Illuminate\\Contracts\\Filesystem\\Filesystem"),
        ("hash", "Illuminate\\Hashing\\HashManager"),
        ("log", "Illuminate\\Log\\LogManager"),
        ("mailer", "Illuminate\\Mail\\Mailer"),
        ("queue", "Illuminate\\Queue\\QueueManager"),
        ("queue.connection", "Illuminate\\Contracts\\Queue\\Queue"),
        ("redirect", "Illuminate\\Routing\\Redirector"),
        ("redis", "Illuminate\\Redis\\RedisManager"),
        ("request", "Illuminate\\Http\\Request"),
        ("router", "Illuminate\\Routing\\Router"),
        ("session", "Illuminate\\Session\\SessionManager"),
        ("session.store", "Illuminate\\Session\\Store"),
        ("url", "Illuminate\\Routing\\UrlGenerator"),
        ("validator", "Illuminate\\Validation\\Factory"),
        ("view", "Illuminate\\View\\Factory"),
    ];
    
    for (abstract_name, concrete_class) in &known_bindings {
        // Only add fallback if this binding wasn't already discovered
        if registry.get_binding(abstract_name).is_some() {
            eprintln!("DEBUG: Skipping fallback for '{}' - already discovered in service provider", abstract_name);
            continue;
        }
        
        let file_path = resolve_class_to_file(concrete_class, root_path);
        
        eprintln!("DEBUG: Adding fallback binding: '{}' -> '{}'", abstract_name, concrete_class);
        
        registry.add_binding(BindingRegistration {
            abstract_name: abstract_name.to_string(),
            concrete_class: concrete_class.to_string(),
            file_path,
            binding_type: BindingType::Singleton,
            registered_in: "Laravel Framework (fallback)".to_string(),
            source_file: None, // Framework fallback bindings don't have a specific source file we can navigate to
            source_line: None,
            priority: 0, // Framework priority
        });
    }
}

/// Resolve Illuminate framework classes
fn resolve_illuminate_class_to_file(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    let path_str = class_name.replace("\\", "/");
    
    if path_str.starts_with("Illuminate/") {
        let relative = path_str.strip_prefix("Illuminate/").unwrap();
        let file_path = root_path
            .join("vendor/laravel/framework/src/Illuminate")
            .join(relative)
            .with_extension("php");
        return Some(file_path);
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_app_class() {
        let root = PathBuf::from("/project");
        let result = resolve_class_to_file("App\\Http\\Middleware\\Authenticate", &root);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("app/Http/Middleware/Authenticate.php"));
    }

    #[test]
    fn test_resolve_illuminate_class() {
        let root = PathBuf::from("/project");
        let result = resolve_class_to_file("Illuminate\\Auth\\Middleware\\Authenticate", &root);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("vendor/laravel/framework"));
    }
}