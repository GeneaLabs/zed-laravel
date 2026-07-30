//! LSP `$/progress` plumbing for the project-warming pipeline.
//!
//! When the LSP boots into a Laravel project of any meaningful size, the
//! user sees several seconds of silence before find-references / rename
//! and friends start returning useful results. The pattern cache is
//! warming up, but there's no signal in the editor — the LSP just looks
//! frozen. This module fixes that by emitting the standard LSP
//! `window/workDoneProgress/create` request followed by `$/progress`
//! notifications, which Zed (and any other LSP client) renders as a
//! status-bar progress indicator with title + message + filled bar.
//!
//! Lifecycle is start → report* → end, modeled as a single-owner type
//! that ends the progress in `Drop` if you forget. Errors from the
//! client are silently ignored: progress UI is non-essential and we
//! never want a missing client capability to break warming itself.

use std::time::{Duration, Instant};

use tower_lsp::lsp_types::notification::Progress as ProgressNotification;
use tower_lsp::lsp_types::request::WorkDoneProgressCreate;
use tower_lsp::lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};
use tower_lsp::Client;

/// Token identifier for the project-warming progress.
pub const INDEXING_TOKEN: &str = "laravel-lsp/indexing";
/// Token identifier for the class-rename progress.
pub const RENAME_TOKEN: &str = "laravel-lsp/rename";

/// The title every `$/progress` entry this server opens is branded with.
///
/// Zed's status bar can host several LSP progress entries at once, so an
/// unbranded title would be ambiguous — the descriptive part ("Indexing
/// 12,345 of 40,589 files") lives in the `message` instead, keeping the
/// title short. Short form ("CE", not "Community Edition") because the
/// status bar is the tightest slot in the UI; the marketplace listing
/// spells the name out in full.
///
/// Single-sourced so the three call sites (startup indexing, reindex,
/// rename) can't drift apart.
pub const PROGRESS_TITLE: &str = "Laravel CE";

/// Minimum interval between `$/progress` report notifications. Faster
/// than this and we'd just be spamming the editor for sub-frame updates
/// the user can't see anyway. Slower and the bar feels jumpy.
const REPORT_THROTTLE: Duration = Duration::from_millis(150);

// ── Slice map ────────────────────────────────────────────────────────
// The unified 0→100% bar is carved into disjoint per-phase slices so it
// stays monotonic (no phase resets to 0). Two branches, because the work
// profile differs sharply:
//
//   COLD (nothing restored from the pattern disk cache — first launch or
//   a schema-invalidated cache): the disk load does nothing, so parsing
//   dominates. Layout:  parse 0..PARSE_SPAN │ magic-build PARSE_SPAN..100.
//
//   WARM (disk cache restored ≥1 file): the ~40k-entry disk-cache load
//   and the granular magic re-resolve dominate; parsing is a small burst
//   over the changed files. Layout:
//     load 0..LOAD_SPAN │ parse LOAD_SPAN..WARM_PARSE_TOP │
//     bulk-op boundary band WARM_PARSE_TOP..WARM_RESOLVE_BASE │
//     re-resolve WARM_RESOLVE_BASE..100.
//
// All values are eyeballed weightings — tune against the per-phase
// timings the warm task logs ("parse …, magic-resolve …"). The priority
// is NEVER FREEZE + monotonic, not proportional accuracy.

/// Top of the disk-cache **load** slice on a warm reload (`load
/// 0..LOAD_SPAN`). The load restores ~40k entries — an mtime stat +
/// position-index rebuild each — the dominant early warm cost, so it
/// earns a wide early slice. Unused on a cold start: with no cache the
/// load is instant and reports nothing, and cold parse owns
/// `0..PARSE_SPAN` instead.
pub const LOAD_SPAN: u32 = 40;

/// Top of the **cold** parse slice: parse `0..PARSE_SPAN`, magic-build
/// `PARSE_SPAN..100`. Parsing is the dominant cold cost.
pub const PARSE_SPAN: u32 = 75;

