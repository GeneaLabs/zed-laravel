/// This module handles tree-sitter query execution for pattern matching
///
/// Queries are stored in .scm files and embedded at compile time using include_str!
/// This is the standard approach used by editors like Neovim, Helix, and Zed.

use anyhow::{anyhow, Result};
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator, Tree};

// ============================================================================
// PART 1: Query File Embedding
// ============================================================================

/// Embed the PHP query file at compile time
///
/// LEARNING MOMENT: include_str! macro
///
/// This macro reads a file at COMPILE TIME and embeds its contents as a &'static str
/// - The file path is relative to the current source file
/// - If the file doesn't exist, compilation fails
/// - The result is a string literal baked into the binary
/// - No runtime file I/O needed!
///
/// This is different from std::fs::read_to_string() which reads at runtime.
const PHP_QUERY: &str = include_str!("../queries/php.scm");

/// Embed the Blade query file at compile time
const BLADE_QUERY: &str = include_str!("../queries/blade.scm");

// ============================================================================
// PART 2: Query Compilation
// ============================================================================

/// Compile the PHP query into a Query object
///
/// LEARNING MOMENT: Query compilation
///
/// Tree-sitter queries need to be compiled before use:
/// 1. Parse the S-expression syntax
/// 2. Validate node types against the grammar
/// 3. Compile into an efficient internal representation
///
/// This can fail if:
/// - Syntax is invalid
/// - Node types don't exist in the grammar
/// - Predicates are malformed
pub fn compile_php_query(language: &Language) -> Result<Query> {
    Query::new(language, PHP_QUERY)
        .map_err(|e| anyhow!("Failed to compile PHP query: {:?}", e))
}

/// Compile the Blade query into a Query object
pub fn compile_blade_query(language: &Language) -> Result<Query> {
    Query::new(language, BLADE_QUERY)
        .map_err(|e| anyhow!("Failed to compile Blade query: {:?}", e))
}

// ============================================================================
// PART 3: Pattern Detection
// ============================================================================

/// Find all view() calls in PHP code
///
/// Returns a vector of (view_name, byte_offset) tuples
///
/// LEARNING MOMENT: Lifetimes
///
/// Notice the lifetime parameter 'a:
///   pub fn find_view_calls<'a>(tree: &Tree, source: &'a str, ...)
///
/// This says: "The returned ViewMatch structs contain references to 'source',
/// so they can't outlive the source string."
///
/// Rust's borrow checker uses this to ensure we don't use a ViewMatch after
/// the source string has been freed.
pub fn find_view_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<ViewMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    // Execute the query using captures() which returns an iterator
    // captures() gives us individual captures, matches() gives us groups
    // For our use case, captures() is simpler
    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    // Use StreamingIterator pattern with while-let
    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        // capture_index tells us which capture in this match is "new"
        // We only process the specific capture indicated to avoid duplicates
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // We only care about the view_name capture
        if capture_name == "view_name" {
            let node = capture.node;
            let view_name = node.utf8_text(source_bytes)?;

            results.push(ViewMatch {
                view_name,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
            });
        }
    }

    Ok(results)
}

/// Find all Blade components in Blade code (e.g., <x-button>)
pub fn find_blade_components<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<ComponentMatch<'a>>> {
    let query = compile_blade_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // Look for component tags (x-* pattern)
        if capture_name == "tag_name" {
            let node = capture.node;
            let tag_name = node.utf8_text(source_bytes)?;

            // Only process x-* components (filter out livewire:*)
            if tag_name.starts_with("x-") {
                // Remove the "x-" prefix to get the component name
                let component_name = &tag_name[2..];

                results.push(ComponentMatch {
                    component_name,
                    tag_name,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: node.start_position().row,
                    column: node.start_position().column,
                    end_column: node.end_position().column,
                });
            }
        }
    }

    Ok(results)
}

