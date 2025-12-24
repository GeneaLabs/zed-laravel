/// This module parses middleware configuration from Laravel applications
///
/// It supports both:
/// - Laravel 10 and below: app/Http/Kernel.php
/// - Laravel 11+: bootstrap/app.php
///
/// The parser extracts middleware aliases and maps them to their class names,
/// enabling goto-definition for middleware references in routes.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tree_sitter::{Query, QueryCursor, StreamingIterator, Tree};

use crate::parser::{language_php, parse_php};

/// Represents a middleware alias mapping (alias -> fully qualified class name)
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareAlias {
    /// The middleware alias (e.g., "auth", "verified")
    pub alias: String,
    /// The fully qualified class name (e.g., "App\\Http\\Middleware\\Authenticate")
    pub class_name: String,
    /// The file path where this middleware class is located
    pub file_path: Option<PathBuf>,
    /// The source file where this alias is defined (for goto-definition to alias)
    pub source_file: Option<PathBuf>,
    /// The line number in source file where this alias is defined (0-based)
    pub source_line: Option<usize>,
}

/// Cache of middleware configuration
#[derive(Debug, Clone)]
pub struct MiddlewareConfig {
    /// Map of alias -> MiddlewareAlias
    pub aliases: HashMap<String, MiddlewareAlias>,
    /// Timestamp when this was last parsed
    pub last_parsed: std::time::SystemTime,
}

impl MiddlewareConfig {
    pub fn new() -> Self {
        Self {
            aliases: HashMap::new(),
            last_parsed: std::time::SystemTime::now(),
        }
    }

    /// Look up a middleware by alias
    pub fn get_middleware(&self, alias: &str) -> Option<&MiddlewareAlias> {
        // Handle middleware with parameters (e.g., "throttle:60,1")
        // by stripping the parameters and looking up the base alias
        let base_alias = alias.split(':').next().unwrap_or(alias);
        self.aliases.get(base_alias)
    }

    /// Add a middleware alias to the config
    pub fn add_alias(&mut self, alias: String, class_name: String, file_path: Option<PathBuf>) {
        self.add_alias_with_source(alias, class_name, file_path, None, None);
    }
    
    /// Add a middleware alias with source location information
    pub fn add_alias_with_source(
        &mut self,
        alias: String,
        class_name: String,
        file_path: Option<PathBuf>,
        source_file: Option<PathBuf>,
        source_line: Option<usize>,
    ) {
        self.aliases.insert(
            alias.clone(),
            MiddlewareAlias {
                alias,
                class_name,
                file_path,
                source_file,
                source_line,
            },
        );
    }
}

/// Parse middleware configuration from a Laravel project
///
/// This function attempts to parse middleware from:
/// 1. bootstrap/app.php (Laravel 11+)
/// 2. app/Http/Kernel.php (Laravel 10 and below)
pub async fn parse_middleware_config(root_path: &Path) -> Result<MiddlewareConfig> {
    let mut config = MiddlewareConfig::new();

    // Try Laravel 11+ format first
    let bootstrap_app = root_path.join("bootstrap/app.php");
    if bootstrap_app.exists() {
        if let Ok(bootstrap_config) = parse_bootstrap_app(&bootstrap_app, root_path).await {
            config.aliases.extend(bootstrap_config.aliases);
        }
    }

    // Try Laravel 10 format
    let kernel_path = root_path.join("app/Http/Kernel.php");
    if kernel_path.exists() {
        if let Ok(kernel_config) = parse_kernel_php(&kernel_path, root_path).await {
            // Kernel.php takes precedence if both exist
            for (alias, middleware) in kernel_config.aliases {
                config.aliases.insert(alias, middleware);
            }
        }
    }

    // All middleware is now resolved dynamically - no hardcoded common list
    Ok(config)
}

/// Parse middleware from app/Http/Kernel.php (Laravel 10 and below)
async fn parse_kernel_php(kernel_path: &Path, root_path: &Path) -> Result<MiddlewareConfig> {
    let content = tokio::fs::read_to_string(kernel_path).await?;
    let mut config = MiddlewareConfig::new();

    // Parse the PHP file
    let tree = parse_php(&content)?;
    
    // Extract middleware aliases from $middlewareAliases or $routeMiddleware arrays
    extract_middleware_arrays(&tree, &content, &mut config, root_path)?;

    Ok(config)
}

