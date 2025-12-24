//! Salsa-inspired incremental computation system for Laravel LSP
//! 
//! This module provides incremental parsing and caching of Laravel files
//! using principles from the Salsa framework but with a simpler implementation
//! that works with the current LSP architecture.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};

/// Core data structures for Laravel-specific parsing
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeTemplate {
    pub file_path: PathBuf,
    pub version: u32,
    pub directives: Vec<BladeDirective>,
    pub components: Vec<BladeComponent>,
    pub variables: Vec<BladeVariable>,
    pub sections: Vec<BladeSection>,
    pub includes: Vec<BladeInclude>,
    pub extends: Vec<BladeExtends>,
    pub parsed_at: std::time::SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeDirective {
    pub name: String,
    pub parameters: Vec<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub directive_type: BladeDirectiveType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BladeDirectiveType {
    Conditional,    // @if, @unless, @switch
    Loop,          // @foreach, @for, @while
    Include,       // @include, @includeIf, @includeWhen
    Template,      // @extends, @section, @yield
    Component,     // @component, @slot
    Auth,          // @auth, @guest
    Custom,        // User-defined directives
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeComponent {
    pub name: String,
    pub attributes: Vec<BladeAttribute>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_self_closing: bool,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeAttribute {
    pub name: String,
    pub value: Option<String>,
    pub is_binding: bool,      // :attribute vs attribute
    pub is_event: bool,        // @click vs onclick
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeVariable {
    pub expression: String,
    pub variable_name: Option<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub is_escaped: bool,      // {{ }} vs {!! !!}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeSection {
    pub name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub has_content: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeInclude {
    pub template_name: String,
    pub parameters: Vec<String>,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub include_type: BladeIncludeType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BladeIncludeType {
    Include,       // @include
    IncludeIf,     // @includeIf
    IncludeWhen,   // @includeWhen
    IncludeUnless, // @includeUnless
    IncludeFirst,  // @includeFirst
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BladeExtends {
    pub template_name: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaravelRoute {
    pub method: String,
    pub uri: String,
    pub controller: Option<String>,
    pub action: Option<String>,
    pub name: Option<String>,
    pub middleware: Vec<String>,
    pub file_path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EloquentModel {
    pub name: String,
    pub table: Option<String>,
    pub fillable: Vec<String>,
    pub guarded: Vec<String>,
    pub casts: Vec<ModelCast>,
    pub relationships: Vec<ModelRelationship>,
    pub file_path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCast {
    pub attribute: String,
    pub cast_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRelationship {
    pub name: String,
    pub relationship_type: String,
    pub related_model: String,
    pub foreign_key: Option<String>,
    pub local_key: Option<String>,
}

/// Incremental computation database inspired by Salsa
#[derive(Debug)]
pub struct IncrementalDatabase {
    /// Cache of parsed Blade templates
    blade_cache: Arc<RwLock<HashMap<PathBuf, BladeTemplate>>>,
    
    /// Cache of parsed routes
    routes_cache: Arc<RwLock<HashMap<PathBuf, Vec<LaravelRoute>>>>,
    
    /// Cache of parsed models
    models_cache: Arc<RwLock<HashMap<PathBuf, Option<EloquentModel>>>>,
    
    /// File version tracking for invalidation
    file_versions: Arc<RwLock<HashMap<PathBuf, u32>>>,
    
    /// Project root path
    project_root: Arc<RwLock<Option<PathBuf>>>,
    
    /// Statistics
    cache_hits: Arc<RwLock<u64>>,
    cache_misses: Arc<RwLock<u64>>,
}

impl Default for IncrementalDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalDatabase {
    pub fn new() -> Self {
        Self {
            blade_cache: Arc::new(RwLock::new(HashMap::new())),
            routes_cache: Arc::new(RwLock::new(HashMap::new())),
            models_cache: Arc::new(RwLock::new(HashMap::new())),
            file_versions: Arc::new(RwLock::new(HashMap::new())),
            project_root: Arc::new(RwLock::new(None)),
            cache_hits: Arc::new(RwLock::new(0)),
            cache_misses: Arc::new(RwLock::new(0)),
        }
    }
    
    /// Set the project root path
    pub async fn set_project_root(&self, root: PathBuf) {
        *self.project_root.write().await = Some(root);
    }
    
    /// Update file content and invalidate cache if needed
    pub async fn update_file(&self, path: PathBuf, _content: String, version: u32) -> bool {
        let mut versions = self.file_versions.write().await;
        let current_version = versions.get(&path).copied().unwrap_or(0);
        
        if current_version >= version {
            return false; // No update needed
        }
        
        versions.insert(path.clone(), version);
        
        // Invalidate relevant caches
        if path.to_string_lossy().contains(".blade.php") {
            self.blade_cache.write().await.remove(&path);
        } else if path.to_string_lossy().contains("/routes/") {
            self.routes_cache.write().await.remove(&path);
        } else if path.to_string_lossy().contains("/Models/") {
            self.models_cache.write().await.remove(&path);
        }
        
        true
    }
    
    /// Parse Blade template with caching
    pub async fn parse_blade_template(&self, path: PathBuf, content: String, version: u32) -> Arc<BladeTemplate> {
        // Check cache first
        {
            let cache = self.blade_cache.read().await;
            if let Some(cached) = cache.get(&path) {
                if cached.version == version {
                    *self.cache_hits.write().await += 1;
                    return Arc::new(cached.clone());
                }
            }
        }
        
        *self.cache_misses.write().await += 1;
        
        // Parse the template
        let template = self.parse_blade_content(&path, &content, version).await;
        
        // Cache the result
        self.blade_cache.write().await.insert(path, template.clone());
        
        Arc::new(template)
    }
    
    /// Get hover information for a position in a Blade file
    pub async fn get_blade_hover(&self, path: PathBuf, content: String, version: u32, line: u32, character: u32) -> Option<String> {
        let template = self.parse_blade_template(path, content, version).await;
        
        // Check directives
        for directive in &template.directives {
            if directive.line == line && 
               character >= directive.column && 
               character <= directive.end_column {
                return Some(format!("**Blade Directive**: `@{}`\n\n{}", 
                    directive.name, 
                    get_directive_documentation(&directive.name)
                ));
            }
        }
        
        // Check components
        for component in &template.components {
            if component.line == line &&
               character >= component.column &&
               character <= component.end_column {
                return Some(format!("**Blade Component**: `<x-{}/>`\n\nLaravel Blade component\n\nAttributes:\n{}", 
                    component.name,
                    component.attributes.iter()
                        .map(|attr| format!("- `{}`", attr.name))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
        
        // Check variables
        for variable in &template.variables {
            if variable.line == line &&
               character >= variable.column &&
               character <= variable.end_column {
                return Some(format!("**Blade Variable**: `{}`\n\n{}", 
                    variable.expression,
                    if variable.is_escaped { "Escaped output" } else { "Raw output" }
                ));
            }
        }
        
        None
    }

    
    // Private parsing methods
    
    async fn parse_blade_content(&self, path: &PathBuf, content: &str, version: u32) -> BladeTemplate {
        let directives = self.extract_blade_directives(content).await;
        let components = self.extract_blade_components(content).await;
        let variables = self.extract_blade_variables(content).await;
        let sections = self.extract_blade_sections(content).await;
        let includes = self.extract_blade_includes(content).await;
        let extends = self.extract_blade_extends(content).await;
        
        BladeTemplate {
            file_path: path.clone(),
            version,
            directives,
            components,
            variables,
            sections,
            includes,
            extends,
            parsed_at: std::time::SystemTime::now(),
        }
    }
    
    async fn extract_blade_directives(&self, content: &str) -> Vec<BladeDirective> {
        let mut directives = Vec::new();
        let directive_regex = regex::Regex::new(r"@(\w+)(?:\((.*?)\))?").unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            for caps in directive_regex.captures_iter(line) {
                let directive_name = caps.get(1).unwrap().as_str().to_string();
                let parameters_str = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                
                let parameters = if !parameters_str.is_empty() {
                    parameters_str.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    Vec::new()
                };
                
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                let directive_type = classify_blade_directive(&directive_name);
                
                directives.push(BladeDirective {
                    name: directive_name,
                    parameters,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    directive_type,
                });
            }
        }
        
        directives
    }
    
    async fn extract_blade_components(&self, content: &str) -> Vec<BladeComponent> {
        let mut components = Vec::new();
        let component_regex = regex::Regex::new(r"<x-([a-zA-Z0-9\-\.]+)([^>]*)(/?)>").unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            for caps in component_regex.captures_iter(line) {
                let component_name = caps.get(1).unwrap().as_str().to_string();
                let attributes_str = caps.get(2).unwrap().as_str();
                let is_self_closing = caps.get(3).unwrap().as_str() == "/";
                
                let attributes = parse_blade_attributes(attributes_str);
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                let (name, namespace) = if component_name.contains('.') {
                    let parts: Vec<&str> = component_name.splitn(2, '.').collect();
                    (parts[1].to_string(), Some(parts[0].to_string()))
                } else {
                    (component_name, None)
                };
                
                components.push(BladeComponent {
                    name,
                    attributes,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    is_self_closing,
                    namespace,
                });
            }
        }
        
        components
    }
    
    async fn extract_blade_variables(&self, content: &str) -> Vec<BladeVariable> {
        let mut variables = Vec::new();
        
        // Escaped variables {{ }}
        let escaped_regex = regex::Regex::new(r"\{\{\s*([^}]+)\s*\}\}").unwrap();
        // Unescaped variables {!! !!}
        let unescaped_regex = regex::Regex::new(r"\{!!\s*([^}]+)\s*!!\}").unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            // Process escaped variables
            for caps in escaped_regex.captures_iter(line) {
                let expression = caps.get(1).unwrap().as_str().trim().to_string();
                let variable_name = extract_variable_name(&expression);
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                variables.push(BladeVariable {
                    expression,
                    variable_name,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    is_escaped: true,
                });
            }
            
            // Process unescaped variables
            for caps in unescaped_regex.captures_iter(line) {
                let expression = caps.get(1).unwrap().as_str().trim().to_string();
                let variable_name = extract_variable_name(&expression);
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                variables.push(BladeVariable {
                    expression,
                    variable_name,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    is_escaped: false,
                });
            }
        }
        
        variables
    }
    
    async fn extract_blade_sections(&self, content: &str) -> Vec<BladeSection> {
        let mut sections = Vec::new();
        let section_regex = regex::Regex::new(r#"@section\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            for caps in section_regex.captures_iter(line) {
                let section_name = caps.get(1).unwrap().as_str().to_string();
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                sections.push(BladeSection {
                    name: section_name,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    has_content: true, // Simplified for now
                });
            }
        }
        
        sections
    }
    
    async fn extract_blade_includes(&self, content: &str) -> Vec<BladeInclude> {
        let mut includes = Vec::new();
        let include_regex = regex::Regex::new(r#"@(include(?:If|When|Unless|First)?)\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            for caps in include_regex.captures_iter(line) {
                let include_directive = caps.get(1).unwrap().as_str();
                let template_name = caps.get(2).unwrap().as_str().to_string();
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                let include_type = match include_directive {
                    "includeIf" => BladeIncludeType::IncludeIf,
                    "includeWhen" => BladeIncludeType::IncludeWhen,
                    "includeUnless" => BladeIncludeType::IncludeUnless,
                    "includeFirst" => BladeIncludeType::IncludeFirst,
                    _ => BladeIncludeType::Include,
                };
                
                includes.push(BladeInclude {
                    template_name,
                    parameters: Vec::new(), // Simplified for now
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                    include_type,
                });
            }
        }
        
        includes
    }
    
    async fn extract_blade_extends(&self, content: &str) -> Vec<BladeExtends> {
        let mut extends = Vec::new();
        let extends_regex = regex::Regex::new(r#"@extends\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
        
        for (line_num, line) in content.lines().enumerate() {
            for caps in extends_regex.captures_iter(line) {
                let template_name = caps.get(1).unwrap().as_str().to_string();
                let start_col = caps.get(0).unwrap().start() as u32;
                let end_col = caps.get(0).unwrap().end() as u32;
                
                extends.push(BladeExtends {
                    template_name,
                    line: line_num as u32,
                    column: start_col,
                    end_column: end_col,
                });
            }
        }
        
        extends
    }

}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_rate: f64,
    pub blade_templates: u64,
    pub route_files: u64,
    pub model_files: u64,
}

// Helper functions

fn parse_blade_attributes(attrs_str: &str) -> Vec<BladeAttribute> {
    let mut attributes = Vec::new();
    let attr_regex = regex::Regex::new(r#"([:@]?)([a-zA-Z\-:]+)(?:=(['"])([^'"]*)\3)?"#).unwrap();
    
    for caps in attr_regex.captures_iter(attrs_str) {
        let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = caps.get(2).unwrap().as_str().to_string();
        let value = caps.get(4).map(|m| m.as_str().to_string());
        
        let is_binding = prefix == ":";
        let is_event = prefix == "@";
        
        attributes.push(BladeAttribute {
            name,
            value,
            is_binding,
            is_event,
        });
    }
    
    attributes
}

fn extract_variable_name(expression: &str) -> Option<String> {
    let var_regex = regex::Regex::new(r"\$([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    var_regex
        .captures(expression)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn classify_blade_directive(name: &str) -> BladeDirectiveType {
    match name {
        "if" | "unless" | "elseif" | "else" | "endif" | "endunless" | 
        "switch" | "case" | "break" | "default" | "endswitch" => BladeDirectiveType::Conditional,
        
        "foreach" | "endforeach" | "for" | "endfor" | "while" | "endwhile" |
        "forelse" | "empty" => BladeDirectiveType::Loop,
        
        "include" | "includeIf" | "includeWhen" | "includeUnless" | "includeFirst" => BladeDirectiveType::Include,
        
        "extends" | "section" | "endsection" | "yield" | "parent" | "show" => BladeDirectiveType::Template,
        
        "component" | "endcomponent" | "slot" | "endslot" => BladeDirectiveType::Component,
        
        "auth" | "endauth" | "guest" | "endguest" => BladeDirectiveType::Auth,
        
        _ => BladeDirectiveType::Custom,
    }
}

/// Get documentation for Blade directives
pub fn get_directive_documentation(directive: &str) -> &'static str {
    match directive {
        "if" => "Conditional statement. Usage: @if($condition) ... @endif",
        "foreach" => "Loop through arrays. Usage: @foreach($items as $item) ... @endforeach",
        "include" => "Include another Blade template. Usage: @include('partial')",
        "extends" => "Extend a parent template. Usage: @extends('layouts.app')",
        "section" => "Define a content section. Usage: @section('content') ... @endsection",
        "yield" => "Output a section's content. Usage: @yield('content')",
        "csrf" => "Generate CSRF token field. Usage: @csrf",
        "method" => "Generate method field for forms. Usage: @method('PUT')",
        "auth" => "Check if user is authenticated. Usage: @auth ... @endauth",
        "guest" => "Check if user is guest. Usage: @guest ... @endguest",
        "can" => "Check user permissions. Usage: @can('update', $post) ... @endcan",
        "component" => "Use a Blade component. Usage: @component('alert') ... @endcomponent",
        "slot" => "Define component slot. Usage: @slot('title') ... @endslot",
        "push" => "Push content to a stack. Usage: @push('scripts') ... @endpush",
        "stack" => "Output a stack's content. Usage: @stack('scripts')",
        "lang" => "Retrieve translation. Usage: @lang('messages.welcome')",
        "json" => "Output JSON-encoded data. Usage: @json($data)",
        "error" => "Display validation errors. Usage: @error('field') ... @enderror",
        "env" => "Get environment variable. Usage: @env('APP_NAME')",
        "production" => "Check if in production. Usage: @production ... @endproduction",
        "dd" => "Dump and die. Usage: @dd($variable)",
        "dump" => "Dump variable. Usage: @dump($variable)",
        _ => "Blade directive - see Laravel documentation for details"
    }
}