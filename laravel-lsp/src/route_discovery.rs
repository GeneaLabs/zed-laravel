//! Route discovery — find named routes across the project, packages, and framework.
//!
//! Laravel route names are registered via `->name('X')` calls in many places:
//! - `routes/*.php` (project, recursively — catches `auth.php`, custom splits)
//! - `vendor/*/routes/*.php` (packages — Fortify, Telescope, Filament, Horizon, etc.)
//! - Service provider `boot()` methods that call `Route::get(...)->name(...)` directly
//! - Macro definitions in `Route::macro('foo', function () { ... })`
//! - Files registered via `loadRoutesFrom('path')` at non-standard locations
//! - Filament `Panel::routes(fn () => ...)` closures
//!
//! Rather than hard-code well-known files, this module discovers candidates by
//! scanning for files whose content shows route-registration shape (a route
//! facade/router token AND a `->name(` call). This naturally captures every
//! pattern listed above without needing per-package knowledge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::Node;
use walkdir::WalkDir;

use crate::vendor_index::{VendorFile, VendorIndex};

/// A `->group(...)`/`::group(...)` callsite that loads an *external file*
/// instead of running a closure body (issue #43). Laravel `require`s the file
/// and applies the group's attributes — including its `->as('admin.')` name
/// prefix — to every route declared in it.
#[derive(Debug, Clone)]
struct ExternalGroupLoad {
    /// The full name prefix this load contributes to the target file: every
    /// enclosing closure group's prefix followed by the load's own
    /// `->as(...)`/`->name(...)`/`['as' => …]` prefix. May be empty.
    edge_prefix: String,
    /// Resolved absolute path of the file this group loads.
    target: PathBuf,
}
/// A located route definition. Stored in [`RouteIndex`] keyed by route name.
#[derive(Debug, Clone)]
pub struct RouteDefinition {
    /// Absolute file path containing the `->name('X')` call.
    pub file: PathBuf,
    /// 0-based line of the `->name(` callsite.
    pub line: u32,
    /// 0-based column where the `->name(` callsite begins.
    pub column: u32,
    /// 0-based column where the `->name(` callsite ends (exclusive).
    pub end_column: u32,
    /// Source priority. Higher wins on conflict (app overrides package overrides framework).
    pub priority: u8,
    /// HTTP method extracted from the route declaration (lowercased: "get", "post",
    /// "any", "match", "view", "redirect", etc.). `None` when the verb can't be
    /// resolved statically — e.g. inside a `Route::macro(...)` body or for
    /// programmatically-built routes.
    pub method: Option<String>,
    /// URI extracted from the first string argument of the verb call.
    /// `None` when the first argument isn't a string literal.
    pub uri: Option<String>,
    /// Controller@action extracted from the second argument. Common shapes:
    /// `[UserController::class, 'show']` → `"UserController@show"`,
    /// `'OldController@method'` → `"OldController@method"`,
    /// `UserController::class` (invokable) → `"UserController"`,
    /// `function/fn closure` → `"Closure"`.
    /// `None` when the second argument is missing or unresolvable (e.g. `Route::view`,
    /// `Route::redirect`).
    pub action: Option<String>,
}

/// Priority levels used when multiple files define the same route name.
/// Higher beats lower — if app and Fortify both register `login`, the app's wins.
pub const PRIORITY_FRAMEWORK: u8 = 0;
pub const PRIORITY_PACKAGE: u8 = 1;
pub const PRIORITY_APP: u8 = 2;

/// In-memory map of route name → definition location.
#[derive(Debug, Default, Clone)]
pub struct RouteIndex {
    pub routes: HashMap<String, RouteDefinition>,
    /// Every file that contributed to this index, keyed by normalized
    /// (lexically-cleaned) absolute path. Includes both files found by
    /// [`discover_route_files`] AND files reached transitively through
    /// `->group(<path>)` external loads — even when they live outside `routes/`
    /// (issue #43). Used by `did_save` to decide whether a saved file should
    /// trigger a route-index rebuild.
    pub source_files: std::collections::HashSet<PathBuf>,
    /// Inherited external-load name prefixes per route file (issue #43),
    /// keyed by normalized path and always including `""`. Computed during
    /// [`build_route_index`] from the same load-graph pass that builds the
    /// routes, so consumers (e.g. the code-lens handler) resolve a file's
    /// fully-qualified route names without re-parsing every route file. Use
    /// [`RouteIndex::external_prefixes_for`], which defaults to `[""]`.
    ///
    /// **Ordered:** `""` first, then the inherited prefixes lexicographically.
    /// A caller taking the first non-empty entry as the file's primary
    /// project-level name therefore gets the same answer every run — see the
    /// sort in `compute_effective_prefixes`.
    pub external_prefixes: HashMap<PathBuf, Vec<String>>,
}

impl RouteIndex {
    /// The inherited external-load name prefixes that apply to `path` (always
    /// includes `""` first, then the rest lexicographically). Reads the map
    /// cached at build time — no I/O.
    pub fn external_prefixes_for(&self, path: &Path) -> Vec<String> {
        self.external_prefixes
            .get(&normalize_path(path))
            .cloned()
            .unwrap_or_else(|| vec![String::new()])
    }
}

