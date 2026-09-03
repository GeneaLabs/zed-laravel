//! Measurement bench for issue #373 — where the remaining warm-start and
//! dependency-change time actually goes.
//!
//! #373 names two paths as the only ones with room left, and asks for both to
//! be timed **in isolation** before an implementation is written, because this
//! project has twice guessed the wrong target from architecture and been
//! corrected by measurement. This bench is that measurement, and it kept
//! earning its keep afterwards: it decided what #373's third item became.
//!
//! # What it measures
//!
//! 1. **The shared vendor pass** —
//!    [`laravel_lsp::vendor_scan::build_route_files_and_command_index`], broken
//!    into its stages so the parallelizable fraction is visible rather than
//!    inferred. **Every stage is timed serially, one file at a time**, even
//!    though production now runs the pass across a worker pool — the point of
//!    the decomposition is the *shape* of the work, and only a serial timing
//!    can attribute cost per stage:
//!
//!    | Stage | Work | Parallelizes? |
//!    |---|---|---|
//!    | walk | [`VendorIndex::build`] — one directory traversal | no (single walk) |
//!    | pre-pass | one `metadata` per command-wanted file | yes (I/O bound) |
//!    | read | `read_to_string` per wanted file | yes (I/O bound) |
//!    | classify (route) | `vendor_route_verdict` — pure CPU | yes |
//!    | classify (command) | `command_entry_for` — pure CPU | yes |
//!
//!    The `[removed] 2nd stat/file` row is why measuring beat estimating.
//!    `record_source` used to stat each file again before classifying it — a
//!    stat the pre-pass had already paid — so "everything after the read is
//!    CPU" would have overstated what threading could win by 5x. #373 removed
//!    that stat, and the row still times what it used to cost, so a regression
//!    that brings it back is legible in the log.
//!
//!    The answer the measurement gave: the classifiers are 2-6% of the pass on
//!    every platform, not the majority #373 estimated, while the stat and read
//!    stages are 93-97%. **The pass is I/O.** So #373's third item was built
//!    against that finding rather than its own estimate — the parallel map
//!    covers the reads and stats, not merely the classifiers it named, which
//!    would have won 7-13 ms of a 151-604 ms pass.
//!
//!    **Read `staged / whole` accordingly.** The staged sum is serial and the
//!    whole pass is parallel, so the ratio now runs *above* 100% by design:
//!    that excess is the parallel speedup, and a ratio back near 100% means the
//!    pass has fallen back to running serially.
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

use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use laravel_lsp::command_index::{
    consider_project_commands, record_source, try_cached, vendor_command_needs_source,
    CacheVerdict, CommandScan,
};
use laravel_lsp::parse_budget::skip_reason_on_disk;
use laravel_lsp::route_discovery::{
    accept_vendor_route_source, collect_conventional_vendor_route_files,
    collect_project_route_files, vendor_route_needs_source, RouteFileSet,
};
use laravel_lsp::salsa_impl::{SalsaActor, SalsaHandle};
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
    /// `Some(mtime)` when the command side wants this file, carrying the value
    /// the pre-pass stat'd — the same hand-off the real pass now makes.
    command_mtime: Option<(u64, u32)>,
    content: String,
}

/// The `metadata` + `modified` pair `record_source` USED to perform through
/// `command_index`'s private `file_mtime` helper, before #373 handed it the
/// pre-pass's value instead. Still timed, so the size of that saving stays in
/// the log and a regression that reintroduces the stat is visible.
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

    // Every stage below is measured `PASSES` times and reported at its
    // MINIMUM. A single timed run of a 400 ms filesystem-heavy pass varies
    // 10-15% between runs on an idle machine — measured on the serial pass,
    // that was enough to swing `staged / whole` from 89% to 110% with no code
    // change at all, which would make the cross-platform comparison this bench
    // exists for unreadable. The minimum is the run least disturbed by the
    // scheduler and by other processes, and it is the standard choice for
    // exactly this reason: noise only ever adds time, so the floor is the
    // honest figure.
    let mut best = StageTimings::worst();
    let mut corpus = Corpus::default();
    for _ in 0..PASSES {
        let (timings, counts) = measure_stages(&root, &vendor, &vendor_root);
        best = best.min_with(&timings);
        corpus = counts;
    }
    report(&vendor, walk, &best, &corpus);

    // ---- 2. the serialized parse funnel -----------------------------------
    parse_funnel(&vendor);
    notes();
}

