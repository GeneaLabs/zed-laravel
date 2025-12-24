//! LRU-based cache system for Laravel Language Server
//! 
//! This module implements a high-performance caching system using LRU (Least Recently Used)
//! eviction policy following industry standards used by rust-analyzer and TypeScript LSP.
//! 
//! Key features:
//! - Memory bounded with configurable limits
//! - Thread-safe with async support
//! - Automatic invalidation on file changes
//! - Industry-standard LRU eviction policy

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, Semaphore};
use lru::LruCache;
use std::num::NonZeroUsize;
use lsp_types::{Url, Position, Hover, Range, HoverContents, MarkupContent, MarkupKind};

/// Information about a Laravel pattern found in code
#[derive(Debug, Clone)]
pub struct PatternInfo {
    pub pattern_type: String,
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub range: Range,
}

/// Cache entry for patterns in a file
#[derive(Debug, Clone)]
pub struct PatternCache {
    /// File version when this was cached
    pub version: i32,
    /// When this was cached
    pub cached_at: Instant,
    /// All patterns found in the file
    pub patterns: HashMap<String, Vec<PatternInfo>>,
}

/// Key for hover cache entries
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct HoverKey {
    pub uri: Url,
    pub line: u32,
    pub character: u32,
    pub version: i32,
}

/// Cached hover result
#[derive(Debug, Clone)]
pub struct CachedHover {
    pub hover: Option<Hover>,
    pub cached_at: Instant,
}

/// Cache statistics for monitoring and debugging
#[derive(Debug, Default)]
pub struct CacheStats {
    pub pattern_cache_hits: u64,
    pub pattern_cache_misses: u64,
    pub hover_cache_hits: u64,
    pub hover_cache_misses: u64,
    pub total_invalidations: u64,
    pub concurrent_computations: u64,
    pub max_concurrent_reached: u64,
    pub stampede_prevented: u64,
}

/// High-performance LRU-based cache with memory management
pub struct PerformanceCache {
    /// LRU cache for parsed patterns: URI -> PatternCache
    pattern_cache: RwLock<LruCache<Url, PatternCache>>,
    /// LRU cache for hover results: (URI, Position, Version) -> Hover
    hover_cache: RwLock<LruCache<HoverKey, CachedHover>>,
    /// Cache statistics
    stats: RwLock<CacheStats>,
    /// Semaphore to limit concurrent pattern computations (prevents stampeding)
    pattern_computation_semaphore: Arc<Semaphore>,
    /// Semaphore to limit concurrent hover computations
    hover_computation_semaphore: Arc<Semaphore>,
    /// Active computations tracker to prevent duplicate work
    active_computations: RwLock<HashMap<String, Arc<tokio::sync::Notify>>>,
}

impl PerformanceCache {
    /// Create a new cache with industry-standard LRU limits
    pub fn new() -> Self {
        // Industry standard cache sizes based on research
        // - Pattern cache: 2000 files (handles medium-large projects)
        // - Hover cache: 1000 recent requests (good balance of hit rate vs memory)
        let pattern_capacity = NonZeroUsize::new(2000).unwrap();
        let hover_capacity = NonZeroUsize::new(1000).unwrap();

        Self {
            pattern_cache: RwLock::new(LruCache::new(pattern_capacity)),
            hover_cache: RwLock::new(LruCache::new(hover_capacity)),
            stats: RwLock::new(CacheStats::default()),
            // Allow up to 4 concurrent pattern computations (CPU bound)
            pattern_computation_semaphore: Arc::new(Semaphore::new(4)),
            // Allow up to 8 concurrent hover computations (lighter weight)
            hover_computation_semaphore: Arc::new(Semaphore::new(8)),
            active_computations: RwLock::new(HashMap::new()),
        }
    }

