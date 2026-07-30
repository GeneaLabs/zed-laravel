//! The user-triggerable "Reindex project" surface (`laravel.reindexProject`).
//!
//! # Why a code action and not a command-palette command
//!
//! Zed extensions cannot register command-palette commands (verified against
//! `zed_extension_api` 0.7.0 — the API has no such hook). The native mechanism
//! an LSP server *does* have is a `source`-kind code action carrying a
//! `workspace/executeCommand` command: Zed shows it in the code-actions menu
//! (`cmd-.`) for any file the server is attached to, regardless of cursor
//! position or diagnostics. So "Laravel CE: Reindex project" is offered as a
//! global code action in every PHP/Blade file, and selecting it sends the
//! `laravel.reindexProject` command back to us, which runs the full cold
//! reindex (see `trigger_reindex` in `main.rs`).
//!
//! # Surviving the client's `only` filter
//!
//! A `textDocument/codeAction` request may carry `context.only`, a list of
//! [`CodeActionKind`]s the client wants back. Kinds are hierarchical: per the
//! LSP spec, a requested `"source"` matches our `"source.reindexProject"`
//! (dotted-segment prefix), and an empty kind matches everything. We honor
//! that filter in [`global_code_actions`] so an automated flow that requests a
//! narrow kind (e.g. `source.fixAll` on save) never pulls in a reindex, while
//! the manual code-actions menu — which requests actions without a kind filter
//! — always gets it.
//!
//! One client-side caveat lives *above* this filter, in the editor UI and so
//! not verifiable from this repo: VS Code (and editors that mirror its
//! semantics) route pure `source.*` actions to a separate "Source Action…"
//! command rather than the quick-fix (`cmd-.`) lightbulb. If Zed does the same,
//! this action would be reachable but not from the `cmd-.` menu. That's a
//! deliberate risk in the chosen `source.reindexProject` kind: the fallback, if
//! a live Zed doesn't surface it in `cmd-.`, is to switch [`REINDEX_ACTION_KIND`]
//! to `CodeActionKind::EMPTY` (no kind — always shown in the menu) or
//! `refactor`. The `only`-filter logic here already admits every one of those
//! kinds, so that swap is a one-line change with no other code impact.
//!
//! This module holds the pure, testable pieces: the command/action constants,
//! the capability options, the `only`-filter logic, the action builder, and
//! the [`IndexingFlightGuard`] that serializes indexing passes. The async
//! orchestration (cache clearing + pipeline re-run) lives in `main.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Command, ExecuteCommandOptions,
    WorkDoneProgressOptions,
};

/// The `workspace/executeCommand` command id that triggers a full cold
/// reindex. Declared in the server capabilities so clients know to route
/// it back to us.
pub const REINDEX_COMMAND: &str = "laravel.reindexProject";

/// The kind of the reindex code action. A custom sub-kind of `source` —
/// clients group `source.*` actions separately from quick-fixes, and the
/// specific tail lets a client target exactly this action if it wants to.
pub const REINDEX_ACTION_KIND: CodeActionKind = CodeActionKind::new("source.reindexProject");

/// The user-visible menu label. Prefixed with "Laravel CE:" because Zed's
/// code-actions menu mixes actions from every attached language server —
/// including, potentially, Laravel's official extension, so the prefix has
/// to name *this* extension unambiguously. Short form: menu labels are a
/// tight slot.
pub const REINDEX_ACTION_TITLE: &str = "Laravel CE: Reindex project";

/// The `execute_command_provider` server capability: the exact list of
/// commands this server implements. Kept here (next to the command id)
/// so the capability and the `execute_command` dispatch can't drift apart.
pub fn execute_command_options() -> ExecuteCommandOptions {
    ExecuteCommandOptions {
        commands: vec![REINDEX_COMMAND.to_string()],
        work_done_progress_options: WorkDoneProgressOptions::default(),
    }
}

/// The always-available code actions for a `textDocument/codeAction` request.
///
/// Returns the "Laravel CE: Reindex project" action when the file is PHP/Blade
/// and the request's `only` filter (if any) admits `source`-kind actions;
/// otherwise an empty vec. Position within the file is deliberately ignored —
/// the action is global by design, so it's reachable from anywhere.
pub fn global_code_actions(
    uri_path: &str,
    only: Option<&[CodeActionKind]>,
) -> Vec<CodeActionOrCommand> {
    // `.ends_with(".php")` covers `.blade.php` too. Non-PHP files the LSP is
    // attached to (`.env`, `phpunit.xml`, …) don't get the action: reindexing
    // from them would work, but offering project-wide actions in a dotenv
    // file reads as noise.
    if !uri_path.ends_with(".php") || !reindex_action_allowed(only) {
        return Vec::new();
    }
    vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: REINDEX_ACTION_TITLE.to_string(),
        kind: Some(REINDEX_ACTION_KIND),
        // No `edit`: the action's entire effect is the command round-trip.
        // The client sends `workspace/executeCommand` with this command id
        // when the user picks the action.
        command: Some(Command {
            title: REINDEX_ACTION_TITLE.to_string(),
            command: REINDEX_COMMAND.to_string(),
            arguments: None,
        }),
        diagnostics: None,
        edit: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })]
}

/// Does the request's `only` filter admit the reindex action?
///
/// `None` means "no filter — send everything". A non-empty list admits us
/// when any requested kind hierarchically matches ours: exact match, an
/// empty kind (matches all), or a dotted-segment prefix (`"source"` matches
/// `"source.reindexProject"`, but `"sourcery"` must not — hence matching on
/// `"{kind}."`, not a bare `starts_with`).
fn reindex_action_allowed(only: Option<&[CodeActionKind]>) -> bool {
    only.is_none_or(|kinds| {
        kinds.iter().any(|kind| {
            let kind = kind.as_str();
            kind.is_empty()
                || kind == REINDEX_ACTION_KIND.as_str()
                || REINDEX_ACTION_KIND
                    .as_str()
                    .starts_with(&format!("{kind}."))
        })
    })
}

/// RAII guard serializing indexing passes: at most one may run at a time.
///
/// [`IndexingFlightGuard::try_acquire`] atomically flips the shared flag
/// `false → true`; while the guard lives, every other `try_acquire` on the
/// same flag fails, so a second `laravel.reindexProject` trigger (or a
/// reindex racing the initial startup warm) no-ops instead of running two
/// warming pipelines over the same shared caches.
///
/// The flag is released in `Drop` — a deliberate Rust idiom: whichever way
/// the owning task ends (normal completion, early `return`, or a panic
/// unwinding the stack), the guard is dropped and the flag clears. No code
/// path can forget to release it, so a crashed warming task can never brick
/// reindexing for the rest of the session.
pub struct IndexingFlightGuard {
    flag: Arc<AtomicBool>,
}

impl IndexingFlightGuard {
    /// Try to claim the indexing slot. Returns `None` if another pass
    /// already holds it.
    ///
    /// `compare_exchange(false, true, ..)` is the atomic "check and set in
    /// one step": unlike a separate `load` + `store`, two concurrent callers
    /// can't both observe `false` and both proceed. `SeqCst` is the
    /// strongest (and simplest to reason about) memory ordering; the flag is
    /// touched a handful of times per session, so its cost is irrelevant.
    pub fn try_acquire(flag: Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self { flag })
    }
}

impl Drop for IndexingFlightGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
