//! Lock-poisoning recovery for the `std::sync::Mutex`-guarded memoization
//! caches (issue #317).
//!
//! Not to be confused with `cache_root_poisoning.rs`, which is about a
//! *persisted on-disk cache* recording a hijacked project root. This module is
//! about `std::sync::Mutex` **poisoning**: a thread that panics while holding a
//! `MutexGuard` marks the mutex poisoned forever, so every later `.lock()`
//! returns `Err`. Under the old `.lock().unwrap()` convention that turned one
//! transient panic anywhere in the process into a permanently panicking cache
//! for the rest of the LSP session.
//!
//! Every production site now acquires with
//! `.lock().unwrap_or_else(|e| e.into_inner())`, which takes the guard back out
//! of the poison error and carries on. These tests pin that behaviour on
//! `vendor_open_magic_lru` — the only one of the six hardened caches reachable
//! from a test, because the other five live behind module-private
//! `OnceLock`/`LazyLock` statics (`class_locator`'s `locator_cache` and
//! `ROOTS`, `chain`'s `parsed_file_cache`, `vendor_member_prover`'s
//! `PACKAGE_MEMBER_READS`, `ComposerAutoload::for_project`'s `CACHE`) that hand
//! out no `Mutex` handle to poison. `vendor_open_magic_lru` is a `Backend`
//! field, so a test can hold its real guard and panic on it.
//!
//! Both tests poison the **real production mutex** (not a stand-in), then check
//! that recovery yields a cache that still works, not merely a `.lock()` that
//! didn't panic. The second test drives the recovery through
//! `record_vendor_open_magic`, the production call site itself, so reverting
//! that one line back to `.unwrap()` turns this file red.

use crate::LaravelLanguageServer;
use std::path::PathBuf;
use std::sync::Arc;
use tower_lsp::LspService;

fn backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

fn poisoned_path() -> PathBuf {
    PathBuf::from("/vendor/acme/pkg/src/PoisonedDuringWrite.php")
}

/// Poison `backend.vendor_open_magic_lru` for real: a spawned thread takes the
/// production lock, writes one entry through the live guard, and panics with
/// the guard still bound.
///
/// Returns once the panicking thread has been joined; the caller asserts on the
/// poison state.
fn poison_lru(backend: &LaravelLanguageServer) {
    let lru = Arc::clone(&backend.vendor_open_magic_lru);
    let handle = std::thread::spawn(move || {
        let mut guard = lru.lock().expect("the cache is not poisoned yet");
        // The mutating cache operation has to happen *through the live guard*,
        // before the panic — that is what leaves the cache "half updated" and
        // makes recovery meaningful rather than a no-op.
        guard.push(poisoned_path(), ());
        // guard still held here
        panic!("simulated panic while the vendor-open-magic guard is live");
    });

    assert!(
        handle.join().is_err(),
        "the spawned thread must actually unwind — a thread that returned Ok \
         never poisoned anything and the rest of the test would be vacuous"
    );
    assert!(
        backend.vendor_open_magic_lru.is_poisoned(),
        "a panic with the guard bound must leave the mutex poisoned — without \
         this the recovery below would be exercising a healthy lock"
    );
}

#[tokio::test]
async fn poisoned_vendor_lru_is_recovered_and_still_holds_its_invariants() {
    let backend = backend();
    poison_lru(&backend);

    // The exact expression every production site now uses.
    let mut recovered = backend
        .vendor_open_magic_lru
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    assert!(
        recovered.peek(&poisoned_path()).is_some(),
        "the entry written before the panic is still there — recovery hands \
         back the same cache, it does not reset it"
    );

    // Not just "the lock opened": the recovered cache still behaves like an LRU.
    let fresh = PathBuf::from("/vendor/acme/pkg/src/AfterRecovery.php");
    assert!(
        recovered.push(fresh.clone(), ()).is_none(),
        "a fresh insert into the recovered cache displaces nothing"
    );
    assert!(
        recovered.get(&fresh).is_some(),
        "the freshly inserted entry is retrievable through the recovered guard"
    );
    assert_eq!(
        recovered.len(),
        2,
        "both the pre-panic entry and the post-recovery one are counted"
    );
    assert!(
        recovered.len() <= crate::VENDOR_OPEN_MAGIC_LRU_CAP,
        "the recovered cache still respects its configured capacity"
    );
}

#[tokio::test]
async fn production_record_path_runs_against_a_poisoned_vendor_lru() {
    let backend = backend();
    poison_lru(&backend);

    // `record_vendor_open_magic` is the production caller of the hardened
    // `main.rs` site. Under `.lock().unwrap()` this call panics; under
    // `.lock().unwrap_or_else(|e| e.into_inner())` it records normally.
    // Two entries against a cap of 128 means no eviction, so no ripple work is
    // triggered and this stays a pure cache assertion.
    let opened = PathBuf::from("/vendor/acme/pkg/src/OpenedAfterPoison.php");
    backend.record_vendor_open_magic(&opened).await;

    let mut lru = backend
        .vendor_open_magic_lru
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(
        lru.get(&opened).is_some(),
        "the production on-open path recorded the vendor file even though the \
         cache mutex was poisoned"
    );
    assert_eq!(
        lru.len(),
        2,
        "the pre-panic entry survived and the production insert was added to it"
    );
    assert!(
        lru.len() <= crate::VENDOR_OPEN_MAGIC_LRU_CAP,
        "the recovered cache still respects its configured capacity"
    );
}
