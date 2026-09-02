//! Step-0 measurement bench for issue #373 — where the remaining warm-start
//! and dependency-change time actually goes, before anything is parallelized.
//!
//! #373 names two paths as the only ones with room left, and asks for both to
//! be timed **in isolation** before an implementation is written, because this
//! project has twice guessed the wrong target from architecture and been
//! corrected by measurement. This bench is that measurement.
//!
//! # What it measures
//!
//! 1. **The shared vendor pass** —
//!    [`laravel_lsp::vendor_scan::build_route_files_and_command_index`], broken
//!    into its four stages so the parallelizable fraction is visible rather
//!    than inferred:
//!
//!    | Stage | Work | Parallelizes? |
//!    |---|---|---|
//!    | walk | [`VendorIndex::build`] — one directory traversal | no (single walk) |
//!    | pre-pass | one `metadata` per command-wanted file | I/O bound |
//!    | read | `read_to_string` per wanted file | I/O bound |
//!    | classify (route) | `accept_vendor_route_source` — pure CPU | yes |
//!    | classify (command) | `record_source` — CPU **plus one `metadata`** | partly |
//!
//!    That last row is the reason the split is measured rather than assumed:
//!    `record_source` (`command_index.rs`) stats each file before classifying
//!    it, so a naive "everything after the read is CPU" reading would overstate
//!    what threading can win.
//!
//! 2. **The serialized parse point** — `SalsaHandle::get_patterns` driven in a
//!    serial loop, which is exactly what `run_magic_batch_once` does per vendor
//!    file after a `composer install`. Every one of those parses funnels
//!    through the single Salsa actor thread, so this measures the funnel, at
//!    several batch sizes.
//!
//! # What it does NOT measure
//!
//! The rest of `run_magic_batch_once` — the registration diff, the surface
//! diff, and the dependent ripple. `Backend` is private to `main.rs`, which is
//! the binary crate, so a bench cannot reach it. What is measured is the stage
//! #373 identifies as the bottleneck, not the whole function.
//!
//! # Honesty notes
//!
//! * **Reads are warm-page-cache.** An untimed full pass runs first, so every
//!   timed stage sees the same warm filesystem cache, the same compiled lazy
//!   regexes, and a settled allocator. Without that levelling the first timed
//!   stage would absorb all three costs and the split would be fiction. The
//!   consequence is that a genuinely cold first start reads slower than the
//!   `read` row here — this bench measures the steady state, which is the
//!   state the parallelization work is aimed at.
//! * **No command scan cache is passed** (`None` everywhere), so the pre-pass
//!   never settles a file from cache and every command-wanted file is read.
//!   That is the cold-cache shape — the worst case, and the one a
//!   `composer install` produces.
//! * **Each parse batch gets a fresh Salsa actor**, so a larger batch never
//!   reads the smaller batch's warm pattern cache. Sharing one actor across
//!   batch sizes would make every batch after the first measure cache hits.
//!
//! # Running
//!
//! ```text
//! cargo bench --bench vendor_parallelism
//! VENDOR_BENCH_ROOT=/path/to/laravel/app cargo bench --bench vendor_parallelism
//! PARSE_BENCH_BATCHES=100,500 cargo bench --bench vendor_parallelism
//! ```
//!
//! It needs a real `vendor/` tree. `test-project/vendor` is gitignored, so the
//! bench prints a skip notice and exits cleanly when the tree is absent rather
//! than failing — CI installs it with `composer update`, a fresh clone does
//! not.

use std::collections::HashSet;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use laravel_lsp::command_index::{
    record_source, try_cached, vendor_command_needs_source, CommandScan,
};
use laravel_lsp::parse_budget::skip_reason_on_disk;
use laravel_lsp::route_discovery::{
    accept_vendor_route_source, vendor_route_needs_source, RouteFileSet,
};
use laravel_lsp::salsa_impl::SalsaActor;
use laravel_lsp::vendor_index::VendorIndex;
use laravel_lsp::vendor_scan::build_route_files_and_command_index;

/// Batch sizes for the parse-funnel measurement, overridable with
/// `PARSE_BENCH_BATCHES`. Chosen to bracket a real dependency change: a single
/// package update rewrites a few hundred files, a `composer update` rewrites
/// thousands.
const DEFAULT_BATCHES: &[usize] = &[250, 1000, 4000];

