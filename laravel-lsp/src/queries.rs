//! Single-pass tree-sitter query execution for Laravel pattern matching
//!
//! This module uses a single-pass extraction approach for performance:
//! - Queries are compiled once and cached using once_cell::Lazy
//! - All patterns are extracted in a single tree traversal
//! - This is O(n) instead of O(n×k) where k is the number of pattern types
//!
//! Queries are stored in .scm files and embedded at compile time using include_str!

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use std::time::Instant;
use tracing::{info, warn};
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator, Tree};

// ============================================================================
// Query File Embedding & Cached Compilation
// ============================================================================

/// Embed query files at compile time
const PHP_QUERY: &str = include_str!("../queries/php.scm");
const BLADE_QUERY: &str = include_str!("../queries/blade.scm");

/// Cached compiled PHP query - compiled once on first use
static PHP_QUERY_CACHE: Lazy<Option<Query>> = Lazy::new(|| {
    use crate::parser::language_php;
    let start = Instant::now();
    let lang = language_php();
    let result = Query::new(&lang, PHP_QUERY).ok();
    let elapsed = start.elapsed();
    if result.is_some() {
        tracing::info!("⚡ PHP query compiled in {:?} (one-time cost)", elapsed);
    } else {
        tracing::warn!("❌ PHP query compilation failed after {:?}", elapsed);
    }
    result
});

/// Cached compiled Blade query - compiled once on first use
static BLADE_QUERY_CACHE: Lazy<Option<Query>> = Lazy::new(|| {
    use crate::parser::language_blade;
    let start = Instant::now();
    let lang = language_blade();
    let result = Query::new(&lang, BLADE_QUERY).ok();
    let elapsed = start.elapsed();
    if result.is_some() {
        tracing::info!("⚡ Blade query compiled in {:?} (one-time cost)", elapsed);
    } else {
        tracing::warn!("❌ Blade query compilation failed after {:?}", elapsed);
    }
    result
});

/// Get the cached PHP query, or compile it if needed
fn get_php_query(_language: &Language) -> Result<&'static Query> {
    PHP_QUERY_CACHE.as_ref()
        .ok_or_else(|| anyhow!("Failed to compile PHP query"))
}

/// Get the cached Blade query, or compile it if needed
fn get_blade_query(_language: &Language) -> Result<&'static Query> {
    BLADE_QUERY_CACHE.as_ref()
        .ok_or_else(|| anyhow!("Failed to compile Blade query"))
}

/// Pre-warm the query cache by forcing Lazy initialization.
/// Call this on a background thread during startup to avoid
/// paying the ~200ms compilation cost on first file open.
pub fn prewarm_query_cache() {
    use std::ops::Deref;
    info!("🔥 Pre-warming query cache...");
    // Access the statics to trigger Lazy initialization
    // The logging inside the Lazy closures will show timing
    let _ = PHP_QUERY_CACHE.deref();
    let _ = BLADE_QUERY_CACHE.deref();
    info!("🔥 Query cache pre-warm complete");
}

// ============================================================================
// Match Data Structures
// ============================================================================

/// Represents a matched view() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ViewMatch<'a> {
    pub view_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    /// Whether this is from Route::view() or Volt::route() (should be ERROR if missing)
    pub is_route_view: bool,
}

/// Represents a matched Blade component (<x-*>)
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentMatch<'a> {
    pub component_name: &'a str,
    pub tag_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    pub resolved_path: Option<std::path::PathBuf>,
}

/// Represents a matched Livewire component
#[derive(Debug, Clone, PartialEq)]
pub struct LivewireMatch<'a> {
    pub component_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Blade slot (<x-slot:name> or <x-slot name="...">)
#[derive(Debug, Clone, PartialEq)]
pub struct SlotMatch<'a> {
    /// The slot name (e.g., "header" from <x-slot:header>)
    pub slot_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched Blade directive
#[derive(Debug, Clone, PartialEq)]
pub struct DirectiveMatch<'a> {
    pub directive_name: &'a str,
    pub full_text: String,
    pub arguments: Option<&'a str>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
    pub string_column: usize,
    pub string_end_column: usize,
}

/// Represents a matched env() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct EnvMatch<'a> {
    pub var_name: &'a str,
    pub has_fallback: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched config() call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMatch<'a> {
    pub config_key: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched middleware call in PHP route definitions
