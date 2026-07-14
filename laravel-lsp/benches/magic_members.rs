//! Magic-member resolution benchmark — measures each caching optimization's
//! ISOLATED contribution to the whole-project "build semantic index" pass.
//!
//! # What the build pass does, and where the cost was
//!
//! The build resolves every captured `$receiver->member` site in every
//! non-vendor file. Each referencing file's receivers resolve to a class via
//! [`laravel_lsp::laravel_introspector`]'s `analyze`, and most projects funnel
//! through a small set of *shared ancestors* — a base `Model`, a base
//! controller, common traits. Three costs stacked up:
//!
//! * **P1** — each `spawn_blocking` worker built its own `ClassViewCache`, so a
//!   model referenced from N files was analyzed N times.
//! * **P3** — `analyze` re-read + re-parsed each ancestor (base model, traits)
//!   *inside every call*, and parsed each file twice (structure + use-aliases).
//! * **P2** — an FQCN not resolvable via Composer/PSR-4 fell back to a full
//!   `WalkDir` of `app/`/`vendor/`, repeated once per referencing file.
//!
//! # How this bench stays HONEST
//!
//! Old-vs-new code can't coexist in one binary, so instead of a bogus
//! "before/after" it measures **each optimization by toggling it** — running the
//! same corpus with the relevant cache **disabled** (reset before every file, so
//! it can never accumulate a hit) vs **enabled** (shared/warm). Every regime
//! resets the *other* two caches to the same state before each file, so each
//! number isolates exactly one optimization with no warm-cache confound.
//!
//! Three shapes, each targeting the optimization it actually exercises:
//!
//! 1. **P1 — shared `ClassViewCache`**, measured on the *shared-model-pool*
//!    shape (many callers → a small model set — the real app shape). Fresh cache
//!    per file vs one shared cache. On a 1:1 distinct-model shape P1 is ~1× *by
//!    design* (no receiver FQCN repeats, so nothing to share) — that shape is
//!    NOT the win and isn't reported as one.
//! 2. **P3 — parsed-file memo**, measured on distinct models with shared
//!    ancestors: cache reset-per-file (every ancestor re-parsed each call) vs
//!    persistent (each ancestor parsed once).
//! 3. **P2 — locator walk cache**, measured on a corpus whose ancestor is NOT
//!    PSR-4-resolvable (forcing the WalkDir fallback): cache reset-per-file
//!    (walk repeated) vs persistent (walked once).
//!
//! ## Running
//!
//! ```text
//! cargo bench --bench magic_members
//! MAGIC_BENCH_SCALES=200,1000 cargo bench --bench magic_members   # custom scales
//! ```
//!
//! ## Synthetic-data caveat
//!
//! This corpus is **synthetic** — shapes approximate a typical Laravel app, but
//! file counts and per-file access counts are generator parameters, not
//! measurements of any project. The numbers show the *shape* of each cost
//! (constant vs linear-in-file-count), not a wall-clock prediction.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use laravel_lsp::class_hierarchy_index::{classes_in_file, ClassHierarchyIndex};
use laravel_lsp::class_locator::reset_locator_cache;
use laravel_lsp::laravel_introspector::chain::reset_parsed_file_cache;
use laravel_lsp::member_capture::capture_member_context;
use laravel_lsp::member_resolver::{
    resolve_member_access_entries, resolve_member_access_entries_with_context, ClassViewCache,
};
use laravel_lsp::parser::{language_php, parse_php};
use laravel_lsp::queries::extract_all_php_patterns;
use laravel_lsp::salsa_impl::{Confidence, MemberAccessReferenceData, MemberContextData};
use laravel_lsp::symbol_index::MagicMemberEntry;

/// Member accesses each caller file reads off its typed receiver.
const ACCESSES_PER_CALLER: usize = 6;

// ─── Corpus generators ────────────────────────────────────────────────────

fn base_model_php() -> &'static str {
    r#"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use App\Concerns\HasAudit;
use App\Concerns\Sluggable;

class BaseModel extends Model
{
    use HasAudit;
    use Sluggable;

    protected $fillable = ['id', 'created_by', 'updated_by'];

    public function creator()
    {
        return $this->belongsTo(BaseModel::class, 'created_by');
    }
}
"#
}

fn has_audit_trait_php() -> &'static str {
    r#"<?php
namespace App\Concerns;

trait HasAudit
{
    public function auditLog()
    {
        return $this->hasMany(BaseModel::class);
    }