/// Resolve the project root to measure: `VENDOR_BENCH_ROOT` if set, else the
/// repo's own `test-project/` beside the `laravel-lsp/` manifest.
fn bench_root() -> PathBuf {
    if let Ok(explicit) = std::env::var("VENDOR_BENCH_ROOT") {
        return PathBuf::from(explicit);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|repo| repo.join("test-project"))
        .unwrap_or_else(|| PathBuf::from("test-project"))
}

fn batch_sizes() -> Vec<usize> {
    match std::env::var("PARSE_BENCH_BATCHES") {
        Ok(raw) => raw
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .collect(),
        Err(_) => DEFAULT_BATCHES.to_vec(),
    }
}

/// Milliseconds, for a column that stays readable across three orders of
/// magnitude (`{:?}` on a `Duration` switches units mid-table).
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Microseconds per item, or `0.0` for an empty set — the per-file column that
/// makes two runs on differently-sized vendor trees comparable.
fn us_each(d: Duration, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    d.as_secs_f64() * 1_000_000.0 / n as f64
}

/// One wanted vendor file, read once so the classify stages can replay it
/// without paying the read again.
struct Sample {
    path: PathBuf,
    wants_route: bool,
    wants_command: bool,
    content: String,
}

/// The `metadata` + `modified` pair `record_source` performs through its
/// private `file_mtime` helper. Timed on its own so the command classifier's
/// CPU can be separated from the stat it hides.
fn stat_mtime(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|_| true)
        .unwrap_or(false)
}

