use std::path::PathBuf;
use zed_extension_api::{self as zed, LanguageServerId, Result};

/// The main struct for our Laravel extension
/// We'll expand this to manage LSP state in Phase 5
struct LaravelExtension {
    /// Whether we've downloaded/installed the language server
    cached_binary_path: Option<String>,
}

/// The Extension trait is what Zed requires us to implement
impl zed::Extension for LaravelExtension {
    /// Creates a new instance of our extension
    fn new() -> Self {
        LaravelExtension {
            cached_binary_path: None,
        }
    }

    /// This method tells Zed what language server to use
    /// Returns the command to start our Laravel Language Server
    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        // For development, use the built binary from our project
        // In production, this would download from GitHub releases
        let binary_path = self.language_server_binary_path(language_server_id, worktree)?;

        Ok(zed::Command {
            command: binary_path,
            args: vec![],
            env: worktree.shell_env(),
        })
    }

    /// Phase 5: Configure LSP initialization options
    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        // Future: Pass Laravel project configuration to the LSP
        Ok(None)
    }
}

impl LaravelExtension {
    /// Get or install the language server binary
    ///
    /// This follows the standard Zed extension pattern with development support:
    /// 1. Check cached path
    /// 2. Check absolute development path (hardcoded for local development)
    /// 3. Try to find in system PATH using worktree.which()
    /// 4. Check extension's working directory
    /// 5. Check relative paths for local development
    /// 6. In production, would download from GitHub releases
    fn language_server_binary_path(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        // Step 1: Check if we have a cached path
        if let Some(path) = &self.cached_binary_path {
            return Ok(path.clone());
        }

        // Step 2: Check absolute development path (for local development)
        // This allows development without environment variables or PATH setup
        let dev_absolute_path = "/Users/mike/Developer/zed-laravel/laravel-lsp/target/release/laravel-lsp";
        if std::path::Path::new(dev_absolute_path).exists() {
            self.cached_binary_path = Some(dev_absolute_path.to_string());
            return Ok(dev_absolute_path.to_string());
        }

        // Step 3: Try to find "laravel-lsp" in the system PATH
        // This is the standard Zed pattern for production
        // worktree.which() returns Option<String>:
        //   - Some(path) if the binary is found in PATH
        //   - None if not found
        if let Some(path) = worktree.which("laravel-lsp") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Step 4: Check the extension's working directory for development binary
        // For development, we copy the binary as "laravel-lsp-binary" to avoid
        // conflict with the "laravel-lsp" directory name
        let dev_binary_name = "laravel-lsp-binary";
        let path = std::path::Path::new(dev_binary_name);
        if path.exists() && path.is_file() {
            self.cached_binary_path = Some(dev_binary_name.to_string());
            return Ok(dev_binary_name.to_string());
        }

        // Also check for the standard binary name (for production/download)
        let binary_name = if cfg!(target_os = "windows") {
            "laravel-lsp.exe"
        } else {
            "laravel-lsp"
        };

        let path = std::path::Path::new(binary_name);
        if path.exists() && path.is_file() {
            self.cached_binary_path = Some(binary_name.to_string());
            return Ok(binary_name.to_string());
        }

        // Step 5: Development fallback - check relative paths
        // When developing locally, the binary might be in the project structure
        let dev_binary_paths = vec![
            // Check for a copied binary at the root
            "./laravel-lsp-binary",
            "laravel-lsp-binary",
            // Check in the build output
            "./laravel-lsp/target/release/laravel-lsp",
            "../laravel-lsp/target/release/laravel-lsp",
            "laravel-lsp/target/release/laravel-lsp",
            // Windows variants
            "./laravel-lsp/target/release/laravel-lsp.exe",
            "../laravel-lsp/target/release/laravel-lsp.exe",
            "laravel-lsp/target/release/laravel-lsp.exe",
        ];

        for path_str in dev_binary_paths {
            let path = std::path::Path::new(path_str);
            if path.exists() && path.is_file() {
                self.cached_binary_path = Some(path_str.to_string());
                return Ok(path_str.to_string());
            }
        }

        // Step 6: Not found - provide helpful error message
        Err(format!(
            "Laravel LSP binary not found.\n\
             \n\
             Make sure you've built it first:\n\
                cd laravel-lsp && cargo build --release\n\
             \n\
             The extension looks in:\n\
             1. /Users/mike/Developer/zed-laravel/laravel-lsp/target/release/laravel-lsp (dev path)\n\
             2. System PATH for 'laravel-lsp'\n\
             3. Extension directory\n\
             4. Relative paths from extension location"
        ).into())
    }
}

// ============================================================================
// PHASE 2: File System Navigation - View Path Resolution
// These functions will be moved into our LSP in Phase 5
// ============================================================================