impl RouteIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a definition. Higher priority replaces lower; equal priority keeps first.
    pub fn insert(&mut self, name: String, def: RouteDefinition) {
        match self.routes.get(&name) {
            Some(existing) if existing.priority >= def.priority => {}
            _ => {
                self.routes.insert(name, def);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&RouteDefinition> {
        self.routes.get(name)
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// A file containing route definitions, paired with its priority tier.
#[derive(Debug, Clone)]
pub struct RouteFile {
    pub path: PathBuf,
    pub priority: u8,
}

thread_local! {
    /// How many [`discover_route_files`] walks this thread has started.
    ///
    /// The walk reads *every* `.php` file under `vendor/` — tens of thousands
    /// on a real project — so it belongs in warm-up (`rebuild_route_index`,
    /// which runs it inside `spawn_blocking`) and nowhere near a per-request
    /// handler. This counter is the seam the regression tests use to prove a
    /// handler answered from the warm [`RouteIndex::external_prefixes`] cache
    /// instead of re-walking (issue #80).
    ///
    /// Thread-local rather than a global `AtomicU64` so tests running in
    /// parallel cannot see each other's walks: a `#[tokio::test]` drives its
    /// current-thread runtime on the test's own thread, and the one production
    /// walk site hands the work to a `spawn_blocking` thread. `Cell<u64>` is
    /// enough — a thread-local is never shared, so no atomics are needed.
    static DISCOVERY_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many [`discover_route_files`] walks the **calling thread** has started.
///
/// Test instrumentation: take a reading, drive a handler, take another. An
/// unchanged count proves the handler touched no route file on disk.
pub fn discovery_walk_count() -> u64 {
    DISCOVERY_WALKS.with(|c| c.get())
}

/// Depth budget for the vendor leg of route discovery, in `WalkDir` terms
/// (`vendor/` itself is depth 0). Was `WalkDir::max_depth(8)` before the shared
/// vendor walk; it is now applied as a predicate over
/// [`VendorIndex`](crate::vendor_index::VendorIndex), which sees the whole
/// tree so that other consumers can apply their own, different budgets.
pub const VENDOR_ROUTE_MAX_DEPTH: usize = 8;

/// Accumulates discovered route files, keeping the highest priority seen per
/// path. Merging by maximum rather than by arrival is what makes discovery
/// independent of walk order.
#[derive(Debug, Default)]
pub struct RouteFileSet(HashMap<PathBuf, u8>);

impl RouteFileSet {
    /// Record `path` at `priority`, keeping whichever priority is higher.
    pub fn promote(&mut self, path: PathBuf, priority: u8) {
        promote(&mut self.0, path, priority);
    }

    pub fn into_files(self) -> Vec<RouteFile> {
        self.0
            .into_iter()
            .map(|(path, priority)| RouteFile { path, priority })
            .collect()
    }
}

/// True when route discovery needs this vendor file's **text** to decide.
///
/// Two kinds of file need no read: one past the depth budget, which discovery
/// never considered at all, and one under a package `routes/` directory, which
/// is a route file by Laravel convention regardless of content. Everything else
/// is decided by content match, so the shared vendor pass reads it for us.
pub fn vendor_route_needs_source(file: &VendorFile) -> bool {
    file.depth <= VENDOR_ROUTE_MAX_DEPTH && !is_under_routes_dir(&file.path)
}

/// Record the vendor route files that need no read — everything under a package
/// `routes/` directory, within the depth budget.
pub fn collect_conventional_vendor_route_files(vendor: &VendorIndex, out: &mut RouteFileSet) {
    for file in vendor.files() {
        if file.depth <= VENDOR_ROUTE_MAX_DEPTH && is_under_routes_dir(&file.path) {
            out.promote(file.path.clone(), priority_for_vendor_path(&file.path));
        }
    }
}

/// Record an already-read vendor file if its text shows route-registration
/// shape (a router token AND a `->name(` call).
///
/// This is what catches macro bodies (Laravel UI's `AuthRouteMethods`),
/// service-provider `boot()` registrations, and Filament-style
/// `Panel::routes(fn () => ...)` panels.
pub fn accept_vendor_route_source(out: &mut RouteFileSet, path: &Path, content: &str) {
    if content_registers_named_routes(content) {
        out.promote(path.to_path_buf(), priority_for_vendor_path(path));
    }
}

/// Record the non-vendor route files: the project's own `routes/` tree, plus
/// app service providers and `bootstrap/app.php` that register routes in
/// `boot()`. Those are content-matched to avoid pulling in unrelated `app/`
/// files.
pub fn collect_project_route_files(root: &Path, out: &mut RouteFileSet) {
    let project_routes = root.join("routes");
    if project_routes.exists() {
        for path in walk_php_files(&project_routes, 6) {
            out.promote(path, PRIORITY_APP);
        }
    }
    for candidate in app_provider_candidates(root) {
        if candidate.exists() && file_registers_named_routes(&candidate) {
            out.promote(candidate, PRIORITY_APP);
        }
    }
}

/// Walk the project to discover every file likely to define named routes.
///
/// The returned list is deduplicated by path. Order is not significant — the
/// final index resolves conflicts via priority.
///
/// **Expensive.** Content-matching the vendor tree means a `read_to_string` of
/// every `.php` file below `vendor/`. Call this from warm-up/rebuild paths
/// only; request handlers read the cached [`RouteIndex`] instead. Warm start
/// shares its vendor reads with the command index — see
/// [`discover_route_files_with_vendor`].
pub fn discover_route_files(root: &Path) -> Vec<RouteFile> {
    discover_route_files_with_vendor(root, &VendorIndex::build(root))
}

/// [`discover_route_files`] driven by an already-built shared vendor walk.
///
/// Identical output to walking `vendor/` here: `vendor` holds the whole tree
/// and the depth budget is re-applied per file by [`vendor_route_needs_source`]
/// and [`collect_conventional_vendor_route_files`]. Splitting it out lets warm
/// start read each vendor file once for this *and* the command index instead of
/// twice (issue #371).
pub fn discover_route_files_with_vendor(root: &Path, vendor: &VendorIndex) -> Vec<RouteFile> {
    DISCOVERY_WALKS.with(|c| c.set(c.get().saturating_add(1)));
    let mut out = RouteFileSet::default();

    collect_project_route_files(root, &mut out);
    collect_conventional_vendor_route_files(vendor, &mut out);
    vendor.for_each_source(vendor_route_needs_source, |file, content| {
        accept_vendor_route_source(&mut out, &file.path, content);
    });

    out.into_files()
}

/// Build a complete route name → location index from the given files.
///
/// `root` is the project root, used to resolve `base_path(...)` arguments in
/// external-file group loads (issue #43). The working set is BFS-expanded along
/// `->group(<path>)` load edges, so a file referenced via
/// `Route::as('admin.')->group(base_path('app/Custom/admin.php'))` is indexed
/// even when it lives OUTSIDE `routes/` and was never returned by
/// [`discover_route_files`]. Referenced files inherit their loader's priority.
///
/// Files reached by such loads inherit the loading group's name prefix
/// transitively, so their routes are indexed under both their bare and prefixed
/// names. The resulting [`RouteIndex::source_files`] lists every contributing
/// file (discovered + referenced), keyed by normalized path.
pub fn build_route_index(root: &Path, files: &[RouteFile]) -> RouteIndex {
    let expansion = expand_load_graph(root, files);
    let effective = compute_effective_prefixes(&expansion.files, &expansion.loads);

    let mut index = RouteIndex::new();
    for file in &expansion.files {
        let key = normalize_path(&file.path);
        index.source_files.insert(key.clone());
        let Some(content) = expansion.contents.get(&key) else {
            continue;
        };
        let inherited = effective.get(&key).cloned().unwrap_or_default();
        for def in extract_named_routes(content, &file.path, file.priority, &inherited) {
            if let Some(name) = def.0 {
                index.insert(name, def.1);
            }
        }
    }
    // Cache the per-file prefix map from the same `effective` data, so the
    // code-lens handler can resolve fully-qualified route names without
    // re-running this whole load-graph pass per request.
    for (key, prefixes) in effective {
        index
            .external_prefixes
            .insert(key, dedup_prefixes(&prefixes));
    }

    // Surface Laravel Folio pages (filesystem-derived routes that never call
    // `Route::`) through the same index, so goto/completion/diagnostics see
    // them. No-op for projects that don't use Folio.
    crate::folio_discovery::inject_folio_routes(root, &mut index);

    index
}

/// Normalize a raw inherited-prefix list into the `[""]`-prefixed, deduplicated
/// form callers expect: the empty prefix always applies (a file is scanned
/// directly), plus each distinct non-empty inherited prefix once.
fn dedup_prefixes(prefixes: &[String]) -> Vec<String> {
    let mut out = vec![String::new()];
    for p in prefixes {
        if !p.is_empty() && !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// Return the inherited external-file name prefixes that apply to `file`
/// (issue #43), ALWAYS including `""` and deduplicated.
///
/// A file referenced via `Route::as('admin.')->group(base_path('that.php'))`
/// from somewhere in the project inherits the loading group's name prefix
/// (`"admin."`) transitively across the entire `->group(<path>)` load graph.
///
/// **Not for request handlers.** This is the uncached reference implementation:
/// it runs [`discover_route_files`] — a `read_to_string` of every `.php` file
/// under `vendor/` — plus the same BFS load-graph expansion and prefix
/// propagation as [`build_route_index`], and only then looks up `file`'s
/// normalized key. [`build_route_index`] already stores the identical answer
/// for every file in [`RouteIndex::external_prefixes`], so anything holding a
/// built index must call [`RouteIndex::external_prefixes_for`] instead. Calling
/// this per request is what made `textDocument/documentSymbol` on a routes file
/// cost ~510 ms against ~0.2 ms elsewhere (issue #80).
///
/// It stays as the independent oracle the tests compare the cached map against
/// — see `external_prefixes_for_file_agrees_with_the_built_index`, which is
/// what licenses every handler to read the cache instead.
///
/// Returns `["".into()]` when `file` isn't reachable (it's still scanned
/// directly, so the empty prefix always applies).
pub fn external_prefixes_for_file(root: &Path, file: &Path) -> Vec<String> {
    let files = discover_route_files(root);
    let expansion = expand_load_graph(root, &files);
    let effective = compute_effective_prefixes(&expansion.files, &expansion.loads);

    let key = normalize_path(file);
    let mut out = vec![String::new()];
    if let Some(prefixes) = effective.get(&key) {
        for p in prefixes {
            if !p.is_empty() && !out.contains(p) {
                out.push(p.clone());
            }
        }
    }
    out
}

/// The fully-expanded working set produced by following `->group(<path>)`
/// external loads from a seed file list. Keyed by normalized path so a file
/// reached more than once is read and indexed exactly once.
struct LoadGraphExpansion {
    /// Every contributing file (seed + transitively referenced), with the
    /// highest priority observed for each.
    files: Vec<RouteFile>,
    /// Each file's source text, keyed by normalized path.
    contents: HashMap<PathBuf, String>,
    /// Each file's external group loads, keyed by normalized path. Captured
    /// during the BFS so [`compute_effective_prefixes`] doesn't re-parse every
    /// file just to rediscover the same edges.
    loads: HashMap<PathBuf, Vec<ExternalGroupLoad>>,
}

/// BFS-expand `files` along external `->group(<path>)` load edges, reading each
/// reachable file's contents. Shared by [`build_route_index`] and
/// [`external_prefixes_for_file`] so both see the identical working set.
fn expand_load_graph(root: &Path, files: &[RouteFile]) -> LoadGraphExpansion {
    // `contents`/`paths`/`priorities` are all keyed by normalized path so a file
    // reached by two routes (discovered + referenced, or via two loaders) is
    // read and indexed exactly once.
    let mut contents: HashMap<PathBuf, String> = HashMap::new();
    // Original (non-normalized) path to use for the RouteDefinition's `file`.
    let mut paths: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut priorities: HashMap<PathBuf, u8> = HashMap::new();
    let mut loads: HashMap<PathBuf, Vec<ExternalGroupLoad>> = HashMap::new();

    // Queue of (path, priority, depth) still to read/expand.
    let mut queue: std::collections::VecDeque<(PathBuf, u8, usize)> =
        std::collections::VecDeque::new();
    for file in files {
        queue.push_back((file.path.clone(), file.priority, 0));
    }

    while let Some((path, priority, depth)) = queue.pop_front() {
        let key = normalize_path(&path);

        // Record the best (highest) priority and remember the path/contents the
        // first time we see this file.
        let already_seen = contents.contains_key(&key);
        priorities
            .entry(key.clone())
            .and_modify(|p| {
                if priority > *p {
                    *p = priority;
                }
            })
            .or_insert(priority);
        if already_seen {
            // Contents already read and this file's edges already expanded;
            // just merging priority above is enough.
            continue;
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            // Unreadable target (e.g. a referenced file that doesn't exist) —
            // record nothing; it simply contributes no routes.
            continue;
        };

        // Discover this file's external `->group(<path>)` targets. Every file
        // is scanned (the result is reused to build the prefix graph), but only
        // an under-depth file enqueues its targets, so a pathological chain
        // can't loop forever (the `already_seen` check breaks cycles directly).
        let loader_dir = path.parent().unwrap_or(root).to_path_buf();
        let file_loads = find_external_group_loads(&text, root, &loader_dir);
        if depth < MAX_LOAD_DEPTH {
            for load in &file_loads {
                let target_key = normalize_path(&load.target);
                if !contents.contains_key(&target_key) {
                    queue.push_back((load.target.clone(), priority, depth + 1));
                }
            }
        }
        loads.insert(key.clone(), file_loads);

        paths.insert(key.clone(), path);
        contents.insert(key, text);
    }

    // Build the full expanded file set.
    let expanded: Vec<RouteFile> = paths
        .iter()
        .map(|(key, original)| RouteFile {
            path: original.clone(),
            priority: priorities.get(key).copied().unwrap_or(PRIORITY_APP),
        })
        .collect();

    LoadGraphExpansion {
        files: expanded,
        contents,
        loads,
    }
}

/// Maximum transitive load depth — a backstop against pathological chains even
/// with the cycle guard in place.
const MAX_LOAD_DEPTH: usize = 10;

/// Build the per-file set of inherited name prefixes contributed by
/// external-file group loads, propagated transitively across the load graph.
///
/// Returns a map keyed by normalized file path. Every entry includes `""`
/// (every file is also scanned directly). Cycles are broken by a per-target
/// visited set, and chains are capped at [`MAX_LOAD_DEPTH`].
fn compute_effective_prefixes(
    files: &[RouteFile],
    loads: &HashMap<PathBuf, Vec<ExternalGroupLoad>>,
) -> HashMap<PathBuf, Vec<String>> {
    // Set of files we actually index — only these can receive inherited
    // prefixes, and only loads pointing at one matter.
    let known: std::collections::HashSet<PathBuf> =
        files.iter().map(|f| normalize_path(&f.path)).collect();

    // edges[source] = Vec<(target, edge_prefix)>
    let mut edges: HashMap<PathBuf, Vec<(PathBuf, String)>> = HashMap::new();
    for file in files {
        let source = normalize_path(&file.path);
        let Some(file_loads) = loads.get(&source) else {
            continue;
        };
        // Each load already carries the enclosing closure groups' prefixes.
        for load in file_loads {
            let target = normalize_path(&load.target);
            if !known.contains(&target) {
                continue;
            }
            edges
                .entry(source.clone())
                .or_default()
                .push((target, load.edge_prefix.clone()));
        }
    }

    // Propagate. For each known file, the inherited prefixes are the set of
    // accumulated edge-prefix concatenations along every load path that reaches
    // it. We DFS from each source so cycles are naturally bounded per traversal.
    let mut effective: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for start in &known {
        propagate(start, "", &edges, &mut effective, &mut Vec::new(), 0);
    }

    // Sort each file's prefixes. The DFS above visits `known` (a `HashSet`) and
    // `files` (a `HashMap` drain, via `discover_route_files`) in whatever order
    // the hasher's per-instance seed produces, so a file reachable under two
    // different prefixes collected them in an order that changed from run to
    // run — and from call to call inside one process.
    //
    // That is not cosmetic. `classify_with_decl_fallback` anchors a route
    // declaration to the FIRST non-empty prefix, so a file loaded by both
    // `Route::as('admin.')->group(...)` and `Route::as('blog.')->group(...)`
    // resolved to `admin.x` or `blog.x` at random — find-references and rename
    // would disagree with themselves between two invocations on the same
    // unedited project.
    //
    // Sorting the output (rather than the traversal) is what makes the result
    // stable: the reachable SET is already start-order independent — each DFS
    // is bounded by its own cycle stack and its own depth budget — so only the
    // order ever varied. Lexicographic is an arbitrary but total rule; the load
    // graph ranks sibling loaders in no other way, and a caller taking `.first()`
    // needs *some* defined answer.
    for prefixes in effective.values_mut() {
        prefixes.sort();
    }
    effective
}

/// Depth-first propagation of accumulated prefixes along load edges. `acc` is
/// the prefix accumulated from the root of this traversal up to (but excluding)
/// `current`. `stack` holds the files on the current path for cycle detection.
fn propagate(
    current: &Path,
    acc: &str,
    edges: &HashMap<PathBuf, Vec<(PathBuf, String)>>,
    effective: &mut HashMap<PathBuf, Vec<String>>,
    stack: &mut Vec<PathBuf>,
    depth: usize,
) {
    if stack.iter().any(|p| p == current) || depth > MAX_LOAD_DEPTH {
        return;
    }
    // Record this accumulated prefix for `current` (skip the empty root case —
    // every file already gets "" implicitly in `extract_named_routes`).
    if !acc.is_empty() {
        let entry = effective.entry(current.to_path_buf()).or_default();
        if !entry.iter().any(|p| p == acc) {
            entry.push(acc.to_string());
        }
    }
    stack.push(current.to_path_buf());
    if let Some(targets) = edges.get(current) {
        for (target, edge_prefix) in targets {
            let next_acc = format!("{}{}", acc, edge_prefix);
            propagate(target, &next_acc, edges, effective, stack, depth + 1);
        }
    }
    stack.pop();
}

// ============================================================================
// Syntax-tree route extraction
// ============================================================================
//
// Route names, group prefixes and resource registrations are read from the PHP
// syntax tree. The previous byte scanners had no concept of comments, so a
// commented-out `function () {` or a bare apostrophe in prose ("it's") shifted
// every brace/paren boundary that followed it: group closures appeared to end
// hundreds of lines late and their name prefixes leaked onto unrelated routes,
// while `->name('x')` inside a commented-out route was indexed as real.

/// One `->method(...)` / `::method(...)` link of a fluent chain.
struct ChainLink<'a> {
    /// Method identifier exactly as written (`get`, `apiResource`, `name`).
    method: String,
    /// The call's `arguments` node.
    args: Node<'a>,
    /// The method-name identifier node — anchors resource definitions.
    name_node: Node<'a>,
    /// The `->` / `?->` / `::` operator node — anchors named-route definitions.
    operator: Option<Node<'a>>,
    /// The call's receiver — the `Route` of `Route::singleton(…)`, the
    /// `$this->app` of `$this->app->singleton(…)`. Read lazily; only the
    /// chain-opening link's receiver is ever consulted.
    receiver: Option<Node<'a>>,
    /// True for `$obj->method(...)`, false for `Class::method(...)`.
    instance: bool,
}

fn is_call_kind(kind: &str) -> bool {
    matches!(
        kind,
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression"
    )
}

/// True when `node` is the outermost call of a fluent chain — i.e. no parent
/// call uses it as the receiver. Chains are walked from this node inward.
fn is_chain_root(node: Node) -> bool {
    match node.parent() {
        Some(parent) if is_call_kind(parent.kind()) => parent
            .child_by_field_name("object")
            .is_none_or(|receiver| receiver.id() != node.id()),
        _ => true,
    }
}

/// Collect a chain's links in SOURCE order (leftmost/innermost call first).
///
/// The AST nests a fluent chain receiver-first, so `Route::get(…)->name(…)` is
/// a `name` call whose `object` is the `get` call. Walking `object` yields the
/// chain outermost-inward; the result is reversed so callers can read it the
/// way the source does.
fn collect_links<'a>(chain: Node<'a>, source: &[u8]) -> Vec<ChainLink<'a>> {
    let mut links = Vec::new();
    let mut current = Some(chain);

    while let Some(node) = current {
        if !is_call_kind(node.kind()) {
            break;
        }
        if let (Some(name_node), Some(args)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("arguments"),
        ) {
            if let Ok(method) = name_node.utf8_text(source) {
                links.push(ChainLink {
                    method: method.to_string(),
                    args,
                    name_node,
                    operator: operator_node(node),
                    receiver: node
                        .child_by_field_name("scope")
                        .or_else(|| node.child_by_field_name("object")),
                    instance: node.kind() != "scoped_call_expression",
                });
            }
        }
        // `scoped_call_expression` has a `scope`, not an `object`, so a facade
        // call (`Route::…`) naturally terminates the walk.
        current = node.child_by_field_name("object");
    }

    links.reverse();
    links
}

/// True when a chain's receiver is Laravel's router, and not some other object
/// that happens to expose a same-named method. Deliberately an allow-list: an
/// unrecognised receiver fails closed (no routes indexed) rather than emitting
/// names that were never registered.
fn is_router_receiver(receiver: &str) -> bool {
    matches!(receiver, "Route" | "Router" | "$router" | "$this->router")
        || receiver.ends_with("\\Route")
        || receiver.ends_with("\\Router")
}

/// The `->` / `?->` / `::` token of a call, used to position the definition at
/// the start of the `->name(` callsite rather than the whole chain.
fn operator_node<'a>(call: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = call.walk();
    for child in call.children(&mut cursor) {
        if matches!(child.kind(), "->" | "?->" | "::") {
            return Some(child);
        }
    }
    None
}

/// The expression of the `n`th positional argument, skipping the `argument`
/// wrapper node.
fn nth_arg_node<'a>(args: Node<'a>, n: usize) -> Option<Node<'a>> {
    let mut cursor = args.walk();
    let mut index = 0;
    for child in args.children(&mut cursor) {
        if child.kind() != "argument" {
            continue;
        }
        if index == n {
            return child.named_child(0);
        }
        index += 1;
    }
    None
}

/// The `n`th argument when it is a string literal — the shape every route name,
/// URI and resource name must have to be statically resolvable.
fn nth_string_arg(args: Node, n: usize, source: &[u8]) -> Option<String> {
    let node = nth_arg_node(args, n)?;
    if !is_string_node(node) {
        return None;
    }
    string_text(node, source)
}

fn is_string_node(node: Node) -> bool {
    matches!(node.kind(), "string" | "encapsed_string")
}

/// Inner text of a string literal, quotes stripped.
fn string_text(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return child.utf8_text(source).ok().map(str::to_string);
        }
    }
    // An empty literal (`''`) has no `string_content` child.
    node.utf8_text(source).ok().map(|raw| {
        raw.trim_start_matches(['\'', '"'])
            .trim_end_matches(['\'', '"'])
            .to_string()
    })
}