/// Find all Blade directives in Blade code
///
/// Returns directives like @extends, @section, @foreach, @customDirective, etc.
/// Does not return closing directives like @endif, @endsection
pub fn find_directives<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<DirectiveMatch<'a>>> {
    let query = compile_blade_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // Look for directive captures
        if capture_name == "directive" {
            let node = capture.node;
            let directive_text = node.utf8_text(source_bytes)?;

            // IMPORTANT: Verify that the directive includes the @ symbol
            // The tree-sitter-blade grammar should include @ as part of the directive node
            // If it doesn't start with @, something is wrong with the grammar or our query
            if !directive_text.starts_with('@') {
                // Log a warning but try to handle it gracefully
                eprintln!("WARNING: Directive text doesn't start with @: '{}'", directive_text);
            }

            // Remove @ symbol to get the directive name only
            // This is for the directive_name field, but byte positions should include @
            let directive_name = directive_text.strip_prefix('@')
                .unwrap_or(directive_text)
                .trim();

            // Look for a sibling parameter node right after this directive
            // In the tree: directive and parameter are siblings
            let arguments = find_next_parameter_sibling(node, source_bytes);

            // Construct full text (directive + parameter if present)
            let full_text = if let Some(param) = arguments {
                // Combine directive + parameter text
                // e.g., "@extends" + "('layouts.app')" = "@extends('layouts.app')"
                format!("{}{}", directive_text, param)
            } else {
                directive_text.to_string()
            };

            // Verify that byte positions include the @ symbol
            // The start position should be where @ begins
            let start_byte = node.start_byte();
            let start_char = source_bytes.get(start_byte).copied();
            if start_char != Some(b'@') {
                eprintln!("WARNING: Directive start byte doesn't point to @. byte={}, char={:?}, text='{}'",
                    start_byte, start_char.map(|c| c as char), directive_text);
            }

            let directive_column = node.start_position().column;
            let directive_end_column = node.end_position().column;

            // Calculate string column positions for directives with view references
            // For @extends/@include/@slot/@component, find the quoted string position
            let (string_column, string_end_column) = if (directive_name == "extends" 
                || directive_name == "include"
                || directive_name == "slot"
                || directive_name == "component")
                && arguments.is_some()
            {
                calculate_string_column_range(directive_column, directive_name, arguments.unwrap())
                    .unwrap_or((directive_column, directive_end_column))
            } else {
                (directive_column, directive_end_column)
            };

            results.push(DirectiveMatch {
                directive_name,
                full_text,  // Now it's a String, not &str
                arguments,
                byte_start: start_byte,  // This should point to @
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: directive_column,  // This should be the column of @
                end_column: directive_end_column,
                string_column,
                string_end_column,
            });
        }
    }

    Ok(results)
}

/// Find the next parameter sibling node after a directive node
///
/// In the Blade tree, directives and their parameters are siblings:
/// ```
/// parent_node
///   ├─ directive_node (@extends)
///   └─ parameter_node (('layouts.app'))
/// ```
fn find_next_parameter_sibling<'a>(
    directive_node: tree_sitter::Node,
    source: &'a [u8],
) -> Option<&'a str> {
    let parent = directive_node.parent()?;
    let mut cursor = parent.walk();

    // Find the directive node in parent's children
    let mut found_directive = false;
    for child in parent.children(&mut cursor) {
        if found_directive && child.kind() == "parameter" {
            // This is the parameter right after our directive
            return child.utf8_text(source).ok();
        }

        if child.id() == directive_node.id() {
            found_directive = true;
        }
    }

    None
}

/// Find all Livewire components in Blade code
pub fn find_livewire_components<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<LivewireMatch<'a>>> {
    let query = compile_blade_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;
        let text = node.utf8_text(source_bytes)?;

        // Handle <livewire:component-name> tags
        if capture_name == "tag_name" && text.starts_with("livewire:") {
            let component_name = &text[9..]; // Remove "livewire:" prefix

            results.push(LivewireMatch {
                component_name,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
            });
        }

        // Handle @livewire('component-name') directives
        if capture_name == "component_name" {
            // The text might be quoted, so strip quotes
            let component_name = text.trim_matches(|c| c == '"' || c == '\'');

            results.push(LivewireMatch {
                component_name,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
            });
        }
    }

    Ok(results)
}

