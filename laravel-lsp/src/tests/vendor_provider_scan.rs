//! Equivalence tests for the shared vendor service-provider scan (issue #371).
//!
//! `register_service_provider_files_with_salsa` and `rescan_vendor_providers`
//! each ran the same two `WalkDir` legs — `vendor/laravel/framework/src/
//! Illuminate` at depth 10 for `*ServiceProvider.php`, and `vendor/` at depth 6
//! for `*ServiceProvider.php` plus `Http/**/Kernel.php` — inline in an
//! `async fn`, blocking a Tokio worker for the whole scan. Both now filter the
//! shared vendor walk through [`vendor_provider_priority`], with the reads in
//! `spawn_blocking`.
//!
//! The tests compare that predicate against an executable copy of the two
//! former legs. Keeping the old implementation is the only way to test a claim
//! about the old implementation.

use crate::{collect_vendor_provider_sources, vendor_provider_priority};
use laravel_lsp::vendor_index::VendorIndex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use walkdir::WalkDir;

/// The two vendor legs exactly as they were written before the shared walk,
/// reduced to the `path -> priority` decision they drove.
fn provider_priorities_two_walks(root: &Path) -> HashMap<PathBuf, u8> {
    let mut out = HashMap::new();

    let framework_path = root.join("vendor/laravel/framework/src/Illuminate");
    if framework_path.exists() {
        for entry in WalkDir::new(&framework_path)
            .max_depth(10)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "php")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with("ServiceProvider.php"))
            {
                out.insert(path.to_path_buf(), 0);
            }
        }
    }

    let vendor_path = root.join("vendor");
    if vendor_path.exists() {
        for entry in WalkDir::new(&vendor_path)
            .max_depth(6)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.starts_with(&framework_path) {
                continue;
            }
            if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let path_str = path.to_string_lossy();
                let is_service_provider = file_name.ends_with("ServiceProvider.php");
                let is_http_kernel = file_name == "Kernel.php"
                    && (path_str.contains("/Http/") || path_str.contains("\\Http\\"));
                if is_service_provider || is_http_kernel {
                    out.insert(path.to_path_buf(), 1);
                }
            }
        }
    }

    out
}

/// What the shared path decides, in the same shape.
fn provider_priorities_shared(root: &Path, vendor: &VendorIndex) -> HashMap<PathBuf, u8> {
    let framework_root = root.join("vendor/laravel/framework/src/Illuminate");
    vendor
        .files()
        .iter()
        .filter_map(|f| vendor_provider_priority(&framework_root, f).map(|p| (f.path.clone(), p)))
        .collect()
}

/// A vendor tree covering every branch of the decision: framework providers
/// inside and outside the depth-10 budget, package providers inside and outside
/// the depth-6 budget, an `Http/Kernel.php`, a `Kernel.php` NOT under `Http/`,
/// a framework-owned `Http/Kernel.php` (which neither leg ever took), and
/// ordinary PHP that is none of the above.
fn seed(root: &Path) {
    let files = [
        // Framework leg, depth 1 below the framework root.
        "vendor/laravel/framework/src/Illuminate/FooServiceProvider.php",
        // Framework leg, depth 10 — the last depth it admitted.
        "vendor/laravel/framework/src/Illuminate/a/b/c/d/e/f/g/h/i/DeepServiceProvider.php",
        // Framework leg, depth 11 — past it.
        "vendor/laravel/framework/src/Illuminate/a/b/c/d/e/f/g/h/i/j/PastServiceProvider.php",
        // A framework Http/Kernel.php: the framework leg took only providers,
        // and the package leg skipped framework paths outright.
        "vendor/laravel/framework/src/Illuminate/Http/Kernel.php",
        // Package leg, well inside depth 6.
        "vendor/acme/pkg/AcmeServiceProvider.php",
        "vendor/acme/pkg/src/Http/Kernel.php",
        // Package leg, depth 6 exactly.
        "vendor/acme/pkg/a/b/c/EdgeServiceProvider.php",
        // Package leg, depth 7 — past it.
        "vendor/acme/pkg/a/b/c/d/PastServiceProvider.php",
        // A Kernel.php not under Http/ — never admitted.
        "vendor/acme/pkg/src/Console/Kernel.php",
        // Ordinary vendor PHP.
        "vendor/acme/pkg/src/Plain.php",
    ];
    for rel in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "<?php\n// provider\n").unwrap();
    }
}