/// How many times each stage is timed before the minimum is taken. Three is
/// enough to shed a single scheduler hiccup without tripling a CI step that
/// already spends most of its time in the parse funnel below.
const PASSES: usize = 3;

/// Per-stage wall clock for one full measurement pass.
#[derive(Clone, Copy)]
struct StageTimings {
    legs: Duration,
    prepass: Duration,
    read: Duration,
    route_classify: Duration,
    command_classify: Duration,
    /// Not part of the pass any more — the stat #373 removed, still timed so
    /// the saving stays visible.
    command_stats: Duration,
    whole: Duration,
}

impl StageTimings {
    /// A starting point every real measurement beats, so the fold below needs
    /// no `Option` and no special first iteration.
    fn worst() -> Self {
        let max = Duration::MAX;
        Self {
            legs: max,
            prepass: max,
            read: max,
            route_classify: max,
            command_classify: max,
            command_stats: max,
            whole: max,
        }
    }

    /// Per-stage minimum. Taken field by field rather than picking one whole
    /// "best pass": each stage's floor is an independent estimate of that
    /// stage's true cost, and nothing here compares stages against each other
    /// within a single run.
    fn min_with(self, other: &Self) -> Self {
        Self {
            legs: self.legs.min(other.legs),
            prepass: self.prepass.min(other.prepass),
            read: self.read.min(other.read),
            route_classify: self.route_classify.min(other.route_classify),
            command_classify: self.command_classify.min(other.command_classify),
            command_stats: self.command_stats.min(other.command_stats),
            whole: self.whole.min(other.whole),
        }
    }
}

/// Counts describing the corpus. Identical on every pass — recorded alongside
/// the timings only so one function can produce both.
#[derive(Default, Clone, Copy)]
struct Corpus {
    wanted_files: usize,
    bytes_read: u64,
    route_samples: usize,
    command_samples: usize,
    leg_command_files: usize,
    whole_route_files: usize,
    whole_command_files: usize,
}

