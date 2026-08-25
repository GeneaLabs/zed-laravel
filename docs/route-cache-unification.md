# Discovery: unifying the route caches

> **Status:** discovery / recommendation for [#48](https://github.com/mike-bronner/zed-laravel/issues/48).
> Gates a future implementation issue — no production code changed here.
> Numbers below come from `cargo bench --bench route_cache` (a **synthetic**
> corpus; see the caveat at the end).

## Outcome (superseded — A was implemented)

> **This document's recommendation was overridden.** Option **A (full unify onto
> tree-sitter)** was implemented for `build_route_index` while fixing a
> user-facing correctness bug: the byte scanners had no notion of PHP comments
> or heredocs, so a commented-out `function () {` or a bare apostrophe in prose
> shifted every group boundary that followed it. Real route files carry both,
> and routes downstream of the corruption were indexed under the wrong prefix
> (or none), producing false "Route not found" diagnostics on routes that
> `artisan route:list` resolves fine.
>
> B (hybrid) would have fixed the first-party case at ~1× cost, but was not the
> option chosen. The measured price of A, re-run with the same bench after the
> migration:
>
> | Corpus | `build_route_index` before | after | ratio |
> |---|---|---|---|
> | 500 vendor files | 38.0 ms | 151.4 ms | 4.0× |
> | 2000 vendor files | 172.8 ms | 580.0 ms | 3.4× |
> | 5000 vendor files | 368.5 ms | 1.43 s | 3.9× |
>
> On a real vendor-heavy project (139 route files, 93.5% vendor) the same
> rebuild went 55.9 ms → 173.7 ms (3.1×), and the index gained 155 correctly
> prefixed names it had previously been getting wrong.
>
> The realistic-path ratio is worse than the ~2.8× parse ratio predicted below
> because `build_route_index` parses each file twice — once to discover
> `->group(<path>)` load edges, once to extract routes. Collapsing those two
> passes onto one cached parse is the obvious next optimization, and the hybrid
> split described under **B** remains available if the cold-start cost proves
> unacceptable on large projects.
>
> Everything below is the original discovery, kept as the record of what was
> measured beforehand.

## TL;DR

**Recommendation: B — hybrid, but deferred.** Unify the *first-party* route
files onto the tree-sitter model (one parse feeding index + declarations +
outline) and keep the byte-scan index for `vendor/`, where it is ~2.8× cheaper
and the rich model is never needed. But the measured payoff is small — AC#4
debunked the headline drift motivation — so treat B as a low-priority cleanup
for whenever the route subsystem is next opened, not a now-task. Full
unification (A) is ruled out by the numbers; status-quo (C) is an acceptable
interim.

## Background