    /// Update patterns for a file (immediate, with stampede protection)
    pub async fn update_patterns(&self, uri: Url, content: String, version: i32) {
        // Create computation key for deduplication
        let computation_key = format!("pattern_{}_{}", uri, version);
        
        // Check if this computation is already in progress
        let notify = {
            let mut active = self.active_computations.write().await;
            if let Some(existing_notify) = active.get(&computation_key) {
                // Another thread is already computing this, wait for it
                let notify = existing_notify.clone();
                drop(active);
                
                // Record stampede prevention
                {
                    let mut stats = self.stats.write().await;
                    stats.stampede_prevented += 1;
                }
                
                notify.notified().await;
                return;
            } else {
                // We're the first to compute this, register ourselves
                let notify = Arc::new(tokio::sync::Notify::new());
                active.insert(computation_key.clone(), notify.clone());
                notify
            }
        };
        
        // Acquire semaphore to limit concurrent computations
        let _permit = self.pattern_computation_semaphore.acquire().await.unwrap();
        
        // Update concurrent computation stats
        {
            let mut stats = self.stats.write().await;
            stats.concurrent_computations += 1;
            let current_permits = 4 - self.pattern_computation_semaphore.available_permits();
            stats.max_concurrent_reached = stats.max_concurrent_reached.max(current_permits as u64);
        }

        let patterns = self.extract_patterns_from_content(&uri, &content).await;
        
        let pattern_cache = PatternCache {
            version,
            cached_at: Instant::now(),
            patterns,
        };

        // Update pattern cache (LRU handles eviction automatically)
        {
            let mut cache_guard = self.pattern_cache.write().await;
            cache_guard.put(uri.clone(), pattern_cache);
        }

        // Invalidate related hover cache entries
        {
            let mut hover_guard = self.hover_cache.write().await;
            
            // Remove all hover entries for this URI
            let keys_to_remove: Vec<HoverKey> = hover_guard
                .iter()
                .filter(|(key, _)| key.uri == uri)
                .map(|(key, _)| key.clone())
                .collect();
            
            for key in keys_to_remove {
                hover_guard.pop(&key);
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.total_invalidations += 1;
        }
        
        // Notify waiting threads and remove from active computations
        {
            let mut active = self.active_computations.write().await;
            active.remove(&computation_key);
        }
        notify.notify_waiters();
    }

    /// Get hover information (with LRU caching and stampede protection)
    pub async fn get_hover(&self, uri: Url, position: Position, version: i32) -> Option<Hover> {
        let hover_key = HoverKey {
            uri: uri.clone(),
            line: position.line,
            character: position.character,
            version,
        };

        // Check hover cache first
        {
            let mut hover_guard = self.hover_cache.write().await;
            if let Some(cached) = hover_guard.get(&hover_key) {
                // Cache hit - LRU automatically updates position
                let mut stats = self.stats.write().await;
                stats.hover_cache_hits += 1;
                return cached.hover.clone();
            }
        }

        // Create computation key for deduplication
        let computation_key = format!("hover_{}_{}_{}", uri, position.line, position.character);
        
        // Check if this computation is already in progress
        let notify = {
            let mut active = self.active_computations.write().await;
            if let Some(existing_notify) = active.get(&computation_key) {
                // Another thread is already computing this, wait for it
                let notify = existing_notify.clone();
                drop(active);
                
                // Record stampede prevention
                {
                    let mut stats = self.stats.write().await;
                    stats.stampede_prevented += 1;
                }
                
                notify.notified().await;
                
                // After waiting, check cache again
                let mut hover_guard = self.hover_cache.write().await;
                if let Some(cached) = hover_guard.get(&hover_key) {
                    return cached.hover.clone();
                }
                return None;
            } else {
                // We're the first to compute this, register ourselves
                let notify = Arc::new(tokio::sync::Notify::new());
                active.insert(computation_key.clone(), notify.clone());
                notify
            }
        };

        // Acquire semaphore to limit concurrent computations
        let _permit = self.hover_computation_semaphore.acquire().await.unwrap();
        
        // Update concurrent computation stats
        {
            let mut stats = self.stats.write().await;
            stats.concurrent_computations += 1;
            let current_permits = 8 - self.hover_computation_semaphore.available_permits();
            stats.max_concurrent_reached = stats.max_concurrent_reached.max(current_permits as u64);
        }

        // Cache miss - compute hover
        let hover = self.compute_hover(&uri, position, version).await;

        // Cache the result (LRU handles eviction automatically)
        {
            let mut hover_guard = self.hover_cache.write().await;
            hover_guard.put(hover_key, CachedHover {
                hover: hover.clone(),
                cached_at: Instant::now(),
            });
        }

        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.hover_cache_misses += 1;
        }
        
        // Notify waiting threads and remove from active computations
        {
            let mut active = self.active_computations.write().await;
            active.remove(&computation_key);
        }
        notify.notify_waiters();

        hover
    }