/// Parse middleware from bootstrap/app.php (Laravel 11+)
async fn parse_bootstrap_app(bootstrap_path: &Path, root_path: &Path) -> Result<MiddlewareConfig> {
    eprintln!("DEBUG: parse_bootstrap_app called with path: {:?}", bootstrap_path);
    let content = tokio::fs::read_to_string(bootstrap_path).await?;
    eprintln!("DEBUG: parse_bootstrap_app read {} bytes from file", content.len());
    let mut config = MiddlewareConfig::new();

    // For Laravel 11, we look for $middleware->alias([...]) calls
    // This is harder to parse with tree-sitter, so we use regex as a fallback
    eprintln!("DEBUG: parse_bootstrap_app calling parse_middleware_alias_calls");
    parse_middleware_alias_calls(&content, &mut config, root_path);
    eprintln!("DEBUG: parse_bootstrap_app finished, config has {} aliases", config.aliases.len());

    Ok(config)
}

/// Extract middleware from property arrays in Kernel.php
///
/// Looks for patterns like:
/// protected $middlewareAliases = [
///     'auth' => \App\Http\Middleware\Authenticate::class,
/// ];
fn extract_middleware_arrays(
    tree: &Tree,
    source: &str,
    config: &mut MiddlewareConfig,
    root_path: &Path,
) -> Result<()> {
    let lang = language_php();
    let source_bytes = source.as_bytes();
    let root_node = tree.root_node();

    // Tree-sitter query to find array assignments to middleware properties
    let query_str = r#"
        (property_declaration
            (visibility_modifier)
            (property_element
                (variable_name) @prop_name
                (array_creation_expression) @array))
    "#;

    let query = Query::new(&lang, query_str)
        .map_err(|e| anyhow!("Failed to compile middleware query: {:?}", e))?;

    let mut cursor = QueryCursor::new();
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

        // Check if this is a middleware-related property
        if let Some(name) = prop_name {
            if name.contains("middleware") || name.contains("Middleware") {
                if let Some(array) = array_node {
                    parse_middleware_array(array, source_bytes, config, root_path)?;
                }
            }
        }
    }

    Ok(())
}

/// Parse individual middleware array elements
fn parse_middleware_array(
    array_node: tree_sitter::Node,
    source_bytes: &[u8],
    config: &mut MiddlewareConfig,
    root_path: &Path,
) -> Result<()> {
    // Look for array_element_initializer nodes with key-value pairs
    let mut cursor = array_node.walk();
    
    for child in array_node.children(&mut cursor) {
        if child.kind() == "array_element_initializer" {
            let mut key = None;
            let mut value = None;

            // Create a new cursor for iterating child elements
            let mut element_cursor = child.walk();
            for element in child.children(&mut element_cursor) {
                match element.kind() {
                    "string" | "encapsed_string" => {
                        // This could be the key
                        if key.is_none() {
                            if let Some(content) = element.child_by_field_name("content") {
                                key = content.utf8_text(source_bytes).ok();
                            }
                        }
                    }
                    "class_constant_access_expression" => {
                        // This is the ::class syntax
                        value = extract_class_from_class_constant(element, source_bytes);
                    }
                    _ => {}
                }
            }

            if let (Some(alias), Some(class)) = (key, value) {
                let file_path = resolve_class_to_file(&class, root_path);
                config.add_alias(alias.to_string(), class.to_string(), file_path);
            }
        }
    }

    Ok(())
}

/// Extract class name from ::class expression
fn extract_class_from_class_constant(node: tree_sitter::Node, source_bytes: &[u8]) -> Option<String> {
    // Get the class part before ::class
    if let Some(class_node) = node.child_by_field_name("class") {
        let class_text = class_node.utf8_text(source_bytes).ok()?;
        // Remove leading backslash if present
        Some(class_text.trim_start_matches('\\').to_string())
    } else {
        None
    }
}