fn main() {
    let root = bench_root();
    let vendor_dir = root.join("vendor");

    println!("vendor-parallelism measurement bench (issue #373, step 0)");
    println!("root: {}", root.display());

    println!("\n== environment ==");
    println!(
        "  os / arch     : {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "  parallelism   : {}",
        std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    );

    if !vendor_dir.is_dir() {
        println!("\nSKIPPED: no vendor/ tree at {}.", vendor_dir.display());
        println!("This bench measures a real dependency tree, and test-project/vendor");
        println!("is gitignored. Run `composer update` in test-project/, or point");
        println!("VENDOR_BENCH_ROOT at a Laravel project, then re-run.");
        return;
    }

    // ---- corpus -----------------------------------------------------------
    //
    // The walk is timed here rather than inside stage 1 because every later
    // stage consumes the same `VendorIndex`; re-walking per stage would
    // measure the walk three times and the stages never.
    let walk_start = Instant::now();
    let vendor = VendorIndex::build(&root);
    let walk = walk_start.elapsed();

    if vendor.is_empty() {
        println!("\nSKIPPED: {} holds no PHP files.", vendor_dir.display());
        return;
    }

    let vendor_root = vendor.vendor_root().to_path_buf();

    // Untimed levelling pass. This warms the page cache, compiles every lazy
    // regex the classifiers use, and settles the allocator, so the first timed
    // stage below is not the one that pays for all three. See the module docs.
    let warmup = build_route_files_and_command_index(&root, &vendor, None);
    black_box(&warmup);
    drop(warmup);

    // ---- stage: command pre-pass -----------------------------------------
    //
    // Mirrors vendor_scan's own pre-pass with no scan cache, so every
    // command-wanted file falls through to "needs a read".
    let prepass_start = Instant::now();
    let mut prepass_scan = CommandScan::default();
    let mut command_needs_read: HashSet<PathBuf> = HashSet::new();
    for file in vendor.files() {
        if vendor_command_needs_source(&vendor_root, file)
            && !try_cached(&mut prepass_scan, None, &file.path)
        {
            command_needs_read.insert(file.path.clone());
        }
    }
    let prepass = prepass_start.elapsed();

    let wants = |file: &laravel_lsp::vendor_index::VendorFile| {
        vendor_route_needs_source(file) || command_needs_read.contains(&file.path)
    };

    // ---- stage: read ------------------------------------------------------
    //
    // Pure read cost: the content is measured and dropped, with no copy into a
    // collection, so the number is `read_to_string` and nothing else.
    let mut wanted_files = 0usize;
    let mut bytes_read = 0u64;
    let read_start = Instant::now();
    vendor.for_each_source(wants, |_file, content| {
        wanted_files += 1;
        bytes_read += content.len() as u64;
        black_box(content.len());
    });
    let read = read_start.elapsed();

    // Untimed: collect the same content so the classify stages replay it from
    // memory. Reads are already warm, so this second pass distorts nothing.
    let mut samples: Vec<Sample> = Vec::with_capacity(wanted_files);
    vendor.for_each_source(wants, |file, content| {
        samples.push(Sample {
            path: file.path.clone(),
            wants_route: vendor_route_needs_source(file),
            wants_command: command_needs_read.contains(&file.path),
            content: content.to_string(),
        });
    });

    let route_samples = samples.iter().filter(|s| s.wants_route).count();
    let command_samples = samples.iter().filter(|s| s.wants_command).count();

    // ---- stage: classify (route) -----------------------------------------
    //
    // Pure CPU. `accept_vendor_route_source` inspects the text and touches no
    // filesystem, so all of this parallelizes.
    let route_start = Instant::now();
    let mut routes = RouteFileSet::default();
    for sample in samples.iter().filter(|s| s.wants_route) {
        accept_vendor_route_source(&mut routes, &sample.path, &sample.content);
    }
    let route_classify = route_start.elapsed();
    black_box(routes.into_files().len());

    // ---- stage: classify (command) ---------------------------------------
    //
    // CPU **plus** one `metadata` per file, inside `record_source`. Only the
    // CPU half parallelizes cleanly, so the stat is measured on its own next.
    let command_start = Instant::now();
    let mut commands = CommandScan::default();
    for sample in samples.iter().filter(|s| s.wants_command) {
        record_source(&mut commands, &sample.path, &sample.content);
    }
    let command_classify = command_start.elapsed();
    black_box(commands.files.len());

    // ---- stage: the stat hidden inside the command classifier -------------
    let stat_start = Instant::now();
    let mut stat_hits = 0usize;
    for sample in samples.iter().filter(|s| s.wants_command) {
        if stat_mtime(&sample.path) {
            stat_hits += 1;
        }
    }
    let command_stats = stat_start.elapsed();
    black_box(stat_hits);

    // ---- cross-check: the whole pass, end to end -------------------------
    //
    // The stages above are a decomposition, not a model. If they do not add up
    // to the real function, the decomposition is wrong and the split below
    // should not be trusted.
    let whole_start = Instant::now();
    let (whole_routes, whole_commands) = build_route_files_and_command_index(&root, &vendor, None);
    let whole = whole_start.elapsed();

    let staged = prepass + read + route_classify + command_classify;
    let cpu_only = route_classify + command_classify.saturating_sub(command_stats);
    let io_only = prepass + read + command_classify.min(command_stats);

    println!("\n== corpus ==");
    println!("  vendor php files    : {}", vendor.len());
    println!("  wanted (read once)  : {wanted_files}");
    println!("    for routes        : {route_samples}");
    println!("    for commands      : {command_samples}");
    println!(
        "  bytes read          : {:.1} MiB",
        bytes_read as f64 / (1024.0 * 1024.0)
    );
    println!("  route files found   : {}", whole_routes.len());
    println!("  commands scanned    : {}", whole_commands.files.len());

    println!("\n== 1. shared vendor pass (vendor_scan::build_route_files_and_command_index) ==");
    println!("  {:<26} {:>10}  {:>12}", "stage", "total ms", "us / file");
    println!("  {:-<26} {:->10}  {:->12}", "", "", "");
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "walk (VendorIndex)",
        ms(walk),
        us_each(walk, vendor.len())
    );
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "pre-pass (stat)",
        ms(prepass),
        us_each(prepass, vendor.len())
    );
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "read (warm cache)",
        ms(read),
        us_each(read, wanted_files)
    );
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "classify: route (CPU)",
        ms(route_classify),
        us_each(route_classify, route_samples)
    );
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "classify: command",
        ms(command_classify),
        us_each(command_classify, command_samples)
    );
    println!(
        "  {:<26} {:>10.2}  {:>12.2}",
        "  of which: stat",
        ms(command_stats),
        us_each(command_stats, command_samples)
    );
    println!("  {:-<26} {:->10}  {:->12}", "", "", "");
    println!("  {:<26} {:>10.2}", "staged sum", ms(staged));
    println!("  {:<26} {:>10.2}", "whole pass (measured)", ms(whole));
    println!(
        "  {:<26} {:>10.1}%",
        "staged / whole",
        if whole.as_secs_f64() > 0.0 {
            staged.as_secs_f64() / whole.as_secs_f64() * 100.0
        } else {
            0.0
        }
    );

    println!("\n  split (walk excluded — a single traversal, not divisible):");
    println!(
        "    CPU, parallelizable : {:>8.2} ms  ({:>4.1}% of staged)",
        ms(cpu_only),
        if staged.as_secs_f64() > 0.0 {
            cpu_only.as_secs_f64() / staged.as_secs_f64() * 100.0
        } else {
            0.0
        }
    );
    println!(
        "    I/O, stat + read    : {:>8.2} ms  ({:>4.1}% of staged)",
        ms(io_only),
        if staged.as_secs_f64() > 0.0 {
            io_only.as_secs_f64() / staged.as_secs_f64() * 100.0
        } else {
            0.0
        }
    );

    // ---- 2. the serialized parse funnel -----------------------------------
    parse_funnel(&vendor);

    println!("\n== notes ==");
    println!("  * Reads are WARM page cache — an untimed full pass runs first so");
    println!("    every stage is levelled. A cold first start reads slower.");
    println!("  * No command scan cache is passed, so every command-wanted file");
    println!("    is read. That is the cold-cache shape a composer install makes.");
    println!("  * 'staged / whole' below ~100% means the decomposition misses");
    println!("    work (the non-vendor legs the whole pass also runs); far from");
    println!("    100% means the split above should not be trusted.");
}

