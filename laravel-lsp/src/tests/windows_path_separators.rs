//! Path matching that assumed a forward-slash separator.
//!
//! Three sites matched literal `"/components/"` or
//! `"resources/views/components/"` against paths that arrive as **strings**,
//! not `Path`s. On Windows those carry `\`, so the markers never fired and the
//! component-variable inference silently produced nothing. One site was worse:
//! it built its marker as
//! `format!("{}resources/views/components/", MAIN_SEPARATOR)`, mixing a
//! platform separator with hardcoded forward slashes — on Windows that is
//! `\resources/views/components/`, which matches no real path anywhere
//! (issue #292).
//!
//! These are pure string functions, so a Windows-shaped path exercises the fix
//! on every platform — no `#[cfg(windows)]`, and the Linux and macOS CI legs
//! catch a regression too.

use crate::LaravelLanguageServer;

const WINDOWS: &str = r"C:\Users\dev\proj\resources\views\components\button.blade.php";
const UNIX: &str = "/home/dev/proj/resources/views/components/button.blade.php";

#[test]
fn component_files_are_recognised_with_either_separator() {
    assert!(
        LaravelLanguageServer::is_component_file(UNIX),
        "forward-slash path must be recognised"
    );
    assert!(
        LaravelLanguageServer::is_component_file(WINDOWS),
        "backslash path must be recognised — this is what Windows actually passes"
    );
}

#[test]
fn non_component_blade_files_are_rejected_with_either_separator() {
    assert!(!LaravelLanguageServer::is_component_file(
        "/home/dev/proj/resources/views/welcome.blade.php"
    ));
    assert!(!LaravelLanguageServer::is_component_file(
        r"C:\Users\dev\proj\resources\views\welcome.blade.php"
    ));
}

#[test]
fn a_component_path_is_not_matched_when_it_is_not_a_blade_file() {
    assert!(!LaravelLanguageServer::is_component_file(
        r"C:\Users\dev\proj\resources\views\components\button.php"
    ));
}

#[test]
fn separator_normalization_leaves_forward_slash_paths_alone() {
    assert_eq!(LaravelLanguageServer::with_forward_slashes(UNIX), UNIX);
    assert_eq!(
        LaravelLanguageServer::with_forward_slashes(WINDOWS),
        "C:/Users/dev/proj/resources/views/components/button.blade.php"
    );
}

/// The marker site: a backslash path must reach the component-name extraction
/// rather than bailing out at the guard. An empty result here would be
/// indistinguishable from "component class not found", so the assertion is on
/// the guard's own behaviour via the normalizer that feeds it.
#[test]
fn the_components_marker_matches_a_backslash_path() {
    let normalized = LaravelLanguageServer::with_forward_slashes(WINDOWS);
    assert!(
        normalized.contains("resources/views/components/"),
        "the normalized Windows path must clear the marker guard: {normalized}"
    );
    let idx = normalized
        .find("components/")
        .expect("component segment must be locatable");
    assert_eq!(
        &normalized[idx + "components/".len()..],
        "button.blade.php",
        "the offset must skip exactly the marker, leaving the component path"
    );
}

// ---------------------------------------------------------------------------
// Shipped-code separator assumptions found by sweeping for the same bug class
// ---------------------------------------------------------------------------

/// `execute_salsa_update` classifies an edited file by matching forward-slash
/// markers against `path.to_string_lossy()` — a real OS path. On Windows that
/// carries backslashes, so a service provider and a config file both fell
/// through to the default branch and were never re-registered with Salsa:
/// middleware aliases, container bindings and config values stopped updating
/// for the rest of the session, silently.
#[test]
fn salsa_dispatch_markers_match_a_windows_path() {
    let provider = r"C:\Users\dev\proj\app\Providers\AppServiceProvider.php";
    let config = r"C:\Users\dev\proj\config\app.php";

    assert!(
        LaravelLanguageServer::with_forward_slashes(provider).contains("app/Providers"),
        "a Windows provider path must reach the ServiceProviderFile branch"
    );
    assert!(
        LaravelLanguageServer::with_forward_slashes(config).contains("/config/"),
        "a Windows config path must reach the ConfigFile branch"
    );
}

/// The Unix spellings must keep matching — the normalization is additive.
#[test]
fn salsa_dispatch_markers_still_match_a_unix_path() {
    assert!(LaravelLanguageServer::with_forward_slashes(
        "/home/dev/proj/app/Providers/AppServiceProvider.php"
    )
    .contains("app/Providers"));
    assert!(
        LaravelLanguageServer::with_forward_slashes("/home/dev/proj/config/app.php")
            .contains("/config/")
    );
}

/// A path that is neither must still fall through to the default branch.
#[test]
fn salsa_dispatch_markers_reject_unrelated_paths() {
    let normalized =
        LaravelLanguageServer::with_forward_slashes(r"C:\Users\dev\proj\app\Models\User.php");
    assert!(!normalized.contains("app/Providers"));
    assert!(!normalized.contains("/config/"));
}