/// The element expressions of an array literal, in order.
fn array_values<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    if node.kind() != "array_creation_expression" {
        return Vec::new();
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "array_element_initializer")
        .filter_map(|element| element.named_child(0))
        .collect()
}

/// Every string literal in an array literal (`['store', 'update']`).
fn string_array(node: Node, source: &[u8]) -> Vec<String> {
    array_values(node)
        .into_iter()
        .filter(|value| is_string_node(*value))
        .filter_map(|value| string_text(value, source))
        .collect()
}

/// The `'as' => 'api.'` entry of a group's array-attribute argument.
fn array_as_prefix(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "array_creation_expression" {
        return None;
    }
    let mut cursor = node.walk();
    for element in node.children(&mut cursor) {
        if element.kind() != "array_element_initializer" {
            continue;
        }
        let mut pair = element.walk();
        let parts: Vec<Node> = element
            .children(&mut pair)
            .filter(|child| child.is_named())
            .collect();
        if parts.len() == 2
            && is_string_node(parts[0])
            && string_text(parts[0], source).as_deref() == Some("as")
            && is_string_node(parts[1])
        {
            return string_text(parts[1], source);
        }
    }
    None
}

fn is_closure_kind(kind: &str) -> bool {
    matches!(
        kind,
        "anonymous_function" | "anonymous_function_creation_expression" | "arrow_function"
    )
}

