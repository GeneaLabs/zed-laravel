//! One bound for every worker pool in the server, and one place that says why
//! (issue #373).
//!
//! The complaint that started issue #80 was a laptop overheating under
//! sustained single-core load. Spreading that work across several cores is
//! actually *kinder* to the machine — the same energy, less time, and the
//! machine returns to idle sooner. The failure mode that matters is different:
//! on a small machine, a pool that takes every core makes the editor stutter,
//! because the editor, this server's own async runtime, and the user's other
//! work all need a share.
//!
//! So every pool is bounded, and bounded the same way.

use std::sync::OnceLock;

/// Built on first use and reused for the process lifetime — a pool is threads,
/// and rebuilding one per call would cost more than the work it fans out.
static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();

/// Worker count for a pool that fans project-file work across cores.
///
/// `min(8, max(2, available_parallelism() / 2))`, which reads as three rules:
///
/// * **Never more than 8.** The warm-start parse pass settled on 8 through
///   measurement (`MAX_CONCURRENT_PARSES` in `main.rs`) and the pass is partly
///   I/O-bound, so threads past that buy little. This function does not change
///   that pass — it has its own tuned semaphore and #373 does not ask to move
///   it — it adopts its ceiling for the pools that had none.
/// * **Never more than half the machine.** Half rather than all so the editor
///   and the async runtime keep headroom. On a 4-core laptop this is 2, where
///   rayon's default global pool would have taken all four.
/// * **Never fewer than 2.** Below that there is no parallelism left to bound,
///   and a single worker would be slower than the serial code it replaced.
///
/// `available_parallelism()` fails on some containerized platforms; that falls
/// back to the floor rather than the ceiling, because a machine that cannot
/// report its core count is more likely to be constrained than large.
pub fn bounded_pool_size() -> usize {
    const CEILING: usize = 8;
    const FLOOR: usize = 2;

    let half = std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(FLOOR);

    half.clamp(FLOOR, CEILING)
}

/// The server's own rayon pool, built once at [`bounded_pool_size`] threads.
///
/// `None` only if rayon refuses to build it, which in practice means the
/// process cannot spawn threads at all.
fn pool() -> Option<&'static rayon::ThreadPool> {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(bounded_pool_size())
            .thread_name(|i| format!("laravel-lsp-cpu-{i}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// Run `op` on the server's bounded rayon pool, so any `par_iter` inside it
/// fans across [`bounded_pool_size`] threads rather than rayon's global pool.
///
/// **Why not the global pool.** rayon's default is one thread per core. On a
/// 20-core workstation that is 20 threads of work; on a 4-core laptop it takes
/// all four, and the editor competes with its own language server for the
/// machine. The global pool is also shared with every other rayon user linked
/// into this binary (salsa among them), so resizing it would reach past this
/// server's own work. A dedicated pool bounds ours and leaves theirs alone.
///
/// Expect no speedup from this — it is a contention fix. The passes it wraps
/// are partly I/O-bound, so the threads it removes were buying little.
///
/// Falls back to running `op` on the calling thread if the pool could not be
/// built. Work inside would then reach the global pool, which is the
/// unbounded behaviour this replaces — worse, but not wrong, and better than
/// refusing to load the cache at all.
pub fn install<R: Send>(op: impl FnOnce() -> R + Send) -> R {
    match pool() {
        Some(pool) => pool.install(op),
        None => op(),
    }
}

#[cfg(test)]
mod tests;
