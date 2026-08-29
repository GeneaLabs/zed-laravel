use super::*;
use tempfile::TempDir;

#[test]
fn test_cache_round_trip() {
    let temp = TempDir::new().unwrap();
    let project_root = temp.path();

    // Create a cache manager and add some data
    let mut manager = CacheManager::load(project_root);

    let mut vendor_scan = ScanResult::new();
    vendor_scan.middleware.insert(
        "auth".to_string(),
        MiddlewareEntry {
            class: "Illuminate\\Auth\\Middleware\\Authenticate".to_string(),
            class_file: Some(
                "vendor/laravel/framework/src/Illuminate/Auth/Middleware/Authenticate.php"
                    .to_string(),
            ),
            source_file: Some("bootstrap/app.php".to_string()),
            line: 10,
        },
    );
    manager.set_vendor_scan(vendor_scan);

    // Save
    manager.save().unwrap();

    // Load fresh
    let loaded = CacheManager::load(project_root);
    assert!(loaded.has_cached_data());

    let middleware = loaded.get_all_middleware();
    assert!(middleware.contains_key("auth"));
}

#[test]
fn test_mtime_comparison() {
    let mtime1 = FileMtime {
        mtime_secs: 1000,
        mtime_nanos: 500,
    };
    let mtime2 = FileMtime {
        mtime_secs: 1000,
        mtime_nanos: 500,
    };
    let mtime3 = FileMtime {
        mtime_secs: 1001,
        mtime_nanos: 0,
    };

    assert_eq!(mtime1, mtime2);
    assert_ne!(mtime1, mtime3);
}

#[test]
fn test_xdg_cache_path() {
    use std::path::Path;

    let project_root = Path::new("/Users/mike/Developer/some-project");

    // Verify we can get a cache path
    let cache_file = get_cache_file(project_root);
    assert!(
        cache_file.is_some(),
        "Should be able to determine cache path"
    );

    let cache_path = cache_file.unwrap();
    println!("Cache path for {:?}: {:?}", project_root, cache_path);

    // Verify the path structure on macOS
    #[cfg(target_os = "macos")]
    {
        let path_str = cache_path.to_string_lossy();
        assert!(
            path_str.contains("Library/Caches/org.mike-bronner.laravel-ce-lsp"),
            "macOS cache should be in ~/Library/Caches/org.mike-bronner.laravel-ce-lsp, got: {}",
            path_str
        );
        assert!(
            path_str.ends_with("cache.json"),
            "Cache file should be cache.json, got: {}",
            path_str
        );
    }

    // Verify the cache directory can be determined
    let cache_dir = get_cache_dir(project_root);
    assert!(cache_dir.is_some());
    println!("Cache dir for {:?}: {:?}", project_root, cache_dir.unwrap());
}

#[test]
fn test_clear_disk_caches_removes_dir_and_is_idempotent() {
    // A unique tempdir as the project root gives a unique cache-dir hash, so
    // this test's disk writes/deletes are isolated to their own directory
    // under the real per-user cache root and can't collide with another run.
    let temp = TempDir::new().unwrap();
    let project_root = temp.path();

    // Seed the per-project cache dir with a stand-in for the .bin/.json caches.
    let cache_dir = get_cache_dir(project_root).expect("cache dir resolvable");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("pattern_cache.bin"), b"stale").unwrap();
    assert!(cache_dir.exists(), "precondition: cache dir seeded");

    // Clearing removes the whole per-project directory.
    clear_disk_caches(project_root).expect("clear must succeed");
    assert!(
        !cache_dir.exists(),
        "clear_disk_caches must remove the per-project cache dir"
    );

    // Idempotent: clearing an already-absent dir is success, not an error.
    clear_disk_caches(project_root).expect("clearing a missing dir is a no-op success");
}

/// Issue #356 removed the `env_vars` section without bumping `CACHE_VERSION`,
/// on the reasoning that a stale key is inert. Nothing else pins that: the
/// version-rejection guard never sees this file, because the version does not
/// change.
///
/// So a cache written by the pre-#356, post-#348 binary — already at
/// `CACHE_VERSION`, still carrying a populated `"env_vars"` object — must load
/// as a normal cache. Not rejected as corruption, not partially dropped: the
/// key is ignored and every surviving section comes back intact.
///
/// The fixture is produced by a real `save()` and then edited as JSON, so the
/// `"env_vars"` shape is grafted onto a genuinely current file rather than a
/// hand-written one that could pass on a parse error.
#[test]
fn a_cache_at_the_current_version_ignores_a_stale_env_vars_key() {
    let temp = TempDir::new().unwrap();
    let project_root = temp.path();

    let mut manager = CacheManager::load(project_root);
    manager.set_laravel_config(CachedLaravelConfig {
        root: project_root.to_path_buf(),
        view_paths: vec![project_root.join("resources/views")],
        ..Default::default()
    });
    let mut vendor_scan = ScanResult::new();
    vendor_scan.middleware.insert(
        "auth".to_string(),
        MiddlewareEntry {
            class: "Illuminate\\Auth\\Middleware\\Authenticate".to_string(),
            class_file: None,
            source_file: Some("bootstrap/app.php".to_string()),
            line: 10,
        },
    );
    manager.set_vendor_scan(vendor_scan);
    manager.save().unwrap();

    // Graft the section the pre-#356 binary wrote back onto the saved file,
    // leaving the version untouched — that is exactly what is already on disk
    // for anyone upgrading across this change.
    let path = manager.cache_path().expect("cache path").to_path_buf();
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        json["version"].as_u64(),
        Some(u64::from(CACHE_VERSION)),
        "precondition: the planted file is at the current version, so the \
         version guard never fires on it"
    );
    json["env_vars"] = serde_json::json!({
        "variables": { "APP_NAME": "Example", "DB_PASSWORD": "" }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    let loaded = CacheManager::load(project_root);
    assert!(
        loaded.has_cached_data(),
        "a stale env_vars key must not be read as corruption — the cache still has data"
    );
    assert_eq!(
        loaded.get_laravel_config().map(|c| c.root.clone()),
        Some(project_root.to_path_buf()),
        "the surviving sections must load intact past the ignored key"
    );
    assert!(
        loaded.get_all_middleware().contains_key("auth"),
        "the surviving sections must load intact past the ignored key"
    );
}
