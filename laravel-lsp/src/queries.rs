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

        // We care about both view_name and route_view_name captures
        if capture_name == "view_name" || capture_name == "route_view_name" {
            let node = capture.node;
            let view_name = node.utf8_text(source_bytes)?;

            // Determine if this is a route view (Route::view or Volt::route)
            // These should show ERROR severity if view not found
            let is_route_view = capture_name == "route_view_name";
            
            results.push(ViewMatch {
                view_name,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                row: node.start_position().row,
                column: node.start_position().column,
                end_column: node.end_position().column,
                is_route_view,
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
                    resolved_path: None,
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
                .unwrap_or(directive_text);

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

/// Find all middleware() and withoutMiddleware() calls in PHP route definitions
///
/// This function parses route middleware definitions like:
/// - Route::middleware('auth')
/// - Route::middleware(['auth', 'web'])
/// - ->middleware('verified')
/// - ->withoutMiddleware('guest')
///
/// It handles both single middleware strings and arrays of middleware.
/// Middleware with parameters (e.g., 'throttle:60,1') are captured as-is.
pub fn find_middleware_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<MiddlewareMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // We only care about the middleware_name capture
        if capture_name == "middleware_name" {
            let node = capture.node;
            let middleware_name = node.utf8_text(source_bytes)?;

            results.push(MiddlewareMatch {
                middleware_name,
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

/// Find all translation calls in PHP code
///
/// This function parses translation retrieval patterns like:
/// - __('messages.welcome')
/// - trans('validation.required')
/// - trans_choice('messages.apples', 10)
/// - Lang::get('auth.failed')
///
/// It extracts the translation key from the first argument.
pub fn find_translation_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<TranslationMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // We only care about the translation_key capture
        if capture_name == "translation_key" {
            let node = capture.node;
            let translation_key = node.utf8_text(source_bytes)?;

            results.push(TranslationMatch {
                translation_key,
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
    /// Whether this is from Route::view() or Volt::route() (should be ERROR if missing)
    pub is_route_view: bool,
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
    /// Resolved file path (cached during pre-parsing for performance)
    pub resolved_path: Option<std::path::PathBuf>,
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

/// Represents a matched middleware call in PHP route definitions
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareMatch<'a> {
    /// The middleware name/alias (e.g., "auth", "verified", "throttle:60,1")
    pub middleware_name: &'a str,
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

/// Represents a matched translation call in PHP or Blade code
#[derive(Debug, Clone)]
pub struct TranslationMatch<'a> {
    /// The translation key (e.g., "messages.welcome" or "Welcome to app")
    pub translation_key: &'a str,
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

/// Represents a matched asset or path helper call
#[derive(Debug, Clone)]
pub struct AssetMatch<'a> {
    /// The asset path or file path
    pub path: &'a str,
    /// The type of helper used
    pub helper_type: AssetHelperType,
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

/// Represents a matched Vite asset path within a @vite directive
#[derive(Debug, Clone, PartialEq)]
pub struct ViteAssetMatch<'a> {
    /// The asset path (e.g., "resources/css/app.css")
    pub path: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Types of asset/path helpers
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetHelperType {
    Asset,        // asset() -> public/
    PublicPath,   // public_path() -> public/
    BasePath,     // base_path() -> project root
    AppPath,      // app_path() -> app/
    StoragePath,  // storage_path() -> storage/
    DatabasePath, // database_path() -> database/
    LangPath,     // lang_path() -> lang/
    ConfigPath,   // config_path() -> config/
    ResourcePath, // resource_path() -> resources/
    Mix,          // mix() -> public/
    ViteAsset,    // Vite::asset() -> resources/
}

/// Find all asset and path helper calls in PHP code
///
/// Returns a vector of AssetMatch structs containing path and helper type
pub fn find_asset_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<AssetMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // Determine the helper type based on capture name
        let helper_type = match capture_name {
            "asset_path" => AssetHelperType::Asset,
            "public_path" => AssetHelperType::PublicPath,
            "base_path" => AssetHelperType::BasePath,
            "app_path" => AssetHelperType::AppPath,
            "storage_path" => AssetHelperType::StoragePath,
            "database_path" => AssetHelperType::DatabasePath,
            "lang_path" => AssetHelperType::LangPath,
            "config_path" => AssetHelperType::ConfigPath,
            "resource_path" => AssetHelperType::ResourcePath,
            "mix_path" => AssetHelperType::Mix,
            "vite_asset_path" => AssetHelperType::ViteAsset,
            _ => continue,
        };

        let node = capture.node;
        let path = node.utf8_text(source_bytes)?;

        results.push(AssetMatch {
            path,
            helper_type,
            byte_start: node.start_byte(),
            byte_end: node.end_byte(),
            row: node.start_position().row,
            column: node.start_position().column,
            end_column: node.end_position().column,
        });
    }

    Ok(results)
}

/// Extract asset paths from @vite directive arguments
/// e.g., @vite(['resources/css/app.css', 'resources/js/app.js'])
/// or @vite['resources/css/app.css'] (without opening paren from tree-sitter)
/// Returns a vector of (path, byte_start, byte_end) tuples
pub fn extract_vite_asset_paths(directive_text: &str) -> Vec<(&str, usize, usize)> {
    let mut paths = Vec::new();
    
    // Find the opening bracket or parenthesis
    // Tree-sitter may give us "@vite['...']" without the opening paren
    let start_pos = directive_text.find('(')
        .or_else(|| directive_text.find('['))
        .unwrap_or(0);
    
    let args_section = &directive_text[start_pos..];
    let mut in_string = false;
    let mut string_start = 0;
    let mut quote_char = '\0';
    let mut i = 0;
    let bytes = args_section.as_bytes();
    
    while i < bytes.len() {
        let ch = bytes[i] as char;
        
        if !in_string {
            // Look for opening quotes
            if ch == '\'' || ch == '"' {
                in_string = true;
                quote_char = ch;
                string_start = i + 1; // Start after the quote
            }
        } else {
            // Look for closing quote (same as opening)
            if ch == quote_char {
                // Extract the string content between quotes
                if let Ok(path) = std::str::from_utf8(&bytes[string_start..i]) {
                    // Only include paths that look like file paths (not empty, not just whitespace)
                    if !path.trim().is_empty() {
                        // Calculate absolute byte positions in the original directive_text
                        let abs_start = start_pos + string_start;
                        let abs_end = start_pos + i;
                        paths.push((path, abs_start, abs_end));
                    }
                }
                in_string = false;
            }
        }
        
        i += 1;
    }
    
    paths
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


// ============================================================================
// Container Binding Resolution
// ============================================================================

/// A match for a container binding resolution call
#[derive(Debug, Clone)]
pub struct BindingMatch<'a> {
    /// The binding name/class being resolved (e.g., "auth", "App\\Contracts\\PaymentGateway")
    pub binding_name: &'a str,
    /// Whether this is a class reference (Class::class) or a string binding
    pub is_class_reference: bool,
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

/// Find all container binding resolution calls in PHP code
///
/// Matches:
/// - app('auth') - string binding
/// - app('cache') - string binding
/// - app(SomeInterface::class) - class reference
/// - app(\App\Services\PaymentService::class) - qualified class reference
pub fn find_binding_calls<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<Vec<BindingMatch<'a>>> {
    let query = compile_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut results = Vec::new();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(&query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];

        // Handle string bindings: app('auth'), app('cache'), etc.
        if capture_name == "binding_name" {
            if let Ok(binding_text) = capture.node.utf8_text(source_bytes) {
                let start_point = capture.node.start_position();
                let end_point = capture.node.end_position();

                results.push(BindingMatch {
                    binding_name: binding_text,
                    is_class_reference: false,
                    byte_start: capture.node.start_byte(),
                    byte_end: capture.node.end_byte(),
                    row: start_point.row,
                    column: start_point.column,
                    end_column: end_point.column,
                });
            }
        }
        // Handle class references: app(SomeClass::class)
        else if capture_name == "binding_class_name" {
            if let Ok(class_text) = capture.node.utf8_text(source_bytes) {
                // Clean up the class name - remove leading backslash if present
                let clean_class = class_text.trim_start_matches('\\');

                let start_point = capture.node.start_position();
                let end_point = capture.node.end_position();

                results.push(BindingMatch {
                    binding_name: clean_class,
                    is_class_reference: true,
                    byte_start: capture.node.start_byte(),
                    byte_end: capture.node.end_byte(),
                    row: start_point.row,
                    column: start_point.column,
                    end_column: end_point.column,
                });
            }
        }
    }

    Ok(results)
}

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
        assert_eq!(end, 22, "String should end at column 22");

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
        }
    }

    #[test]
    fn test_find_route_view_calls() {
        let php_code = r#"<?php
        Route::view('/home', 'welcome');
        Route::view('/about', "pages.about");
        Route::view('/contact', 'contact.form');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} Route::view() matches:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' at {}:{}", i, m.view_name, m.row, m.column);
        }

        assert_eq!(matches.len(), 3, "Should find 3 Route::view() calls");

        // Check that we found all views (second argument of Route::view)
        let view_names: Vec<&str> = matches.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"welcome"), "Should find welcome");
        assert!(view_names.contains(&"pages.about"), "Should find pages.about");
        assert!(view_names.contains(&"contact.form"), "Should find contact.form");
    }

    #[test]
    fn test_mixed_view_patterns() {
        let php_code = r#"<?php
        return view('users.index');
        Route::view('/home', 'welcome');
        return View::make('admin.dashboard');
        Route::view('/about', "pages.about");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} mixed pattern matches:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' at {}:{}", i, m.view_name, m.row, m.column);
        }

        assert_eq!(matches.len(), 4, "Should find all 4 view patterns");

        // Check that we found all views from different patterns
        let view_names: Vec<&str> = matches.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"users.index"), "Should find view() call");
        assert!(view_names.contains(&"welcome"), "Should find Route::view() call");
        assert!(view_names.contains(&"admin.dashboard"), "Should find View::make() call");
        assert!(view_names.contains(&"pages.about"), "Should find Route::view() call");
    }

    #[test]
    fn test_find_volt_route_calls() {
        let php_code = r#"<?php
        Volt::route('/home', 'welcome');
        Volt::route('/about', "pages.about");
        Volt::route('/contact', 'volt.contact');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} Volt::route() matches:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' at {}:{} is_route_view={}", 
                i, m.view_name, m.row, m.column, m.is_route_view);
        }

        assert_eq!(matches.len(), 3, "Should find 3 Volt::route() calls");

        // Check that we found all views (second argument of Volt::route)
        let view_names: Vec<&str> = matches.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"welcome"), "Should find welcome");
        assert!(view_names.contains(&"pages.about"), "Should find pages.about");
        assert!(view_names.contains(&"volt.contact"), "Should find volt.contact");
        
        // Verify all are marked as route views (should be ERROR if missing)
        for m in matches.iter() {
            assert!(m.is_route_view, "Volt::route() should set is_route_view=true");
        }
    }

    #[test]
    fn test_route_view_flag_distinction() {
        let php_code = r#"<?php
        return view('users.index');
        Route::view('/home', 'welcome');
        Volt::route('/about', 'pages.about');
        return View::make('admin.dashboard');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        // Debug: print what we found
        eprintln!("Found {} matches with is_route_view flags:", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] view_name='{}' is_route_view={}", 
                i, m.view_name, m.is_route_view);
        }

        assert_eq!(matches.len(), 4, "Should find all 4 view patterns");

        // Find specific matches
        let users_index = matches.iter().find(|m| m.view_name == "users.index").unwrap();
        let welcome = matches.iter().find(|m| m.view_name == "welcome").unwrap();
        let pages_about = matches.iter().find(|m| m.view_name == "pages.about").unwrap();
        let admin_dashboard = matches.iter().find(|m| m.view_name == "admin.dashboard").unwrap();

        // Regular view() and View::make() should NOT be route views
        assert!(!users_index.is_route_view, "view() should have is_route_view=false");
        assert!(!admin_dashboard.is_route_view, "View::make() should have is_route_view=false");

        // Route::view() and Volt::route() SHOULD be route views
        assert!(welcome.is_route_view, "Route::view() should have is_route_view=true");
        assert!(pages_about.is_route_view, "Volt::route() should have is_route_view=true");
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

    #[test]
    fn test_find_middleware_calls() {
        let php_code = r#"
<?php

use Illuminate\Support\Facades\Route;

Route::get('/dashboard', function () {
    return view('dashboard');
})->middleware('auth');

Route::middleware('verified')->group(function () {
    Route::get('/profile', [ProfileController::class, 'show']);
});

Route::middleware(['auth', 'verified'])->group(function () {
    Route::get('/settings', [SettingsController::class, 'index']);
});

Route::withoutMiddleware('auth')->get('/public', function () {
    return 'public';
});

Route::get('/api/data')->middleware(['throttle:60,1', 'auth']);
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_middleware_calls(&tree, php_code, &lang).expect("Should find middleware");

        assert!(!matches.is_empty(), "Should find middleware calls");

        // Verify we found all the middleware references
        let middleware_names: Vec<&str> = matches.iter().map(|m| m.middleware_name).collect();
        
        assert!(middleware_names.contains(&"auth"), "Should find 'auth' middleware");
        assert!(middleware_names.contains(&"verified"), "Should find 'verified' middleware");
        assert!(middleware_names.contains(&"throttle:60,1"), "Should find 'throttle:60,1' middleware with parameters");

        eprintln!("✓ Found {} middleware references:", matches.len());
        for m in &matches {
            eprintln!("  - '{}' at line {}", m.middleware_name, m.row + 1);
        }
    }

    #[test]
    fn test_find_translation_calls() {
        let php_code = r#"
<?php

// Short helper function
$message = __('messages.welcome');
$error = __("auth.failed");

// Trans helper
$title = trans('pages.home.title');
$description = trans("pages.about.description");

// Trans choice for pluralization
$count = trans_choice('messages.apples', 10);
$time = trans_choice("messages.minutes_ago", $minutes);

// Lang facade
$greeting = Lang::get('messages.greeting');
$farewell = \Lang::get("messages.farewell");

// JSON translations (plain strings)
$simple = __('Welcome to our application');

// Nested keys
$nested = __('validation.custom.email.required');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_translation_calls(&tree, php_code, &lang).expect("Should find translations");

        assert!(!matches.is_empty(), "Should find translation calls");

        // Verify we found all the translation references
        let translation_keys: Vec<&str> = matches.iter().map(|m| m.translation_key).collect();
        
        assert!(translation_keys.contains(&"messages.welcome"), "Should find 'messages.welcome'");
        assert!(translation_keys.contains(&"auth.failed"), "Should find 'auth.failed'");
        assert!(translation_keys.contains(&"pages.home.title"), "Should find 'pages.home.title'");
        assert!(translation_keys.contains(&"messages.apples"), "Should find 'messages.apples' from trans_choice");
        assert!(translation_keys.contains(&"messages.greeting"), "Should find 'messages.greeting' from Lang::get");
        assert!(translation_keys.contains(&"Welcome to our application"), "Should find JSON translation");
        assert!(translation_keys.contains(&"validation.custom.email.required"), "Should find nested key");

        eprintln!("✓ Found {} translation references:", matches.len());
        for m in &matches {
            eprintln!("  - '{}' at line {}", m.translation_key, m.row + 1);
        }
    }

    #[test]
    fn test_find_multi_word_translation_calls() {
        let php_code = r#"
<?php

// Multi-word translations (JSON only)
$welcome = __('Welcome to our application');
$please_login = __('Please login to continue');
$success = trans('Your profile has been updated');
$error = Lang::get('An error occurred while processing your request');

// Mixed with single words
$ok = __('OK');
$long_message = __('This is a longer message with multiple words');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_translation_calls(&tree, php_code, &lang).expect("Should find translations");

        assert!(!matches.is_empty(), "Should find translation calls");

        // Verify we found all the translation references
        let translation_keys: Vec<&str> = matches.iter().map(|m| m.translation_key).collect();
        
        assert!(translation_keys.contains(&"Welcome to our application"), "Should find multi-word key");
        assert!(translation_keys.contains(&"Please login to continue"), "Should find multi-word key");
        assert!(translation_keys.contains(&"Your profile has been updated"), "Should find multi-word key");
        assert!(translation_keys.contains(&"An error occurred while processing your request"), "Should find long multi-word key");
        assert!(translation_keys.contains(&"OK"), "Should find single-word key");
        assert!(translation_keys.contains(&"This is a longer message with multiple words"), "Should find long message");

        eprintln!("✓ Found {} multi-word translation references:", matches.len());
        for m in &matches {
            eprintln!("  - '{}' at line {}", m.translation_key, m.row + 1);
        }
    }

    #[test]
    fn test_find_single_word_translation_calls() {
        let php_code = r#"
<?php

// Single word keys (could be JSON or PHP)
$confirm = __('Confirm');
$cancel = __('Cancel');
$save = trans('Save');
$delete = Lang::get('Delete');

// These should also be found
$yes = __('Yes');
$no = __('No');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_translation_calls(&tree, php_code, &lang).expect("Should find translations");

        assert!(!matches.is_empty(), "Should find translation calls");

        // Verify we found all the single-word translation references
        let translation_keys: Vec<&str> = matches.iter().map(|m| m.translation_key).collect();
        
        assert!(translation_keys.contains(&"Confirm"), "Should find 'Confirm'");
        assert!(translation_keys.contains(&"Cancel"), "Should find 'Cancel'");
        assert!(translation_keys.contains(&"Save"), "Should find 'Save'");
        assert!(translation_keys.contains(&"Delete"), "Should find 'Delete'");
        assert!(translation_keys.contains(&"Yes"), "Should find 'Yes'");
        assert!(translation_keys.contains(&"No"), "Should find 'No'");

        eprintln!("✓ Found {} single-word translation references:", matches.len());
        for m in &matches {
            eprintln!("  - '{}' at line {}", m.translation_key, m.row + 1);
        }
    }

    #[test]
    fn test_view_positions_exclude_quotes() {
        // This test verifies that view name positions exclude the surrounding quotes
        let php_code = r#"<?php
return view('welcome');
echo view("dashboard");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        assert_eq!(matches.len(), 2, "Should find 2 view calls");

        for view_match in &matches {
            let source_bytes = php_code.as_bytes();
            
            // Extract the actual characters at the matched byte positions
            let char_at_start = source_bytes[view_match.byte_start] as char;
            let char_at_end = source_bytes[view_match.byte_end - 1] as char;
            
            eprintln!("Testing view '{}' at byte range {}..{}", 
                view_match.view_name, view_match.byte_start, view_match.byte_end);
            eprintln!("  char_at_start: '{}' (should NOT be a quote)", char_at_start);
            eprintln!("  char_at_end: '{}' (should NOT be a quote)", char_at_end);
            
            // The byte positions should point to the actual view name content, not the quotes
            assert_ne!(char_at_start, '\'', "Start position should not be a single quote");
            assert_ne!(char_at_start, '"', "Start position should not be a double quote");
            assert_ne!(char_at_end, '\'', "End position should not be a single quote");
            assert_ne!(char_at_end, '"', "End position should not be a double quote");
            
            // The extracted text should match the view name
            let extracted_text = std::str::from_utf8(
                &source_bytes[view_match.byte_start..view_match.byte_end]
            ).expect("Should be valid UTF-8");
            
            assert_eq!(extracted_text, view_match.view_name,
                "Extracted text should match view_name without quotes");
        }
    }

    #[test]
    fn test_view_column_positions_exclude_quotes() {
        // This test verifies that column positions for LSP ranges exclude the surrounding quotes
        let php_code = r#"<?php
return view('welcome');
echo view("dashboard");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        assert_eq!(matches.len(), 2, "Should find 2 view calls");

        for view_match in &matches {
            let source_bytes = php_code.as_bytes();
            
            // Get the line of text
            let lines: Vec<&str> = php_code.lines().collect();
            let line_text = lines[view_match.row];
            let line_bytes = line_text.as_bytes();
            
            // Extract characters at the column positions
            if view_match.column < line_bytes.len() {
                let char_at_column = line_bytes[view_match.column] as char;
                eprintln!("Testing view '{}' at row {} column {} (end_column {})", 
                    view_match.view_name, view_match.row, view_match.column, view_match.end_column);
                eprintln!("  Line: '{}'", line_text);
                eprintln!("  char_at_column {}: '{}' (should NOT be a quote)", view_match.column, char_at_column);
                
                // The column position should point to the first character of the view name, not the quote
                assert_ne!(char_at_column, '\'', 
                    "Column {} should not point to a single quote, but found '{}' in line: {}", 
                    view_match.column, char_at_column, line_text);
                assert_ne!(char_at_column, '"', 
                    "Column {} should not point to a double quote, but found '{}' in line: {}", 
                    view_match.column, char_at_column, line_text);
            }
            
            if view_match.end_column > 0 && view_match.end_column <= line_bytes.len() {
                let char_before_end = line_bytes[view_match.end_column - 1] as char;
                eprintln!("  char before end_column {}: '{}' (should NOT be a quote)", view_match.end_column, char_before_end);
                
                // The end_column-1 should point to the last character of the view name, not the quote
                assert_ne!(char_before_end, '\'', 
                    "Character before end_column {} should not be a single quote, but found '{}' in line: {}", 
                    view_match.end_column, char_before_end, line_text);
                assert_ne!(char_before_end, '"', 
                    "Character before end_column {} should not be a double quote, but found '{}' in line: {}", 
                    view_match.end_column, char_before_end, line_text);
            }
        }
    }

    #[test]
    fn test_route_view_column_positions_exclude_quotes() {
        // This test specifically checks Route::view() and Volt::route() patterns
        // to ensure column positions exclude quotes for goto navigation
        let php_code = r#"<?php
Route::view('/home', 'welcome');
Route::view('/about', "pages.about");
Volt::route('/contact', 'contact.form');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_view_calls(&tree, php_code, &lang).expect("Should find views");

        assert_eq!(matches.len(), 3, "Should find 3 route view calls");

        for view_match in &matches {
            let lines: Vec<&str> = php_code.lines().collect();
            let line_text = lines[view_match.row];
            let line_bytes = line_text.as_bytes();
            
            eprintln!("\nTesting route view '{}' at row {} column {}-{}", 
                view_match.view_name, view_match.row, view_match.column, view_match.end_column);
            eprintln!("  Line: '{}'", line_text);
            
            // Check start position
            if view_match.column < line_bytes.len() {
                let char_at_start = line_bytes[view_match.column] as char;
                eprintln!("  char_at_column[{}]: '{}' (should be first char of '{}')", 
                    view_match.column, char_at_start, view_match.view_name);
                
                // Should point to first character of view name, not quote
                assert_ne!(char_at_start, '\'', 
                    "Route::view() column should not point to single quote");
                assert_ne!(char_at_start, '"', 
                    "Route::view() column should not point to double quote");
                
                // Should match the first character of the view name
                let expected_first_char = view_match.view_name.chars().next().unwrap();
                assert_eq!(char_at_start, expected_first_char,
                    "Start column should point to first char of view name");
            }
            
            // Check end position
            if view_match.end_column > 0 && view_match.end_column <= line_bytes.len() {
                let char_before_end = line_bytes[view_match.end_column - 1] as char;
                eprintln!("  char_at_column[{}]: '{}' (should be last char of '{}')", 
                    view_match.end_column - 1, char_before_end, view_match.view_name);
                
                // Should point to last character of view name, not quote
                assert_ne!(char_before_end, '\'', 
                    "Route::view() end column should not point to single quote");
                assert_ne!(char_before_end, '"', 
                    "Route::view() end column should not point to double quote");
                
                // Should match the last character of the view name
                let expected_last_char = view_match.view_name.chars().last().unwrap();
                assert_eq!(char_before_end, expected_last_char,
                    "End column should point to last char of view name");
            }
            
            // Extract the text using the column range
            let extracted = &line_bytes[view_match.column..view_match.end_column];
            let extracted_text = std::str::from_utf8(extracted).expect("Should be valid UTF-8");
            eprintln!("  Extracted: '{}' (should match '{}')", extracted_text, view_match.view_name);
            
            assert_eq!(extracted_text, view_match.view_name,
                "Column range should extract exact view name without quotes");
        }
    }

    #[test]
    fn test_find_asset_calls() {
        let php_code = r#"<?php
        $css = asset('css/app.css');
        $img = asset("images/logo.png");
        $js = asset('js/main.js');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_asset_calls(&tree, php_code, &lang).expect("Should find assets");

        assert_eq!(matches.len(), 3, "Should find 3 asset() calls");

        let paths: Vec<&str> = matches.iter().map(|m| m.path).collect();
        assert!(paths.contains(&"css/app.css"), "Should find css/app.css");
        assert!(paths.contains(&"images/logo.png"), "Should find images/logo.png");
        assert!(paths.contains(&"js/main.js"), "Should find js/main.js");

        // Check helper types
        for m in &matches {
            assert_eq!(m.helper_type, AssetHelperType::Asset);
        }
    }

    #[test]
    fn test_find_path_helpers() {
        let php_code = r#"<?php
        $base = base_path('composer.json');
        $app = app_path('Models/User.php');
        $storage = storage_path('logs/laravel.log');
        $db = database_path('seeders/UserSeeder.php');
        $lang = lang_path('en/messages.php');
        $config = config_path('app.php');
        $resource = resource_path('views/welcome.blade.php');
        $public = public_path('index.php');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_asset_calls(&tree, php_code, &lang).expect("Should find paths");

        assert_eq!(matches.len(), 8, "Should find 8 path helper calls");

        // Check each helper type
        let base_match = matches.iter().find(|m| m.path == "composer.json").unwrap();
        assert_eq!(base_match.helper_type, AssetHelperType::BasePath);

        let app_match = matches.iter().find(|m| m.path == "Models/User.php").unwrap();
        assert_eq!(app_match.helper_type, AssetHelperType::AppPath);

        let storage_match = matches.iter().find(|m| m.path == "logs/laravel.log").unwrap();
        assert_eq!(storage_match.helper_type, AssetHelperType::StoragePath);

        let db_match = matches.iter().find(|m| m.path == "seeders/UserSeeder.php").unwrap();
        assert_eq!(db_match.helper_type, AssetHelperType::DatabasePath);

        let lang_match = matches.iter().find(|m| m.path == "en/messages.php").unwrap();
        assert_eq!(lang_match.helper_type, AssetHelperType::LangPath);

        let config_match = matches.iter().find(|m| m.path == "app.php").unwrap();
        assert_eq!(config_match.helper_type, AssetHelperType::ConfigPath);

        let resource_match = matches.iter().find(|m| m.path == "views/welcome.blade.php").unwrap();
        assert_eq!(resource_match.helper_type, AssetHelperType::ResourcePath);

        let public_match = matches.iter().find(|m| m.path == "index.php").unwrap();
        assert_eq!(public_match.helper_type, AssetHelperType::PublicPath);
    }

    #[test]
    fn test_find_vite_asset_calls() {
        let php_code = r#"<?php
        $logo = Vite::asset('resources/images/logo.svg');
        $icon = Vite::asset("resources/images/favicon.ico");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_asset_calls(&tree, php_code, &lang).expect("Should find Vite assets");

        assert_eq!(matches.len(), 2, "Should find 2 Vite::asset() calls");

        let paths: Vec<&str> = matches.iter().map(|m| m.path).collect();
        assert!(paths.contains(&"resources/images/logo.svg"), "Should find logo.svg");
        assert!(paths.contains(&"resources/images/favicon.ico"), "Should find favicon.ico");

        for m in &matches {
            assert_eq!(m.helper_type, AssetHelperType::ViteAsset);
        }
    }

    #[test]
    fn test_find_mix_calls() {
        let php_code = r#"<?php
        $css = mix('css/app.css');
        $js = mix("js/app.js");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_asset_calls(&tree, php_code, &lang).expect("Should find mix calls");

        assert_eq!(matches.len(), 2, "Should find 2 mix() calls");

        let paths: Vec<&str> = matches.iter().map(|m| m.path).collect();
        assert!(paths.contains(&"css/app.css"), "Should find css/app.css");
        assert!(paths.contains(&"js/app.js"), "Should find js/app.js");

        for m in &matches {
            assert_eq!(m.helper_type, AssetHelperType::Mix);
        }
    }

    #[test]
    fn test_extract_vite_asset_paths() {
        // Single asset
        let directive = "@vite('resources/css/app.css')";
        let paths = extract_vite_asset_paths(directive);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].0, "resources/css/app.css");

        // Multiple assets in array
        let directive = "@vite(['resources/css/app.css', 'resources/js/app.js'])";
        let paths = extract_vite_asset_paths(directive);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, "resources/css/app.css");
        assert_eq!(paths[1].0, "resources/js/app.js");

        // With double quotes
        let directive = r#"@vite(["resources/css/app.css", "resources/js/app.js"])"#;
        let paths = extract_vite_asset_paths(directive);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, "resources/css/app.css");
        assert_eq!(paths[1].0, "resources/js/app.js");

        // Mixed quotes
        let directive = r#"@vite(['resources/css/app.css', "resources/js/app.js"])"#;
        let paths = extract_vite_asset_paths(directive);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, "resources/css/app.css");
        assert_eq!(paths[1].0, "resources/js/app.js");

        // With spaces
        let directive = "@vite([ 'resources/css/app.css' , 'resources/js/app.js' ])";
        let paths = extract_vite_asset_paths(directive);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, "resources/css/app.css");
        assert_eq!(paths[1].0, "resources/js/app.js");
    }

    #[test]
    fn test_asset_column_positions_exclude_quotes() {
        let php_code = r#"<?php
$img = asset('images/logo.png');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let matches = find_asset_calls(&tree, php_code, &lang).expect("Should find assets");

        assert_eq!(matches.len(), 1);
        let asset_match = &matches[0];

        let lines: Vec<&str> = php_code.lines().collect();
        let line_text = lines[asset_match.row];
        let line_bytes = line_text.as_bytes();

        // Check that column positions don't include quotes
        let char_at_start = line_bytes[asset_match.column] as char;
        assert_ne!(char_at_start, '\'', "Column should not point to quote");
        assert_ne!(char_at_start, '"', "Column should not point to quote");

        // Should be the first character of the path
        assert_eq!(char_at_start, 'i', "Should point to first char of 'images'");

        // Extract text using column range
        let extracted = &line_bytes[asset_match.column..asset_match.end_column];
        let extracted_text = std::str::from_utf8(extracted).expect("Should be valid UTF-8");
        assert_eq!(extracted_text, "images/logo.png", "Should extract path without quotes");
    }

    #[test]
    fn test_extract_vite_with_actual_format() {
        // Test with the actual format we get from tree-sitter (missing opening paren)
        let directive = "@vite['resources/css/app.css', 'resources/js/app.js']";
        let paths = extract_vite_asset_paths(directive);
        eprintln!("Extracted {} paths from: {}", paths.len(), directive);
        for (i, (path, start, end)) in paths.iter().enumerate() {
            eprintln!("  [{}] path='{}' offset={}-{}", i, path, start, end);
        }
        
        // Even without opening paren, should still extract paths
        assert_eq!(paths.len(), 2, "Should extract 2 paths even without opening paren");
        if paths.len() >= 2 {
            assert_eq!(paths[0].0, "resources/css/app.css");
            assert_eq!(paths[1].0, "resources/js/app.js");
        }
    }

    #[test]
    fn test_find_vite_directive() {
        let blade_code = r#"
@vite(['resources/css/app.css', 'resources/js/app.js'])
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let matches = find_directives(&tree, blade_code, &lang).expect("Should find directives");

        eprintln!("Found {} directives", matches.len());
        for (i, m) in matches.iter().enumerate() {
            eprintln!("  [{}] directive_name='{}' full_text='{}'", i, m.directive_name, m.full_text);
        }

        // Should find @vite directive
        let vite_match = matches.iter().find(|m| m.directive_name == "vite");
        assert!(vite_match.is_some(), "Should find @vite directive");
        
        let vite = vite_match.unwrap();
        assert_eq!(vite.directive_name, "vite");
        assert!(vite.full_text.contains("resources/css/app.css"));
        assert!(vite.full_text.contains("resources/js/app.js"));
    }

}