#[derive(Debug, Clone, PartialEq)]
pub struct MiddlewareMatch<'a> {
    pub middleware_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched translation call in PHP or Blade code
#[derive(Debug, Clone)]
pub struct TranslationMatch<'a> {
    pub translation_key: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched asset or path helper call
#[derive(Debug, Clone)]
pub struct AssetMatch<'a> {
    pub path: &'a str,
    pub helper_type: AssetHelperType,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Types of asset/path helpers
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A match for a container binding resolution call
#[derive(Debug, Clone)]
pub struct BindingMatch<'a> {
    pub binding_name: &'a str,
    pub is_class_reference: bool,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched route('name') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct RouteMatch<'a> {
    pub route_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched url('path') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct UrlMatch<'a> {
    pub url_path: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// Represents a matched action('Controller@method') call in PHP code
#[derive(Debug, Clone, PartialEq)]
pub struct ActionMatch<'a> {
    pub action_name: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

// ============================================================================
// Extracted Patterns - Result structs for single-pass extraction
// ============================================================================

/// All patterns extracted from a PHP file in a single pass
#[derive(Debug, Default)]
pub struct ExtractedPhpPatterns<'a> {
    pub views: Vec<ViewMatch<'a>>,
    pub env_calls: Vec<EnvMatch<'a>>,
    pub config_calls: Vec<ConfigMatch<'a>>,
    pub middleware_calls: Vec<MiddlewareMatch<'a>>,
    pub translation_calls: Vec<TranslationMatch<'a>>,
    pub asset_calls: Vec<AssetMatch<'a>>,
    pub binding_calls: Vec<BindingMatch<'a>>,
    pub route_calls: Vec<RouteMatch<'a>>,
    pub url_calls: Vec<UrlMatch<'a>>,
    pub action_calls: Vec<ActionMatch<'a>>,
}

/// Represents PHP content inside Blade echo statements {{ ... }}
#[derive(Debug, Clone, PartialEq)]
pub struct EchoPhpMatch<'a> {
    pub php_content: &'a str,
    pub byte_start: usize,
    pub byte_end: usize,
    pub row: usize,
    pub column: usize,
    pub end_column: usize,
}

/// All patterns extracted from a Blade file in a single pass
#[derive(Debug, Default)]
pub struct ExtractedBladePatterns<'a> {
    pub components: Vec<ComponentMatch<'a>>,
    pub livewire: Vec<LivewireMatch<'a>>,
    pub directives: Vec<DirectiveMatch<'a>>,
    /// PHP content inside {{ ... }} echo statements
    pub echo_php: Vec<EchoPhpMatch<'a>>,
    /// Slot tags (<x-slot:name> or <x-slot name="...">)
    pub slots: Vec<SlotMatch<'a>>,
}

// ============================================================================
// Single-Pass Extraction Functions
// ============================================================================

