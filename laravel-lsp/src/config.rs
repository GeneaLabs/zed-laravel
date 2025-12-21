/// Laravel project configuration parser
///
/// This module discovers Laravel project configuration by reading:
/// - composer.json (to detect Livewire and other packages)
/// - config/view.php (for view paths and namespaces)
/// - config/livewire.php (for Livewire component paths)
///
/// This allows the LSP to work with any Laravel project structure,
/// not just the default conventions.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Laravel project configuration
///
/// This structure holds all discovered paths and settings
/// from the Laravel project configuration files.
#[derive(Debug, Clone)]
pub struct LaravelConfig {
    /// The project root directory
    pub root: PathBuf,

    /// View paths (default: ["resources/views"])
    /// Can have multiple paths, checked in order
    pub view_paths: Vec<PathBuf>,

    /// Component namespace paths (key: namespace, value: path)
    /// Default: { "": "resources/views/components" }
    pub component_paths: HashMap<String, PathBuf>,

    /// Livewire component path (if Livewire is installed)
    /// Default: "app/Livewire" or "app/Http/Livewire"
    pub livewire_path: Option<PathBuf>,

    /// Whether Livewire is installed
    pub has_livewire: bool,
}

impl LaravelConfig {
    /// Find the Laravel project root by walking up from a file path
    ///
    /// Looks for Laravel-specific markers:
    /// - composer.json
    /// - artisan file
    /// - app/ and resources/ directories together
    ///
    /// Returns None if no Laravel project root is found.
    pub fn find_project_root(file_path: &Path) -> Option<PathBuf> {
        let mut current = file_path;

        // If it's a file, start from its parent directory
        if current.is_file() {
            current = current.parent()?;
        }

        // Walk up the directory tree
        loop {
            // Check for Laravel markers
            let has_composer = current.join("composer.json").exists();
            let has_artisan = current.join("artisan").exists();
            let has_app = current.join("app").is_dir();
            let has_resources = current.join("resources").is_dir();

            // If we find composer.json + artisan, it's very likely a Laravel project
            if has_composer && has_artisan {
                info!("Found Laravel project root at {:?} (composer.json + artisan)", current);
                return Some(current.to_path_buf());
            }

            // Or if we find composer.json + app/ + resources/
            if has_composer && has_app && has_resources {
                info!("Found Laravel project root at {:?} (composer.json + app + resources)", current);
                return Some(current.to_path_buf());
            }

            // Move up one directory
            current = current.parent()?;
        }
    }

    /// Discover Laravel configuration from a project root
    ///
    /// This is the main entry point - it reads all config files
    /// and builds a complete picture of the project structure.
    pub fn discover(root: &Path) -> Result<Self> {
        info!("Discovering Laravel configuration at {:?}", root);

        let mut config = Self {
            root: root.to_path_buf(),
            view_paths: Vec::new(),
            component_paths: HashMap::new(),
            livewire_path: None,
            has_livewire: false,
        };

        // Step 1: Detect Livewire from composer.json
        config.detect_livewire()?;

        // Step 2: Parse config/view.php for view paths
        config.parse_view_config()?;

        // Step 3: Parse Livewire config if installed
        if config.has_livewire {
            config.parse_livewire_config()?;
        }

        // Step 4: Apply defaults if nothing was found
        config.apply_defaults();

        info!("Laravel config discovered:");
        info!("  View paths: {:?}", config.view_paths);
        info!("  Component paths: {:?}", config.component_paths);
        info!("  Livewire path: {:?}", config.livewire_path);

        Ok(config)
    }

    /// Detect if Livewire is installed by checking composer.json
    fn detect_livewire(&mut self) -> Result<()> {
        let composer_path = self.root.join("composer.json");

        if !composer_path.exists() {
            debug!("composer.json not found, assuming no Livewire");
            return Ok(());
        }

        let content = fs::read_to_string(&composer_path)?;
        let composer: ComposerJson = serde_json::from_str(&content)?;

        // Check if Livewire is in require or require-dev
        self.has_livewire = composer.require.contains_key("livewire/livewire")
            || composer.require_dev.as_ref()
                .map(|dev| dev.contains_key("livewire/livewire"))
                .unwrap_or(false);

        if self.has_livewire {
            info!("Livewire detected in composer.json");
        } else {
            debug!("Livewire not found in composer.json");
        }

        Ok(())
    }

