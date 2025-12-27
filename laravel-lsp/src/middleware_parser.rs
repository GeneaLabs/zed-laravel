//! Middleware and class resolution utilities
//!
//! This module provides utilities for resolving PHP class names to file paths
//! using PSR-4 autoloading conventions.

use std::path::{Path, PathBuf};

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
    fn test_resolve_class_to_file() {
        let root = PathBuf::from("/project");

        // Test App namespace
        let result = resolve_class_to_file("App\\Http\\Middleware\\Authenticate", &root);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.ends_with("Authenticate.php"));
        assert!(path.to_string_lossy().contains("app/Http/Middleware"));
    }
}
