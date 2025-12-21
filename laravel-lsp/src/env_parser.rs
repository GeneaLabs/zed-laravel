/// Environment variable parser for Laravel .env files
///
/// This module handles parsing of .env files and tracking environment variables
/// across .env, .env.example, and .env.local files.
///
/// Laravel loads env files in this priority order:
/// 1. .env (highest priority - actual values)
/// 2. .env.local (local overrides)
/// 3. .env.example (template/documentation)

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, info};

/// Represents a single environment variable definition
#[derive(Debug, Clone)]
pub struct EnvVariable {
    /// The variable name (e.g., "APP_NAME")
    pub name: String,
    
    /// The value (e.g., "Laravel")
    /// Can be empty string if defined as VAR= with no value
    pub value: String,
    
    /// Which file this was defined in
    pub file_path: PathBuf,
    
    /// Line number where defined (0-based for internal use)
    pub line: usize,
    
    /// Column where the variable name starts (0-based)
    pub column: usize,
    
    /// Column where the value starts (after the =)
    pub value_column: usize,
    
    /// Whether this variable is commented out
    pub is_commented: bool,
}

/// Parsed environment files with caching metadata
#[derive(Debug)]
pub struct EnvFileCache {
    /// All parsed environment variables (name -> variable)
    /// If a var appears in multiple files, the highest priority wins
    pub variables: HashMap<String, EnvVariable>,
    
    /// Track which files were parsed and when
    pub file_metadata: HashMap<PathBuf, FileMetadata>,
    
    /// The project root path
    pub project_root: PathBuf,
}

/// Metadata about a parsed env file
#[derive(Debug, Clone)]
pub struct FileMetadata {
    /// When this file was last modified
    pub last_modified: SystemTime,
    
    /// When we last parsed it
    pub last_parsed: SystemTime,
    
    /// Whether the file exists
    pub exists: bool,
}

