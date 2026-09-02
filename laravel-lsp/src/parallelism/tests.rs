use super::*;

/// The three rules the bound exists to enforce, checked against whatever
/// machine the test runs on. Asserting a literal would only re-state the
/// hardware, and would flip between a CI runner and a workstation.
#[test]
fn bounded_pool_size_stays_within_its_three_rules() {
    let size = bounded_pool_size();

    assert!(size >= 2, "never fewer than 2 workers, got {size}");
    assert!(size <= 8, "never more than 8 workers, got {size}");

    if let Ok(cores) = std::thread::available_parallelism() {
        let cores = cores.get();
        // Never more than half the machine — except where the floor lifts it,
        // which is the point of the floor. A 2-core runner gets 2, not 1.
        assert!(
            size <= (cores / 2).max(2),
            "took more than half of {cores} cores: {size}"
        );
    }
}

/// A 4-core laptop is the case the bound exists for: rayon's default global
/// pool would take all four and leave the editor competing with the server.
/// The formula has to yield 2 there, and 8 on a large workstation.
#[test]
fn the_formula_halves_small_machines_and_caps_large_ones() {
    // The pure arithmetic, exercised at the sizes that matter without
    // depending on the host's core count.
    fn bound(cores: usize) -> usize {
        (cores / 2).clamp(2, 8)
    }

    // Note the sizes below are the formula, not this machine.
    assert_eq!(bound(1), 2, "single core still gets the floor");
    assert_eq!(bound(2), 2, "two cores get the floor, not one");
    assert_eq!(bound(4), 2, "a 4-core laptop keeps half the machine free");
    assert_eq!(bound(8), 4);
    assert_eq!(bound(16), 8, "the ceiling binds before half does");
    assert_eq!(bound(20), 8, "an M1 Ultra is capped at the warm path's 8");
    assert_eq!(bound(128), 8, "a big server is still capped at 8");
}

/// The bound is only worth anything if the work actually lands on the bounded
/// pool. `rayon::current_num_threads()` reports the pool the caller is running
/// inside, so reading it through `install` is a direct check that a `par_iter`
/// in there fans across our threads rather than rayon's global ones.
#[test]
fn install_runs_its_closure_on_the_bounded_pool() {
    let inside = install(rayon::current_num_threads);
    assert_eq!(
        inside,
        bounded_pool_size(),
        "work inside install() must see the bounded pool's width"
    );
}

/// The failure this guards is silent: dropping the `install` wrapper leaves the
/// code compiling, passing, and quietly back on the global pool. Comparing
/// against the width outside `install` catches that on any machine where the
/// two differ — every machine with more than 2×8 cores, and every machine with
/// fewer than 4.
#[test]
fn the_global_pool_is_not_what_the_load_pass_runs_on() {
    let outside = rayon::current_num_threads();
    let inside = install(rayon::current_num_threads);

    if outside == bounded_pool_size() {
        // This host's global pool happens to match the bound, so the
        // comparison proves nothing here. The assertion above still holds it.
        return;
    }
    assert_ne!(
        inside, outside,
        "install() handed the closure the global pool ({outside} threads)"
    );
}

/// A pool is threads; rebuilding one per call would cost more than the work it
/// fans out. Two calls must land on the same pool.
#[test]
fn the_pool_is_built_once_and_reused() {
    let first = pool().map(|p| p.current_num_threads());
    let second = pool().map(|p| p.current_num_threads());

    assert_eq!(first, second);
    assert!(
        std::ptr::eq(
            pool().expect("pool builds") as *const _,
            pool().expect("pool builds") as *const _
        ),
        "each call rebuilt the pool instead of reusing the OnceLock"
    );
}
