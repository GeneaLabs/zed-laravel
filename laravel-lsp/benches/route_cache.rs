//! Route-cache strategy benchmark — discovery for issue #48.
//!
//! Today route data is materialized into three caches built by two parsing
//! strategies. This bench measures the two strategies head-to-head so the
//! "unify onto one canonical source" decision rests on numbers, not intuition:
//!
//! * **byte-scan** — [`extract_named_routes`], the strategy behind
//!   `build_route_index` (the whole-project name→location index, incl.
//!   `vendor/`).
//! * **tree-sitter** — [`extract_route_chains`], the strategy behind the
//!   per-file declaration cache (rename / find-references) and the
//!   document-symbols outline.
//!
//! It generates a synthetic corpus at three vendor scales (~500 / ~2000 /
//! ~5000 files) whose route files mimic real-world shapes — named routes,
//! prefixed/named groups, nested groups, `Route::resource`, unnamed closure
//! routes — then measures:
//!
//! 1. **init path** — parse every file in the corpus once with each strategy
//!    (in-memory, isolating parse cost from disk I/O);
//! 2. **route-save path** — re-parse a single representative project file with
//!    each strategy;
//! 3. **realistic init** — the real `build_route_index` (discovery + disk I/O +
//!    load-graph expansion + byte-scan) against the on-disk corpus, i.e. the
//!    cost paid today;
//! 4. **memory** — estimated heap footprint of caching the tree-sitter rich
//!    per-file model (`Vec<RouteChainNode>`) for every route file.
//!
//! It also prints the `Route::resource` granularity difference (issue #48 AC#4).
//!
//! ## Running
//!
//! ```text
//! cargo bench --bench route_cache
//! ROUTE_BENCH_SCALES=200,1000 cargo bench --bench route_cache   # custom scales
//! ```
//!
//! ## Synthetic-data caveat
//!
//! This corpus is **synthetic**. The route-file shapes approximate typical
//! Laravel files, but the vendor/project ratio and per-file route counts are
//! generator parameters, not measurements of any specific project. To get the
//! literal numbers for a real codebase, point [`build_route_index`] at that
//! project's `vendor/` tree (the realistic-init harness already does exactly
//! that against the synthetic root).

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use laravel_lsp::route_chain::{extract_route_chains, RouteChainNode};
use laravel_lsp::route_discovery::{
    build_route_index, discover_route_files, extract_named_routes, PRIORITY_APP,
};

/// Files generated per synthetic vendor package (a package usually ships a
/// handful of route files under its own `routes/` dir).
const FILES_PER_PKG: usize = 6;
/// Fixed number of first-party `routes/` files (real apps have a small,
/// roughly constant set regardless of how many vendor packages they pull in).
const PROJECT_FILES: usize = 12;
/// Iterations for the single-file route-save micro-bench.
const SAVE_ITERS: u32 = 500;

