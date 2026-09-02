use super::*;

#[test]
fn detects_v4_with_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v4.0.3" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn detects_v3_with_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v3.5.18" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V3);
}

#[test]
fn detects_v4_without_v_prefix() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "4.0.3" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn detects_compact_json_spacing() {
    let lock = r#"{"packages":[{"name":"livewire/livewire","version":"v4.0.0"}]}"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn unknown_when_package_missing() {
    let lock = r#"{
        "packages": [
            { "name": "laravel/framework", "version": "v12.0.0" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn unknown_for_malformed_version() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "dev-main" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn picks_livewire_version_not_neighboring_package() {
    // Defensive: when livewire is sandwiched between other packages, the
    // resolver must read the version field belonging to livewire's object,
    // not one of the neighbors. The 500-byte lookahead window keeps us
    // inside the same JSON object as long as `name` and `version` are close
    // — which is the composer.lock convention.
    let lock = r#"{
        "packages": [
            { "name": "laravel/framework", "version": "v12.5.0" },
            { "name": "livewire/livewire", "version": "v4.0.3" },
            { "name": "laravel/prompts", "version": "v0.3.0" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::V4);
}

#[test]
fn unknown_for_v2_or_unrecognized_major() {
    let lock = r#"{
        "packages": [
            { "name": "livewire/livewire", "version": "v2.12.6" }
        ]
    }"#;
    assert_eq!(detect_from_composer_lock(lock), LivewireVersion::Unknown);
}

#[test]
fn unknown_for_empty_input() {
    assert_eq!(detect_from_composer_lock(""), LivewireVersion::Unknown);
}

// === Installation detection (transitive-dependency defect) ==================

#[test]
fn is_installed_finds_livewire_in_packages() {
    let lock = r#"{ "packages": [
        { "name": "livewire/flux", "version": "v2.9.1" },
        { "name": "livewire/livewire", "version": "v3.6.4" }
    ], "packages-dev": [] }"#;
    assert!(
        is_installed(lock),
        "a transitively-installed Livewire is still installed"
    );
}

#[test]
fn is_installed_finds_livewire_in_packages_dev() {
    // The claim made in `is_installed`'s docs: the whole lock is searched, so
    // `packages-dev` counts. A dev-only Livewire still writes to `vendor/` and
    // still has components under `app/Livewire` worth completing.
    let lock = r#"{ "packages": [
        { "name": "laravel/framework", "version": "v12.0.1" }
    ], "packages-dev": [
        { "name": "livewire/livewire", "version": "v3.6.4" }
    ] }"#;
    assert!(is_installed(lock), "a dev-only Livewire is still installed");
}

#[test]
fn is_installed_is_false_without_livewire() {
    let lock = r#"{ "packages": [
        { "name": "laravel/framework", "version": "v12.0.1" },
        { "name": "livewire/flux", "version": "v2.9.1" }
    ], "packages-dev": [] }"#;
    assert!(
        !is_installed(lock),
        "livewire/flux is not livewire/livewire — a prefix match here would \
         report Livewire installed for any package under the livewire/ vendor"
    );
}

#[test]
fn is_installed_is_false_for_an_empty_lock() {
    assert!(!is_installed(""));
    assert!(!is_installed("{}"));
}

#[test]
fn is_installed_tolerates_compact_json() {
    // `composer.lock` is pretty-printed by Composer, but a lock that has been
    // through a formatter or a tool must not silently read as "not installed".
    let lock = r#"{"packages":[{"name":"livewire/livewire","version":"v3.6.4"}]}"#;
    assert!(is_installed(lock));
}