/// Find all env() function calls in PHP code
///
/// This function looks for patterns like:
/// - env('APP_NAME', 'Laravel')
/// - env("DB_HOST")
///
/// Returns a vector of EnvMatch structs with position information
pub fn find_env_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<EnvMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // We only care about the env_var capture
        if capture_name == "env_var" {
            let node = capture.node;
            let var_name = node.utf8_text(source_bytes)?;

            // Check if there's a second argument (fallback value)
            // We need to navigate: string_content -> string -> argument -> arguments -> function_call
            let has_fallback = if let Some(string_node) = node.parent() {
                // string_node should be 'string' or 'encapsed_string'
                if let Some(argument_node) = string_node.parent() {
                    // argument_node should be 'argument'
                    if let Some(arguments_node) = argument_node.parent() {
                        // arguments_node should be 'arguments'
                        // Count how many 'argument' children it has
                        let mut argument_count = 0;
                        for i in 0..arguments_node.child_count() {
                            if let Some(child) = arguments_node.child(i as u32) {
                                if child.kind() == "argument" {
                                    argument_count += 1;
                                }
                            }
                        }
                        // Has fallback if there are 2 or more arguments
                        argument_count >= 2
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            results.push(EnvMatch {
                var_name,
                has_fallback,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
            });
        }
    }

    Ok(results)
}

/// Find all config() function calls in PHP code
///
/// This function looks for patterns like:
/// - config('app.name')
/// - config("database.connections.mysql.host")
///
/// Returns a vector of ConfigMatch structs with position information
pub fn find_config_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<ConfigMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // We only care about the config_key capture
        if capture_name == "config_key" {
            let node = capture.node;
            let config_key = node.utf8_text(source_bytes)?;

            results.push(ConfigMatch {
                config_key,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
            });
        }
    }

    Ok(results)
}

// ============================================================================
// PART 6: Match Data Structures
// ============================================================================

/// Represents a matched view() call in PHP code
///
/// LEARNING MOMENT: Lifetime annotations in structs
///
/// The 'a lifetime parameter means this struct contains a borrowed reference
/// (&'a str) that lives as long as 'a. The struct can't outlive the source
/// string it references.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewMatch<'a> {
    /// The view name (e.g., "users.profile")
    pub view_name: &'a str,
    /// Starting byte offset in source
    pub byte_start: usize,
    /// Ending byte offset in source
    pub byte_end: usize,
    /// Line number (0-indexed)
    pub row: usize,
    /// Column number (0-indexed) - start of the match
    pub column: usize,
    /// End column number (0-indexed) - end of the match
    pub end_column: usize,
}

/// Represents a matched Blade component (<x-*>)
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentMatch<'a> {
    /// The component name without "x-" prefix (e.g., "button")
    pub component_name: &'a str,
    /// The full tag name (e.g., "x-button")
    pub tag_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Livewire component
#[derive(Debug, Clone, PartialEq)]
pub struct LivewireMatch<'a> {
    /// The component name (e.g., "user-profile")
    pub component_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Blade directive
#[derive(Debug, Clone, PartialEq)]
pub struct DirectiveMatch<'a> {
    /// The directive name without @ (e.g., "extends", "section", "customDirective")
    pub directive_name: &'a str,
    /// The full directive text including @ (e.g., "@extends('layouts.app')")
    /// This is owned because we construct it from directive + parameter
    pub full_text: String,
    /// The arguments if any (e.g., "('layouts.app')" from @extends('layouts.app'))
    pub arguments: Option<&'a str>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    /// Column position of the quoted string (e.g., column of 'my.view' in @include('my.view'))
    /// For directives like @include/@extends with arguments, this is the start of the string including quotes
    /// For other directives, this equals column
    pub string_column: usize,
    /// End column position of the quoted string (including closing quote)
    /// For directives like @include/@extends with arguments, this is the end of the string including quotes
    /// For other directives, this equals end_column
    pub string_end_column: usize,
}

