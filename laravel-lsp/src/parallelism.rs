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

#[cfg(test)]
mod tests;