After the route-handling consolidation (PR #47) route data is still
materialized into **three caches built by two parsing strategies**:

| Store | Strategy | Scope / keying | Feeds |
|---|---|---|---|
| `route_index` (`build_route_index`) | byte-scan | whole project incl. `vendor/*/routes`; rebuilt on init + route-file save | hover, goto, completion, route diagnostic (`main.rs`, `folio_discovery`) |
| `route_decl_cache` | tree-sitter (`route_chain`) | per-file, keyed by mtime | rename, find-references (`route_name_locator`) |
| document-symbols | tree-sitter (`route_chain`) | per-file, mtime-keyed | outline (`route_outline`, `document_symbols`) |

The two tree-sitter consumers already share one walker
(`route_chain::extract_route_chains`). The remaining split is **byte-scan index
vs. tree-sitter walkers**. The byte-scan index is a deliberate optimization: it
bulk-indexes the whole project **including `vendor/`** (potentially thousands of
route files), where byte-scanning is far cheaper than spinning up a tree-sitter
parse per file.

The question this discovery answers: is it worth collapsing onto one canonical
per-file model, and if so, how — given that unifying onto tree-sitter likely
**slows** the vendor-heavy bulk index but **speeds up** per-file ops?

## What was measured

A committed benchmark, `laravel-lsp/benches/route_cache.rs`, generates a
synthetic corpus at three vendor scales (~500 / ~2000 / ~5000 vendor files,
plus a fixed 12 first-party `routes/` files) whose files mimic real Laravel
shapes — named routes, prefixed+named groups, nested groups, `Route::resource`,
and unnamed closure routes. It then measures, with `extract_named_routes`
(byte-scan) vs `extract_route_chains` (tree-sitter):

1. **init path** — parse the whole corpus once, in memory, with each strategy;
2. **route-save path** — re-parse a single representative project file;
3. **realistic init** — the real `build_route_index` (discovery + disk I/O +
   load-graph expansion + byte-scan), i.e. the cost paid today;
4. **memory** — estimated heap footprint of caching the tree-sitter rich
   per-file model (`Vec<RouteChainNode>`) for every file.

Re-run it any time with `cargo bench --bench route_cache`. Override the scales
with `ROUTE_BENCH_SCALES=200,1000 cargo bench --bench route_cache`.

### Results

```
route-cache strategy benchmark (issue #48) — SYNTHETIC corpus
project route files (fixed): 12 | files per vendor pkg: 6

== Route::resource granularity (AC#4) ==
source: Route::resource('photos', PhotoController::class);
  byte-scan      -> 7 named routes: photos.create, photos.destroy, photos.edit, photos.index, photos.show, photos.store, photos.update
  tree-sitter    -> 1 chain node(s), verb=Some("resource"), uri=Some("photos"), name=None

========================================================
SCALE: 500 vendor files + 12 project files = 512 total (97.7% vendor)
--------------------------------------------------------
[init / whole corpus, in-memory parse only]
  byte-scan   :   18.62ms  (36.36 us/file, 7263 route-defs)
  tree-sitter :   51.55ms  (100.69 us/file, 4066 chain nodes)
  tree-sitter / byte-scan slowdown: 2.8x
[route-save / single project file re-parse]
  byte-scan   :   88.36µs
  tree-sitter :  313.68µs
  tree-sitter / byte-scan slowdown: 3.5x
[realistic init / build_route_index, disk + discovery]
  build_route_index:   38.04ms  (74.29 us/file, 3862 names indexed)
[memory / cached tree-sitter rich model, all files]
  estimated heap: 0.73 MiB  (1495 bytes/file)

========================================================
SCALE: 2000 vendor files + 12 project files = 2012 total (99.4% vendor)
--------------------------------------------------------
[init / whole corpus, in-memory parse only]
  byte-scan   :   70.02ms  (34.80 us/file, 26692 route-defs)
  tree-sitter :  192.80ms  (95.83 us/file, 15299 chain nodes)
  tree-sitter / byte-scan slowdown: 2.8x
[route-save / single project file re-parse]
  byte-scan   :   88.54µs
  tree-sitter :  318.88µs
  tree-sitter / byte-scan slowdown: 3.6x
[realistic init / build_route_index, disk + discovery]
  build_route_index:  172.80ms  (85.88 us/file, 14359 names indexed)
[memory / cached tree-sitter rich model, all files]
  estimated heap: 2.74 MiB  (1430 bytes/file)

========================================================
SCALE: 5000 vendor files + 12 project files = 5012 total (99.8% vendor)
--------------------------------------------------------
[init / whole corpus, in-memory parse only]
  byte-scan   :  171.65ms  (34.25 us/file, 65234 route-defs)
  tree-sitter :  479.42ms  (95.65 us/file, 37743 chain nodes)
  tree-sitter / byte-scan slowdown: 2.8x
[route-save / single project file re-parse]
  byte-scan   :   89.19µs
  tree-sitter :  309.01µs
  tree-sitter / byte-scan slowdown: 3.5x
[realistic init / build_route_index, disk + discovery]
  build_route_index:  368.46ms  (73.52 us/file, 35462 names indexed)
[memory / cached tree-sitter rich model, all files]
  estimated heap: 6.77 MiB  (1416 bytes/file)

NOTE: synthetic corpus. Re-point build_route_index at a real
vendor/ tree for literal per-project numbers (see module docs).
```

#### 1–2. Init throughput and route-save (AC#1, AC#2)

| Scale (total files) | byte-scan init | tree-sitter init | TS/BS slowdown | byte-scan save | tree-sitter save | realistic `build_route_index` |
|---|---|---|---|---|---|---|
| ~500 vendor (512 total) | 18.62 ms | 51.55 ms | 2.8× | 88.36 µs | 313.68 µs | 38.04 ms |
| ~2000 vendor (2012 total) | 70.02 ms | 192.80 ms | 2.8× | 88.54 µs | 318.88 µs | 172.80 ms |
| ~5000 vendor (5012 total) | 171.65 ms | 479.42 ms | 2.8× | 89.19 µs | 309.01 µs | 368.46 ms |

Init is parse-dominated, and tree-sitter costs a **flat ~2.8× more than
byte-scan** at every scale — it does not amortize away with size (per-file
throughput is a stable ~35 µs/file byte-scan vs ~96–100 µs/file tree-sitter).
The realistic `build_route_index` (disk I/O + discovery + load-graph expansion +
byte-scan) lands at ~73–86 µs/file, above the in-memory byte-scan figure because
I/O and discovery add fixed overhead on top of the parse. The consequence for
**design A**: replacing the byte-scan parse with the ~2.8×-costlier tree-sitter
one across the whole corpus turns a 172 ms in-memory parse into 479 ms at 5000
files — realistic init would climb from ~368 ms toward ~675 ms, paid on **every
cold init and every route-file-save rebuild**, and worst exactly on the
vendor-heavy projects the byte-scan index was built to serve. The route-save
micro-bench shows the same ~3.5× gap for a single file (88 µs vs ~313 µs), but
at single-file scale both are sub-millisecond and imperceptible.

#### 3. Vendor vs project file ratio (AC#3)

Real Laravel apps have a small, roughly constant set of first-party route files
(`routes/web.php`, `api.php`, `console.php`, a handful of domain splits — call
it ~5–15), while `vendor/` route files scale with installed packages. The
synthetic corpus models this with a **fixed 12 project files** against
500/2000/5000 vendor files, i.e. vendor is **97.7%–99.8%** of all
indexed route files. This is the regime that matters: **the byte-scan index
exists almost entirely to serve `vendor/`.** First-party files are a rounding
error in the bulk-index cost but are the *only* files that need rename/outline.

> Synthesised from composer patterns, not a measured count of one project. The
> committed bench can be re-pointed at a real `vendor/` tree for literal numbers.

#### 4. `Route::resource` granularity difference (AC#4)

The two strategies **both surface resources** — the original issue's "outline
omits resources" premise was wrong (confirmed in code: `route_chain.rs`
`ROUTE_VERBS` includes `"resource"`/`"apiResource"`, and `route_outline.rs`
folds each into a resource-type leaf (`RESOURCE` / `APIRESOURCE`) — the verb is
upper-cased verbatim, so `apiResource` surfaces as `APIRESOURCE`, not `RESOURCE`).
The real difference is **granularity**:

```
== Route::resource granularity (AC#4) ==
source: Route::resource('photos', PhotoController::class);
  byte-scan      -> 7 named routes: photos.create, photos.destroy, photos.edit, photos.index, photos.show, photos.store, photos.update
  tree-sitter    -> 1 chain node(s), verb=Some("resource"), uri=Some("photos"), name=None
```

- **byte-scan** (`extract_resource_routes`, `route_discovery.rs`) expands a
  resource into its named CRUD sub-routes — `photos.index`, `photos.create`,
  `photos.store`, `photos.show`, `photos.edit`, `photos.update`,
  `photos.destroy` (apiResource drops `create`/`edit`).
- **tree-sitter** (`route_chain`) surfaces a **single** `RESOURCE` leaf with
  `verb = "resource"`, `uri = "photos"`, `name = None`.

**Is this a user-visible inconsistency?** Assessed per workflow:

- **hover / goto / completion** — fed by the byte-scan `RouteIndex`, so
  `route('photos.show')` resolves and completes. ✅ The granular names are
  exactly what these features need; tree-sitter alone could not serve them
  without re-deriving the CRUD expansion.
- **outline** — fed by tree-sitter, shows one `RESOURCE photos` node rather
  than seven rows. This is **defensible, not a bug**: an outline is a structural
  map of the source file, and the source literally has one `Route::resource`
  call. Expanding it to seven synthetic rows would *misrepresent* the file.