    /// Get patterns for a file (with LRU caching)
    pub async fn get_patterns(&self, uri: &Url, version: i32) -> Option<HashMap<String, Vec<PatternInfo>>> {
        let mut cache_guard = self.pattern_cache.write().await;
        
        if let Some(cached) = cache_guard.get(uri) {
            if cached.version == version {
                // Cache hit - LRU automatically updates position
                let mut stats = self.stats.write().await;
                stats.pattern_cache_hits += 1;
                return Some(cached.patterns.clone());
            }
        }

        // Cache miss
        let mut stats = self.stats.write().await;
        stats.pattern_cache_misses += 1;
        None
    }

    /// Check if system is under heavy load
    pub async fn is_under_load(&self) -> bool {
        let pattern_permits = self.pattern_computation_semaphore.available_permits();
        let hover_permits = self.hover_computation_semaphore.available_permits();
        
        // Consider under load if less than 25% capacity available
        pattern_permits < 1 || hover_permits < 2
    }

    /// Get performance report for debugging
    pub async fn get_performance_report(&self) -> String {
        let stats = self.stats.read().await;
        let pattern_cache = self.pattern_cache.read().await;
        let hover_cache = self.hover_cache.read().await;
        
        format!(
            "🚀 LRU Cache Performance Report:\n\
             📊 Pattern Cache: {} / {} entries\n\
             📊 Hover Cache: {} / {} entries\n\
             ✅ Pattern hits: {} / misses: {}\n\
             ✅ Hover hits: {} / misses: {}\n\
             🔄 Total invalidations: {}\n\
             🚦 Max concurrent: {} / Stampede prevented: {}\n\
             💾 Memory: Bounded by LRU limits",
            pattern_cache.len(), pattern_cache.cap(),
            hover_cache.len(), hover_cache.cap(),
            stats.pattern_cache_hits, stats.pattern_cache_misses,
            stats.hover_cache_hits, stats.hover_cache_misses,
            stats.total_invalidations,
            stats.max_concurrent_reached, stats.stampede_prevented
        )
    }