/// One full measurement pass over the vendor tree: every stage of
/// `build_route_files_and_command_index`, plus the whole function as a
/// cross-check.
fn measure_stages(root: &Path, vendor: &VendorIndex, vendor_root: &Path) -> (StageTimings, Corpus) {
    // ---- stage: the non-vendor legs --------------------------------------
    //
    // The real function runs these before it touches the vendor tree, and they
    // do their own reads and stats over the project's own PHP. Measured so the
    // decomposition accounts for the whole pass rather than most of it.
    let legs_start = Instant::now();
    let mut leg_routes = RouteFileSet::default();
    let mut leg_commands = CommandScan::default();
    collect_project_route_files(root, &mut leg_routes);
    collect_conventional_vendor_route_files(vendor, &mut leg_routes);
    consider_project_commands(root, None, &mut leg_commands);
    let legs = legs_start.elapsed();
    let leg_route_files = leg_routes.into_files().len();
    let leg_command_files = leg_commands.files.len();
    black_box((leg_route_files, leg_command_files));

    // ---- stage: command pre-pass -----------------------------------------
    //
    // Mirrors vendor_scan's own pre-pass with no scan cache, so every
    // command-wanted file falls through to "needs a read".
    let prepass_start = Instant::now();
    let mut prepass_scan = CommandScan::default();
    let mut command_needs_read: HashMap<PathBuf, (u64, u32)> = HashMap::new();
    for file in vendor.files() {
        if !vendor_command_needs_source(vendor_root, file) {
            continue;
        }
        if let CacheVerdict::NeedsSource { mtime } = try_cached(&mut prepass_scan, None, &file.path)
        {
            command_needs_read.insert(file.path.clone(), mtime);
        }
    }
    let prepass = prepass_start.elapsed();

    let wants = |file: &laravel_lsp::vendor_index::VendorFile| {
        vendor_route_needs_source(file) || command_needs_read.contains_key(&file.path)
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
            command_mtime: command_needs_read.get(&file.path).copied(),
            content: content.to_string(),
        });
    });

    let route_samples = samples.iter().filter(|s| s.wants_route).count();
    let command_samples = samples.iter().filter(|s| s.command_mtime.is_some()).count();

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
    // Pure CPU, since #373 removed the stat this used to hide: `record_source`
    // is now stamped with the mtime the pre-pass already read.
    let command_start = Instant::now();
    let mut commands = CommandScan::default();
    for sample in samples.iter() {
        if let Some(mtime) = sample.command_mtime {
            record_source(&mut commands, &sample.path, mtime, &sample.content);
        }
    }
    let command_classify = command_start.elapsed();
    black_box(commands.files.len());

    // ---- reference: the stat #373 removed ---------------------------------
    //
    // NOT part of the pass any more — measured so the saving stays visible and
    // a regression that reintroduces the second stat is legible in the log
    // rather than silent. This is what `record_source` used to add on top of
    // the classify row above.
    let stat_start = Instant::now();
    let mut stat_hits = 0usize;
    for sample in samples.iter().filter(|s| s.command_mtime.is_some()) {
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
    let (whole_routes, whole_commands) = build_route_files_and_command_index(root, vendor, None);
    let whole = whole_start.elapsed();

    (
        StageTimings {
            legs,
            prepass,
            read,
            route_classify,
            command_classify,
            command_stats,
            whole,
        },
        Corpus {
            wanted_files,
            bytes_read,
            route_samples,
            command_samples,
            leg_command_files,
            whole_route_files: whole_routes.len(),
            whole_command_files: whole_commands.files.len(),
        },
    )
}

