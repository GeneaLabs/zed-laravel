//! Regression: the warm parse loop must advance its progress count in task
//! COMPLETION order, not spawn order.
//!
//! The warming pass spawns one parse task per file and drains them with a
//! `tokio::task::JoinSet` via `join_next`, which yields each task as it
//! finishes. Previously it awaited the spawned handles in SPAWN order
//! (`for h in handles { h.await }`): a single slow file (this project has
//! 6–23 MB seeder files that parse for seconds) blocked the loop on that
//! one handle, freezing the progress count (~23%) while every fast file
//! finished invisibly in the background — then the backlog drained at once
//! and the bar blasted to done.
//!
//! This test models the drain: one slow task spawned FIRST, then many
//! instant tasks. With completion-order draining, all the fast tasks are
//! observed before the slow one, so the count climbs smoothly. With the
//! old spawn-order await, the slow task (spawned first) would gate the
//! whole drain and `before_slow` would be 0.

use std::time::Duration;

#[tokio::test]
async fn parse_progress_advances_on_completion_not_spawn_order() {
    let mut set = tokio::task::JoinSet::new();

    // Spawn the SLOW task first: with an in-spawn-order await it would
    // block the drain before any fast task could be counted.
    set.spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "slow"
    });
    let fast = 20;
    for _ in 0..fast {
        set.spawn(async { "fast" });
    }

    // Drain in completion order, mirroring the warm parse loop.
    let mut completed = 0usize;
    let mut before_slow = 0usize;
    let mut saw_slow = false;
    while let Some(res) = set.join_next().await {
        completed += 1;
        if res.unwrap() == "slow" {
            saw_slow = true;
        } else if !saw_slow {
            before_slow += 1;
        }
    }

    assert_eq!(completed, fast + 1, "every task is drained");
    assert!(saw_slow, "the slow task is eventually drained");
    // The instant tasks finish long before the 300 ms sleeper, so their
    // completions are ALL observed first — the count reaches `fast` while
    // the slow task is still sleeping, exactly the smooth advance the bar
    // needs. In-spawn-order draining would force this to 0 (the slow task,
    // spawned first, would be awaited before any fast one).
    assert_eq!(
        before_slow, fast,
        "all fast completions observed before the slow one (completion order)"
    );
}