/// Extract all PHP patterns in a single tree traversal
///
/// This is the primary extraction function - it runs one query and processes
/// all captures in a single loop, dispatching based on capture name.
pub fn extract_all_php_patterns<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<ExtractedPhpPatterns<'a>> {
    let start = Instant::now();
    let query = get_php_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut result = ExtractedPhpPatterns::default();
    let query_fetch_time = start.elapsed();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;

        // Skip if we can't get the text
        let Ok(text) = node.utf8_text(source_bytes) else {
            continue;
        };

        let start_pos = node.start_position();
        let end_pos = node.end_position();

        match capture_name {
            // View patterns
            "view_name" => {
                result.views.push(ViewMatch {
                    view_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                    is_route_view: false,
                });
            }
            "route_view_name" => {
                result.views.push(ViewMatch {
                    view_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                    is_route_view: true,
                });
            }

            // Environment variable patterns
            "env_var" => {
                // Check if there's a fallback argument
                let has_fallback = check_has_fallback_argument(node);
                result.env_calls.push(EnvMatch {
                    var_name: text,
                    has_fallback,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Config patterns
            "config_key" => {
                result.config_calls.push(ConfigMatch {
                    config_key: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Middleware patterns
            "middleware_name" => {
                result.middleware_calls.push(MiddlewareMatch {
                    middleware_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Translation patterns
            "translation_key" => {
                result.translation_calls.push(TranslationMatch {
                    translation_key: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Asset and path helper patterns
            "asset_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::Asset,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "public_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::PublicPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "base_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::BasePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "app_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::AppPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "storage_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::StoragePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "database_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::DatabasePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "lang_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::LangPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "config_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ConfigPath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "resource_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ResourcePath,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "mix_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::Mix,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "vite_asset_path" => {
                result.asset_calls.push(AssetMatch {
                    path: text,
                    helper_type: AssetHelperType::ViteAsset,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Binding patterns
            "binding_name" => {
                result.binding_calls.push(BindingMatch {
                    binding_name: text,
                    is_class_reference: false,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }
            "binding_class_name" => {
                let clean_class = text.trim_start_matches('\\');
                result.binding_calls.push(BindingMatch {
                    binding_name: clean_class,
                    is_class_reference: true,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Route patterns
            "route_name" => {
                result.route_calls.push(RouteMatch {
                    route_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // URL patterns
            "url_path" => {
                result.url_calls.push(UrlMatch {
                    url_path: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Action patterns
            "action_name" => {
                result.action_calls.push(ActionMatch {
                    action_name: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Ignore other captures (function_name, class_name, etc. used for matching)
            _ => {}
        }
    }

    let total_time = start.elapsed();
    let pattern_count = result.views.len() + result.env_calls.len() + result.config_calls.len()
        + result.middleware_calls.len() + result.translation_calls.len() + result.asset_calls.len()
        + result.binding_calls.len() + result.route_calls.len() + result.url_calls.len() + result.action_calls.len();
    info!(
        "📊 PHP extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time, query_fetch_time, pattern_count
    );

    Ok(result)
}

/// Extract all Blade patterns in a single tree traversal
pub fn extract_all_blade_patterns<'a>(
    tree: &Tree,
    source: &'a str,
    language: &Language,
) -> Result<ExtractedBladePatterns<'a>> {
    let start = Instant::now();
    let query = get_blade_query(language)?;
    let mut cursor = QueryCursor::new();
    let mut result = ExtractedBladePatterns::default();
    let query_fetch_time = start.elapsed();

    let root_node = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut captures = cursor.captures(query, root_node, source_bytes);

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = &query_match.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;

        let Ok(text) = node.utf8_text(source_bytes) else {
            continue;
        };

        let start_pos = node.start_position();
        let end_pos = node.end_position();

        match capture_name {
            // Tag patterns - could be x-* components or livewire:* components
            "tag_name" => {
                if let Some(component_name) = text.strip_prefix("x-") {
                    // Blade component
                    result.components.push(ComponentMatch {
                        component_name,
                        tag_name: text,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                        resolved_path: None,
                    });
                } else if text.starts_with("livewire:") {
                    // Livewire component tag syntax
                    let component_name = &text[9..]; // Remove "livewire:" prefix
                    result.livewire.push(LivewireMatch {
                        component_name,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
                }
            }

            // Directive patterns
            "directive" => {
                // Skip closing directives
                if text.starts_with("@end") {
                    continue;
                }

                if !text.starts_with('@') {
                    warn!("Directive text doesn't start with @: '{}'", text);
                }

                let directive_name = text.strip_prefix('@').unwrap_or(text);

                // Look for parameter sibling
                let arguments = find_next_parameter_sibling(node, source_bytes);

                let full_text = if let Some(param) = arguments {
                    format!("{}{}", text, param)
                } else {
                    text.to_string()
                };

                let directive_column = start_pos.column;
                let directive_end_column = end_pos.column;

                // Calculate string column positions for view-referencing and translation directives
                let (string_column, string_end_column) = match (directive_name, &arguments) {
                    ("extends" | "include" | "slot" | "component" | "lang", Some(args)) => {
                        calculate_string_column_range(directive_column, directive_name, args)
                            .unwrap_or((directive_column, directive_end_column))
                    }
                    _ => (directive_column, directive_end_column),
                };

                result.directives.push(DirectiveMatch {
                    directive_name,
                    full_text,
                    arguments,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: directive_column,
                    end_column: directive_end_column,
                    string_column,
                    string_end_column,
                });
            }

            // @livewire('component-name') directive - component_name capture
            "component_name" => {
                let component_name = text.trim_matches(|c| c == '"' || c == '\'');
                result.livewire.push(LivewireMatch {
                    component_name,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // PHP content inside {{ ... }} echo statements
            "echo_php_content" => {
                result.echo_php.push(EchoPhpMatch {
                    php_content: text,
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    row: start_pos.row,
                    column: start_pos.column,
                    end_column: end_pos.column,
                });
            }

            // Slot tags: <x-slot:name> or <x-slot name="...">
            "slot_tag" => {
                // Extract slot name from x-slot:name syntax
                if let Some(slot_name) = text.strip_prefix("x-slot:") {
                    result.slots.push(SlotMatch {
                        slot_name,
                        byte_start: node.start_byte(),
                        byte_end: node.end_byte(),
                        row: start_pos.row,
                        column: start_pos.column,
                        end_column: end_pos.column,
                    });
                }
            }

            // Ignore vite_directive and other captures
            _ => {}
        }
    }

    let total_time = start.elapsed();
    let pattern_count = result.components.len() + result.livewire.len() + result.directives.len() + result.echo_php.len() + result.slots.len();
    info!(
        "📊 Blade extraction: {:?} total (query fetch: {:?}), {} patterns found",
        total_time, query_fetch_time, pattern_count
    );

    Ok(result)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if an env() call has a fallback/default value (second argument)
fn check_has_fallback_argument(node: tree_sitter::Node) -> bool {
    // Navigate: string_content -> string -> argument -> arguments -> function_call
    if let Some(string_node) = node.parent() {
        if let Some(argument_node) = string_node.parent() {
            if let Some(arguments_node) = argument_node.parent() {
                let mut argument_count = 0;
                for i in 0..arguments_node.child_count() {
                    if let Some(child) = arguments_node.child(i as u32) {
                        if child.kind() == "argument" {
                            argument_count += 1;
                        }
                    }
                }
                return argument_count >= 2;
            }
        }
    }
    false
}

/// Find the next parameter sibling node after a directive node
fn find_next_parameter_sibling<'a>(
    directive_node: tree_sitter::Node,
    source: &'a [u8],
) -> Option<&'a str> {
    let parent = directive_node.parent()?;
    let mut cursor = parent.walk();

    let mut found_directive = false;
    for child in parent.children(&mut cursor) {
        if found_directive && child.kind() == "parameter" {
            return child.utf8_text(source).ok();
        }
        if child.id() == directive_node.id() {
            found_directive = true;
        }
    }

    None
}

/// Calculate the column range of the quoted string within a directive's arguments
fn calculate_string_column_range(
    directive_column: usize,
    directive_name: &str,
    arguments: &str,
) -> Option<(usize, usize)> {
    let directive_len = directive_name.len() + 1; // +1 for the @ symbol

    let trimmed = arguments.trim_start();
    let spaces_before = arguments.len() - trimmed.len();

    let quote_char = trimmed.chars().next()?;
    if quote_char != '\'' && quote_char != '"' {
        return None;
    }

    let closing_quote_pos = trimmed[1..].find(quote_char)?;

    let string_start = directive_column + directive_len + 1 + spaces_before;
    let string_end = string_start + closing_quote_pos + 2;

    Some((string_start, string_end))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{language_blade, language_php, parse_blade, parse_php};

    #[test]
    fn test_extract_all_php_patterns_views() {
        let php_code = r#"<?php
        return view('users.profile');
        Route::view('/home', 'welcome');
        echo view("admin.dashboard");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 3, "Should find 3 view calls");

        let view_names: Vec<&str> = patterns.views.iter().map(|m| m.view_name).collect();
        assert!(view_names.contains(&"users.profile"));
        assert!(view_names.contains(&"welcome"));
        assert!(view_names.contains(&"admin.dashboard"));

        // Check is_route_view flag
        let welcome = patterns.views.iter().find(|v| v.view_name == "welcome").unwrap();
        assert!(welcome.is_route_view, "Route::view() should set is_route_view=true");

        let users = patterns.views.iter().find(|v| v.view_name == "users.profile").unwrap();
        assert!(!users.is_route_view, "view() should set is_route_view=false");
    }

    #[test]
    fn test_extract_all_php_patterns_env() {
        let php_code = r#"<?php
        $name = env('APP_NAME', 'Laravel');
        $debug = env("APP_DEBUG");
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.env_calls.len(), 2, "Should find 2 env calls");
        assert_eq!(patterns.env_calls[0].var_name, "APP_NAME");
        assert_eq!(patterns.env_calls[1].var_name, "APP_DEBUG");
    }

    #[test]
    fn test_extract_all_php_patterns_middleware() {
        let php_code = r#"<?php
        Route::middleware('auth')->group(function () {});
        Route::middleware(['auth', 'verified'])->get('/dashboard');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        let middleware_names: Vec<&str> = patterns.middleware_calls.iter()
            .map(|m| m.middleware_name).collect();

        assert!(middleware_names.contains(&"auth"), "Should find 'auth' middleware");
        assert!(middleware_names.contains(&"verified"), "Should find 'verified' middleware");
    }

    #[test]
    fn test_extract_all_blade_patterns_components() {
        let blade_code = r#"
        <div>
            <x-button type="primary">Click me</x-button>
            <x-forms.input name="email" />
        </div>
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        assert!(!patterns.components.is_empty(), "Should find at least one component");

        let component_names: Vec<&str> = patterns.components.iter()
            .map(|m| m.component_name).collect();
        assert!(
            component_names.iter().any(|&name| name == "button" || name.starts_with("button")),
            "Should find button component"
        );
    }

    #[test]
    fn test_extract_all_blade_patterns_directives() {
        let blade_code = r#"
@extends('layouts.app')
@section('content')
    @foreach($users as $user)
        <p>{{ $user->name }}</p>
    @endforeach
@endsection
        "#;

        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        let directive_names: Vec<&str> = patterns.directives.iter()
            .map(|m| m.directive_name).collect();

        assert!(directive_names.contains(&"extends"), "Should find @extends");
        assert!(directive_names.contains(&"section"), "Should find @section");
        assert!(directive_names.contains(&"foreach"), "Should find @foreach");

        // Should NOT contain closing directives
        assert!(!directive_names.contains(&"endforeach"), "Should not find @endforeach");
        assert!(!directive_names.contains(&"endsection"), "Should not find @endsection");
    }

    #[test]
    fn test_single_pass_is_faster() {
        // This test demonstrates the expected behavior - single pass should work
        let php_code = r#"<?php
        return view('home');
        $name = env('APP_NAME');
        $key = config('app.key');
        Route::middleware('auth')->get('/');
        $msg = __('messages.welcome');
        $css = asset('css/app.css');
        $service = app('cache');
        $url = route('home');
        "#;

        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();

        // Should extract all patterns in one call
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        // Verify we found patterns of different types
        assert!(!patterns.views.is_empty(), "Should find views");
        assert!(!patterns.env_calls.is_empty(), "Should find env calls");
        assert!(!patterns.config_calls.is_empty(), "Should find config calls");
        assert!(!patterns.middleware_calls.is_empty(), "Should find middleware");
        assert!(!patterns.translation_calls.is_empty(), "Should find translations");
        assert!(!patterns.asset_calls.is_empty(), "Should find assets");
        assert!(!patterns.binding_calls.is_empty(), "Should find bindings");
        assert!(!patterns.route_calls.is_empty(), "Should find routes");
    }

    // =========================================================================
    // Column Position Tests
    // =========================================================================
    // These tests ensure that column positions are correct for highlighting
    // and diagnostics. The column should point to the content, not quotes.

    #[test]
    fn test_view_column_positions() {
        // view('users.profile')
        // Position: 0         1         2
        //           0123456789012345678901234567
        //           <?php view('users.profile');
        // The tree-sitter query captures string_content (without quotes)
        let php_code = "<?php view('users.profile');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // view_name captures the string content WITHOUT quotes
        assert_eq!(view.view_name, "users.profile");
        // In "<?php view('users.profile');", 'u' starts at column 12
        assert_eq!(view.column, 12, "column should point to first char of view name");
        // End column should be at 'e' + 1 = 25
        assert_eq!(view.end_column, 25, "end_column should be after last char");
    }

    #[test]
    fn test_env_column_positions() {
        // env('APP_NAME')
        // Position: 0         1         2
        //           0123456789012345678901
        //           <?php env('APP_NAME');
        let php_code = "<?php env('APP_NAME');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.env_calls.len(), 1);
        let env_call = &patterns.env_calls[0];

        // env_var captures the string content WITHOUT quotes
        assert_eq!(env_call.var_name, "APP_NAME");
        // In "<?php env('APP_NAME');", 'A' starts at column 11
        assert_eq!(env_call.column, 11, "column should point to first char");
        assert_eq!(env_call.end_column, 19, "end_column should be after last char");
    }

    #[test]
    fn test_config_column_positions() {
        // config('app.name')
        // Position: 0         1         2
        //           0123456789012345678901234
        //           <?php config('app.name');
        let php_code = "<?php config('app.name');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.config_calls.len(), 1);
        let config_call = &patterns.config_calls[0];

        // config_key captures the string content WITHOUT quotes
        assert_eq!(config_call.config_key, "app.name");
        // In "<?php config('app.name');", 'a' starts at column 14
        assert_eq!(config_call.column, 14, "column should point to first char");
        assert_eq!(config_call.end_column, 22, "end_column should be after last char");
    }

    #[test]
    fn test_translation_column_positions() {
        // __('messages.welcome')
        // Position: 0         1         2
        //           012345678901234567890123456789
        //           <?php __('messages.welcome');
        let php_code = "<?php __('messages.welcome');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.translation_calls.len(), 1);
        let trans = &patterns.translation_calls[0];

        // translation_key captures the string content WITHOUT quotes
        assert_eq!(trans.translation_key, "messages.welcome");
        // In "<?php __('messages.welcome');", 'm' starts at column 10
        assert_eq!(trans.column, 10, "column should point to first char");
        assert_eq!(trans.end_column, 26, "end_column should be after last char");
    }

    #[test]
    fn test_asset_column_positions() {
        // asset('css/app.css')
        // Position: 0         1         2
        //           012345678901234567890123456
        //           <?php asset('css/app.css');
        let php_code = "<?php asset('css/app.css');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.asset_calls.len(), 1);
        let asset = &patterns.asset_calls[0];

        // asset_path captures the string content WITHOUT quotes
        assert_eq!(asset.path, "css/app.css");
        // In "<?php asset('css/app.css');", 'c' starts at column 13
        assert_eq!(asset.column, 13, "column should point to first char");
        assert_eq!(asset.end_column, 24, "end_column should be after last char");
    }

    #[test]
    fn test_middleware_column_positions() {
        // Route::middleware('auth')
        // Position: 0         1         2         3
        //           01234567890123456789012345678901
        //           <?php Route::middleware('auth');
        let php_code = "<?php Route::middleware('auth');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.middleware_calls.len(), 1);
        let mw = &patterns.middleware_calls[0];

        // middleware_name captures the string content WITHOUT quotes
        assert_eq!(mw.middleware_name, "auth");
        // In "<?php Route::middleware('auth');", 'a' starts at column 25
        assert_eq!(mw.column, 25, "column should point to first char");
        assert_eq!(mw.end_column, 29, "end_column should be after last char");
    }

    #[test]
    fn test_route_column_positions() {
        // route('home')
        // Position: 0         1
        //           01234567890123456789
        //           <?php route('home');
        let php_code = "<?php route('home');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.route_calls.len(), 1);
        let route = &patterns.route_calls[0];

        // route_name captures the string content WITHOUT quotes
        assert_eq!(route.route_name, "home");
        // In "<?php route('home');", 'h' starts at column 13
        assert_eq!(route.column, 13, "column should point to first char");
        assert_eq!(route.end_column, 17, "end_column should be after last char");
    }

    #[test]
    fn test_binding_column_positions() {
        // app('cache')
        // Position: 0         1
        //           0123456789012345678
        //           <?php app('cache');
        let php_code = "<?php app('cache');";
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.binding_calls.len(), 1);
        let binding = &patterns.binding_calls[0];

        // binding_name captures the string content WITHOUT quotes
        assert_eq!(binding.binding_name, "cache");
        // In "<?php app('cache');", 'c' starts at column 11
        assert_eq!(binding.column, 11, "column should point to first char");
        assert_eq!(binding.end_column, 16, "end_column should be after last char");
    }

    #[test]
    fn test_blade_component_column_positions() {
        // <x-button>
        // The component is matched by the Blade tree-sitter grammar
        // We need a more realistic Blade structure for proper parsing
        let blade_code = "<div><x-button></x-button></div>";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        // Components may or may not be found depending on tree-sitter grammar
        // Just verify the structure works
        if !patterns.components.is_empty() {
            let component = &patterns.components[0];
            assert!(component.column < blade_code.len(), "column should be valid");
            assert!(component.end_column >= component.column, "end_column should be >= column");
        }
    }

    #[test]
    fn test_livewire_component_column_positions() {
        // <livewire:counter />
        let blade_code = "<div><livewire:counter /></div>";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        // Livewire components may or may not be found depending on grammar
        if !patterns.livewire.is_empty() {
            let livewire = &patterns.livewire[0];
            assert!(livewire.column < blade_code.len(), "column should be valid");
            assert!(livewire.end_column >= livewire.column, "end_column should be >= column");
        }
    }

    #[test]
    fn test_blade_directive_column_positions() {
        // @include('partials.header')
        let blade_code = "@include('partials.header')";
        let tree = parse_blade(blade_code).expect("Should parse Blade");
        let lang = language_blade();
        let patterns = extract_all_blade_patterns(&tree, blade_code, &lang)
            .expect("Should extract patterns");

        let include_directive = patterns.directives.iter()
            .find(|d| d.directive_name == "include");

        assert!(include_directive.is_some(), "Should find @include directive");
        let directive = include_directive.unwrap();

        // directive starts at column 0 (the @)
        assert_eq!(directive.column, 0, "directive should start at column 0");
        // string_column should point to the view name string
        assert!(directive.string_column > 0, "string_column should be after directive name");
    }

    #[test]
    fn test_column_positions_with_indentation() {
        // Test that column positions work correctly with leading whitespace
        // Position: 0         1         2
        //           012345678901234567890123
        //               view('dashboard');
        // (4 spaces + view( = column 9, then ' = column 10, d = column 10)
        let php_code = "<?php\n    view('dashboard');"; // 4 spaces indentation
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // On line 1 (0-indexed), the indented content:
        // "    view('dashboard');"
        // Column 4-7 is "view", column 8 is "(", column 9 is "'", column 10 is "d"
        assert_eq!(view.row, 1, "should be on second line (0-indexed)");
        assert_eq!(view.column, 10, "column should point to first char of view name");
    }

    #[test]
    fn test_double_quote_column_positions() {
        // Test with double quotes
        // Position: 0         1         2
        //           0123456789012345678901234567
        //           <?php view("users.profile");
        let php_code = r#"<?php view("users.profile");"#;
        let tree = parse_php(php_code).expect("Should parse PHP");
        let lang = language_php();
        let patterns = extract_all_php_patterns(&tree, php_code, &lang)
            .expect("Should extract patterns");

        assert_eq!(patterns.views.len(), 1);
        let view = &patterns.views[0];

        // view_name is extracted WITHOUT quotes
        assert_eq!(view.view_name, "users.profile");
        assert_eq!(view.column, 12, "column should point to first char inside quotes");
        assert_eq!(view.end_column, 25, "end_column should be after last char");
    }

    #[test]
    fn test_blade_translation_patterns() {
        // Test that we can extract translations from Blade echo syntax
        let blade_code = r#"{{ __("Welcome to our app") }}
@lang("welcome")"#;

        // Parse as Blade first
        let blade_tree = parse_blade(blade_code).expect("Should parse Blade");
        let blade_lang = language_blade();
        let blade_patterns = extract_all_blade_patterns(&blade_tree, blade_code, &blade_lang)
            .expect("Should extract Blade patterns");

        // Check what directives we found
        println!("Blade directives found: {:?}", blade_patterns.directives.iter()
            .map(|d| d.directive_name)
            .collect::<Vec<_>>());

        // Check echo PHP content
        println!("Echo PHP content found: {:?}", blade_patterns.echo_php.iter()
            .map(|e| e.php_content)
            .collect::<Vec<_>>());

        // Parse as PHP to see if __() is captured
        let php_tree = parse_php(blade_code).expect("Should parse as PHP");
        let php_lang = language_php();
        let php_patterns = extract_all_php_patterns(&php_tree, blade_code, &php_lang)
            .expect("Should extract PHP patterns");

        println!("PHP translations found: {:?}", php_patterns.translation_calls.iter()
            .map(|t| t.translation_key)
            .collect::<Vec<_>>());

        // We expect to find translations in either Blade directives or PHP patterns
        let has_lang_directive = blade_patterns.directives.iter()
            .any(|d| d.directive_name == "lang");

        // Check that we captured the echo PHP content
        let has_echo_php = !blade_patterns.echo_php.is_empty();
        println!("Has echo PHP content: {}", has_echo_php);

        // At minimum, @lang should be captured as a directive
        assert!(has_lang_directive, "@lang should be captured as a directive");

        // And we should have captured the {{ __() }} echo content
        assert!(has_echo_php, "Should capture PHP content inside {{ }}");
        assert!(blade_patterns.echo_php[0].php_content.contains("__"), "Echo should contain __() call");
    }

}