/// The closure passed to a `group(...)` call, if it has one. Handles both the
/// fluent form and the legacy `group(['as' => 'x'], function () {})` form.
fn closure_argument<'a>(args: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "argument" {
            continue;
        }
        if let Some(expr) = child.named_child(0) {
            if is_closure_kind(expr.kind()) {
                return Some(expr);
            }
        }
    }
    None
}

/// The name prefix a `group(...)` chain contributes to the routes inside it.
///
/// A chained `->name('admin.')` / `->as('admin.')` wins; otherwise the group's
/// `['as' => 'admin.']` array attribute is used. Returns `None` for a group
/// that sets no prefix (e.g. a bare `->middleware(...)->group(...)`), which
/// contributes nothing to its children's names.
fn group_prefix(links: &[ChainLink], group_index: usize, source: &[u8]) -> Option<String> {
    // Nearest setter to the left of `group(` governs — the chain reads left to
    // right, so a later `->name()` supersedes an earlier one.
    let chained = links[..group_index]
        .iter()
        .rev()
        .find(|link| link.method == "name" || link.method == "as")
        .and_then(|link| nth_string_arg(link.args, 0, source));

    chained.or_else(|| {
        nth_arg_node(links[group_index].args, 0).and_then(|first| array_as_prefix(first, source))
    })
}

