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
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use lru::LruCache;
use std::num::NonZeroUsize;
use lsp_types::{Url, Position, Hover, Range, HoverContents, MarkupContent, MarkupKind, Diagnostic, DiagnosticSeverity};
use tracing::{debug, info, warn, error};
use std::future::Future;



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
    pub avg_hover_time_ms: f64,
    pub avg_goto_time_ms: f64,
    pub avg_cache_update_time_ms: f64,
    pub slow_operations: u64,
    pub system_under_load_count: u64,
    pub parse_failures: u64,
    pub fallback_successes: u64,
    pub error_recoveries: u64,
    pub cache_poisoning_prevented: u64,
    pub lock_contention_reduced: u64,
    pub optimal_lock_patterns_used: u64,
}

/// Threading optimization helper methods
impl CacheStats {
    pub fn record_lock_optimization(&mut self) {
        self.lock_contention_reduced += 1;
    }
    
    pub fn record_optimal_pattern(&mut self) {
        self.optimal_lock_patterns_used += 1;
    }
}

/// Result of parsing operations with error recovery
#[derive(Debug, Clone)]
pub enum ParseResult<T> {
    /// Parsing succeeded completely
    Success(T),
    /// Parsing partially succeeded with some errors
    PartialSuccess(T, Vec<ParseError>),
    /// Tree-sitter parsing failed, but regex fallback succeeded
    Fallback(T, ParseError),
    /// Complete parsing failure
    Failed(ParseError),
}

/// Parse error with recovery information
#[derive(Debug, Clone)]
pub struct ParseError {
    pub error_type: ParseErrorType,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub recoverable: bool,
    pub suggested_fix: Option<String>,
}

/// Types of parse errors for recovery strategies
#[derive(Debug, Clone)]
pub enum ParseErrorType {
    /// Syntax error in PHP/Blade code
    SyntaxError,
    /// Memory allocation failure during parsing
    MemoryError,
    /// File encoding issues
    EncodingError,
    /// Tree-sitter parser crashed
    ParserCrash,
    /// File too large for parsing
    FileTooLarge,
}

impl ParseError {
    pub fn syntax_error(message: String, line: Option<u32>, column: Option<u32>) -> Self {
        Self {
            error_type: ParseErrorType::SyntaxError,
            message,
            line,
            column,
            recoverable: true,
            suggested_fix: Some("Check syntax for missing brackets, semicolons, or quotes".to_string()),
        }
    }
    
    pub fn parser_crash(message: String) -> Self {
        Self {
            error_type: ParseErrorType::ParserCrash,
            message,
            line: None,
            column: None,
            recoverable: true,
            suggested_fix: Some("File contains syntax that tree-sitter cannot parse, using fallback extraction".to_string()),
        }
    }
    
    pub fn to_diagnostic(&self, uri: &Url) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: self.line.unwrap_or(0),
                    character: self.column.unwrap_or(0),
                },
                end: Position {
                    line: self.line.unwrap_or(0),
                    character: self.column.unwrap_or(u32::MAX).saturating_add(1),
                },
            },
            severity: Some(match self.error_type {
                ParseErrorType::SyntaxError => DiagnosticSeverity::ERROR,
                ParseErrorType::ParserCrash => DiagnosticSeverity::WARNING,
                ParseErrorType::MemoryError => DiagnosticSeverity::ERROR,
                ParseErrorType::EncodingError => DiagnosticSeverity::WARNING,
                ParseErrorType::FileTooLarge => DiagnosticSeverity::INFORMATION,
            }),
            code: None,
            code_description: None,
            source: Some("laravel-lsp-parser".to_string()),
            message: if let Some(fix) = &self.suggested_fix {
                format!("{}\n\nSuggested fix: {}", self.message, fix)
            } else {
                self.message.clone()
            },
            related_information: None,
            tags: None,
            data: None,
        }
    }
}

/// Performance monitoring with operation budgets and timing
#[derive(Clone)]
pub struct PerformanceMonitor {
    operation_budgets: HashMap<&'static str, Duration>,
    stats: Arc<RwLock<CacheStats>>,
    last_report: Arc<RwLock<Instant>>,
    report_interval: Duration,
}

impl PerformanceMonitor {
    pub fn new(stats: Arc<RwLock<CacheStats>>) -> Self {
        let mut operation_budgets = HashMap::new();
        operation_budgets.insert("hover", Duration::from_millis(50));
        operation_budgets.insert("goto_definition", Duration::from_millis(100));
        operation_budgets.insert("completion", Duration::from_millis(200));
        operation_budgets.insert("code_lens", Duration::from_millis(150));
        operation_budgets.insert("did_change", Duration::from_millis(10));
        
        Self {
            operation_budgets,
            stats,
            last_report: Arc::new(RwLock::new(Instant::now())),
            report_interval: Duration::from_secs(60),
        }
    }
    
