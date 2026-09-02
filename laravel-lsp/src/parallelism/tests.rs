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

    assert_eq!(bound(1), 2, "single core still gets the floor");
    assert_eq!(bound(2), 2, "two cores get the floor, not one");
    assert_eq!(bound(4), 2, "a 4-core laptop keeps half the machine free");
    assert_eq!(bound(8), 4);
    assert_eq!(bound(16), 8, "the ceiling binds before half does");
    assert_eq!(bound(20), 8, "an M1 Ultra is capped at the warm path's 8");
    assert_eq!(bound(128), 8, "a big server is still capped at 8");
}