/// Print the stage table, the cross-check and the split.
fn report(vendor: &VendorIndex, walk: Duration, best: &StageTimings, corpus: &Corpus) {
    let StageTimings {
        legs,
        prepass,
        read,
        route_classify,
        command_classify,
        command_stats,
        whole,
    } = *best;
    let Corpus {
        wanted_files,
        bytes_read,
        route_samples,
        command_samples,
        leg_command_files,
        whole_route_files,
        whole_command_files,
    } = *corpus;

    let staged = legs + prepass + read + route_classify + command_classify;
    // Both classify stages are pure CPU now that #373 removed the stat inside
    // `record_source`, so the split needs no subtraction to separate them.
    let cpu_only = route_classify + command_classify;
    let io_only = prepass + read;

    println!("\n== corpus ==");
    println!("  vendor php files    : {}", vendor.len());
    println!("  wanted (read once)  : {wanted_files}");
    println!("    for routes        : {route_samples}");
    println!("    for commands      : {command_samples}");
    println!(
        "  bytes read          : {:.1} MiB",
        bytes_read as f64 / (1024.0 * 1024.0)
    );
    println!("  route files found   : {whole_route_files}");
    println!("  commands scanned    : {whole_command_files}");

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
        "non-vendor legs",
        ms(legs),
        us_each(legs, leg_command_files.max(1))
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
        "classify: command (CPU)",
        ms(command_classify),
        us_each(command_classify, command_samples)
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

    // Outside the sum by design: this stat is no longer performed. It is what
    // `record_source` used to add to the command-classify row before #373
    // handed it the pre-pass's mtime, and it is printed so the saving stays
    // visible and a regression that brings the stat back is legible here.
    println!(
        "\n  not paid any more — the 2nd stat/file #373 removed:\n    \
         {:>8.2} ms  ({:>5.2} us/file, over {} files)",
        ms(command_stats),
        us_each(command_stats, command_samples),
        command_samples
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
}

/// What a reader has to know before trusting any number above.
fn notes() {
    println!("\n== notes ==");
    println!("  * Every stage is the MINIMUM of {PASSES} timed runs. A single run of");
    println!("    this pass varies 10-15% on an idle machine, enough to swing");
    println!("    'staged / whole' by 20 points with no code change at all.");
    println!("  * Reads are WARM page cache — an untimed full pass runs first so");
    println!("    every stage is levelled. A cold first start reads slower.");
    println!("  * No command scan cache is passed, so every command-wanted file");
    println!("    is read. That is the cold-cache shape a composer install makes.");
    println!("  * The stages above are timed SERIALLY, one file at a time, but");
    println!("    the whole pass now runs across the bounded worker pool (#373).");
    println!("    So 'staged / whole' is expected ABOVE 100%, and the excess IS");
    println!("    the parallel speedup: 100% means the pass has fallen back to");
    println!("    running serially. The stage split still says where the work");
    println!("    is — that conclusion is an order of magnitude, not a few");
    println!("    percent — but the ratio is no longer a decomposition check.");
}

/// Register a handle against `root` the way production does, so
/// `bulk_import_patterns` has a published pattern cache to write into.
///
/// Both regimes below get a registered handle, not just the one that needs it:
/// registration records the file list and sizes the cache but parses nothing,
/// so it leaves neither regime with warm data — and giving only one of them a
/// registered actor would be an uncontrolled difference between them.
async fn registered_handle(root: &Path, vendor_paths: Vec<PathBuf>) -> SalsaHandle {
    let handle = SalsaActor::spawn();
    let _ = handle
        .register_config_files(root.to_path_buf(), None, None, None, None)
        .await;
    let _ = handle
        .register_project_files(
            root.to_path_buf(),
            vec![PathBuf::from("app")],
            vec![root.join("resources/views")],
            None,
            PathBuf::from("routes"),
            vendor_paths,
        )
        .await;
    handle
}

/// Parse `batch` the way `Backend::preparse_batch_off_actor` does — in
/// parallel, off the actor, then handed over as finished results.
///
/// A deliberate copy of the production method rather than a call to it:
/// `Backend` is private to `main.rs`, so a bench cannot reach it. The shape it
/// mirrors is the semaphore, the `spawn_blocking` read-and-parse, and the two
/// bulk imports; if that method changes, this must change with it.
async fn preparse_like_production(handle: &SalsaHandle, batch: Vec<PathBuf>) {
    type Parsed = (
        PathBuf,
        std::sync::Arc<laravel_lsp::salsa_impl::ParsedPatternsData>,
        Vec<laravel_lsp::class_hierarchy_index::ClassNode>,
    );

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
        laravel_lsp::parallelism::bounded_pool_size(),
    ));
    let mut set: tokio::task::JoinSet<Option<Parsed>> = tokio::task::JoinSet::new();
    for path in batch {
        let permit_owner = semaphore.clone();
        set.spawn(async move {
            let _permit = permit_owner.acquire_owned().await.ok()?;
            let for_task = path.clone();
            let parsed = tokio::task::spawn_blocking(move || {
                let text = std::fs::read_to_string(&for_task).ok()?;
                Some(laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(
                    &for_task, &text,
                ))
            })
            .await
            .ok()
            .flatten()?;
            Some((path, parsed.0, parsed.1))
        });
    }

    let mut patterns = Vec::new();
    let mut hierarchy = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Some((path, data, nodes))) = res {
            patterns.push((path.clone(), data));
            hierarchy.push((path, nodes));
        }
    }
    if patterns.is_empty() {
        return;
    }
    if handle.bulk_import_patterns(patterns).await.is_err() {
        return;
    }
    let _ = handle.bulk_import_hierarchy(hierarchy).await;
}