/// Walk `node`, invoking `sink` once per fluent-chain root with that chain's
/// links and the group-name prefix in force where the chain is written.
///
/// Recursion descends through every node, so chains registered inside macro
/// bodies, service-provider methods and closures passed to arbitrary helpers
/// are all reached — matching what the old whole-file byte scan covered. A
/// `group(...)` closure's body is walked with the group's prefix appended;
/// everything else keeps the prefix it inherited.
fn walk_chains<'a, F>(node: Node<'a>, source: &'a [u8], prefix: &str, sink: &mut F)
where
    F: FnMut(&[ChainLink<'a>], &str),
{
    if is_call_kind(node.kind()) && is_chain_root(node) {
        let links = collect_links(node, source);
        let group_index = links.iter().position(|link| link.method == "group");
        let nested = match group_index {
            Some(index) => format!(
                "{prefix}{}",
                group_prefix(&links, index, source).unwrap_or_default()
            ),
            None => prefix.to_string(),
        };

        sink(&links, prefix);

        for (index, link) in links.iter().enumerate() {
            let mut cursor = link.args.walk();
            for arg in link.args.children(&mut cursor) {
                if arg.kind() != "argument" {
                    continue;
                }
                let is_group_body = Some(index) == group_index
                    && arg
                        .named_child(0)
                        .is_some_and(|e| is_closure_kind(e.kind()));
                let inner = if is_group_body {
                    nested.as_str()
                } else {
                    prefix
                };
                walk_chains(arg, source, inner, sink);
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_chains(child, source, prefix, sink);
    }
}

/// Reconstruct a route's HTTP method, URI and controller action from the verb
/// call that opens its chain. Returns a partial result when a piece is absent —
/// a closure route still resolves method + URI.
fn chain_metadata(links: &[ChainLink], source: &[u8]) -> RouteMetadata {
    let Some(verb) = links
        .iter()
        .find(|link| HTTP_VERBS.contains(&link.method.as_str()))
    else {
        return RouteMetadata::default();
    };

    RouteMetadata {
        method: Some(verb.method.clone()),
        uri: nth_string_arg(verb.args, 0, source),
        action: nth_arg_node(verb.args, 1).and_then(|arg| action_text(arg, source)),
    }
}

/// Render a route's second argument as a display string:
/// `[UserController::class, 'show']` → `UserController@show`,
/// `DashboardController::class` → `DashboardController`, a closure →
/// `Closure`, and any other string literal (a view name, a redirect target)
/// verbatim.
fn action_text(node: Node, source: &[u8]) -> Option<String> {
    if is_closure_kind(node.kind()) {
        return Some("Closure".to_string());
    }
    if node.kind() == "array_creation_expression" {
        let values = array_values(node);
        let (class, method) = (values.first()?, values.get(1)?);
        if !is_string_node(*method) {
            return None;
        }
        return Some(format!(
            "{}@{}",
            class_reference_name(*class, source)?,
            string_text(*method, source)?
        ));
    }
    if let Some(class) = class_reference_name(node, source) {
        return Some(class);
    }
    if is_string_node(node) {
        let text = string_text(node, source)?;
        // Legacy `'Controller@method'` syntax renders with a short class name.
        if let Some((class, method)) = text.split_once('@') {
            return Some(format!("{}@{}", short_class_name(class), method));
        }
        return Some(text);
    }
    None
}

/// Short class name of a `Foo\Bar::class` expression.
fn class_reference_name(node: Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let class = text.strip_suffix("::class")?.trim();
    Some(short_class_name(class).to_string())
}

/// The 0-based definition span of a callsite: from `anchor` to the end of
/// `last`. Multi-line spans collapse to the anchor column, since a
/// `RouteDefinition` addresses a single line.
fn definition_span(anchor: Node, last: Node) -> (u32, u32, u32) {
    let start = anchor.start_position();
    let end = last.end_position();
    let end_column = if end.row == start.row {
        end.column as u32
    } else {
        start.column as u32
    };
    (start.row as u32, start.column as u32, end_column)
}

/// Extract every named route defined in `content`.
///
/// `inherited_prefixes` are name prefixes contributed by external-file group
/// loads that reach this file (issue #43); the file is always scanned under the
/// empty prefix too, so it contributes both its bare and prefixed names.
/// Passing `&[]` means "no inherited prefix".
pub fn extract_named_routes(
    content: &str,
    file: &Path,
    priority: u8,
    inherited_prefixes: &[String],
) -> Vec<(Option<String>, RouteDefinition)> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    let source = content.as_bytes();

    // Always include the empty prefix (the file is scanned directly too), and
    // dedupe so a load graph reaching this file twice with the same prefix
    // doesn't duplicate its routes.
    let mut effective: Vec<&str> = vec![""];
    for prefix in inherited_prefixes {
        if !prefix.is_empty() && !effective.contains(&prefix.as_str()) {
            effective.push(prefix.as_str());
        }
    }

    let mut results = Vec::new();
    walk_chains(tree.root_node(), source, "", &mut |links, prefix| {
        emit_chain_routes(
            links,
            prefix,
            source,
            file,
            priority,
            &effective,
            &mut results,
        );
    });
    results
}

/// Emit every route definition a single fluent chain registers.
fn emit_chain_routes(
    links: &[ChainLink],
    prefix: &str,
    source: &[u8],
    file: &Path,
    priority: u8,
    effective: &[&str],
    results: &mut Vec<(Option<String>, RouteDefinition)>,
) {
    // A chain ending in `group(...)` configures its children; its own
    // `->name('admin.')` is their prefix, not a route of its own.
    if links.iter().any(|link| link.method == "group") {
        return;
    }

    let metadata = chain_metadata(links, source);

    for link in links {
        if link.instance && link.method == "name" {
            emit_named_route(
                link, prefix, source, file, priority, effective, &metadata, results,
            );
        }
        // `singleton(...)` is also the service container's binding method, so
        // — unlike `resource(...)`, which collides with nothing — the singleton
        // forms register routes only when the chain opens on the router itself.
        // Without this, `$this->app->singleton('cache.limiter', …)` in a
        // provider that also declares a named route indexes the phantom names
        // `cache.limiter.show`/`.edit`/`.update`.
        let registers_resource = match link.method.as_str() {
            "resource" | "apiResource" => true,
            "singleton" | "apiSingleton" => links
                .first()
                .and_then(|first| first.receiver)
                .and_then(|node| node.utf8_text(source).ok())
                .is_some_and(is_router_receiver),
            _ => false,
        };
        if registers_resource {
            emit_resource_routes(
                links, link, prefix, source, file, priority, effective, results,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_named_route(
    link: &ChainLink,
    prefix: &str,
    source: &[u8],
    file: &Path,
    priority: u8,
    effective: &[&str],
    metadata: &RouteMetadata,
    results: &mut Vec<(Option<String>, RouteDefinition)>,
) {
    let Some(argument) = nth_arg_node(link.args, 0).filter(|node| is_string_node(*node)) else {
        return;
    };
    let Some(literal) = string_text(argument, source) else {
        return;
    };

    let anchor = link.operator.unwrap_or(link.name_node);
    let (line, column, end_column) = definition_span(anchor, argument);
    let leaf = format!("{prefix}{literal}");

    for inherited in effective {
        results.push((
            Some(format!("{inherited}{leaf}")),
            RouteDefinition {
                file: file.to_path_buf(),
                line,
                column,
                end_column,
                priority,
                method: metadata.method.clone(),
                uri: metadata.uri.clone(),
                action: metadata.action.clone(),
            },
        ));
    }
}

/// Expand a resource registration into the named routes Laravel synthesizes for
/// it — `photos.index`, `photos.show`, … — after applying any `->only([...])` /
/// `->except([...])` filter on the same chain.
///
/// Covers all four forms Laravel's `ResourceRegistrar` handles: `resource(...)`,
/// `apiResource(...)`, `singleton(...)` and `apiSingleton(...)`. They differ only
/// in their default action set; everything downstream of that is shared.
///
/// A slashed URI contributes only its last segment to the names; the rest is a
/// URI prefix. The full URI is still carried on each definition for display.
///
/// Punted (common case only): `->names([...])` overrides, the plural
/// `Route::resources([…])` / `Route::singletons([…])` registrations, and
/// shallow/nested resources.
#[allow(clippy::too_many_arguments)]
fn emit_resource_routes(
    links: &[ChainLink],
    link: &ChainLink,
    prefix: &str,
    source: &[u8],
    file: &Path,
    priority: u8,
    effective: &[&str],
    results: &mut Vec<(Option<String>, RouteDefinition)>,
) {
    let Some(raw) = nth_string_arg(link.args, 0, source) else {
        return;
    };
    let uri = raw.trim_matches('/');
    if uri.is_empty() {
        return;
    }
    // Everything before the last `/` is a URI prefix, not part of the route
    // name: `Route::resource('admin/photos', …)` registers `photos.index`, not
    // `admin/photos.index`. Laravel routes any slashed name through
    // `ResourceRegistrar::prefixedResource`, whose `getResourcePrefix` keeps
    // only the final segment as the resource name.
    let resource = uri.rsplit('/').next().unwrap_or(uri);

    let defaults = match link.method.as_str() {
        "apiResource" => API_RESOURCE_ACTIONS.to_vec(),
        "singleton" => singleton_actions(links, false),
        "apiSingleton" => singleton_actions(links, true),
        _ => RESOURCE_ACTIONS.to_vec(),
    };
    let (line, column, end_column) = definition_span(link.name_node, link.args);

    for action in resource_actions(links, &defaults, source) {
        let leaf = format!("{prefix}{resource}.{action}");
        for inherited in effective {
            results.push((
                Some(format!("{inherited}{leaf}")),
                RouteDefinition {
                    file: file.to_path_buf(),
                    line,
                    column,
                    end_column,
                    priority,
                    // A resource registers several verbs; none of them applies
                    // to the group as a whole.
                    method: None,
                    uri: Some(uri.to_string()),
                    action: Some(action.to_string()),
                },
            ));
        }
    }
}

/// The action set surviving a resource registration's `->only([...])` /
/// `->except([...])` filter. `only` wins when both are present. A non-array
/// argument is ignored, leaving the defaults in place.
fn resource_actions(
    links: &[ChainLink],
    defaults: &[&'static str],
    source: &[u8],
) -> Vec<&'static str> {
    let array_arg = |method: &str| -> Option<Vec<String>> {
        let link = links.iter().find(|link| link.method == method)?;
        let node = nth_arg_node(link.args, 0)?;
        if node.kind() != "array_creation_expression" {
            return None;
        }
        Some(string_array(node, source))
    };

    if let Some(only) = array_arg("only") {
        return defaults
            .iter()
            .copied()
            .filter(|action| only.iter().any(|kept| kept == action))
            .collect();
    }
    if let Some(except) = array_arg("except") {
        return defaults
            .iter()
            .copied()
            .filter(|action| !except.iter().any(|dropped| dropped == action))
            .collect();
    }
    defaults.to_vec()
}

/// The default action set a `singleton(...)` / `apiSingleton(...)` registration
/// starts from, before the chain's own `->only([...])` / `->except([...])` filter.
///
/// `->creatable()` widens the defaults with `create`/`store`/`destroy`;
/// `->destroyable()` adds `destroy` alone. `creatable` wins when both are on the
/// chain — it already implies `destroy`, so the two can't duplicate it, and
/// neither ordering drops an action.
///
/// `apiSingleton` is not a separate default set: `Router::apiSingleton` registers
/// an ordinary singleton with an implicit `only => [...]`, which
/// `getResourceMethods` intersects with the (possibly widened) defaults above.
/// That single mechanism is why a bare `apiSingleton` yields just `show`+`update`
/// while `store`/`destroy` surface only alongside `creatable`/`destroyable` — and
/// why `create`/`edit`, which render forms, can never appear on the API form.
fn singleton_actions(links: &[ChainLink], api: bool) -> Vec<&'static str> {
    let has = |method: &str| links.iter().any(|link| link.method == method);

    let mut actions: Vec<&'static str> = match (has("creatable"), has("destroyable")) {
        (true, _) => [&["create", "store"][..], SINGLETON_ACTIONS, &["destroy"]].concat(),
        (false, true) => [SINGLETON_ACTIONS, &["destroy"][..]].concat(),
        (false, false) => SINGLETON_ACTIONS.to_vec(),
    };
    if api {
        actions.retain(|action| API_SINGLETON_ONLY.contains(action));
    }
    actions
}

/// Default action set for `Route::singleton(...)` — a single resource with no
/// index and no identifier, so no `index`/`create`/`store`/`destroy`.
/// Mirrors `ResourceRegistrar::$singletonResourceDefaults`.
const SINGLETON_ACTIONS: &[&str] = &["show", "edit", "update"];

/// The implicit `only => [...]` filter `Router::apiSingleton(...)` applies on top
/// of the singleton defaults.
const API_SINGLETON_ONLY: &[&str] = &["store", "show", "update", "destroy"];

/// Default action set for `Route::resource(...)` — full CRUD.
const RESOURCE_ACTIONS: &[&str] = &[
    "index", "create", "store", "show", "edit", "update", "destroy",
];

/// Default action set for `Route::apiResource(...)` — no `create`/`edit` (those
/// render forms, which an API doesn't serve).
const API_RESOURCE_ACTIONS: &[&str] = &["index", "store", "show", "update", "destroy"];

/// Resolved data describing the verb call that opens a route's fluent chain.
/// Each field can be `None` when static extraction can't pin it down — callers
/// must handle missing data gracefully (typically by omitting it from display).
#[derive(Debug, Default, Clone)]
pub struct RouteMetadata {
    pub method: Option<String>,
    pub uri: Option<String>,
    pub action: Option<String>,
}

/// HTTP verbs (and verb-shaped methods like `view`, `redirect`) that Laravel's
/// router exposes for registering routes. Matched against `Route::<verb>(` and
/// `->name`-chained `<receiver>-><verb>(` callsites when reconstructing route
/// metadata. Lowercased — comparisons are case-sensitive against PHP source.
const HTTP_VERBS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "options",
    "any",
    "match",
    "view",
    "redirect",
    "permanentRedirect",
];

/// Decode a single-quoted or double-quoted string literal into the raw inner text.
/// Returns `None` for non-string-literal arguments (variables, expressions, etc.).
fn parse_string_literal(slice: &[u8]) -> Option<String> {
    if slice.len() < 2 {
        return None;
    }
    let quote = slice[0];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    if *slice.last()? != quote {
        return None;
    }
    let inner = &slice[1..slice.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b == b'\\' && i + 1 < inner.len() {
            out.push(inner[i + 1] as char);
            i += 2;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    Some(out)
}

/// Take the last `\`-separated segment of a PHP FQN. `App\Http\UserController` → `UserController`.
fn short_class_name(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// Quick content check — does this file likely register named routes?
///
/// Looks for both a route-registration token and a `->name(` call. False
/// positives are tolerable (the index lookup just won't find an entry); false
/// negatives are not (we'd miss valid route definitions).
///
/// Registration shape can be any of:
/// - `Route::` / `Router::` / `$router->` static or facade call
/// - `Route::macro(...)` or `RouteRegistrar` references
/// - An HTTP-verb method invocation (`->get(`, `->post(`, etc.) — covers
///   route macro bodies that bind via `$this->get(...)` (e.g., Laravel UI's
///   `AuthRouteMethods`).
fn file_registers_named_routes(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content_registers_named_routes(&content)
}

/// Content-only variant for testability — same logic as
/// [`file_registers_named_routes`] but operates on a string.
fn content_registers_named_routes(content: &str) -> bool {
    if !content.contains("->name(") {
        return false;
    }
    if content.contains("Route::")
        || content.contains("Router::")
        || content.contains("$router->")
        || content.contains("$this->router->")
        || content.contains("RouteRegistrar")
    {
        return true;
    }

    // HTTP verb invocations also imply route registration shape.
    // Laravel's router/registrar exposes these methods, so finding any of
    // them paired with `->name(` strongly indicates a route definition.
    const VERB_CALLS: &[&str] = &[
        "->get(",
        "->post(",
        "->put(",
        "->patch(",
        "->delete(",
        "->options(",
        "->any(",
        "->match(",
        "->redirect(",
        "->view(",
        "->resource(",
        "->apiResource(",
        "->fallback(",
    ];
    VERB_CALLS.iter().any(|verb| content.contains(verb))
}

fn walk_php_files(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn app_provider_candidates(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        root.join("bootstrap/app.php"),
        root.join("app/Http/Kernel.php"),
    ];
    let providers = root.join("app/Providers");
    if providers.exists() {
        paths.extend(walk_php_files(&providers, 4));
    }
    paths
}

fn is_under_routes_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("routes"))
}

fn priority_for_vendor_path(path: &Path) -> u8 {
    let s = path.to_string_lossy();
    if s.contains("/laravel/framework/") || s.contains("\\laravel\\framework\\") {
        PRIORITY_FRAMEWORK
    } else {
        PRIORITY_PACKAGE
    }
}

fn promote(seen: &mut HashMap<PathBuf, u8>, path: PathBuf, priority: u8) {
    seen.entry(path)
        .and_modify(|p| {
            if priority > *p {
                *p = priority;
            }
        })
        .or_insert(priority);
}

/// Find every `->group(<path>)`/`::group(<path>)` callsite in `content` that
/// loads an *external file* rather than running a closure body (issue #43).
///
/// Each load carries the complete name prefix in force at its callsite — the
/// enclosing closure groups' prefixes plus its own — so the caller can build
/// the load graph's edges without re-deriving group nesting.
///
/// `root` is the project root (for `base_path(...)`); `loader_dir` is the
/// directory of the file being scanned (for `__DIR__ . '...'`).
fn find_external_group_loads(
    content: &str,
    root: &Path,
    loader_dir: &Path,
) -> Vec<ExternalGroupLoad> {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return Vec::new();
    };
    let source = content.as_bytes();
    let mut loads = Vec::new();

    walk_chains(tree.root_node(), source, "", &mut |links, prefix| {
        let Some(index) = links.iter().position(|link| link.method == "group") else {
            return;
        };
        let group = &links[index];
        // A closure group runs inline; only a path argument loads a file.
        if closure_argument(group.args).is_some() {
            return;
        }

        // With an `['as' => …]` array literal first, the path is the SECOND
        // argument; the fluent `->as(…)->group($path)` form puts it first.
        let array_first = nth_arg_node(group.args, 0)
            .is_some_and(|first| first.kind() == "array_creation_expression");
        let Some(path_node) = nth_arg_node(group.args, usize::from(array_first)) else {
            return;
        };
        let Ok(text) = path_node.utf8_text(source) else {
            return;
        };
        let Some(target) = resolve_path_argument(text.as_bytes(), root, loader_dir) else {
            return;
        };

        let own = group_prefix(links, index, source).unwrap_or_default();
        loads.push(ExternalGroupLoad {
            edge_prefix: format!("{prefix}{own}"),
            target,
        });
    });

    loads
}

/// Resolve a `->group(...)` path argument to an absolute path. Recognized
/// forms (the file need not exist — `.`/`..` are normalized lexically):
/// - `base_path('sub/dir')` → `root/sub/dir`; `base_path()` → `root`
/// - `__DIR__ . '/sub'` / `__DIR__.'sub'` → `loader_dir/sub`
/// - bare string literal `'x'` → absolute as-is, else `root/x`
/// - anything else → `None` (skip).
fn resolve_path_argument(slice: &[u8], root: &Path, loader_dir: &Path) -> Option<PathBuf> {
    let text = std::str::from_utf8(slice).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    // base_path('...') / base_path()
    if let Some(rest) = text.strip_prefix("base_path") {
        let rest = rest.trim_start();
        let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
        if inner.is_empty() {
            return Some(normalize_path(root));
        }
        let sub = parse_string_literal(inner.as_bytes())?;
        return Some(normalize_path(&root.join(sub)));
    }

    // __DIR__ . '...'  (the dot and surrounding whitespace are optional spacing)
    if let Some(rest) = text.strip_prefix("__DIR__") {
        let rest = rest.trim_start().strip_prefix('.')?.trim_start();
        let sub = parse_string_literal(rest.as_bytes())?;
        let sub = sub.trim_start_matches('/');
        return Some(normalize_path(&loader_dir.join(sub)));
    }

    // Bare string literal.
    if let Some(s) = parse_string_literal(text.as_bytes()) {
        let p = Path::new(&s);
        if p.is_absolute() {
            return Some(normalize_path(p));
        }
        return Some(normalize_path(&root.join(s)));
    }

    None
}

/// Lexically normalize a path: collapse `.` and resolve `..` against prior
/// components without touching the filesystem (the target may not exist).
///
/// Public so callers (e.g. `did_save` in `main.rs`) can normalize a path the
/// same way before comparing it against [`RouteIndex::source_files`].
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop only a real directory segment; preserve root/prefix and
                // any leading `..` that can't be resolved lexically.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests;