    public function getAuditedAtAttribute()
    {
        return $this->updated_at;
    }
}
"#
}

fn sluggable_trait_php() -> &'static str {
    r#"<?php
namespace App\Concerns;

trait Sluggable
{
    protected $sluggableColumns = ['slug', 'title'];

    public function scopeBySlug($query, $slug)
    {
        return $query->where('slug', $slug);
    }

    public function getSlugAttribute()
    {
        return $this->attributes['slug'] ?? '';
    }
}
"#
}

/// A concrete model extending the shared base + composing the shared traits.
/// `base` is the parent class name so a corpus can point every model at an
/// ancestor that is (or isn't) PSR-4-resolvable.
fn model_php(idx: usize, base: &str) -> String {
    format!(
        r#"<?php
namespace App\Models;

class Model{idx} extends {base}
{{
    protected $fillable = ['name{idx}', 'email{idx}', 'status{idx}'];

    public function scopeActive{idx}($query)
    {{
        return $query->where('active', true);
    }}

    public function related{idx}()
    {{
        return $this->hasMany(BaseModel::class);
    }}
}}
"#
    )
}

/// Caller `caller_idx` reads several members off a `Model{model_idx}` receiver,
/// forcing the full receiver chain (`Model{model_idx}` → base → traits) to build.
fn caller_php(caller_idx: usize, model_idx: usize) -> String {
    let members = [
        format!("$m->name{model_idx}"),
        format!("$m->active{model_idx}()"),
        format!("$m->related{model_idx}()"),
        "$m->slug".to_string(),
        "$m->bySlug('x')".to_string(),
        "$m->auditLog()".to_string(),
    ];
    let reads = members
        .iter()
        .take(ACCESSES_PER_CALLER)
        .enumerate()
        .map(|(i, expr)| format!("        $v{i} = {expr};"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?php
namespace App\Http\Controllers;

use App\Models\Model{model_idx};

class Controller{caller_idx}
{{
    public function show(Model{model_idx} $m)
    {{
{reads}
    }}
}}
"#
    )
}

/// Pre-captured caller input: source (the resolver re-parses the live tree) plus
/// its captured `->member` reference sites and a synthetic file path (the M1
/// capture keys vendor/Blade decisions off it).
struct CallerInput {
    source: String,
    refs: Vec<Arc<MemberAccessReferenceData>>,
    path: PathBuf,
}

/// An on-disk synthetic project plus the pre-captured caller inputs and the
/// class→file index the resolver consults.
struct Corpus {
    _dir: tempfile::TempDir,
    root: PathBuf,
    index: ClassHierarchyIndex,
    callers: Vec<CallerInput>,
}

fn write_and_index(index: &mut ClassHierarchyIndex, path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).expect("create parent dir");
    fs::write(path, body).expect("write source file");
    index.insert_file(path, classes_in_file(path, body));
}

/// Capture a caller's member accesses — the exact shape `handle_get_patterns`
/// stores in the pattern cache.
fn capture_refs(source: &str) -> Vec<Arc<MemberAccessReferenceData>> {
    let tree = parse_php(source).expect("parse caller");
    let lang = language_php();
    extract_all_php_patterns(&tree, source, &lang)
        .expect("extract patterns")
        .member_accesses
        .iter()
        .map(|m| {
            Arc::new(MemberAccessReferenceData {
                member: m.member.to_string(),
                receiver: m.receiver.to_string(),
                receiver_byte_start: m.receiver_byte_start,
                receiver_byte_end: m.receiver_byte_end,
                is_nullsafe: m.is_nullsafe,
                form: m.form,
                line: m.row as u32,
                column: m.column as u32,
                end_column: m.end_column as u32,
                declaring_fqcn: None,
                kind: None,
                confidence: Confidence::Unresolved,
            })
        })
        .collect()
}