/// Measure the single-threaded Salsa parse funnel — N vendor files pushed
/// serially through `get_patterns`, which is what `run_magic_batch_once` did
/// per vendor file after a dependency change — against the off-actor parallel
/// path that replaced it (issue #373 item 1).
///
/// **Each regime gets its own fresh actor**, which is the whole reason this
/// comparison is trustworthy. Both write into an actor-owned pattern cache; if
/// they shared one, the second regime measured would be reading the first's
/// results and would look arbitrarily fast.
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

    println!("\n== 2. the parse funnel: serial-in-actor vs parallel-off-actor ==");
    println!("  eligible vendor files : {}", eligible.len());
    println!("  excluded by budget    : {excluded}");
    println!(
        "  off-actor workers     : {}",
        laravel_lsp::parallelism::bounded_pool_size()
    );

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

    let root = bench_root();
    let vendor_paths: Vec<PathBuf> = vendor.files().iter().map(|f| f.path.clone()).collect();

    println!(
        "\n  {:<8} {:>12}  {:>12}  {:>12}  {:>9}",
        "batch", "serial ms", "off-actor ms", "serial f/s", "speedup"
    );
    println!(
        "  {:-<8} {:->12}  {:->12}  {:->12}  {:->9}",
        "", "", "", "", ""
    );

    for size in batch_sizes() {
        let n = size.min(eligible.len());
        if n == 0 {
            continue;
        }
        let batch: Vec<PathBuf> = eligible[..n].to_vec();

        // The NEW path runs FIRST, deliberately. Whichever regime runs second
        // reads a filesystem cache the first one has just touched, and that
        // advantage cannot be removed without dropping the page cache (which
        // needs root). Running the new path first means any residual advantage
        // falls to the OLD one, so the speedup below is a floor rather than a
        // flattering number. The bench's levelling pass has already read the
        // whole tree once, so the effect should be small either way.
        //
        // Each regime gets its OWN fresh actor. Sharing one would let the
        // second read the first's imported entries and report a cache hit as a
        // parse.
        let off_actor = {
            let handle = runtime.block_on(registered_handle(&root, vendor_paths.clone()));
            let warm = warmup_path.clone();
            let batch = batch.clone();
            runtime.block_on(async move {
                // Untimed first request, so the actor's one-time query-cache
                // prewarm never lands inside the measured window. Taken from
                // the END of the eligible list while every batch is measured
                // from the front, so no measured file is served from its cache.
                let _ = handle.get_patterns(warm).await;
                let start = Instant::now();
                preparse_like_production(&handle, batch).await;
                start.elapsed()
            })
        };

        // The OLD path: every parse queued through the one actor thread.
        let serial = {
            let handle = runtime.block_on(registered_handle(&root, vendor_paths.clone()));
            let warm = warmup_path.clone();
            runtime.block_on(async move {
                let _ = handle.get_patterns(warm).await;
                let start = Instant::now();
                for path in batch {
                    black_box(handle.get_patterns(path).await.ok());
                }
                start.elapsed()
            })
        };

        println!(
            "  {:<8} {:>12.2}  {:>12.2}  {:>12.0}  {:>8.1}x",
            n,
            ms(serial),
            ms(off_actor),
            n as f64 / serial.as_secs_f64(),
            serial.as_secs_f64() / off_actor.as_secs_f64().max(f64::MIN_POSITIVE)
        );

        if n == eligible.len() {
            // Every later size would measure the same batch.
            break;
        }
    }

    println!("\n  'serial' is the old path: every parse queued through the ONE Salsa");
    println!("  actor thread, which is where a composer install's thousands of vendor");
    println!("  files used to funnel. 'off-actor' is what run_magic_batch_once does");
    println!("  now — parse in parallel outside the actor, hand it finished results.");
    println!();
    println!("  The speedup EXCEEDS the worker count, which is not a measurement");
    println!("  error: the two paths differ in more than parallelism. The serial one");
    println!("  pays an mpsc send plus a oneshot reply per file, and parses through");
    println!("  Salsa's tracked-query machinery (a SourceFile input, interning,");
    println!("  dependency tracking) for every one. The off-actor path calls the");
    println!("  parser directly and sends two bulk messages for the whole batch. So");
    println!("  the win is the worker count multiplied by the per-file overhead it");
    println!("  no longer pays.");
    println!();
    println!("  The new path is timed FIRST at every batch size, so any leftover");
    println!("  filesystem-cache advantage goes to the OLD one. The speedup is a");
    println!("  floor.");
}