/// Deterministic LCG so corpus shapes vary between files yet reproduce exactly
/// across runs (no `rand` dependency, results are stable to compare over time).
fn next_rand(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// Generate a first-party route file: a richer mix of named routes, nested
/// prefixed+named groups, resources, and unnamed closure routes.
fn project_route_file(seed: u64, idx: usize) -> String {
    let mut s = seed ^ (idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::from(
        "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\nuse App\\Http\\Controllers\\FooController;\n\n",
    );
    let routes = 8 + (next_rand(&mut s) % 24) as usize; // 8..=31
    for r in 0..routes {
        match next_rand(&mut s) % 10 {
            0 | 1 => out.push_str(&format!(
                "Route::resource('res{r}', FooController::class);\n"
            )),
            2 => {
                out.push_str(&format!(
                    "Route::prefix('grp{r}')->name('grp{r}.')->group(function () {{\n"
                ));
                let inner = 2 + (next_rand(&mut s) % 5) as usize;
                for k in 0..inner {
                    out.push_str(&format!(
                        "    Route::get('/g{r}/{k}', [FooController::class, 'show'])->name('show{k}');\n"
                    ));
                }
                out.push_str("});\n");
            }
            3 => out.push_str(&format!(
                "Route::get('/anon{r}', function () {{ return view('x'); }});\n"
            )),
            _ => {
                let verb =
                    ["get", "post", "put", "patch", "delete"][(next_rand(&mut s) % 5) as usize];
                out.push_str(&format!(
                    "Route::{verb}('/path{r}', [FooController::class, 'act{r}'])->name('app.path{r}');\n"
                ));
            }
        }
    }
    out
}

/// Generate a vendor package route file: a flat `Route::group([...], fn)` of
/// mostly named routes plus the occasional resource, as packages typically ship.
fn vendor_route_file(seed: u64, pkg: usize, idx: usize) -> String {
    let mut s = seed ^ (pkg as u64).wrapping_mul(0x0100_0000_01B3) ^ (idx as u64);
    let mut out = String::from("<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\n");
    out.push_str(&format!(
        "Route::group(['prefix' => 'pkg{pkg}', 'as' => 'pkg{pkg}.'], function () {{\n"
    ));
    let routes = 3 + (next_rand(&mut s) % 8) as usize; // 3..=10
    for r in 0..routes {
        if next_rand(&mut s).is_multiple_of(6) {
            out.push_str(&format!(
                "    Route::resource('vres{r}', 'Vendor\\\\Controller');\n"
            ));
        } else {
            let verb = ["get", "post", "put", "delete"][(next_rand(&mut s) % 4) as usize];
            out.push_str(&format!(
                "    Route::{verb}('/v{r}', 'Vendor\\\\Controller@m{r}')->name('widget{r}');\n"
            ));
        }
    }
    out.push_str("});\n");
    out
}

/// An on-disk synthetic project: `routes/*.php` (first-party) plus
/// `vendor/acme/pkgN/routes/*.php` (package routes), shaped so
/// [`discover_route_files`] finds every file.
struct Corpus {
    _dir: tempfile::TempDir, // kept alive so the tempdir isn't removed
    root: PathBuf,
    project_files: Vec<PathBuf>,
    vendor_files: Vec<PathBuf>,
}

fn build_corpus(n_vendor: usize) -> Corpus {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path().to_path_buf();
    let seed = 0x00C0_FFEE_u64;

    let routes_dir = root.join("routes");
    fs::create_dir_all(&routes_dir).expect("create routes/");
    let mut project_files = Vec::with_capacity(PROJECT_FILES);
    for i in 0..PROJECT_FILES {
        let path = routes_dir.join(format!("group{i}.php"));
        fs::write(&path, project_route_file(seed, i)).expect("write project route file");
        project_files.push(path);
    }

    let pkgs = n_vendor.div_ceil(FILES_PER_PKG);
    let mut vendor_files = Vec::with_capacity(n_vendor);
    for pkg in 0..pkgs {
        let pkg_routes = root
            .join("vendor")
            .join(format!("acme/pkg{pkg}"))
            .join("routes");
        fs::create_dir_all(&pkg_routes).expect("create vendor routes/");
        for f in 0..FILES_PER_PKG {
            if vendor_files.len() >= n_vendor {
                break;
            }
            let path = pkg_routes.join(format!("routes{f}.php"));
            fs::write(&path, vendor_route_file(seed, pkg, f)).expect("write vendor route file");
            vendor_files.push(path);
        }
    }

    Corpus {
        _dir: dir,
        root,
        project_files,
        vendor_files,
    }
}

fn read_all(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| fs::read_to_string(p).unwrap_or_default())
        .collect()
}

/// Recursively count every `Route::*` chain node (parity with the byte-scan
/// route-definition count, modulo resource granularity — see AC#4 demo).
fn count_chain_nodes(chains: &[RouteChainNode]) -> usize {
    chains
        .iter()
        .map(|n| 1 + count_chain_nodes(&n.group_children))
        .sum()
}

/// Estimated retained heap bytes of caching the tree-sitter rich per-file model
/// (`Vec<RouteChainNode>` + owned strings), excluding the tree-sitter `Tree`
/// itself (design A folds the tree into this model and drops the tree).
fn model_heap_bytes(chains: &[RouteChainNode]) -> usize {
    let mut bytes = std::mem::size_of_val(chains);
    for n in chains {
        bytes += n.verb.as_ref().map_or(0, String::capacity);
        bytes += n.uri.as_ref().map_or(0, String::capacity);
        bytes += n.prefix_arg.as_ref().map_or(0, String::capacity);
        bytes += n.name.as_ref().map_or(0, |a| a.segment.capacity());
        bytes += model_heap_bytes(&n.group_children);
    }
    bytes
}

/// Byte-scan the whole corpus in memory; returns (elapsed, route-defs found).
fn bench_bytescan(contents: &[String]) -> (Duration, usize) {
    let path = Path::new("routes/web.php");
    let start = Instant::now();
    let mut total = 0;
    for c in contents {
        total += black_box(extract_named_routes(c, path, PRIORITY_APP, &[])).len();
    }
    (start.elapsed(), total)
}

/// Tree-sitter the whole corpus in memory; returns (elapsed, chain nodes found,
/// estimated cached-model bytes).
fn bench_treesitter(contents: &[String]) -> (Duration, usize, usize) {
    let start = Instant::now();
    let mut nodes = 0;
    let mut bytes = 0;
    for c in contents {
        let chains = black_box(extract_route_chains(c));
        nodes += count_chain_nodes(&chains);
        bytes += model_heap_bytes(&chains);
    }
    (start.elapsed(), nodes, bytes)
}

/// Single-file re-parse cost for each strategy (the route-save path).
fn bench_single_file(content: &str) -> (Duration, Duration) {
    let path = Path::new("routes/web.php");
    let t0 = Instant::now();
    for _ in 0..SAVE_ITERS {
        black_box(extract_named_routes(
            black_box(content),
            path,
            PRIORITY_APP,
            &[],
        ));
    }
    let bytescan = t0.elapsed() / SAVE_ITERS;
    let t1 = Instant::now();
    for _ in 0..SAVE_ITERS {
        black_box(extract_route_chains(black_box(content)));
    }
    let treesitter = t1.elapsed() / SAVE_ITERS;
    (bytescan, treesitter)
}

/// The real init cost paid today: discovery + disk read + load-graph expansion
/// + byte-scan, via `build_route_index`.
fn bench_realistic_init(root: &Path) -> (Duration, usize) {
    let start = Instant::now();
    let files = discover_route_files(root);
    let index = build_route_index(root, &files);
    (start.elapsed(), index.len())
}

fn per_file_us(elapsed: Duration, files: usize) -> f64 {
    if files == 0 {
        0.0
    } else {
        elapsed.as_secs_f64() * 1e6 / files as f64
    }
}

/// AC#4 — demonstrate the `Route::resource` granularity difference directly.
fn resource_granularity_demo() {
    let src = "<?php\nuse Illuminate\\Support\\Facades\\Route;\nRoute::resource('photos', PhotoController::class);\n";
    let bytescan = extract_named_routes(src, Path::new("routes/web.php"), PRIORITY_APP, &[]);
    let treesitter = extract_route_chains(src);

    println!("\n== Route::resource granularity (AC#4) ==");
    println!("source: Route::resource('photos', PhotoController::class);");
    let mut names: Vec<String> = bytescan.iter().filter_map(|(n, _)| n.clone()).collect();
    names.sort();
    println!(
        "  byte-scan      -> {} named routes: {}",
        names.len(),
        names.join(", ")
    );
    println!(
        "  tree-sitter    -> {} chain node(s), verb={:?}, uri={:?}, name={:?}",
        treesitter.len(),
        treesitter.first().and_then(|n| n.verb.clone()),
        treesitter.first().and_then(|n| n.uri.clone()),
        treesitter
            .first()
            .and_then(|n| n.name.as_ref().map(|a| a.segment.clone())),
    );
}

fn parse_scales() -> Vec<usize> {
    match std::env::var("ROUTE_BENCH_SCALES") {
        Ok(raw) => raw
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .collect(),
        Err(_) => vec![500, 2000, 5000],
    }
}

fn main() {
    let scales = parse_scales();
    println!("route-cache strategy benchmark (issue #48) — SYNTHETIC corpus");
    println!(
        "project route files (fixed): {PROJECT_FILES} | files per vendor pkg: {FILES_PER_PKG}\n"
    );

    resource_granularity_demo();

    for &n_vendor in &scales {
        let corpus = build_corpus(n_vendor);
        let total_files = corpus.project_files.len() + corpus.vendor_files.len();
        let vendor_pct = 100.0 * corpus.vendor_files.len() as f64 / total_files as f64;

        let project_contents = read_all(&corpus.project_files);
        let vendor_contents = read_all(&corpus.vendor_files);
        let all_contents: Vec<String> = project_contents
            .iter()
            .cloned()
            .chain(vendor_contents.iter().cloned())
            .collect();

        println!("\n========================================================");
        println!(
            "SCALE: {} vendor files + {} project files = {} total ({:.1}% vendor)",
            corpus.vendor_files.len(),
            corpus.project_files.len(),
            total_files,
            vendor_pct
        );
        println!("--------------------------------------------------------");

        // 1. init path — whole corpus, in-memory, each strategy
        let (bs_dur, bs_defs) = bench_bytescan(&all_contents);
        let (ts_dur, ts_nodes, ts_bytes) = bench_treesitter(&all_contents);
        println!("[init / whole corpus, in-memory parse only]");
        println!(
            "  byte-scan   : {:>9.2?}  ({:.2} us/file, {} route-defs)",
            bs_dur,
            per_file_us(bs_dur, total_files),
            bs_defs
        );
        println!(
            "  tree-sitter : {:>9.2?}  ({:.2} us/file, {} chain nodes)",
            ts_dur,
            per_file_us(ts_dur, total_files),
            ts_nodes
        );
        println!(
            "  tree-sitter / byte-scan slowdown: {:.1}x",
            ts_dur.as_secs_f64() / bs_dur.as_secs_f64().max(f64::EPSILON)
        );

        // 2. route-save path — single representative project file
        let (save_bs, save_ts) = bench_single_file(&project_contents[0]);
        println!("[route-save / single project file re-parse]");
        println!("  byte-scan   : {save_bs:>9.2?}");
        println!("  tree-sitter : {save_ts:>9.2?}");
        println!(
            "  tree-sitter / byte-scan slowdown: {:.1}x",
            save_ts.as_secs_f64() / save_bs.as_secs_f64().max(f64::EPSILON)
        );

        // 3. realistic init — build_route_index (discovery + I/O + byte-scan)
        let (real_dur, indexed) = bench_realistic_init(&corpus.root);
        println!("[realistic init / build_route_index, disk + discovery]");
        println!(
            "  build_route_index: {:>9.2?}  ({:.2} us/file, {} names indexed)",
            real_dur,
            per_file_us(real_dur, total_files),
            indexed
        );

        // 4. memory — rich per-file model cache
        let mb = ts_bytes as f64 / (1024.0 * 1024.0);
        let bytes_per_file = ts_bytes as f64 / total_files as f64;
        println!("[memory / cached tree-sitter rich model, all files]");
        println!("  estimated heap: {mb:.2} MiB  ({bytes_per_file:.0} bytes/file)");
    }

    println!("\nNOTE: synthetic corpus. Re-point build_route_index at a real");
    println!("vendor/ tree for literal per-project numbers (see module docs).");
}
