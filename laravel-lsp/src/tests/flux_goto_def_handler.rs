//! End-to-end coverage for Flux goto-definition (issue #108 — follow-up from
//! Holmes's round-2 review of #60 / PR #99).
//!
//! AC #1 of #60 ("clickable go-to-definition for `<flux:…>` tags") is satisfied
//! in code: a Flux tag routes through `Backend::create_component_location_from_salsa`
//! (`main.rs`), which resolves the tag to a file via `resolve_component_existing_file`
//! and returns a `GotoDefinitionResponse::Link` carrying a `LocationLink`. But it
//! was only covered *below* the handler: `component_candidate_paths` (path
//! construction) and the first-existing-candidate existence check each have
//! tempdir unit tests in `salsa_impl/tests.rs`, yet nothing drove the async
//! handler chain `create_component_location_from_salsa` →
//! `resolve_component_existing_file` → `LocationLink` for a `flux:*` tag. A
//! regression in that handler arm — a dropped `Link` wrap, an inverted
//! existence gate, a `None` returned for a tag that *does* resolve, or a phantom
//! `LocationLink` for one that doesn't — would have slipped through every
//! lower-level test.
//!
//! These tests drive the **real** handler on a server built through the same
//! `tower_lsp::LspService` harness `blade_var_rename_handler.rs` /
//! `folio_rename.rs` use. The config cache and root path are primed directly, so
//! `get_cached_config` short-circuits the (inert) Salsa actor and the chain runs
//! purely against a tempdir: `file_exists_cached` reads real files off disk
//! (the document cache is empty), exactly as it does for the live server.
//!
//! A follow-up (#183) extends the same handler-level coverage to the other
//! resolution paths the chain walks for a `flux:*` tag:
//!   - the **anonymous-component-path** registration form
//!     (`Blade::anonymousComponentPath`), where the registered directory IS the
//!     components directory — contrast the namespace form the first tests use; and
//!   - the **directory-self** candidate `{base}/{last}.blade.php` — the third
//!     file shape `push_component_file_candidates` emits (`salsa_impl.rs`), which
//!     no handler-level test exercised until now.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{ComponentReferenceData, LaravelConfigData};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::LspService;

/// A minimal Flux component file — the markup is irrelevant to resolution
/// (goto-def only resolves the *path*), but a real body keeps the fixture
/// honest.
const FLUX_BLADE: &str = "<button {{ $attributes }}>{{ $slot }}</button>\n";