/// Measure the single-threaded Salsa parse funnel: N vendor files pushed
/// serially through `get_patterns`, which is what `run_magic_batch_once` does
/// per vendor file after a dependency change.
fn parse_funnel(vendor: &VendorIndex) {
    // Only files the parse budget admits — `run_magic_batch_once` skips the
    // rest (`*.json.php` and anything over the size cap), so including them
    // would measure work production never does.
    let mut eligible: Vec<PathBuf> = Vec::new();
    let mut excluded = 0usize;
    for file in vendor.files() {
        if skip_reason_on_disk(&file.path).is_some() {
            excluded += 1;
        } else {
            eligible.push(file.path.clone());
        }
    }

    println!("\n== 2. serialized parse point (SalsaHandle::get_patterns, serial loop) ==");
    println!("  eligible vendor files : {}", eligible.len());
    println!("  excluded by budget    : {excluded}");

    if eligible.len() < 2 {
        println!("  SKIPPED: not enough eligible files to measure.");
        return;
    }

    // Reserved as each fresh actor's untimed first request, so the actor's
    // one-time query-cache prewarm never lands inside a measured batch. It is
    // taken from the END of the list and every batch is measured from the
    // front, so no measured file is ever served from the warm-up's cache.
    let warmup_path = eligible
        .pop()
        .expect("eligible has at least two entries here");

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            println!("  SKIPPED: could not build a tokio runtime: {e}");
            return;
        }
    };

    println!(
        "\n  {:<10} {:>10}  {:>12}  {:>14}",
        "batch", "total ms", "us / file", "files / sec"
    );
    println!("  {:-<10} {:->10}  {:->12}  {:->14}", "", "", "", "");

    for size in batch_sizes() {
        let n = size.min(eligible.len());
        if n == 0 {
            continue;
        }
        let batch: Vec<PathBuf> = eligible[..n].to_vec();

        // A FRESH actor per batch. Sharing one would let every batch after the
        // first read the previous batch's warm pattern cache, which measures
        // cache hits rather than parses.
        let handle = SalsaActor::spawn();
        let warm = warmup_path.clone();

        let elapsed = runtime.block_on(async move {
            let _ = handle.get_patterns(warm).await;

            let start = Instant::now();
            for path in batch {
                black_box(handle.get_patterns(path).await.ok());
            }
            start.elapsed()
        });

        println!(
            "  {:<10} {:>10.2}  {:>12.2}  {:>14.0}",
            n,
            ms(elapsed),
            us_each(elapsed, n),
            n as f64 / elapsed.as_secs_f64()
        );

        if n == eligible.len() {
            // Every later size would measure the same batch.
            break;
        }
    }

    println!("\n  Every parse above runs on the ONE Salsa actor thread. A composer");
    println!("  install rewrites thousands of vendor files straight into this loop.");
}