/// Top of the **warm** parse slice: parse `LOAD_SPAN..WARM_PARSE_TOP`, a
/// small burst over the changed files. The
/// `WARM_PARSE_TOP..WARM_RESOLVE_BASE` gap above it is the boundary band
/// where the opaque bulk phases (disk-cache save, magic-restore decode,
/// index import) advance the bar with forced phase messages.
pub const WARM_PARSE_TOP: u32 = 48;

/// Base of the **warm** re-resolve slice: the granular magic re-resolve
/// (`refresh_files_magic`) fills `WARM_RESOLVE_BASE..100`. Call sites
/// clamp with `max(parse_top)` so a cold-parse / warm-magic mix (pattern
/// cache stale but magic cache fresh) continues from `PARSE_SPAN` instead
/// of jumping backwards.
pub const WARM_RESOLVE_BASE: u32 = 55;

// Compile-time warm slice-layout invariant: load, parse, boundary band,
// and re-resolve are strictly ordered and the re-resolve slice is
// non-empty (its base sits below 100).
const _: () = assert!(
    LOAD_SPAN < WARM_PARSE_TOP && WARM_PARSE_TOP < WARM_RESOLVE_BASE && WARM_RESOLVE_BASE < 100,
);

/// Choose the parse phase's `(base, top)` slice of the unified bar from
/// two *independent* signals, so all three reload shapes pace sensibly
/// and stay monotonic:
///
/// - `cache_scanned` — did the disk-cache load slice run? True whenever a
///   cache existed and was scanned (entries restored OR dropped), so the
///   bar already climbed to [`LOAD_SPAN`]. Sets the **base**: parse
///   continues from `LOAD_SPAN`, or from `0` on a true cold start.
/// - `has_cached_hits` — is this a small changed set (the warm shape)?
///   Sets the **top**: a warm burst tops out at [`WARM_PARSE_TOP`]; a
///   large re-parse (all-dropped, or cold) runs up to [`PARSE_SPAN`].
///
/// Shapes: fully-warm → `LOAD_SPAN..WARM_PARSE_TOP`; all-dropped →
/// `LOAD_SPAN..PARSE_SPAN` (the sensible hybrid — the load ran but every
/// file must re-parse); fully-cold → `0..PARSE_SPAN`.
pub fn parse_slice(cache_scanned: bool, has_cached_hits: bool) -> (u32, u32) {
    let base = if cache_scanned { LOAD_SPAN } else { 0 };
    let top = if has_cached_hits {
        WARM_PARSE_TOP
    } else {
        PARSE_SPAN
    };
    (base, top)
}

/// Map `done / total` into the `base..=base + span` slice of a single
/// 0–100% progress bar. Multi-phase pipelines give each phase a
/// disjoint `(base, span)` slice so the combined bar is monotonic —
/// no per-phase reset to 0%. Saturating and clamped: the result never
/// exceeds 100, `done > total` pins to the top of the slice, and
/// `total == 0` (nothing to do in this phase) reports `base`.
pub fn weighted_pct(done: usize, total: usize, base: u32, span: u32) -> u32 {
    if total == 0 {
        return base.min(100);
    }
    let filled = (done.min(total) as u64 * u64::from(span) / total as u64) as u32;
    base.saturating_add(filled).min(100)
}

/// Active progress handle. `report` is throttled so call sites don't
/// need to be careful about update frequency. `end` consumes self; drop
/// without ending also ends (with a fallback message) so a panic in the
/// middle of an operation doesn't leave a stale progress bar.
///
/// General-purpose despite the name — used for both project warming
/// (determinate, with a filled bar) and class rename (indeterminate
/// spinner). Each owner passes its own `token`.
pub struct IndexingProgress {
    client: Client,
    token: NumberOrString,
    /// Set to false after `end` runs so `Drop` doesn't double-end.
    active: bool,
    last_report: Instant,
    /// Highest percentage sent so far on a *determinate* bar, used to floor
    /// every later report so the fill never moves backwards within a
    /// session (a work-done bar that snaps 40→0 across a phase hand-off
    /// looks broken). `None` marks an *indeterminate* spinner (begun with
    /// no percentage): those pass their percentage through untouched — a
    /// spinner has no fill to protect. See [`clamp_monotonic`].
    last_pct: Option<u32>,
}