/// A `LaravelConfigData` rooted at `root` that registers the `flux` anonymous-
/// component namespace at `components/flux` relative to the `resources/views`
/// view path — the registration Flux's service provider makes, and the one AC
/// #1 names. Mirrors `flux_config` in `flux_component_hover.rs`; every other
/// field is empty.
fn flux_config(root: PathBuf) -> LaravelConfigData {
    let mut anonymous_component_namespaces = HashMap::new();
    anonymous_component_namespaces.insert("flux".to_string(), "components/flux".to_string());
    LaravelConfigData {
        root,
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces,
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A `LaravelConfigData` rooted at `root` that registers the `flux` prefix as an
/// *anonymous component path* (`Blade::anonymousComponentPath`) pointing at
/// `flux_dir`. Unlike the namespace form `flux_config` builds, the registered
/// directory IS the components directory, so `<flux:button>` resolves to
/// `{flux_dir}/button.blade.php` directly — no `components/` segment. Mirrors the
/// `anonymous_component_paths` arm of `flux_config` in `flux_component_hover.rs`;
/// every other field is empty.
fn flux_paths_config(root: PathBuf, flux_dir: PathBuf) -> LaravelConfigData {
    let mut anonymous_component_paths = HashMap::new();
    anonymous_component_paths.insert("flux".to_string(), flux_dir);
    LaravelConfigData {
        root,
        view_paths: vec![PathBuf::from("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths,
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// Build a backend for handler-level goto-def tests from an explicit `config`:
/// `LspService::new` wires up a real `Client`, `inner().clone()` hands back the
/// `LaravelLanguageServer`, and we prime the two pieces of state
/// `resolve_component_existing_file` reads — the cached config and the project
/// root. With `cached_config` primed, `get_cached_config` never touches the Salsa
/// actor the harness spawns, so it stays inert. The document cache is left empty,
/// so `file_exists_cached` falls through to a real `metadata` check against the
/// tempdir.
async fn backend_with_config(root: &Path, config: LaravelConfigData) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.cached_config.write().await = Some(config);
    backend
}

/// Build a backend primed with the namespace-form `flux_config` — the
/// registration the first tests in this module exercise.
async fn flux_backend(root: &Path) -> LaravelLanguageServer {
    backend_with_config(root, flux_config(root.to_path_buf())).await
}

/// Write `body` to `relpath` under `dir`, creating parent directories, and
/// return the absolute path written.
fn write_file(dir: &Path, relpath: &str, body: &str) -> PathBuf {
    let full = dir.join(relpath);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(&full, body).unwrap();
    full
}

/// A `ComponentReferenceData` for `name`, the way the parser hands one to the
/// goto-def handler. Only `name` drives resolution; the position fields just
/// shape the returned `origin_selection_range`.
fn flux_ref(name: &str) -> ComponentReferenceData {
    ComponentReferenceData {
        name: name.to_string(),
        tag_name: name.to_string(),
        line: 0,
        column: 0,
        end_column: name.len() as u32,
    }
}

/// Pull the single `LocationLink` out of a goto-def response, asserting the
/// `Link` shape the handler must return for a resolved component.
fn single_link_target(resp: GotoDefinitionResponse) -> PathBuf {
    let links = match resp {
        GotoDefinitionResponse::Link(links) => links,
        other => panic!("expected GotoDefinitionResponse::Link, got {other:?}"),
    };
    assert_eq!(
        links.len(),
        1,
        "exactly one LocationLink for a resolved tag"
    );
    links[0]
        .target_uri
        .to_file_path()
        .expect("target_uri is a file:// URL")
}

#[tokio::test]
async fn flux_tag_resolves_to_location_link() {
    // `<flux:button>` → the anonymous-namespace file
    // `resources/views/components/flux/button.blade.php`. The handler must
    // return a `Link` whose target is exactly that file.
    let dir = TempDir::new().unwrap();
    let expected = write_file(
        dir.path(),
        "resources/views/components/flux/button.blade.php",
        FLUX_BLADE,
    );

    let backend = flux_backend(dir.path()).await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:button"))
        .await
        .expect("a Flux tag with a backing file resolves to a LocationLink");

    assert_eq!(
        single_link_target(resp),
        expected,
        "goto-def lands on the Flux component file under components/flux"
    );
}

#[tokio::test]
async fn flux_dotted_tag_resolves_to_nested_location_link() {
    // A dotted Flux name nests under the namespace directory:
    // `<flux:icon.arrow-right>` → `…/components/flux/icon/arrow-right.blade.php`.
    let dir = TempDir::new().unwrap();
    let expected = write_file(
        dir.path(),
        "resources/views/components/flux/icon/arrow-right.blade.php",
        FLUX_BLADE,
    );

    let backend = flux_backend(dir.path()).await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:icon.arrow-right"))
        .await
        .expect("a dotted Flux tag resolves the dot to a nested directory");

    assert_eq!(
        single_link_target(resp),
        expected,
        "the dotted segment maps to a nested path under components/flux"
    );
}

#[tokio::test]
async fn flux_dotted_tag_resolves_via_index_convention() {
    // Flux also backs a dotted tag with an `index.blade.php` inside the named
    // directory. With no `arrow-right.blade.php` present, the handler must still
    // resolve `<flux:icon.arrow-right>` to
    // `…/components/flux/icon/arrow-right/index.blade.php`.
    let dir = TempDir::new().unwrap();
    let expected = write_file(
        dir.path(),
        "resources/views/components/flux/icon/arrow-right/index.blade.php",
        FLUX_BLADE,
    );

    let backend = flux_backend(dir.path()).await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:icon.arrow-right"))
        .await
        .expect("the index.blade.php convention backs a dotted Flux tag");

    assert_eq!(
        single_link_target(resp),
        expected,
        "the directory's index.blade.php resolves when no direct file exists"
    );
}

#[tokio::test]
async fn unresolvable_flux_tag_returns_none() {
    // No file exists at any candidate path for `<flux:ghost>`. The handler must
    // return `None` — never a phantom `LocationLink` pointing at a missing file.
    let dir = TempDir::new().unwrap();
    // A real but unrelated Flux component, to prove the `None` is about the
    // missing target and not an empty/unconfigured project.
    write_file(
        dir.path(),
        "resources/views/components/flux/button.blade.php",
        FLUX_BLADE,
    );

    let backend = flux_backend(dir.path()).await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:ghost"))
        .await;

    assert!(
        resp.is_none(),
        "an unresolvable Flux tag yields no LocationLink, got {resp:?}"
    );
}

#[tokio::test]
async fn flux_tag_resolves_via_anonymous_component_path() {
    // The *other* registration form: `Blade::anonymousComponentPath` registers
    // the `flux` prefix against a directory that IS the components dir, so
    // `<flux:button>` resolves to `{flux_dir}/button.blade.php` directly — no
    // `components/` segment (contrast `flux_tag_resolves_to_location_link`, which
    // drives the namespace form). The registered directory sits *outside* Flux's
    // conventional fallback locations (`resources/views/flux`,
    // `vendor/livewire/flux…`), so only the anonymous-component-path branch can
    // produce this candidate — the assertion can't pass by coincidence.
    let dir = TempDir::new().unwrap();
    let expected = write_file(
        dir.path(),
        "packages/flux/resources/views/button.blade.php",
        FLUX_BLADE,
    );
    let flux_dir = dir.path().join("packages/flux/resources/views");

    let backend = backend_with_config(
        dir.path(),
        flux_paths_config(dir.path().to_path_buf(), flux_dir),
    )
    .await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:button"))
        .await
        .expect("a Flux tag backed by an anonymous-component-path registration resolves");

    assert_eq!(
        single_link_target(resp),
        expected,
        "goto-def lands on the file under the registered anonymous-component path"
    );
}

#[tokio::test]
async fn flux_tag_resolves_via_self_named_candidate() {
    // The third file shape `push_component_file_candidates` emits is the
    // directory-self convention `{base}/{last}.blade.php` (`salsa_impl.rs`):
    // `<flux:button>` → `…/components/flux/button/button.blade.php`. With neither
    // the direct file (`…/flux/button.blade.php`) nor the directory index
    // (`…/flux/button/index.blade.php`) present, the handler must fall through to
    // this self-named file. Uses the namespace registration form
    // (`flux_config`/`flux_backend`), same as the first three tests.
    let dir = TempDir::new().unwrap();
    let expected = write_file(
        dir.path(),
        "resources/views/components/flux/button/button.blade.php",
        FLUX_BLADE,
    );

    let backend = flux_backend(dir.path()).await;
    let resp = backend
        .create_component_location_from_salsa(&flux_ref("flux:button"))
        .await
        .expect(
            "the directory-self convention backs a Flux tag when no direct or index file exists",
        );

    assert_eq!(
        single_link_target(resp),
        expected,
        "the self-named {{base}}/{{last}}.blade.php candidate resolves when the direct and index files are absent"
    );
}