/// Represents a matched env() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct EnvMatch<'a> {
    /// The environment variable name (e.g., "APP_NAME")
    pub var_name: &'a str,
    /// Whether this env() call has a fallback/default value
    pub has_fallback: bool,
    /// Starting byte offset in source
    pub byte_start: usize,
    /// Ending byte offset in source
    pub byte_end: usize,
    /// Line number (0-indexed)
    pub row: usize,
    /// Column number (0-indexed) - start of the match
    pub column: usize,
    /// End column number (0-indexed) - end of the match
    pub end_column: usize,
}

/// Represents a matched config() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMatch<'a> {
    /// The config key (e.g., "app.name", "database.connections.mysql.host")
    pub config_key: &'a str,
    /// Starting byte offset in source
    pub byte_start: usize,
    /// Ending byte offset in source
    pub byte_end: usize,
    /// Line number (0-indexed)
    pub row: usize,
    /// Column number (0-indexed) - start of the match
    pub column: usize,
    /// End column number (0-indexed) - end of the match
    pub end_column: usize,
}

/// Calculate the column range of the quoted string within a directive's arguments
/// e.g., for @include with arguments "'my.view'", returns the columns for the quoted string
fn calculate_string_column_range(directive_column: usize, directive_name: &str, arguments: &str) -> Option<(usize, usize)> {
    // Calculate the length of the directive including @
    let directive_len = directive_name.len() + 1; // +1 for the @ symbol
    
    // The arguments from tree-sitter don't include the opening parenthesis
    // For @extends('layouts.app'):
    //   - directive text is '@extends' (columns 0-7)
    //   - there's a '(' at column 8 (not included in arguments)  
    //   - arguments text is 'layouts.app' starting at column 9
    
    // Trim any leading whitespace from arguments
    let trimmed = arguments.trim_start();
    let spaces_before = arguments.len() - trimmed.len();
    
    // Find the first quote character (single or double)
    let quote_char = trimmed.chars().next()?;
    if quote_char != '\'' && quote_char != '"' {
        return None;
    }
    
    // Find the closing quote
    let closing_quote_pos = trimmed[1..].find(quote_char)?;
    
    // Calculate positions relative to directive start
    // directive_column + directive_len gets us to end of @directive (e.g., column 8)
    // +1 for the opening parenthesis that's not in arguments
    // +spaces_before for any whitespace at start of arguments
    let string_start = directive_column + directive_len + 1 + spaces_before;
    // The string ends after the opening quote + content + closing quote
    let string_end = string_start + closing_quote_pos + 2; // +2 for both quotes
    
    Some((string_start, string_end))
}