/// Floor `new` at `last` so a determinate bar never decreases: a later
/// report may raise the fill or hold it, never lower it. `None` (a report
/// with no percentage) holds at `last`. Pure and total so the monotonic
/// guarantee is unit-testable without a mock client.
fn clamp_monotonic(new: Option<u32>, last: u32) -> u32 {
    match new {
        Some(p) => p.max(last),
        None => last,
    }
}

impl IndexingProgress {
    /// Create the progress token on the client and emit the `Begin`
    /// notification with the persistent `title` and an initial `message`.
    /// `percentage` is `Some(0)` for a determinate bar, or `None` for an
    /// indeterminate spinner (use this when you can't report incremental
    /// progress, e.g. a single blocking scan). Returns `None` if the client
    /// doesn't honour the create request — the caller proceeds without UI.
    ///
    /// Passing the initial message into `Begin` (rather than a separate
    /// `Report`) matters: there's an observable gap between `Begin` and the
    /// first follow-up report, and without an initial message the status-bar
    /// entry shows just the title for that gap, looking stuck.
    pub async fn begin(
        client: Client,
        token: impl Into<String>,
        title: impl Into<String>,
        initial_message: impl Into<String>,
        percentage: Option<u32>,
    ) -> Option<Self> {
        let token = NumberOrString::String(token.into());
        let title = title.into();
        let initial_message = initial_message.into();

        // Ask the client to allocate the progress token. Some clients
        // (older ones) don't support this; we'd rather skip the UI than
        // fail the operation.
        if client
            .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .await
            .is_err()
        {
            return None;
        }

        client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title,
                        cancellable: Some(false),
                        message: Some(initial_message),
                        // The `percentage` field is a separate numeric channel
                        // from the message: clients that render a bar use it for
                        // fill, `None` yields an indeterminate spinner.
                        percentage,
                    },
                )),
            })
            .await;

        Some(Self {
            client,
            token,
            active: true,
            last_report: Instant::now(),
            // Seed the monotonic floor with the initial fill: a fresh
            // session (or reindex) begins here, so nothing before it
            // constrains the bar. `None` here means indeterminate — the
            // clamp is bypassed entirely for spinners.
            last_pct: percentage,
        })
    }

    /// Send an incremental update. Calls within `REPORT_THROTTLE` of the
    /// previous report are dropped — pass `force=true` to bypass the
    /// throttle (use this for phase transitions you want guaranteed to
    /// land, e.g. "Discovering files" → "Indexing files").
    pub async fn report(
        &mut self,
        message: impl Into<String>,
        percentage: Option<u32>,
        force: bool,
    ) {
        if !self.active {
            return;
        }
        if !force && self.last_report.elapsed() < REPORT_THROTTLE {
            return;
        }
        self.last_report = Instant::now();

        // Floor the fill on a determinate bar so it never moves backwards;
        // the message always updates, only the percentage is clamped, so a
        // phase-transition report still shows its text while the bar holds.
        // Indeterminate spinners (`last_pct == None`) pass through untouched.
        let percentage = match self.last_pct {
            Some(last) => {
                let sent = clamp_monotonic(percentage, last);
                self.last_pct = Some(sent);
                Some(sent)
            }
            None => percentage,
        };

        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                    WorkDoneProgressReport {
                        cancellable: Some(false),
                        message: Some(message.into()),
                        percentage,
                    },
                )),
            })
            .await;
    }

    /// Finalize the progress. The status bar entry disappears after the
    /// brief `message` flash. Consumes self.
    pub async fn end(mut self, message: impl Into<String>) {
        if !self.active {
            return;
        }
        self.active = false;
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(message.into()),
                })),
            })
            .await;
    }
}