    /// Parse config/view.php to discover view paths
    ///
    /// Laravel's view config can contain:
    /// - 'paths' => array of view directories
    /// - 'compiled' => where compiled views go (we don't need this)
    fn parse_view_config(&mut self) -> Result<()> {
        let config_path = self.root.join("config/view.php");

        if !config_path.exists() {
            debug!("config/view.php not found, will use defaults");
            return Ok(());
        }

        let content = fs::read_to_string(&config_path)?;

        // Extract view paths using simple parsing
        // Look for 'paths' => [...] in the config array
        if let Some(paths) = self.extract_paths_from_config(&content) {
            self.view_paths = paths;
            info!("Discovered {} view paths from config/view.php", self.view_paths.len());
        } else {
            debug!("Could not parse view paths from config/view.php");
        }

        Ok(())
    }

    /// Parse Livewire configuration
    ///
    /// Livewire can be configured in:
    /// - config/livewire.php (Livewire v3)
    /// - Published config file
    fn parse_livewire_config(&mut self) -> Result<()> {
        let config_path = self.root.join("config/livewire.php");

        if !config_path.exists() {
            debug!("config/livewire.php not found, using default path");
            return Ok(());
        }

        let content = fs::read_to_string(&config_path)?;

        // Look for 'class_namespace' or 'view_path' in Livewire config
        // For now, we'll use simple string matching
        // In a more sophisticated version, we'd use tree-sitter-php

        // Try to extract the namespace path
        if let Some(path) = self.extract_livewire_path(&content) {
            self.livewire_path = Some(path);
            info!("Discovered Livewire path from config: {:?}", self.livewire_path);
        }

        Ok(())
    }

    /// Apply default Laravel conventions if no config was found
    fn apply_defaults(&mut self) {
        // Default view paths
        if self.view_paths.is_empty() {
            self.view_paths.push(PathBuf::from("resources/views"));
            debug!("Using default view path: resources/views");
        }

        // Default component paths (inside the first view path)
        if self.component_paths.is_empty() {
            if let Some(first_view_path) = self.view_paths.first() {
                self.component_paths.insert(
                    String::new(), // Default namespace
                    first_view_path.join("components"),
                );
                debug!("Using default component path: {:?}/components", first_view_path);
            }
        }

        // Default Livewire paths (try both old and new conventions)
        if self.has_livewire && self.livewire_path.is_none() {
            // Try Livewire v3 location first
            let v3_path = PathBuf::from("app/Livewire");
            if self.root.join(&v3_path).exists() {
                self.livewire_path = Some(v3_path);
                debug!("Using Livewire v3 path: app/Livewire");
            } else {
                // Fall back to Livewire v2 location
                let v2_path = PathBuf::from("app/Http/Livewire");
                if self.root.join(&v2_path).exists() {
                    self.livewire_path = Some(v2_path);
                    debug!("Using Livewire v2 path: app/Http/Livewire");
                } else {
                    // Default to v3 convention even if directory doesn't exist yet
                    self.livewire_path = Some(v3_path);
                    debug!("Using default Livewire path: app/Livewire (may not exist yet)");
                }
            }
        }
    }

    /// Extract view paths from config/view.php content
    ///
    /// This uses simple regex-like parsing to find the 'paths' array.
    /// For a more robust solution, we could use tree-sitter-php.
    fn extract_paths_from_config(&self, content: &str) -> Option<Vec<PathBuf>> {
        // Look for lines like: resource_path('views'), base_path('templates'), etc.
        let mut paths = Vec::new();

        // Common Laravel helper patterns:
        // - resource_path('views')
        // - base_path('resources/views')
        // - '/absolute/path/to/views'

        // Simple approach: look for resource_path('views') pattern
        if content.contains("resource_path('views')") || content.contains("resource_path(\"views\")") {
            paths.push(PathBuf::from("resources/views"));
        }

        // Look for other common patterns
        for line in content.lines() {
            // Match: base_path('some/path')
            if let Some(path) = extract_base_path(line) {
                paths.push(PathBuf::from(path));
            }

            // Match: '/absolute/path' (quoted strings)
            if let Some(path) = extract_quoted_path(line) {
                if path.starts_with('/') {
                    // This is an absolute path - we can't use it
                    // In a real implementation, we'd need to handle this differently
                    warn!("Absolute path found in config, skipping: {}", path);
                } else {
                    paths.push(PathBuf::from(path));
                }
            }
        }

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }

    /// Extract Livewire component path from config
    fn extract_livewire_path(&self, content: &str) -> Option<PathBuf> {
        // Look for 'class_namespace' => 'App\\Livewire'
        // This would map to app/Livewire

        // Simple pattern matching
        if content.contains("App\\\\Livewire") || content.contains("App\\Livewire") {
            return Some(PathBuf::from("app/Livewire"));
        }

        if content.contains("App\\\\Http\\\\Livewire") || content.contains("App\\Http\\Livewire") {
            return Some(PathBuf::from("app/Http/Livewire"));
        }

        None
    }

    /// Resolve a view name to possible file paths
    ///
    /// Returns all possible paths where this view could exist,
    /// in order of priority.
    pub fn resolve_view_path(&self, view_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Handle package views (e.g., "package::view.name")
        let (namespace, actual_view) = if let Some(pos) = view_name.find("::") {
            let namespace = &view_name[..pos];
            let view = &view_name[pos + 2..];
            (Some(namespace), view)
        } else {
            (None, view_name)
        };

        // Convert dots to path separators
        let view_path = actual_view.replace('.', "/");

        // Check each view path
        for base_path in &self.view_paths {
            let mut full_path = self.root.join(base_path).join(&view_path);
            full_path.set_extension("blade.php");
            paths.push(full_path);
        }

        // TODO: Handle namespaced views (would require package discovery)
        if let Some(_ns) = namespace {
            warn!("Namespaced views not fully supported yet: {}", view_name);
        }

        paths
    }

    /// Resolve a component name to file path
    pub fn resolve_component_path(&self, component_name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Component name uses dots: "forms.input" -> "forms/input.blade.php"
        let component_path = component_name.replace('.', "/");

        // Check each component path
        for (_namespace, base_path) in &self.component_paths {
            let mut full_path = self.root.join(base_path).join(&component_path);
            full_path.set_extension("blade.php");
            paths.push(full_path);
        }

        // If no component paths found, use default within view paths
        if paths.is_empty() {
            for view_path in &self.view_paths {
                let mut full_path = self.root.join(view_path).join("components").join(&component_path);
                full_path.set_extension("blade.php");
                paths.push(full_path);
            }
        }

        paths
    }

    /// Resolve a Livewire component name to file path
    pub fn resolve_livewire_path(&self, component_name: &str) -> Option<PathBuf> {
        let livewire_base = self.livewire_path.as_ref()?;

        // Convert component name to PascalCase path
        // "user-profile" -> "UserProfile.php"
        // "admin.dashboard" -> "Admin/Dashboard.php"

        let parts: Vec<&str> = component_name.split('.').collect();
        let mut path = self.root.join(livewire_base);

        for (i, part) in parts.iter().enumerate() {
            let pascal_case = kebab_to_pascal_case(part);

            if i == parts.len() - 1 {
                // Last part becomes the PHP file
                path.push(format!("{}.php", pascal_case));
            } else {
                // Other parts are directories
                path.push(pascal_case);
            }
        }

        Some(path)
    }
}

/// Convert kebab-case to PascalCase
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

/// Extract base_path(...) calls from a line
fn extract_base_path(line: &str) -> Option<&str> {
    // Match: base_path('some/path') or base_path("some/path")
    if let Some(start) = line.find("base_path(") {
        let after = &line[start + 10..];
        if let Some(quote_start) = after.find(|c| c == '\'' || c == '"') {
            let quote_char = after.chars().nth(quote_start)?;
            let after_quote = &after[quote_start + 1..];
            if let Some(quote_end) = after_quote.find(quote_char) {
                return Some(&after_quote[..quote_end]);
            }
        }
    }
    None
}

/// Extract quoted paths from a line (for absolute paths)
fn extract_quoted_path(line: &str) -> Option<&str> {
    // This is a simple implementation
    // A real version would need more sophisticated parsing
    None
}

// ============================================================================
// Composer.json structure
// ============================================================================

#[derive(Debug, Deserialize)]
struct ComposerJson {
    #[serde(default)]
    require: HashMap<String, String>,

    #[serde(default, rename = "require-dev")]
    require_dev: Option<HashMap<String, String>>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
}