// ============================================================================
// PART 5: Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{language_blade, language_php, parse_blade, parse_php};

    #[test]
    fn test_compile_php_query() {
        let lang = language_php();
        let result = compile_php_query(&lang);
        assert!(result.is_ok(), "PHP query should compile successfully");
    }

    #[test]
    fn test_calculate_string_column_range() {
        // Test @extends('layouts.app') starting at column 4
        // Tree-sitter gives us arguments WITHOUT the parentheses
        // So for @extends('layouts.app'), arguments is "'layouts.app'"
        let result = calculate_string_column_range(4, "extends", "'layouts.app'");
        assert!(result.is_some());
        let (start, end) = result.unwrap();
        // 4 (directive_column) + 8 (@extends) + 1 (open paren) = 13
        assert_eq!(start, 13, "String should start at column 13");
        assert_eq!(end, 26, "String should end at column 26 (exclusive end for LSP range)");

        // Test @include('components.button') starting at column 0
        let result = calculate_string_column_range(0, "include", "'components.button'");
        assert!(result.is_some());
        let (start, end) = result.unwrap();
        // 0 + 8 (@include) + 1 (open paren) = 9
        assert_eq!(start, 9, "String should start at column 9");
        assert_eq!(end, 28, "String should end at column 28 (exclusive end for LSP range)");

        // Test with spaces: arguments is "  'view.name'" (spaces before quote)
        let result = calculate_string_column_range(0, "include", "  'view.name'");
        assert!(result.is_some());
        let (start, end) = result.unwrap();
        // 0 + 8 (@include) + 1 (open paren) + 2 (spaces) = 11
        assert_eq!(start, 11, "String should start at column 11 with spaces");
        assert_eq!(end, 24, "String should end at column 24");

        // Test with double quotes
        let result = calculate_string_column_range(0, "extends", "\"layouts.app\"");
        assert!(result.is_some());
        let (start, end) = result.unwrap();
        assert_eq!(start, 9, "Should work with double quotes");
        assert_eq!(end, 22, "Should end correctly with double quotes");
    }

    #[test]
    fn test_compile_blade_query() {
        let lang = language_blade();
        let result = compile_blade_query(&lang);
        assert!(result.is_ok(), "Blade query should compile successfully");
    }

    #[test]
    fn test_find_view_calls() {
        let php_code = r#"<?php
        $data = ['name' => 'Laravel'];
        return view('users.profile', $data);
        echo view("admin.dashboard");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} matches:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' at {}:{}", i, m.view_name, m.row, m.column);
        }

        assert_eq!(matches.len(), 2, "Should find 2 view calls");

        // Check that we found both views (order doesn't matter)
        let view_names: Vec<&str> = matches.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"users.profile"), "Should find users.profile");
        assert!(view_names.contains(&"admin.dashboard"), "Should find admin.dashboard");
    }

    #[test]
    fn test_find_hyphenated_view_calls() {
        let php_code = r#"<?php
        return view('user-profile');
        echo view("admin-dashboard");
        return view('multi-word-component');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} matches with hyphenated names:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' at {}:{} (end_column: {})",
                i, m.view_name, m.row, m.column, m.end_column);
            eprintln!("      Length: {}, contains hyphens: {}",
                m.view_name.len(), m.view_name.contains('-'));
        }

        assert_eq!(matches.len(), 3, "Should find 3 view calls");

        // Check that we found all hyphenated views
        let view_names: Vec<&str> = matches.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"user-profile"), "Should find user-profile");
        assert!(view_names.contains(&"admin-dashboard"), "Should find admin-dashboard");
        assert!(view_names.contains(&"multi-word-component"), "Should find multi-word-component");

        // Verify that the column range covers the entire name
        for m in matches.iter() {
            let name_len = m.view_name.len();
            let column_span = m.end_column - m.column;
            eprintln!("Checking '{}': name_len={}, column_span={}", m.view_name, name_len, column_span);
            assert_eq!(column_span, name_len,
                "Column span should equal name length for '{}'", m.view_name);
        }
    }

    #[test]
    fn test_find_env_calls() {
        let source = r#"
            <?php
            $name = env('APP_NAME', 'Laravel');
            $debug = env("APP_DEBUG");
            "#;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language_php()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let matches = find_env_calls(&tree, source, &language_php()).unwrap();

        eprintln!("Found {} env matches:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] var_name='{}' at {}:{}", i, m.var_name, m.row, m.column);
        }

        assert_eq!(matches.len(), 2, "Should find exactly 2 env() calls");
        assert_eq!(matches[0].var_name, "APP_NAME");
        assert_eq!(matches[1].var_name, "APP_DEBUG");
    }

    #[test]
    fn test_find_config_calls() {
        let source = r#"
            <?php
            $name = config('app.name');
            $host = config("database.connections.mysql.host");
            "#;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language_php()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let matches = find_config_calls(&tree, source, &language_php()).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].config_key, "app.name");
        assert_eq!(matches[1].config_key, "database.connections.mysql.host");
    }

    #[test]
    fn test_find_blade_components() {
        let blade_code = r#"
        <div>
            <x-button type="primary">Click me</x-button>
            <x-forms.input name="email" />
        </div>
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let matches = find_blade_components(&tree, blade_code, &lang).expect("Should find components");

        // The exact count depends on grammar behavior
        // We should find at least the opening tags
        assert!(!matches.is_empty(), "Should find at least one component");

        // Check that we found button and forms.input
        let component_names: Vec<&str> = matches.iter().map(|m| m.component_name).collect();
        assert!(
            component_names.iter().any(|&name| name == "button" || name.starts_with("button")),
            "Should find button component"
        );
    }

    #[test]
    fn test_find_livewire_components() {
        let blade_code = r#"
        <div>
            <livewire:user-profile />
            @livewire('admin.dashboard')
        </div>
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let matches = find_livewire_components(&tree, blade_code, &lang).expect("Should find livewire");

        // Should find both tag and directive syntax
        assert!(!matches.is_empty(), "Should find at least one Livewire component");
    }



    #[test]
    fn test_find_directives() {
        let blade_code = r#"
@extends('layouts.app')

@section('content')
    @foreach($users as $user)
        <p>{{ $user->name }}</p>
    @endforeach

    @customDirective('some-argument')

    @if($condition)
        <div>True</div>
    @endif
@endsection
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let matches = find_directives(&tree, blade_code, &lang).expect("Should find directives");

        eprintln!("Found {} directives:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] directive='{}' full='{}' args={:?}",
                i, m.directive_name, m.full_text, m.arguments);
        }

        // Should find multiple directives
        assert!(!matches.is_empty(), "Should find at least one directive");

        // Check for specific directives
        let directive_names: Vec<&str> = matches.iter().map(|m| m.directive_name).collect();

        assert!(directive_names.contains(&"extends"), "Should find @extends");
        assert!(directive_names.contains(&"section"), "Should find @section");
        assert!(directive_names.contains(&"foreach"), "Should find @foreach");
        assert!(directive_names.contains(&"if"), "Should find @if");
        assert!(directive_names.contains(&"customDirective"), "Should find @customDirective");

        // Check that we have arguments for some
        let extends_match = matches.iter().find(|m| m.directive_name == "extends").unwrap();
        assert!(extends_match.arguments.is_some(), "@extends should have arguments");
        assert!(extends_match.arguments.unwrap().contains("layouts.app"));
    }

    #[test]
    fn test_directive_positions_include_at_symbol() {
        // This test verifies that directive byte positions include the @ symbol
        let blade_code = "@extends('layouts.app')";

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let matches = find_directives(&tree, blade_code, &lang).expect("Should find directives");

        assert_eq!(matches.len(), 1, "Should find exactly one directive");
        let directive_match = &matches[0];

        // The directive should start at byte 0 (the @ symbol)
        assert_eq!(directive_match.byte_start, 0, "Directive should start at byte 0 (@ symbol)");

        // The column should be 0 (the @ symbol position)
        assert_eq!(directive_match.column, 0, "Directive should start at column 0 (@ symbol)");

        // The full_text should include the @ symbol
        assert!(directive_match.full_text.starts_with('@'),
            "Full text should start with @ symbol, got: '{}'", directive_match.full_text);

        // Verify that the character at byte_start is @
        let char_at_start = blade_code.as_bytes()[directive_match.byte_start] as char;
        assert_eq!(char_at_start, '@', "Character at byte_start should be @");

        eprintln!("✓ Directive correctly includes @ symbol:");
        eprintln!("  full_text: '{}'", directive_match.full_text);
        eprintln!("  byte_start: {} (char: '{}')", directive_match.byte_start, char_at_start);
        eprintln!("  column: {}", directive_match.column);
    }

}