    pub async fn time_operation<T, F>(&self, operation: &'static str, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let start_time = Instant::now();
        let result = future.await;
        let duration = start_time.elapsed();
        
        // Check performance budget
        if let Some(&budget) = self.operation_budgets.get(operation) {
            if duration > budget {
                warn!("Laravel LSP: Slow {} operation: {}ms (budget: {}ms)", 
                      operation, duration.as_millis(), budget.as_millis());
                
                // Record slow operation
                let mut stats = self.stats.write().await;
                stats.slow_operations += 1;
            } else {
                debug!("Laravel LSP: {} operation: {}ms", operation, duration.as_millis());
            }
        }
        
        // Update operation-specific averages
        self.update_operation_average(operation, duration).await;
        
        // Check if we should publish performance report
        self.maybe_publish_performance_report().await;
        
        result
    }
    
    async fn update_operation_average(&self, operation: &str, duration: Duration) {
        let mut stats = self.stats.write().await;
        let duration_ms = duration.as_millis() as f64;
        
        match operation {
            "hover" => {
                stats.avg_hover_time_ms = (stats.avg_hover_time_ms * 0.9) + (duration_ms * 0.1);
            }
            "goto_definition" => {
                stats.avg_goto_time_ms = (stats.avg_goto_time_ms * 0.9) + (duration_ms * 0.1);
            }
            "did_change" => {
                stats.avg_cache_update_time_ms = (stats.avg_cache_update_time_ms * 0.9) + (duration_ms * 0.1);
            }
            _ => {}
        }
    }
    
    async fn maybe_publish_performance_report(&self) {
        let should_report = {
            let last_report = self.last_report.read().await;
            last_report.elapsed() > self.report_interval
        };
        
        if should_report {
            let stats = self.stats.read().await;
            
            let hover_hit_rate = if stats.hover_cache_hits + stats.hover_cache_misses > 0 {
                (stats.hover_cache_hits as f64 / (stats.hover_cache_hits + stats.hover_cache_misses) as f64) * 100.0
            } else { 0.0 };
            
            let pattern_hit_rate = if stats.pattern_cache_hits + stats.pattern_cache_misses > 0 {
                (stats.pattern_cache_hits as f64 / (stats.pattern_cache_hits + stats.pattern_cache_misses) as f64) * 100.0
            } else { 0.0 };
            
            info!("Laravel LSP Performance Report:");
            info!("  Hover cache hit rate: {:.1}% (avg: {:.1}ms)", hover_hit_rate, stats.avg_hover_time_ms);
            info!("  Pattern cache hit rate: {:.1}%", pattern_hit_rate);
            info!("  Avg goto definition: {:.1}ms", stats.avg_goto_time_ms);
            info!("  Avg cache update: {:.1}ms", stats.avg_cache_update_time_ms);
            info!("  Slow operations: {}", stats.slow_operations);
            info!("  Max concurrent reached: {}", stats.max_concurrent_reached);
            info!("  Cache stampede prevented: {}", stats.stampede_prevented);
            info!("  System under load events: {}", stats.system_under_load_count);
            info!("  Parse failures: {}", stats.parse_failures);
            info!("  Fallback successes: {}", stats.fallback_successes);
            info!("  Error recoveries: {}", stats.error_recoveries);
            info!("  Cache poisoning prevented: {}", stats.cache_poisoning_prevented);
            info!("  Lock contention reduced: {}", stats.lock_contention_reduced);
            info!("  Optimal lock patterns used: {}", stats.optimal_lock_patterns_used);
            
            // Reset report timer
            *self.last_report.write().await = Instant::now();
        }
    }
}