/// Converts a Laravel view name to a file path
/// 
/// # Examples
/// - `"welcome"` -> `"resources/views/welcome.blade.php"`
/// - `"users.profile"` -> `"resources/views/users/profile.blade.php"`
/// - `"admin.dashboard.index"` -> `"resources/views/admin/dashboard/index.blade.php"`
/// 
/// Phase 5: This will be handled by the LSP's textDocument/definition handler
pub fn view_name_to_path(view_name: &str) -> PathBuf {
    // Start with the base views directory
    let mut path = PathBuf::from("resources/views");
    
    // Handle package views (e.g., "package::view.name")
    let actual_view = if let Some(pos) = view_name.find("::") {
        // Package view - would need special handling
        &view_name[pos + 2..]
    } else {
        view_name
    };
    
    // Split the view name by dots and convert to path segments
    for segment in actual_view.split('.') {
        path.push(segment);
    }
    
    // Add the Blade extension
    path.set_extension("blade.php");
    
    path
}

/// Parses a PHP file looking for view() calls
/// Returns a list of (view_name, line_number, column) tuples
/// 
/// Phase 5: The LSP will use this to respond to textDocument/definition requests
pub fn find_view_references(php_content: &str) -> Vec<(String, usize, usize)> {
    let mut references = Vec::new();
    
    for (line_num, line) in php_content.lines().enumerate() {
        // Look for view() calls
        if let Some(start_pos) = line.find("view(") {
            let after_view = &line[start_pos + 5..];
            
            if let Some(quote_start) = after_view.find(|c| c == '\'' || c == '"') {
                let quote_char = after_view.chars().nth(quote_start).unwrap();
                let after_quote = &after_view[quote_start + 1..];
                
                if let Some(quote_end) = after_quote.find(quote_char) {
                    let view_name = &after_quote[..quote_end];
                    let column = start_pos + 5 + quote_start + 1; // Position of view name
                    references.push((view_name.to_string(), line_num, column));
                }
            }
        }
        
        // Also look for View::make() calls
        if let Some(start_pos) = line.find("View::make(") {
            let after_view = &line[start_pos + 11..];
            
            if let Some(quote_start) = after_view.find(|c| c == '\'' || c == '"') {
                let quote_char = after_view.chars().nth(quote_start).unwrap();
                let after_quote = &after_view[quote_start + 1..];
                
                if let Some(quote_end) = after_quote.find(quote_char) {
                    let view_name = &after_quote[..quote_end];
                    let column = start_pos + 11 + quote_start + 1;
                    references.push((view_name.to_string(), line_num, column));
                }
            }
        }
    }
    
    references
}

/// Finds Blade component references in Blade templates
/// Returns component names and their positions
/// 
/// Phase 4: Will use tree-sitter for accurate parsing
/// Phase 5: LSP will handle this for textDocument/definition
pub fn find_blade_components(blade_content: &str) -> Vec<(String, usize, usize)> {
    let mut components = Vec::new();
    
    for (line_num, line) in blade_content.lines().enumerate() {
        // Look for <x-component> tags
        let mut search_from = 0;
        while let Some(pos) = line[search_from..].find("<x-") {
            let actual_pos = search_from + pos;
            let after_tag = &line[actual_pos + 3..];
            
            // Find the end of the component name
            if let Some(end) = after_tag.find(|c: char| c.is_whitespace() || c == '>' || c == '/') {
                let component_name = &after_tag[..end];
                components.push((component_name.to_string(), line_num, actual_pos));
            }
            
            search_from = actual_pos + 3;
        }
    }
    
    components
}

/// Converts a Blade component name to its file path
/// 
/// # Examples
/// - `"button"` -> `"resources/views/components/button.blade.php"`
/// - `"forms.input"` -> `"resources/views/components/forms/input.blade.php"`
pub fn component_name_to_path(component_name: &str) -> PathBuf {
    let mut path = PathBuf::from("resources/views/components");
    
    // Convert dots to directory separators (same as views)
    for segment in component_name.split('.') {
        // Convert kebab-case to path segments
        path.push(segment);
    }
    
    path.set_extension("blade.php");
    path
}