impl EnvFileCache {
    /// Create a new empty cache for a Laravel project
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            variables: HashMap::new(),
            file_metadata: HashMap::new(),
            project_root,
        }
    }
    
    /// Parse all env files and populate the cache
    ///
    /// This checks .env, .env.local, and .env.example in priority order
    /// and merges them into a single variables map.
    pub fn parse_all(&mut self) -> Result<()> {
        // Clear existing cache
        self.variables.clear();
        self.file_metadata.clear();
        
        debug!("EnvParser: Starting to parse env files from root: {:?}", self.project_root);
        
        // Define env files in reverse priority order
        // We parse them backwards so higher priority files overwrite lower ones
        let env_files = vec![
            self.project_root.join(".env.example"),
            self.project_root.join(".env.local"),
            self.project_root.join(".env"),
        ];
        
        for env_path in env_files {
            if env_path.exists() {
                info!("EnvParser: Parsing env file: {:?}", env_path);
                self.parse_file(&env_path)?;
            } else {
                debug!("EnvParser: Env file not found: {:?}", env_path);
                // Track non-existent files too
                self.file_metadata.insert(
                    env_path.clone(),
                    FileMetadata {
                        last_modified: SystemTime::UNIX_EPOCH,
                        last_parsed: SystemTime::now(),
                        exists: false,
                    },
                );
            }
        }
        
        info!("EnvParser: Finished parsing, total variables: {}", self.variables.len());
        Ok(())
    }
    
    /// Parse a single env file and merge into cache
    fn parse_file(&mut self, path: &Path) -> Result<()> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("Failed to get metadata for {:?}", path))?;
        
        let modified = metadata.modified()
            .with_context(|| format!("Failed to get modified time for {:?}", path))?;
        
        // Parse the file
        let variables = parse_env_file(path)?;
        
        info!("EnvParser: Found {} variables in {:?}", variables.len(), path);
        
        // Merge into cache (overwrites existing due to priority)
        for var in variables {
            debug!("EnvParser: Adding variable '{}' = '{}'", var.name, var.value);
            self.variables.insert(var.name.clone(), var);
        }
        
        // Track metadata
        self.file_metadata.insert(
            path.to_path_buf(),
            FileMetadata {
                last_modified: modified,
                last_parsed: SystemTime::now(),
                exists: true,
            },
        );
        
        Ok(())
    }
    
    /// Check if any env files have been modified and need reparsing
    pub fn needs_refresh(&self) -> bool {
        for (path, metadata) in &self.file_metadata {
            if !metadata.exists {
                // Check if file now exists
                if path.exists() {
                    return true;
                }
            } else {
                // Check if file was modified
                if let Ok(current_metadata) = std::fs::metadata(path) {
                    if let Ok(current_modified) = current_metadata.modified() {
                        if current_modified > metadata.last_modified {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
    
    /// Get a variable by name
    pub fn get(&self, name: &str) -> Option<&EnvVariable> {
        self.variables.get(name)
    }
    
    /// Check if a variable exists
    pub fn contains(&self, name: &str) -> bool {
        self.variables.contains_key(name)
    }
    
    /// Get all variable names
    pub fn variable_names(&self) -> Vec<String> {
        self.variables.keys().cloned().collect()
    }
}

/// Parse env content from a string (for editor buffers)
///
/// This is used when parsing .env files that are open in the editor
/// and may have unsaved changes.
pub fn parse_env_content(content: &str, file_path: PathBuf) -> Result<Vec<EnvVariable>> {
    let mut variables = Vec::new();
    
    for (line_idx, line) in content.lines().enumerate() {
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }
        
        // Check if line is commented
        let is_commented = line.trim_start().starts_with('#');
        let working_line = if is_commented {
            line.trim_start().trim_start_matches('#').trim_start()
        } else {
            line
        };
        
        // Parse VAR=value format
        if let Some((name_part, value_part)) = working_line.split_once('=') {
            let name = name_part.trim();
            
            // Skip if not a valid variable name (comments, etc.)
            if name.is_empty() || name.contains(' ') {
                continue;
            }
            
            // Parse the value, handling quotes
            let value = parse_env_value(value_part.trim());
            
            // Calculate column positions
            let name_column = line.find(name).unwrap_or(0);
            let value_column = line.find('=').map(|pos| pos + 1).unwrap_or(name_column);
            
            variables.push(EnvVariable {
                name: name.to_string(),
                value,
                file_path: file_path.clone(),
                line: line_idx,
                column: name_column,
                value_column,
                is_commented,
            });
        }
    }
    
    Ok(variables)
}

/// Parse a single .env file into a vector of EnvVariable structs
///
/// LEARNING MOMENT: File I/O and string parsing in Rust
///
/// This function demonstrates:
/// - Reading files with error handling
/// - Line-by-line iteration
/// - String manipulation and parsing
/// - Building collections incrementally
pub fn parse_env_file(path: &Path) -> Result<Vec<EnvVariable>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read env file: {:?}", path))?;
    
    parse_env_content(&content, path.to_path_buf())
}

/// Parse an env value, handling quotes and special characters
///
/// LEARNING MOMENT: String processing in Rust
///
/// .env files can have values like:
/// - APP_NAME=Laravel (no quotes)
/// - APP_NAME="Laravel Application" (double quotes)
/// - APP_NAME='Laravel' (single quotes)
/// - DB_PASSWORD="" (empty quoted string)
/// - COMMENT=value # inline comment
fn parse_env_value(raw_value: &str) -> String {
    let trimmed = raw_value.trim();
    
    // Handle inline comments (# after the value)
    // But be careful not to remove # inside quotes
    let value_before_comment = if let Some(comment_pos) = find_comment_position(trimmed) {
        &trimmed[..comment_pos]
    } else {
        trimmed
    };
    
    let value = value_before_comment.trim();
    
    // Handle quoted values
    if value.len() >= 2 {
        // Double quotes
        if value.starts_with('"') && value.ends_with('"') {
            return value[1..value.len()-1].to_string();
        }
        
        // Single quotes
        if value.starts_with('\'') && value.ends_with('\'') {
            return value[1..value.len()-1].to_string();
        }
    }
    
    // No quotes, return as-is
    value.to_string()
}

/// Find the position of an inline comment (#) that's not inside quotes
///
/// This is a simplified implementation. A full implementation would need
/// to handle escaped quotes, but this covers most real-world .env files.
fn find_comment_position(s: &str) -> Option<usize> {
    let mut in_double_quotes = false;
    let mut in_single_quotes = false;
    
    for (i, ch) in s.chars().enumerate() {
        match ch {
            '"' if !in_single_quotes => in_double_quotes = !in_double_quotes,
            '\'' if !in_double_quotes => in_single_quotes = !in_single_quotes,
            '#' if !in_double_quotes && !in_single_quotes => return Some(i),
            _ => {}
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    
    #[test]
    fn test_parse_simple_env_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "APP_NAME=Laravel").unwrap();
        writeln!(file, "APP_ENV=local").unwrap();
        writeln!(file, "APP_DEBUG=true").unwrap();
        
        let vars = parse_env_file(file.path()).unwrap();
        
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[0].name, "APP_NAME");
        assert_eq!(vars[0].value, "Laravel");
        assert_eq!(vars[1].name, "APP_ENV");
        assert_eq!(vars[1].value, "local");
    }
    
    #[test]
    fn test_parse_quoted_values() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "APP_NAME=\"My Application\"").unwrap();
        writeln!(file, "APP_KEY='base64:abc123'").unwrap();
        writeln!(file, "EMPTY=\"\"").unwrap();
        
        let vars = parse_env_file(file.path()).unwrap();
        
        assert_eq!(vars[0].value, "My Application");
        assert_eq!(vars[1].value, "base64:abc123");
        assert_eq!(vars[2].value, "");
    }
    
    #[test]
    fn test_parse_comments() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "APP_NAME=Laravel").unwrap();
        writeln!(file, "# APP_DEBUG=false").unwrap();
        writeln!(file, "APP_ENV=local # inline comment").unwrap();
        
        let vars = parse_env_file(file.path()).unwrap();
        
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[0].name, "APP_NAME");
        assert!(!vars[0].is_commented);
        assert_eq!(vars[1].name, "APP_DEBUG");
        assert!(vars[1].is_commented);
        assert_eq!(vars[2].value, "local");
    }
    
    #[test]
    fn test_parse_empty_lines() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "APP_NAME=Laravel").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "APP_ENV=local").unwrap();
        
        let vars = parse_env_file(file.path()).unwrap();
        
        assert_eq!(vars.len(), 2);
    }
    
    #[test]
    fn test_env_cache() {
        let temp_dir = tempfile::tempdir().unwrap();
        let env_path = temp_dir.path().join(".env");
        
        let mut file = std::fs::File::create(&env_path).unwrap();
        writeln!(file, "APP_NAME=TestApp").unwrap();
        writeln!(file, "APP_ENV=testing").unwrap();
        
        let mut cache = EnvFileCache::new(temp_dir.path().to_path_buf());
        cache.parse_all().unwrap();
        
        assert_eq!(cache.variables.len(), 2);
        assert!(cache.contains("APP_NAME"));
        assert_eq!(cache.get("APP_NAME").unwrap().value, "TestApp");
    }
    
    #[test]
    fn test_env_priority() {
        let temp_dir = tempfile::tempdir().unwrap();
        
        // Create .env.example
        let example_path = temp_dir.path().join(".env.example");
        let mut example_file = std::fs::File::create(&example_path).unwrap();
        writeln!(example_file, "APP_NAME=Example").unwrap();
        
        // Create .env (higher priority)
        let env_path = temp_dir.path().join(".env");
        let mut env_file = std::fs::File::create(&env_path).unwrap();
        writeln!(env_file, "APP_NAME=Production").unwrap();
        
        let mut cache = EnvFileCache::new(temp_dir.path().to_path_buf());
        cache.parse_all().unwrap();
        
        // .env should win over .env.example
        assert_eq!(cache.get("APP_NAME").unwrap().value, "Production");
    }
}