/// Build a corpus: shared ancestors + `pool` models + `n` callers (caller `i`
/// targets `Model{i % pool}`). `resolvable_base` selects whether every model
/// extends the PSR-4-resolvable `BaseModel` (fast-path resolution) or an
/// ancestor that is NOT PSR-4-resolvable (forcing the locator WalkDir).
fn build_corpus(n: usize, pool: usize, resolvable_base: bool) -> Corpus {
    let pool = pool.clamp(1, n.max(1));
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path().to_path_buf();
    let mut index = ClassHierarchyIndex::default();

    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .expect("write composer.json");

    write_and_index(
        &mut index,
        &root.join("app/Models/BaseModel.php"),
        base_model_php(),
    );
    write_and_index(
        &mut index,
        &root.join("app/Concerns/HasAudit.php"),
        has_audit_trait_php(),
    );
    write_and_index(
        &mut index,
        &root.join("app/Concerns/Sluggable.php"),
        sluggable_trait_php(),
    );

    // The parent every model extends. For the locator-miss shape we make models
    // extend a base whose FQCN has no PSR-4 shape and no composer mapping, and
    // we do NOT register it in the index or place it on a PSR-4 path — so
    // resolving it drops through to the WalkDir fallback every time.
    let base = if resolvable_base {
        "BaseModel"
    } else {
        // A namespaced-but-unmapped ancestor: `Legacy\Support\LegacyBase` won't
        // match the `App\` PSR-4 prefix, so the resolver walks to find it.
        write_and_index(
            &mut index,
            // Placed somewhere the app-side basename walk CAN find it, but only
            // via WalkDir (no PSR-4 shape resolves `Legacy\Support\LegacyBase`).
            &root.join("app/Legacy/LegacyBase.php"),
            r#"<?php
namespace Legacy\Support;
use Illuminate\Database\Eloquent\Model;
class LegacyBase extends Model {
    protected $fillable = ['id'];
    public function scopeLegacy($query) { return $query; }
}
"#,
        );
        "\\Legacy\\Support\\LegacyBase"
    };

    for i in 0..pool {
        write_and_index(
            &mut index,
            &root.join(format!("app/Models/Model{i}.php")),
            &model_php(i, base),
        );
    }

    let callers = (0..n)
        .map(|i| {
            let source = caller_php(i, i % pool);
            let refs = capture_refs(&source);
            let path = root.join(format!("app/Http/Controllers/Controller{i}.php"));
            CallerInput { source, refs, path }
        })
        .collect();

    Corpus {
        _dir: dir,
        root,
        index,
        callers,
    }
}

// ─── Measurement primitives ────────────────────────────────────────────────

/// Reset all three global/shared caches to a cold state. Called between regimes
/// (and, where a regime "disables" a cache, before every file) so no warm-cache
/// state leaks across a measurement.
fn reset_all_global_caches() {
    reset_parsed_file_cache();
    reset_locator_cache();
}

/// Resolve one caller against `index`/`root` with the given `ClassViewCache`.
fn resolve_one(corpus: &Corpus, caller: &CallerInput, cache: &ClassViewCache) -> usize {
    let entries = resolve_member_access_entries(
        &caller.source,
        &caller.refs,
        &corpus.index,
        cache,
        &corpus.root,
        None,
    );
    black_box(entries).len()
}

fn per_file_us(elapsed: Duration, files: usize) -> f64 {
    if files == 0 {
        0.0
    } else {
        elapsed.as_secs_f64() * 1e6 / files as f64
    }
}

// ─── P1: shared ClassViewCache (shared-model-pool shape) ───────────────────