- **rename / find-references** — fed by tree-sitter via `route_decl_cache`.
  Resource sub-routes have **no `->name()` token to rename** (the names are
  synthesised by Laravel, not written in source), so there is nothing for rename
  to target. ✅ No inconsistency — the granularity gap is invisible here too.

**Conclusion:** the granularity difference is **correct behaviour on both
sides**, dictated by what each consumer needs, not drift. It does **not** by
itself justify forcing both consumers onto one representation — if anything it
argues *against* full unification, because the two views legitimately differ.

#### 5. Memory footprint of a cached rich model (AC#5)

| Scale (total files) | est. cached tree-sitter model | bytes/file |
|---|---|---|
| ~500 vendor | 0.73 MiB | 1495 B |
| ~2000 vendor | 2.74 MiB | 1430 B |
| ~5000 vendor | 6.77 MiB | 1416 B |

This is the **folded** model (`Vec<RouteChainNode>` + owned strings), not the
tree-sitter `Tree` — design A would drop the tree after folding. Even caching a
rich model for *every* file (vendor included) costs ~1.4–1.5 KB/file and stays
under 7 MiB at 5000 files. **Memory is not a constraint on any design** and
does not argue for or against unification.

## Candidate designs

- **A. Full unify onto tree-sitter** — one per-file model cache, all views
  (name→location index, declarations, outline) derived from it. Parse every
  route file (incl. vendor) once with tree-sitter, cache by mtime, compute the
  cross-file prefix graph from the models.
  - Pros: one representation, one invalidation path; per-file ops stop
    double-parsing.
  - Cons: pays tree-sitter cost on the whole `vendor/` bulk — the ~2.8×
    slower init regime, worst exactly where there are most files; and it must
    *re-implement* the resource CRUD-name expansion (AC#4) on top of the
    tree-sitter model, since the outline's single-leaf shape can't feed
    hover/goto/completion.

- **B. Hybrid** — one tree-sitter parse per **first-party** route file (reused
  for the index entry + declarations + outline), keep **byte-scan only for
  `vendor/`** bulk (which never needs rename or outline). Kills the
  double-parse and the drift surface for first-party routes while preserving
  vendor indexing speed.
  - Pros: removes the per-file double-parse for the files that actually get
    renamed/outlined; keeps the cheap byte-scan where it matters most (vendor);
    canonical source for first-party routes.
  - Cons: still two code paths (but split by a clear, stable boundary —
    first-party vs vendor — rather than by view); must re-derive resource
    CRUD-name expansion from the tree-sitter model for *first-party* resources
    so hover/goto/completion keep resolving; the byte-scan resource expansion
    stays for vendor.

- **C. Status quo** — keep three caches; document the boundaries and accept the
  (currently low) drift risk.
  - Pros: zero risk, zero work; the caches already share the `route_chain`
    walker across the two tree-sitter consumers.
  - Cons: three representations + three invalidation paths remain a latent
    place future edits can disagree.

## Recommendation

**Pick B (hybrid) — but defer it.** The measurements settle the *shape* and
downgrade the *urgency*.

**Why not A.** Tree-sitter is a flat ~2.8× slower than byte-scan on the bulk
parse (35 µs → ~96 µs per file, stable across 500 / 2000 / 5000 files), and
vendor is 97.7–99.8% of all indexed route files. Vendor files never need rename
or outline, so parsing them with tree-sitter buys nothing and costs a ~2.8× init
regression (≈368 ms → ≈675 ms at 5000 files) on every cold start and route-save
rebuild — worst exactly on the vendor-heavy projects the byte-scan index exists
to serve. **A is ruled out by the data.**

**Why B over C.** The only files that need the rich model are the ~dozen
first-party route files, and today those are parsed *twice* — once by the
byte-scan index, once by tree-sitter for rename/outline — through two
independent invalidation paths. B collapses the first-party side onto one
tree-sitter model feeding all three views, removing the double-parse and the
(small) first-party drift surface, while keeping byte-scan for the vendor bulk
where it wins.

**The honest caveat: the payoff is modest.** AC#4 debunked the headline drift
motivation — both strategies already surface resources; the granularity
difference (named CRUD sub-routes vs a single `RESOURCE` leaf) is correct-by-
design per consumer, not drift. With that gone, B's remaining benefit is ~2.7 ms
of init parse for a dozen files plus one fewer invalidation path — real, but not
pressing, and it *adds* the first-party resource-expansion code path and a new
first-party/vendor boundary to maintain. So: **adopt B as the target
architecture whenever the route subsystem is next opened, but don't schedule it
ahead of higher-value work.** C (status quo) is a fine interim — the one
deliverable C still owes is *documentation*: record the first-party/vendor
boundary and the resource-granularity-by-design finding (above) in the route
modules so the next editor doesn't mistake the granularity gap for a bug.

## Implementation sketch (for the recommended option)

**Target: B (hybrid).** Estimated effort **Medium** — the split boundary is
trivial; the risk is parity, making first-party index entries derived from the
tree-sitter model match byte-scan exactly so existing tests stay green.

**Affected modules**

- `route_discovery.rs` — `discover_route_files` / `build_route_index`: partition
  discovered files into **first-party** (`routes/` plus project files reached
  through `->group(<path>)` external loads) vs **vendor** (`vendor/*/routes`).
  Build vendor index entries with the existing byte-scan (`extract_named_routes`);
  build first-party index entries from the tree-sitter model instead.
- `route_chain.rs` — the per-file model (`extract_route_chains` →
  `Vec<RouteChainNode>`) becomes the canonical first-party source. Add a
  derivation that walks `RouteChainNode` (incl. `RESOURCE` leaves and
  group-prefix accumulation) into `Vec<(Option<String>, RouteDefinition)>`
  index entries, mirroring byte-scan's resource CRUD-name expansion so
  `route('photos.show')` still resolves for first-party resources.
- `route_decl_cache` / `document_symbols` / new index consumer — all three read
  the **same** mtime-keyed per-file model for first-party files: one parse,
  three views (this is the drift-surface kill).
- `main.rs` / `salsa_impl.rs` — `did_save` rebuild path: a first-party
  route-file save re-parses once (tree-sitter) and refreshes index + decl-cache +
  outline from the one model; a vendor route-file save (rare) still byte-scans.

**Key data-structure changes**

- A canonical per-file `RouteFileModel { chains: Vec<RouteChainNode>, mtime }`
  cache — ideally a Salsa `#[salsa::tracked]` query keyed by `SourceFile`, per
  the repo's cache-invalidation convention — feeding the three derived views.
- `fn index_entries_from_chains(&[RouteChainNode], inherited_prefixes: &[String])
  -> Vec<(Option<String>, RouteDefinition)>` in `route_chain.rs`, including
  resource expansion and group-prefix accumulation, replacing the first-party
  portion of `extract_named_routes`.
- A `is_vendor(path)` predicate (path under `vendor/` → byte-scan; else →
  tree-sitter model) as the single split point.

**Sequencing (rough)**

1. Add `index_entries_from_chains` + a **parity test** asserting it produces the
   same `(name, location)` set as `extract_named_routes` over the existing route
   fixtures (this is where the effort lives — resource expansion + nested
   group prefixes must match byte-scan bit-for-bit).
2. Wire the first-party/vendor split into `build_route_index` and the `did_save`
   rebuild; route first-party files through the model, keep vendor on byte-scan.
3. Point `route_decl_cache` and document-symbols at the same cached model for
   first-party files; delete the now-redundant first-party byte-scan pass.

Net production LOC is modest; the parity test suite dominates the work. Because
the payoff is small (per the recommendation), this can land opportunistically
the next time someone is already in `route_discovery.rs`.

---

### Synthetic-data caveat

The corpus is **synthetic**: route-file shapes approximate typical Laravel
files, but the vendor/project ratio and per-file route counts are generator
parameters, not measurements of any specific project. The committed bench is
the deliverable that lets the team get literal numbers — re-point
`build_route_index` at a real `vendor/` tree (the realistic-init harness already
runs it against the synthetic root) to replace these representative ranges with
project-specific figures.