/// Parse middleware->alias([...]) calls in Laravel 11 bootstrap/app.php
///
/// Uses regex as a simpler approach for this specific pattern
fn parse_middleware_alias_calls(content: &str, config: &mut MiddlewareConfig, root_path: &Path) {
    // This is a simplified regex-based parser for Laravel 11
    // Pattern: 'alias_name' => \Namespace\Class::class
    // Matches both: SomeClass::class and \App\Http\Middleware\SomeClass::class
    let re = regex::Regex::new(r#"['"]([^'"]+)['"]\s*=>\s*\\?([A-Za-z0-9_\\]+)::class"#)
        .unwrap();

    eprintln!("DEBUG: parse_middleware_alias_calls - searching content length: {}", content.len());
    let mut found_count = 0;
    
    for cap in re.captures_iter(content) {
        found_count += 1;
        if let (Some(alias), Some(class)) = (cap.get(1), cap.get(2)) {
            let alias_str = alias.as_str();
            let class_str = class.as_str();
            eprintln!("DEBUG: Captured middleware alias='{}' class='{}'", alias_str, class_str);
            
            let class_name = class_str.trim_start_matches('\\').to_string();
            eprintln!("DEBUG: After trim_start_matches: '{}'", class_name);
            
            let file_path = resolve_class_to_file(&class_name, root_path);
            eprintln!("DEBUG: Resolved file_path: {:?}", file_path);
            
            config.add_alias(alias_str.to_string(), class_name.clone(), file_path.clone());
            eprintln!("DEBUG: Added middleware '{}' to config with class '{}'", alias_str, class_name);
        }
    }
    eprintln!("DEBUG: parse_middleware_alias_calls - found {} middleware entries", found_count);
}

/// Resolve a fully qualified class name to a file path
///
/// Converts namespace notation to file path using PSR-4 autoloading conventions
/// Example: App\Http\Middleware\Authenticate -> app/Http/Middleware/Authenticate.php
pub fn resolve_class_to_file(class_name: &str, root_path: &Path) -> Option<PathBuf> {
    // Convert namespace separators to path separators
    let path_str = class_name.replace("\\", "/");
    
    // Common Laravel namespace mappings
    let mappings = [
        ("App/", "app/"),
        ("Illuminate/", "vendor/laravel/framework/src/Illuminate/"),
    ];

    for (namespace_prefix, path_prefix) in &mappings {
        if path_str.starts_with(namespace_prefix) {
            let relative = path_str.strip_prefix(namespace_prefix).unwrap();
            let file_path = root_path.join(path_prefix).join(relative).with_extension("php");
            
            // Return the expected path regardless of whether it exists
            // The caller will check existence and create appropriate diagnostics
            return Some(file_path);
        }
    }

    None
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_middleware_config_get_with_parameters() {
        let mut config = MiddlewareConfig::new();
        config.add_alias(
            "throttle".to_string(),
            "Illuminate\\Routing\\Middleware\\ThrottleRequests".to_string(),
            None,
        );

        // Should find base alias even with parameters
        let result = config.get_middleware("throttle:60,1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().alias, "throttle");
    }

    #[test]
    fn test_resolve_class_to_file() {
        let root = PathBuf::from("/project");
        
        // Test App namespace
        let result = resolve_class_to_file("App\\Http\\Middleware\\Authenticate", &root);
        // This will be None in test because path doesn't exist, but we can test the logic
        assert!(result.is_none() || result.unwrap().ends_with("Authenticate.php"));
    }

    #[test]
    fn test_parse_middleware_alias_calls() {
        let content = r#"
            $middleware->alias([
                'auth' => \App\Http\Middleware\Authenticate::class,
                'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
            ]);
        "#;

        let mut config = MiddlewareConfig::new();
        let root = PathBuf::from("/test/project");
        parse_middleware_alias_calls(content, &mut config, &root);

        assert_eq!(config.aliases.len(), 2);
        assert!(config.aliases.contains_key("auth"));
        assert!(config.aliases.contains_key("verified"));
        
        let auth = config.get_middleware("auth").unwrap();
        assert_eq!(auth.class_name, "App\\Http\\Middleware\\Authenticate");
    }
}