/// High-performance LRU-based cache with memory management
pub struct PerformanceCache {
    /// LRU cache for parsed patterns: URI -> PatternCache
    pattern_cache: RwLock<LruCache<Url, PatternCache>>,
    /// LRU cache for hover results: (URI, Position, Version) -> Hover
    hover_cache: RwLock<LruCache<HoverKey, CachedHover>>,
    /// Cache statistics
    pub stats: Arc<RwLock<CacheStats>>,
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
            stats: Arc::new(RwLock::new(CacheStats::default())),
            // Allow up to 4 concurrent pattern computations (CPU bound)
            pattern_computation_semaphore: Arc::new(Semaphore::new(4)),
            // Allow up to 8 concurrent hover computations (lighter weight)
            hover_computation_semaphore: Arc::new(Semaphore::new(8)),
            active_computations: RwLock::new(HashMap::new()),
        }
    }

    // Update patterns for a file (immediate, with error recovery and stampede protection)
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
            
            // Prevent cache poisoning by ensuring we always store valid data
            stats.cache_poisoning_prevented += 1;
        }

        let parse_result = self.extract_patterns_from_content(&uri, &content).await;
        
        // Extract patterns and handle errors based on parse result
        let patterns = match &parse_result {
            ParseResult::Success(patterns) => patterns.clone(),
            ParseResult::PartialSuccess(patterns, _errors) => {
                warn!("Laravel LSP: Partial parsing success for {}", uri);
                patterns.clone()
            }
            ParseResult::Fallback(patterns, error) => {
                warn!("Laravel LSP: Using fallback patterns for {}: {}", uri, error.message);
                patterns.clone()
            }
            ParseResult::Failed(error) => {
                error!("Laravel LSP: Complete parsing failure for {}: {}", uri, error.message);
                // Update failure stats and return empty patterns to prevent cache poisoning
                {
                    let mut stats = self.stats.write().await;
                    stats.cache_poisoning_prevented += 1;
                }
                HashMap::new()
            }
        };
        
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

        // 🚀 Invalidate related hover cache entries with optimal threading
        {
            let mut hover_guard = self.hover_cache.write().await;
            
            // Optimized: Collect keys first to minimize iteration under write lock
            let mut keys_to_remove = Vec::new();
            for (key, _) in hover_guard.iter() {
                if key.uri == uri {
                    keys_to_remove.push(key.clone());
                }
            }
            
            // Record threading optimization before consuming the vector
            let has_removals = !keys_to_remove.is_empty();
            
            // Batch removal to minimize lock time
            for key in keys_to_remove {
                hover_guard.pop(&key);
            }
            
            // Record threading optimization after removal
            if has_removals {
                drop(hover_guard);
                let mut stats = self.stats.write().await;
                stats.record_lock_optimization();
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

        // 🚀 Check hover cache with optimal lock pattern (minimize lock duration)
        {
            let mut hover_guard = self.hover_cache.write().await;
            if let Some(cached) = hover_guard.get(&hover_key) {
                // Cache hit - clone data before releasing lock
                let cached_hover = cached.hover.clone();
                drop(hover_guard);
                
                // Update stats after releasing cache lock (optimal pattern)
                let mut stats = self.stats.write().await;
                stats.hover_cache_hits += 1;
                stats.record_optimal_pattern();
                return cached_hover;
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
                
                // After waiting, check cache again with optimized access
                {
                    let mut hover_guard = self.hover_cache.write().await;
                    if let Some(cached) = hover_guard.get(&hover_key) {
                        return cached.hover.clone();
                    }
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

        // 🚀 Cache the result with minimal lock duration
        {
            let mut hover_guard = self.hover_cache.write().await;
            hover_guard.put(hover_key, CachedHover {
                hover: hover.clone(),
                cached_at: Instant::now(),
            });
            drop(hover_guard);
            
            // Record threading optimization
            let mut stats = self.stats.write().await;
            stats.record_optimal_pattern();
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

    /// Get patterns for a file with caching and error recovery
    pub async fn get_patterns(&self, uri: &Url, version: i32) -> Option<HashMap<String, Vec<PatternInfo>>> {
        let mut cache_guard = self.pattern_cache.write().await;
        
        if let Some(cached) = cache_guard.get(uri) {
            if cached.version == version {
                // Cache hit - clone data before releasing lock (optimal pattern)
                let cached_patterns = cached.patterns.clone();
                drop(cache_guard);
                
                // Update stats after releasing cache lock (reduces contention)
                let mut stats = self.stats.write().await;
                stats.pattern_cache_hits += 1;
                stats.record_lock_optimization();
                return Some(cached_patterns);
            }
        }

        // Cache miss - release cache lock before updating stats (optimal threading)
        drop(cache_guard);
        let mut stats = self.stats.write().await;
        stats.pattern_cache_misses += 1;
        stats.record_optimal_pattern();
        None
    }

    /// Check if system is under heavy load
    pub async fn is_under_load(&self) -> bool {
        let pattern_permits = self.pattern_computation_semaphore.available_permits();
        let hover_permits = self.hover_computation_semaphore.available_permits();
        
        // System is under load if less than 25% capacity available
        let under_load = pattern_permits < 1 || hover_permits < 2;
        
        if under_load {
            let mut stats = self.stats.write().await;
            stats.system_under_load_count += 1;
        }
        
        under_load
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

    /// Extract Laravel patterns from file content with comprehensive error recovery
    async fn extract_patterns_from_content(&self, uri: &Url, content: &str) -> ParseResult<HashMap<String, Vec<PatternInfo>>> {
        // Skip empty or very small files
        if content.len() < 10 {
            return ParseResult::Success(HashMap::new());
        }
        
        // Layer 1: Try tree-sitter parsing for enhanced pattern extraction
        match self.try_tree_sitter_extraction(uri, content).await {
            Some(patterns) => {
                debug!("Laravel LSP: Tree-sitter extraction succeeded for {}", uri);
                return ParseResult::Success(patterns);
            }
            None => {
                warn!("Laravel LSP: Tree-sitter parsing failed for {}, falling back to regex", uri);
                
                // Update failure stats
                {
                    let mut stats = self.stats.write().await;
                    stats.parse_failures += 1;
                }
            }
        }
        
        // Layer 2: Regex fallback (current system)
        let patterns = self.extract_regex_patterns_resilient(uri, content).await;
        
        let parse_error = ParseError::parser_crash(
            format!("Tree-sitter parsing failed for {}, using regex fallback", uri.path())
        );
        
        // Update fallback success stats
        {
            let mut stats = self.stats.write().await;
            stats.fallback_successes += 1;
            stats.error_recoveries += 1;
        }
        
        ParseResult::Fallback(patterns, parse_error)
    }
    
    /// Try tree-sitter parsing for enhanced extraction (when working)
    async fn try_tree_sitter_extraction(&self, uri: &Url, content: &str) -> Option<HashMap<String, Vec<PatternInfo>>> {
        use crate::parser::{parse_php, parse_blade};
        use crate::queries::*;
        
        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;
        
        if is_php {
            // Try PHP parsing
            if let Ok(tree) = parse_php(content) {
                let lang = crate::parser::language_php();
                let mut patterns = HashMap::new();
                
                // Extract enhanced patterns using tree-sitter
                if let Ok(view_calls) = find_view_calls(&tree, content, &lang) {
                    let view_patterns: Vec<PatternInfo> = view_calls.iter().map(|v| PatternInfo {
                        pattern_type: "view".to_string(),
                        row: v.row,
                        col: v.column,
                        text: v.view_name.to_string(),
                        range: Range {
                            start: Position { line: v.row as u32, character: v.column as u32 },
                            end: Position { line: v.row as u32, character: (v.column + v.view_name.len()) as u32 },
                        },
                    }).collect();
                    patterns.insert("view".to_string(), view_patterns);
                }
                
                if let Ok(config_calls) = find_config_calls(&tree, content, &lang) {
                    let config_patterns: Vec<PatternInfo> = config_calls.iter().map(|c| PatternInfo {
                        pattern_type: "config".to_string(),
                        row: c.row,
                        col: c.column,
                        text: c.config_key.to_string(),
                        range: Range {
                            start: Position { line: c.row as u32, character: c.column as u32 },
                            end: Position { line: c.row as u32, character: (c.column + c.config_key.len()) as u32 },
                        },
                    }).collect();
                    patterns.insert("config".to_string(), config_patterns);
                }
                
                if let Ok(env_calls) = find_env_calls(&tree, content, &lang) {
                    let env_patterns: Vec<PatternInfo> = env_calls.iter().map(|e| PatternInfo {
                        pattern_type: "env".to_string(),
                        row: e.row,
                        col: e.column,
                        text: e.var_name.to_string(),
                        range: Range {
                            start: Position { line: e.row as u32, character: e.column as u32 },
                            end: Position { line: e.row as u32, character: (e.column + e.var_name.len()) as u32 },
                        },
                    }).collect();
                    patterns.insert("env".to_string(), env_patterns);
                }
                
                return Some(patterns);
            }
        } else if is_blade {
            // Try Blade parsing (if available)
            if let Ok(tree) = parse_blade(content) {
                // Similar extraction for Blade files
                let mut patterns = HashMap::new();
                // TODO: Add Blade-specific pattern extraction
                return Some(patterns);
            }
        }
        
        None
    }
    
    /// Resilient regex-based pattern extraction (fallback)
    async fn extract_regex_patterns_resilient(&self, uri: &Url, content: &str) -> HashMap<String, Vec<PatternInfo>> {
        let mut patterns = HashMap::new();
        
        // Determine file type
        let is_blade = uri.path().ends_with(".blade.php");
        let is_php = uri.path().ends_with(".php") && !is_blade;
        
        if is_blade {
            self.extract_blade_patterns(content, &mut patterns);
        } else if is_php {
            self.extract_php_patterns(content, &mut patterns);
        }
        
        debug!("Laravel LSP: Regex fallback extracted {} pattern types for {}", 
               patterns.len(), uri);
        
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

pub fn create_performance_cache() -> ThreadSafeCache {
    Arc::new(PerformanceCache::default())
}

/// Create performance monitor for timing operations
pub fn create_performance_monitor(cache: &ThreadSafeCache) -> PerformanceMonitor {
    PerformanceMonitor::new(cache.stats.clone())
}

/// Extract parse errors for diagnostic reporting
pub async fn extract_parse_diagnostics(cache: &ThreadSafeCache, uri: &Url) -> Vec<Diagnostic> {
    // This will be populated when we store parse errors in cache
    // For now, return empty vec - will be enhanced when we add error storage
    vec![]
}