#[test]
fn shared_walk_selects_the_same_provider_files_as_the_two_walks() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);

    let shared = provider_priorities_shared(root, &vendor);
    let oracle = provider_priorities_two_walks(root);

    let mut shared_sorted: Vec<_> = shared.iter().collect();
    let mut oracle_sorted: Vec<_> = oracle.iter().collect();
    shared_sorted.sort();
    oracle_sorted.sort();

    assert_eq!(
        shared_sorted, oracle_sorted,
        "the shared predicate must select exactly the files, and the tiers, \
         that the two former walks did"
    );
    assert!(
        !oracle.is_empty(),
        "fixture check — an empty oracle would make the equality vacuous"
    );
}

#[test]
fn each_branch_of_the_provider_decision_is_pinned() {
    // Discriminates the equality above: it would still hold if both sides were
    // wrong in the same way, since only the shared side is under test here.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);
    let shared = provider_priorities_shared(root, &vendor);
    let at = |rel: &str| shared.get(&root.join(rel)).copied();

    assert_eq!(
        at("vendor/laravel/framework/src/Illuminate/FooServiceProvider.php"),
        Some(0),
        "a framework provider is tier 0"
    );
    assert_eq!(
        at("vendor/laravel/framework/src/Illuminate/a/b/c/d/e/f/g/h/i/DeepServiceProvider.php"),
        Some(0),
        "depth 10 below the FRAMEWORK root is inside its budget — measuring \
         that budget from vendor/ instead would cut it off here"
    );
    assert_eq!(
        at("vendor/laravel/framework/src/Illuminate/a/b/c/d/e/f/g/h/i/j/PastServiceProvider.php"),
        None,
        "depth 11 is past it"
    );
    assert_eq!(
        at("vendor/laravel/framework/src/Illuminate/Http/Kernel.php"),
        None,
        "a framework Http/Kernel.php belongs to neither leg: the framework leg \
         took only providers, the package leg skipped framework paths"
    );
    assert_eq!(
        at("vendor/acme/pkg/AcmeServiceProvider.php"),
        Some(1),
        "a package provider is tier 1"
    );
    assert_eq!(
        at("vendor/acme/pkg/src/Http/Kernel.php"),
        Some(1),
        "a package Http/Kernel.php is taken for its middleware definitions"
    );
    assert_eq!(
        at("vendor/acme/pkg/a/b/c/EdgeServiceProvider.php"),
        Some(1),
        "depth 6 below vendor/ is inside the package budget"
    );
    assert_eq!(
        at("vendor/acme/pkg/a/b/c/d/PastServiceProvider.php"),
        None,
        "depth 7 is past it"
    );
    assert_eq!(
        at("vendor/acme/pkg/src/Console/Kernel.php"),
        None,
        "a Kernel.php outside Http/ was never a middleware source"
    );
    assert_eq!(
        at("vendor/acme/pkg/src/Plain.php"),
        None,
        "ordinary vendor PHP is not a provider"
    );
}

#[test]
fn collect_reads_only_the_selected_files() {
    // The read budget: ~73 of 16,051 files on a real project. If the predicate
    // and the collector ever disagreed, this scan would go from a few dozen
    // reads to the whole tree.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    seed(root);
    let vendor = VendorIndex::build(root);

    let sources = collect_vendor_provider_sources(root, &vendor);
    let selected = provider_priorities_shared(root, &vendor);

    assert_eq!(
        sources.len(),
        selected.len(),
        "one source per selected file, and nothing else read"
    );
    assert!(
        sources.len() < vendor.len(),
        "fixture check — the scan must read strictly fewer files ({}) than the \
         tree holds ({})",
        sources.len(),
        vendor.len()
    );
    for source in &sources {
        assert_eq!(
            selected.get(&source.path).copied(),
            Some(source.priority),
            "each collected source carries the tier the predicate assigned it"
        );
        assert!(source.content.contains("<?php"), "and the file's real text");
    }
}

#[test]
fn a_project_without_vendor_yields_no_provider_sources() {
    // Both former functions guarded each leg on `exists()`. The shared index is
    // simply empty instead — the scan must degrade the same way, not panic on
    // the empty vendor root.
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("app")).unwrap();
    let vendor = VendorIndex::build(tmp.path());

    assert!(collect_vendor_provider_sources(tmp.path(), &vendor).is_empty());
}