    /// Compute hover information for a position
    async fn compute_hover(&self, uri: &Url, position: Position, version: i32) -> Option<Hover> {
        // Get patterns from cache
        let patterns = self.get_patterns(uri, version).await?;
        
        // Find pattern at cursor position
        for (pattern_type, pattern_list) in &patterns {
            for pattern in pattern_list {
                if self.position_in_range(position, pattern.range) {
                    let hover_text = self.generate_hover_content(pattern_type, &pattern.text);
                    
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: hover_text,
                        }),
                        range: Some(pattern.range),
                    });
                }
            }
        }
        
        None
    }

    /// Extract Laravel patterns from file content
    async fn extract_patterns_from_content(&self, uri: &Url, content: &str) -> HashMap<String, Vec<PatternInfo>> {
        let mut patterns = HashMap::new();
        
        // Skip empty or very small files
        if content.len() < 10 {
            return patterns;
        }
        
        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;
        
        if is_blade {
            self.extract_blade_patterns(content, &mut patterns);
        } else if is_php {
            self.extract_php_patterns(content, &mut patterns);
        }
        
        patterns
    }

    /// Extract Laravel patterns from Blade files
    fn extract_blade_patterns(&self, content: &str, patterns: &mut HashMap<String, Vec<PatternInfo>>) {
        use regex::Regex;
        use lazy_static::lazy_static;
        
        lazy_static! {
            static ref BLADE_DIRECTIVES: Regex = Regex::new(r"@(\w+)").unwrap();
            static ref BLADE_COMPONENTS: Regex = Regex::new(r"<x-([a-zA-Z0-9\-\.]+)").unwrap();
            static ref BLADE_INCLUDES: Regex = Regex::new(r#"@include\s*\(\s*['""]([^'"]*)['""]"#).unwrap();
            static ref BLADE_EXTENDS: Regex = Regex::new(r#"@extends\s*\(\s*['""]([^'"]*)['""]"#).unwrap();
        }
        
        // Extract Blade directives
        for captures in BLADE_DIRECTIVES.find_iter(content) {
            let directive = captures.as_str();
            let (line, col) = self.get_line_col(content, captures.start());
            
            patterns.entry("blade_directive".to_string()).or_insert_with(Vec::new).push(PatternInfo {
                pattern_type: "blade_directive".to_string(),
                row: line,
                col,
                text: directive.to_string(),
                range: Range {
                    start: Position { line: line as u32, character: col as u32 },
                    end: Position { line: line as u32, character: (col + directive.len()) as u32 },
                },
            });
        }
        
        // Extract other Blade patterns...
        // (Similar implementation for components, includes, extends)
    }

    /// Extract Laravel patterns from PHP files  
    fn extract_php_patterns(&self, content: &str, patterns: &mut HashMap<String, Vec<PatternInfo>>) {
        use regex::Regex;
        use lazy_static::lazy_static;
        
        lazy_static! {
            static ref VIEW_CALLS: Regex = Regex::new(r#"view\s*\(\s*['""]([^'"]*)['""]"#).unwrap();
            static ref ROUTE_CALLS: Regex = Regex::new(r#"route\s*\(\s*['""]([^'"]*)['""]"#).unwrap();
            static ref CONFIG_CALLS: Regex = Regex::new(r#"config\s*\(\s*['""]([^'"]*)['""]"#).unwrap();
        }
        
        // Extract view() calls
        for captures in VIEW_CALLS.captures_iter(content) {
            if let Some(view_match) = captures.get(1) {
                let view_name = view_match.as_str();
                let (line, col) = self.get_line_col(content, view_match.start());
                
                patterns.entry("view_call".to_string()).or_insert_with(Vec::new).push(PatternInfo {
                    pattern_type: "view_call".to_string(),
                    row: line,
                    col,
                    text: view_name.to_string(),
                    range: Range {
                        start: Position { line: line as u32, character: col as u32 },
                        end: Position { line: line as u32, character: (col + view_name.len()) as u32 },
                    },
                });
            }
        }
        
        // Extract other PHP patterns... (route, config, etc.)
    }

    /// Generate hover content based on pattern type and text
    fn generate_hover_content(&self, pattern_type: &str, text: &str) -> String {
        match pattern_type {
            "blade_directive" => {
                format!("**Blade Directive**: `{}`\n\nBlade template directive for controlling template flow and functionality.", text)
            }
            "blade_component" => {
                format!("**Blade Component**: `{}`\n\nBlade component reference. Components are reusable UI elements.", text)
            }
            "blade_include" => {
                format!("**Blade Include**: `{}`\n\nIncludes another Blade template at this location.", text)
            }
            "view_call" => {
                format!("**Laravel View**: `{}`\n\nReferences a Blade template in the `resources/views` directory.", text)
            }
            "route_call" => {
                format!("**Laravel Route**: `{}`\n\nReferences a named route defined in your route files.", text)
            }
            "config_call" => {
                format!("**Laravel Config**: `{}`\n\nAccesses a configuration value from the `config` directory.", text)
            }
            _ => format!("**Laravel Pattern**: `{}`", text),
        }
    }

    /// Check if a position is within a range
    fn position_in_range(&self, position: Position, range: Range) -> bool {
        if position.line < range.start.line || position.line > range.end.line {
            return false;
        }
        
        if position.line == range.start.line && position.character < range.start.character {
            return false;
        }
        
        if position.line == range.end.line && position.character > range.end.character {
            return false;
        }
        
        true
    }

    /// Get line and column number from byte offset
    fn get_line_col(&self, content: &str, offset: usize) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        
        for (i, ch) in content.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        
        (line, col)
    }
}

impl Default for PerformanceCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe wrapper for the LRU-based cache
pub type ThreadSafeCache = Arc<PerformanceCache>;

/// Create a new thread-safe LRU-based cache
pub fn create_performance_cache() -> ThreadSafeCache {
    Arc::new(PerformanceCache::new())
}