/// P1 disabled: fresh `ClassViewCache` per file (today's per-worker shape). The
/// parsed-file + locator caches stay PERSISTENT and identical across both P1
/// regimes so they don't confound the comparison — only the ClassViewCache
/// sharing differs.
fn p1_fresh(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        let cache = ClassViewCache::new();
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

/// P1 enabled: one shared `ClassViewCache` for all files.
fn p1_shared(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let cache = ClassViewCache::new();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

// ─── P3: parsed-file memo (distinct models, shared ancestors) ──────────────

/// P3 disabled: reset the parsed-file cache before EVERY file, so each `analyze`
/// re-reads + re-parses every ancestor (the pre-optimization behavior). Uses a
/// fresh `ClassViewCache` per file too (holding P1 constant-off), and keeps the
/// locator cache warm so only the parse cost differs.
fn p3_reset_per_file(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        reset_parsed_file_cache(); // disable the memo: every ancestor re-parsed
        let cache = ClassViewCache::new();
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

/// P3 enabled: parsed-file memo persists across files (each ancestor parsed
/// once). Fresh `ClassViewCache` per file so this isolates the parse memo, not P1.
fn p3_persistent(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        let cache = ClassViewCache::new();
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

// ─── P2: locator walk cache (unresolvable ancestor → WalkDir) ──────────────

/// P2 disabled: reset the locator cache before every file, so the unresolvable
/// ancestor is re-walked on each caller. Parsed-file cache reset too, so the
/// walk cost isn't masked by the parse memo; fresh `ClassViewCache` per file.
fn p2_reset_per_file(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        reset_locator_cache();
        reset_parsed_file_cache();
        let cache = ClassViewCache::new();
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

/// P2 enabled: locator cache persists (ancestor walked once, then cached). Reset
/// the parsed-file cache per file so ONLY the locator caching differs from the
/// disabled regime above.
fn p2_persistent(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        reset_parsed_file_cache();
        let cache = ClassViewCache::new();
        total += resolve_one(corpus, c, &cache);
    }
    (start.elapsed(), total)
}

// ─── M1: single-parse capture (tree re-parse vs captured context) ──────────

/// Build the parse-time context for every caller, timed separately — this is
/// the ONE-TIME capture cost the resolve passes trade the per-file re-parse
/// for. Runs on the same corpus the M1 resolve regimes use.
fn m1_capture_contexts(corpus: &Corpus) -> (Vec<MemberContextData>, Duration) {
    let start = Instant::now();
    let contexts: Vec<MemberContextData> = corpus
        .callers
        .iter()
        .map(|c| {
            let tree = parse_php(&c.source).expect("parse caller");
            capture_member_context(&c.path, &c.source, Some(&tree), &c.refs, false)
                .expect("caller with member refs captures context")
        })
        .collect();
    (contexts, start.elapsed())
}

/// M1 disabled: today's re-parse path — each caller is re-parsed from source at
/// resolve time (`resolve_member_access_entries` parses `source` internally).
fn m1_tree_reparse(corpus: &Corpus) -> (Duration, usize) {
    reset_all_global_caches();
    let cache = ClassViewCache::new();
    let start = Instant::now();
    let mut total = 0;
    for c in &corpus.callers {
        let entries = resolve_member_access_entries(
            &c.source,
            &c.refs,
            &corpus.index,
            &cache,
            &corpus.root,
            None,
        );
        total += black_box(entries).len();
    }
    (start.elapsed(), total)
}

/// Assert the two M1 paths resolve the SAME entries — canonicalized
/// (fqcn, member, line, column, end_column) and sorted, so a divergence in an
/// FQCN or position can't slip past a matching count. Runs before the timed
/// regimes; panics on divergence so a broken capture fails the bench loudly.
fn m1_assert_entries_equivalent(corpus: &Corpus, contexts: &[MemberContextData]) {
    let canon = |v: Vec<MagicMemberEntry>| -> Vec<(String, String, u32, u32, u32)> {
        v.into_iter()
            .map(|e| (e.fqcn, e.member, e.line, e.column, e.end_column))
            .collect()
    };
    let cache = ClassViewCache::new();
    let mut tree: Vec<(String, String, u32, u32, u32)> = Vec::new();
    let mut captured: Vec<(String, String, u32, u32, u32)> = Vec::new();
    for (c, ctx) in corpus.callers.iter().zip(contexts.iter()) {
        tree.extend(canon(resolve_member_access_entries(
            &c.source,
            &c.refs,
            &corpus.index,
            &cache,
            &corpus.root,
            None,
        )));
        captured.extend(canon(resolve_member_access_entries_with_context(
            ctx,
            &c.refs,
            &corpus.index,
            &cache,
            &corpus.root,
            None,
        )));
    }
    tree.sort();
    captured.sort();
    assert!(
        tree == captured,
        "M1: captured entries diverge from the tree path (canonical fqcn/member/pos): \
         {} tree vs {} captured",
        tree.len(),
        captured.len()
    );
}

/// M1 enabled: resolve from the captured context — NO re-parse at resolve time.
/// `contexts` were built once by [`m1_capture_contexts`] (its cost is reported
/// separately); this measures only the resolve half.
fn m1_captured(corpus: &Corpus, contexts: &[MemberContextData]) -> (Duration, usize) {
    reset_all_global_caches();
    let cache = ClassViewCache::new();
    let start = Instant::now();
    let mut total = 0;
    for (c, ctx) in corpus.callers.iter().zip(contexts.iter()) {
        let entries = resolve_member_access_entries_with_context(
            ctx,
            &c.refs,
            &corpus.index,
            &cache,
            &corpus.root,
            None,
        );
        total += black_box(entries).len();
    }
    (start.elapsed(), total)
}

// ─── Reporting ─────────────────────────────────────────────────────────────

fn report(part: &str, n: usize, off: (Duration, usize), on: (Duration, usize)) {
    let (off_dur, off_entries) = off;
    let (on_dur, on_entries) = on;
    // Equivalence: toggling a pure memo must not change what's resolved.
    assert_eq!(
        off_entries, on_entries,
        "{part}: entry count changed when toggling the cache — not pure memoization"
    );
    let speedup = off_dur.as_secs_f64() / on_dur.as_secs_f64().max(f64::EPSILON);
    println!("  [{part}] {off_entries} entries resolved");
    println!(
        "    disabled : {:>10.2?}  ({:.2} us/caller)",
        off_dur,
        per_file_us(off_dur, n)
    );
    println!(
        "    enabled  : {:>10.2?}  ({:.2} us/caller)",
        on_dur,
        per_file_us(on_dur, n)
    );
    println!("    speedup  : {speedup:.2}x");
}

fn parse_scales() -> Vec<usize> {
    match std::env::var("MAGIC_BENCH_SCALES") {
        Ok(raw) => raw
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .collect(),
        Err(_) => vec![200, 1000, 5000],
    }
}

fn main() {
    let scales = parse_scales();
    println!("magic-member resolution benchmark — SYNTHETIC corpus");
    println!(
        "each optimization measured in isolation (toggled on/off), other caches held constant"
    );
    println!("shared ancestors: BaseModel + 2 traits | accesses/caller: {ACCESSES_PER_CALLER}");

    for &n in &scales {
        println!("\n========================================================");
        println!("SCALE: {n} callers");

        // P1 — shared ClassViewCache. The shape that shows it: many callers over
        // a small model pool (n/10). On 1:1 it's ~1x BY DESIGN (below).
        println!("--------------------------------------------------------");
        let pool_corpus = build_corpus(n, (n / 10).max(1), true);
        report(
            "P1 shared cache, n/10 pool",
            n,
            p1_fresh(&pool_corpus),
            p1_shared(&pool_corpus),
        );

        // P1 on the 1:1 shape — reported honestly as ~1x (no receiver FQCN
        // repeats, so a shared ClassViewCache has nothing to collapse).
        let distinct_corpus = build_corpus(n, n, true);
        report(
            "P1 shared cache, 1:1 (expected ~1x by design)",
            n,
            p1_fresh(&distinct_corpus),
            p1_shared(&distinct_corpus),
        );

        // P3 — parsed-file memo, distinct models with shared ancestors.
        println!("--------------------------------------------------------");
        report(
            "P3 parsed-file memo, 1:1",
            n,
            p3_reset_per_file(&distinct_corpus),
            p3_persistent(&distinct_corpus),
        );

        // P2 — locator walk cache, ancestor forced through the WalkDir fallback.
        println!("--------------------------------------------------------");
        let miss_corpus = build_corpus(n, n, false);
        report(
            "P2 locator walk cache, 1:1 (unresolvable ancestor)",
            n,
            p2_reset_per_file(&miss_corpus),
            p2_persistent(&miss_corpus),
        );

        // M1 — single-parse capture. Same corpus as P1's n/10 pool (the real
        // app shape). Disabled = today's re-parse-at-resolve; enabled = resolve
        // from context captured at parse. The capture cost is reported on its
        // OWN line — it is paid ONCE at parse (amortized across every later
        // resolve: warm rebuild, save-refresh, find-references), not per resolve.
        println!("--------------------------------------------------------");
        let (contexts, capture_cost) = m1_capture_contexts(&pool_corpus);
        // Canonical-entry equivalence (fqcn/member/pos, sorted) BEFORE timing —
        // a matching count alone must not pass as equal.
        m1_assert_entries_equivalent(&pool_corpus, &contexts);
        report(
            "M1 single-parse capture, n/10 pool",
            n,
            m1_tree_reparse(&pool_corpus),
            m1_captured(&pool_corpus, &contexts),
        );
        println!(
            "    capture  : {:>10.2?}  ({:.2} us/caller, paid once at parse)",
            capture_cost,
            per_file_us(capture_cost, n)
        );
    }

    println!("\nNOTE: synthetic corpus; absolute times are machine-dependent. Each");
    println!("'speedup' is that ONE optimization toggled on vs off with the other");
    println!("caches held constant — an honest per-Part contribution, not a headline.");
    println!("P1 on the 1:1 shape is ~1x by construction (no shared receiver FQCNs);");
    println!("its win is the n/10 pool shape. Reset hooks make the comparison");
    println!("apples-to-apples (no warm-cache carryover between regimes).");
}