impl Drop for IndexingProgress {
    /// Safety net: if the warming pipeline panics or returns early
    /// without calling `end`, we still need to clear the status-bar
    /// entry. Spawn a fire-and-forget task because `Drop` can't await.
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let client = self.client.clone();
        let token = self.token.clone();
        tokio::spawn(async move {
            client
                .send_notification::<ProgressNotification>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd {
                            message: Some("Interrupted.".into()),
                        },
                    )),
                })
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status-bar title is a public brand surface: it's how a user
    /// tells this extension's progress entry apart from the official
    /// Laravel extension's when both are installed. Pinned to the exact
    /// literal so a rename can't silently regress it.
    #[test]
    fn progress_title_is_the_short_brand_name() {
        assert_eq!(PROGRESS_TITLE, "Laravel CE");
    }

    #[test]
    fn phase_one_fills_zero_to_parse_span() {
        assert_eq!(weighted_pct(0, 40, 0, PARSE_SPAN), 0);
        assert_eq!(weighted_pct(40, 40, 0, PARSE_SPAN), PARSE_SPAN);
    }

    #[test]
    fn phase_two_fills_parse_span_to_one_hundred() {
        let span = 100 - PARSE_SPAN;
        assert_eq!(weighted_pct(0, 40, PARSE_SPAN, span), PARSE_SPAN);
        assert_eq!(weighted_pct(40, 40, PARSE_SPAN, span), 100);
    }

    /// Walk both phases end-to-end (awkward prime total so integer
    /// division exercises every rounding step) and assert the unified
    /// bar never moves backwards — including across the phase hand-off.
    #[test]
    fn unified_bar_is_monotonic_non_decreasing() {
        let total = 137;
        let mut last = 0;
        for done in 0..=total {
            let pct = weighted_pct(done, total, 0, PARSE_SPAN);
            assert!(pct >= last, "phase 1 regressed at {done}: {pct} < {last}");
            last = pct;
        }
        for done in 0..=total {
            let pct = weighted_pct(done, total, PARSE_SPAN, 100 - PARSE_SPAN);
            assert!(pct >= last, "phase 2 regressed at {done}: {pct} < {last}");
            last = pct;
        }
        assert_eq!(last, 100);
    }

    #[test]
    fn phase_boundary_is_continuous() {
        // Phase 1's final report and phase 2's first report meet at
        // exactly PARSE_SPAN — no gap, no backwards jump at the hand-off.
        assert_eq!(
            weighted_pct(9, 9, 0, PARSE_SPAN),
            weighted_pct(0, 9, PARSE_SPAN, 100 - PARSE_SPAN),
        );
    }

    #[test]
    fn empty_phase_reports_its_base() {
        assert_eq!(weighted_pct(0, 0, 0, PARSE_SPAN), 0);
        assert_eq!(weighted_pct(0, 0, PARSE_SPAN, 100 - PARSE_SPAN), PARSE_SPAN);
    }

    /// The warm disk-cache load slice fills `0..LOAD_SPAN` — the ~40k
    /// entries the reporter animates while the freshness pass runs.
    #[test]
    fn warm_load_slice_fills_zero_to_load_span() {
        assert_eq!(weighted_pct(0, 40_000, 0, LOAD_SPAN), 0);
        assert_eq!(weighted_pct(40_000, 40_000, 0, LOAD_SPAN), LOAD_SPAN);
    }

    /// The warm slices meet at their boundaries: load tops out at
    /// LOAD_SPAN where parse begins, parse tops out at WARM_PARSE_TOP,
    /// and the re-resolve slice runs WARM_RESOLVE_BASE..100. (The const
    /// ordering itself is a compile-time assert by the consts.)
    #[test]
    fn warm_slices_meet_at_their_boundaries() {
        // load → parse hand-off at LOAD_SPAN.
        assert_eq!(weighted_pct(9, 9, 0, LOAD_SPAN), LOAD_SPAN);
        assert_eq!(
            weighted_pct(0, 5, LOAD_SPAN, WARM_PARSE_TOP - LOAD_SPAN),
            LOAD_SPAN,
        );
        // parse tops out at WARM_PARSE_TOP.
        assert_eq!(
            weighted_pct(5, 5, LOAD_SPAN, WARM_PARSE_TOP - LOAD_SPAN),
            WARM_PARSE_TOP,
        );
        // re-resolve runs WARM_RESOLVE_BASE..100.
        assert_eq!(
            weighted_pct(0, 300, WARM_RESOLVE_BASE, 100 - WARM_RESOLVE_BASE),
            WARM_RESOLVE_BASE,
        );
        assert_eq!(
            weighted_pct(300, 300, WARM_RESOLVE_BASE, 100 - WARM_RESOLVE_BASE),
            100,
        );
    }

    /// Walk the whole warm reload — load slice, parse slice, boundary-band
    /// phase reports, then the re-resolve slice — and assert the bar never
    /// moves backwards, including across every hand-off.
    #[test]
    fn warm_reload_bar_is_monotonic_non_decreasing() {
        let mut last = 0;
        let mut check = |pct: u32, at: &str| {
            assert!(pct >= last, "bar regressed at {at}: {pct} < {last}");
            last = pct;
        };
        // Disk-cache load: the dominant early warm phase (prime total so
        // integer division exercises every rounding step).
        let load_total = 40_003;
        for done in 0..=load_total {
            check(weighted_pct(done, load_total, 0, LOAD_SPAN), "load");
        }
        // Parse burst: a handful of changed files, base = LOAD_SPAN.
        for done in 0..=3 {
            check(
                weighted_pct(done, 3, LOAD_SPAN, WARM_PARSE_TOP - LOAD_SPAN),
                "parse",
            );
        }
        // Boundary band: the forced phase reports the opaque bulk ops
        // emit — save/restore at the parse top, import at the resolve base.
        check(WARM_PARSE_TOP, "save/restore boundary");
        check(WARM_RESOLVE_BASE, "import boundary");
        // Granular re-resolve: the dominant late warm phase.
        let total = 137;
        for done in 0..=total {
            check(
                weighted_pct(done, total, WARM_RESOLVE_BASE, 100 - WARM_RESOLVE_BASE),
                "re-resolve",
            );
        }
        assert_eq!(last, 100);
    }

    /// `parse_slice` picks the right `(base, top)` for each of the three
    /// reload shapes — the base continues the load slice iff the cache was
    /// scanned, the top widens iff this isn't a small warm changed set.
    #[test]
    fn parse_slice_covers_the_three_reload_shapes() {
        // fully-warm: cache scanned, small changed set.
        assert_eq!(parse_slice(true, true), (LOAD_SPAN, WARM_PARSE_TOP));
        // all-dropped: cache scanned, but every file must re-parse.
        assert_eq!(parse_slice(true, false), (LOAD_SPAN, PARSE_SPAN));
        // fully-cold: no cache scanned, everything parses from 0.
        assert_eq!(parse_slice(false, false), (0, PARSE_SPAN));
        // base < top holds for every shape (a non-empty parse slice).
        for &(base, top) in &[
            parse_slice(true, true),
            parse_slice(true, false),
            parse_slice(false, false),
        ] {
            assert!(base < top, "empty parse slice: {base}..{top}");
        }
    }

    /// The all-dropped warm reload (a composer update / branch switch bumps
    /// every cached file's mtime → the load slice ran but restored nothing,
    /// so every file re-parses). This is the shape that regressed: the load
    /// slice climbed to LOAD_SPAN, then the parse transition snapped the bar
    /// to 0. Assert `parse_slice` bases parse at LOAD_SPAN and the full
    /// load → parse → magic sequence never decreases and reaches 100.
    #[test]
    fn all_dropped_reload_is_monotonic_from_load_span() {
        let (parse_base, parse_top) = parse_slice(true, false);
        assert_eq!(
            parse_base, LOAD_SPAN,
            "parse must continue from the load slice"
        );
        assert_eq!(parse_top, PARSE_SPAN);
        let resolve_base = parse_top.max(WARM_RESOLVE_BASE);
        assert_eq!(
            resolve_base, PARSE_SPAN,
            "large re-parse hands magic PARSE_SPAN..100"
        );

        let mut last = 0;
        let mut check = |pct: u32, at: &str| {
            assert!(pct >= last, "bar regressed at {at}: {pct} < {last}");
            last = pct;
        };
        // Load slice: every entry dropped, but the reporter still advanced.
        let load_total = 40_003;
        for done in 0..=load_total {
            check(weighted_pct(done, load_total, 0, LOAD_SPAN), "load");
        }
        // Parse: the whole project re-parses, base = LOAD_SPAN.
        let parse_total = 40_003;
        for done in 0..=parse_total {
            check(
                weighted_pct(done, parse_total, parse_base, parse_top - parse_base),
                "parse",
            );
        }
        // Magic: PARSE_SPAN..100.
        let magic_total = 137;
        for done in 0..=magic_total {
            check(
                weighted_pct(done, magic_total, resolve_base, 100 - resolve_base),
                "magic",
            );
        }
        assert_eq!(last, 100);
    }

    /// Mixed branch: the pattern disk cache was stale (cold parse,
    /// slice 0..PARSE_SPAN) but the magic cache restored (warm
    /// re-resolve). The call site clamps the resolve base with
    /// `max(parse_top)`, so the re-resolve continues from PARSE_SPAN
    /// instead of jumping back to WARM_RESOLVE_BASE.
    #[test]
    fn cold_parse_warm_resolve_stays_monotonic() {
        let parse_top = PARSE_SPAN; // cold parse tops out here
        let resolve_base = parse_top.max(WARM_RESOLVE_BASE);
        assert_eq!(resolve_base, PARSE_SPAN);
        assert_eq!(
            weighted_pct(9, 9, 0, parse_top),
            weighted_pct(0, 50, resolve_base, 100 - resolve_base),
        );
        assert_eq!(weighted_pct(50, 50, resolve_base, 100 - resolve_base), 100);
    }

    /// A decreasing input sequence must come out non-decreasing, and a
    /// `None` report must hold the last value — the structural guarantee
    /// that a determinate bar never snaps backwards (e.g. the load slice's
    /// ~40 → a parse transition's 0 on an all-dropped warm reload).
    #[test]
    fn clamp_monotonic_never_decreases() {
        // Raw inputs a caller might send across phase hand-offs, including
        // a backwards jump (40 → 0) and holds (None).
        let inputs = [
            Some(0),
            Some(10),
            Some(40),
            Some(0),  // backwards jump — must be floored to 40
            None,     // hold — stays at 40
            Some(25), // still below the floor — stays at 40
            Some(55),
            None,
            Some(100),
        ];
        let mut last = 0;
        let mut outputs = Vec::new();
        for new in inputs {
            last = clamp_monotonic(new, last);
            outputs.push(last);
        }
        // Output is non-decreasing.
        for pair in outputs.windows(2) {
            assert!(pair[1] >= pair[0], "regressed: {} < {}", pair[1], pair[0]);
        }
        assert_eq!(outputs, [0, 10, 40, 40, 40, 40, 55, 55, 100]);
        // None on its own is a pure hold.
        assert_eq!(clamp_monotonic(None, 73), 73);
        // A raise still raises.
        assert_eq!(clamp_monotonic(Some(90), 73), 90);
    }

    #[test]
    fn result_is_clamped() {
        // base + span overshooting 100 clamps.
        assert_eq!(weighted_pct(10, 10, 90, 20), 100);
        // done > total pins to the top of the slice, never past it.
        assert_eq!(weighted_pct(50, 10, 0, PARSE_SPAN), PARSE_SPAN);
        // An absurd base still clamps to 100 (with and without work).
        assert_eq!(weighted_pct(0, 5, 150, 10), 100);
        assert_eq!(weighted_pct(0, 0, 150, 10), 100);
    }
}