/// Finds Livewire component references
/// Returns component names and positions
pub fn find_livewire_components(content: &str) -> Vec<(String, usize, usize)> {
    let mut components = Vec::new();
    
    for (line_num, line) in content.lines().enumerate() {
        // Look for <livewire:component-name> tags
        if let Some(pos) = line.find("<livewire:") {
            let after_tag = &line[pos + 10..];
            if let Some(end) = after_tag.find(|c: char| c.is_whitespace() || c == '>' || c == '/') {
                let component_name = &after_tag[..end];
                components.push((component_name.to_string(), line_num, pos));
            }
        }
        
        // Look for @livewire('component-name') directives
        if let Some(pos) = line.find("@livewire(") {
            let after_directive = &line[pos + 10..];
            if let Some(quote_start) = after_directive.find(|c| c == '\'' || c == '"') {
                let quote_char = after_directive.chars().nth(quote_start).unwrap();
                let after_quote = &after_directive[quote_start + 1..];
                
                if let Some(quote_end) = after_quote.find(quote_char) {
                    let component_name = &after_quote[..quote_end];
                    components.push((component_name.to_string(), line_num, pos));
                }
            }
        }
    }
    
    components
}

/// Converts a Livewire component name to its PHP class path
/// 
/// # Examples
/// - `"user-profile"` -> `"app/Livewire/UserProfile.php"`
/// - `"admin.dashboard"` -> `"app/Livewire/Admin/Dashboard.php"`
pub fn livewire_component_to_path(component_name: &str) -> PathBuf {
    let mut path = PathBuf::from("app/Livewire");
    
    // Split by dots for nested components
    let parts: Vec<&str> = component_name.split('.').collect();
    
    for (i, part) in parts.iter().enumerate() {
        // Convert kebab-case to PascalCase
        let pascal_case = kebab_to_pascal_case(part);
        
        if i == parts.len() - 1 {
            // Last part becomes the PHP file
            path.push(format!("{}.php", pascal_case));
        } else {
            // Other parts are directories
            path.push(pascal_case);
        }
    }
    
    path
}

/// Converts kebab-case to PascalCase
/// "user-profile" -> "UserProfile"
fn kebab_to_pascal_case(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

// ============================================================================
// Future LSP Implementation Notes
// ============================================================================
// 
// Phase 5 will implement a Language Server Protocol server that:
// 
// 1. Handles textDocument/definition requests:
//    - Parse the document to find what's under the cursor
//    - Determine if it's a view(), route(), config(), component, etc.
//    - Return the file location to navigate to
// 
// 2. Handles textDocument/hover requests:
//    - Show information about the item under cursor
//    - Display the resolved file path
// 
// 3. Handles textDocument/completion requests:
//    - Suggest view names based on existing files
//    - Suggest route names from routes files
//    - Suggest config keys from config files
// 
// The LSP will run as a separate process and communicate via JSON-RPC

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_view_name() {
        let path = view_name_to_path("welcome");
        assert_eq!(path.to_str().unwrap(), "resources/views/welcome.blade.php");
    }

    #[test]
    fn test_nested_view_name() {
        let path = view_name_to_path("users.profile");
        assert!(path.to_str().unwrap().contains("profile.blade.php"));
    }

    #[test]
    fn test_package_view_name() {
        let path = view_name_to_path("admin::dashboard");
        // Should strip the package prefix
        assert!(path.to_str().unwrap().contains("dashboard.blade.php"));
    }

    #[test]
    fn test_find_view_references() {
        let php_code = r#"
            return view('home.index');
            return view("about");
        "#;
        
        let refs = find_view_references(php_code);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "home.index");
        assert_eq!(refs[1].0, "about");
    }

    #[test]
    fn test_blade_component_parsing() {
        let blade = r#"
            <x-button type="primary">Click</x-button>
            <x-forms.input name="email" />
        "#;
        
        let components = find_blade_components(blade);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].0, "button");
        assert_eq!(components[1].0, "forms.input");
    }

    #[test]
    fn test_component_path_resolution() {
        let path = component_name_to_path("forms.input");
        assert_eq!(
            path.to_str().unwrap(),
            "resources/views/components/forms/input.blade.php"
        );
    }

    #[test]
    fn test_livewire_component_parsing() {
        let blade = r#"
            <livewire:user-profile />
            @livewire('search-users')
        "#;
        
        let components = find_livewire_components(blade);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].0, "user-profile");
        assert_eq!(components[1].0, "search-users");
    }

    #[test]
    fn test_livewire_path_resolution() {
        let path = livewire_component_to_path("user-profile");
        assert_eq!(path.to_str().unwrap(), "app/Livewire/UserProfile.php");
        
        let path = livewire_component_to_path("admin.user-settings");
        assert_eq!(path.to_str().unwrap(), "app/Livewire/Admin/UserSettings.php");
    }

    #[test]
    fn test_kebab_to_pascal_case() {
        assert_eq!(kebab_to_pascal_case("user-profile"), "UserProfile");
        assert_eq!(kebab_to_pascal_case("search-users"), "SearchUsers");
        assert_eq!(kebab_to_pascal_case("simple"), "Simple");
    }
}

// This macro registers our extension with Zed
zed::register_extension!(LaravelExtension);