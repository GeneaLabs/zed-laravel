//! Equivalence of the off-actor batch pre-parse to the actor's own parse
//! (issue #373, item 1).
//!
//! `run_magic_batch_once` used to query `get_patterns` per file in a serial
//! loop, which parsed each file *inside* the single Salsa actor thread. It now
//! parses the whole batch outside the actor first
//! (`Backend::preparse_batch_off_actor`) and hands the actor finished results
//! through `bulk_import_patterns` + `bulk_import_hierarchy`, so those queries
//! become cache hits.
//!
//! That is only a speed change if the two producers agree. Two of them have to:
//!
//! * `pattern_indexer::parse_owned_with_hierarchy` (off-actor) must produce the
//!   same `ParsedPatternsData` the actor's `handle_get_patterns` produces, or a
//!   watched change would index a file differently from a save;
//! * `bulk_import_hierarchy` must leave `file_class_surfaces` reporting what an
//!   in-actor parse leaves it reporting, or the batch's surface diff — the
//!   thing that decides which dependents get rippled — would fire on files that
//!   did not change, or miss files that did.
//!
//! Each test below drives two independent actors over the same fixture, one per
//! route, and compares them. `Backend` is private to `main.rs`, so these
//! exercise the seam the pre-parse writes through rather than the method
//! itself.

use laravel_lsp::salsa_impl::SalsaHandle;
use std::fs;
use std::path::{Path, PathBuf};

/// A model with a base class, a trait and an interface, so the hierarchy has
/// every edge kind `insert_file` records — a fixture that declares only a bare
/// class would pass even if `extends` / `implements` / trait edges were dropped.
const MODEL: &str = r#"<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Contracts\Auth\Authenticatable;
use App\Concerns\HasTeams;

class User extends Model implements Authenticatable
{
    use HasTeams;

    protected $fillable = ['name', 'email'];

    public function teams()
    {
        return view('users.teams');
    }
}
"#;

fn write(path: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

/// A registered handle, which is what publishes the shared pattern cache that
/// `bulk_import_patterns` writes into. Without registration that call fails
/// with "pattern cache not published", which is the state the production code
/// treats as "fall back to parsing in the actor".
async fn handle_for(root: &Path) -> SalsaHandle {
    let handle = laravel_lsp::salsa_impl::SalsaActor::spawn();
    handle
        .register_config_files(root.to_path_buf(), None, None, None, None)
        .await
        .expect("actor registers the tempdir project root");
    handle
        .register_project_files(
            root.to_path_buf(),
            vec![PathBuf::from("app")],
            vec![root.join("resources/views")],
            None,
            PathBuf::from("routes"),
            laravel_lsp::vendor_index::VendorIndex::build(root)
                .files()
                .iter()
                .map(|f| f.path.clone())
                .collect(),
        )
        .await
        .expect("actor registers the tempdir project");
    handle
}

/// Parse off the actor exactly as `preparse_batch_off_actor` does, then import
/// both halves — the production sequence, minus the `JoinSet` that only decides
/// how many run at once.
async fn import_off_actor(handle: &SalsaHandle, path: &Path) {
    let text = fs::read_to_string(path).expect("fixture is readable");
    let (data, nodes) = laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(path, &text);
    handle
        .bulk_import_patterns(vec![(path.to_path_buf(), data)])
        .await
        .expect("pattern cache is published for a registered project");
    handle
        .bulk_import_hierarchy(vec![(path.to_path_buf(), nodes)])
        .await
        .expect("hierarchy import round-trips the actor");
}

#[tokio::test]
async fn the_off_actor_import_leaves_the_same_class_surfaces_as_an_actor_parse() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let model = write(&root.join("app/Models/User.php"), MODEL);

    // Route A: the actor parses the file itself, which is what the serial loop
    // did before this change.
    let in_actor = handle_for(root).await;
    in_actor
        .get_patterns(model.clone())
        .await
        .expect("actor answers")
        .expect("the model parses");
    let actor_surfaces = in_actor
        .file_class_surfaces(model.clone())
        .await
        .expect("actor answers");

    // Route B: parsed outside and imported, which is what the batch does now.
    let off_actor = handle_for(root).await;
    import_off_actor(&off_actor, &model).await;
    let imported_surfaces = off_actor
        .file_class_surfaces(model.clone())
        .await
        .expect("actor answers");

    assert!(
        !actor_surfaces.is_empty(),
        "the fixture must declare a class, or this test proves nothing"
    );
    assert_eq!(
        actor_surfaces, imported_surfaces,
        "the batch's surface diff decides which dependents ripple — an \
         off-actor parse that surfaces different signatures would ripple the \
         wrong files"
    );
}

#[tokio::test]
async fn a_file_with_no_class_surfaces_empty_through_both_routes() {
    // `insert_file` early-returns on an empty node list while the actor's miss
    // path skips the call entirely. The two are only equivalent because of that
    // early return, so pin it: a plain function file must surface nothing
    // either way, and must not leave a phantom entry behind.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let helper = write(
        &root.join("app/helpers.php"),
        "<?php\n\nfunction app_path_helper() { return config('app.name'); }\n",
    );

    let in_actor = handle_for(root).await;
    in_actor.get_patterns(helper.clone()).await.unwrap();
    let actor_surfaces = in_actor.file_class_surfaces(helper.clone()).await.unwrap();

    let off_actor = handle_for(root).await;
    import_off_actor(&off_actor, &helper).await;
    let imported_surfaces = off_actor.file_class_surfaces(helper.clone()).await.unwrap();

    assert!(actor_surfaces.is_empty());
    assert_eq!(actor_surfaces, imported_surfaces);
}

#[tokio::test]
async fn the_off_actor_parse_extracts_what_the_actor_parse_extracts() {
    // Compared WITHOUT going through the import, deliberately. Importing first
    // and then reading back through `get_patterns` cannot fail: a dropped
    // import is a cache miss, the actor parses the file itself, and the test
    // ends up comparing the actor's parse to the actor's parse. The claim that
    // needs pinning is that the two PRODUCERS agree, so both are called
    // directly.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let model = write(&root.join("app/Models/User.php"), MODEL);

    let in_actor = handle_for(root).await;
    let actor_parsed = in_actor
        .get_patterns(model.clone())
        .await
        .unwrap()
        .expect("the model parses");

    let text = fs::read_to_string(&model).unwrap();
    let (off_actor_parsed, _) =
        laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(&model, &text);

    // `view('users.teams')` is the observable pattern in the fixture. Comparing
    // name AND position catches both a missing extraction and a shifted column,
    // and position is what hover and go-to-definition navigate on — an imported
    // entry is served to them verbatim, so a shifted column is a wrong jump.
    let extract = |data: &laravel_lsp::salsa_impl::ParsedPatternsData| -> Vec<_> {
        data.views
            .iter()
            .map(|v| (v.name.clone(), v.line, v.column, v.end_column))
            .collect()
    };

    let actor_views = extract(&actor_parsed);
    assert!(
        !actor_views.is_empty(),
        "the fixture must contain a view() call, or this test proves nothing"
    );
    assert_eq!(actor_views, extract(&off_actor_parsed));
}

#[tokio::test]
async fn an_imported_entry_is_served_without_the_actor_reparsing() {
    // The import is only a saving if the actor SERVES it. Proven by importing
    // patterns for a path whose file no longer exists: the actor's miss path
    // would have to read it from disk and would fail, so an answer here can
    // only have come from the imported entry.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let model = write(&root.join("app/Models/User.php"), MODEL);

    let handle = handle_for(root).await;
    let text = fs::read_to_string(&model).unwrap();
    let (data, _) = laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(&model, &text);
    let expected_views = data.views.len();
    handle
        .bulk_import_patterns(vec![(model.clone(), data)])
        .await
        .expect("pattern cache is published for a registered project");

    fs::remove_file(&model).expect("fixture is removable");

    let served = handle
        .get_patterns(model)
        .await
        .unwrap()
        .expect("the imported entry answers for a file that is no longer on disk");

    assert!(expected_views > 0, "the fixture must contain a view() call");
    assert_eq!(served.views.len(), expected_views);
}

#[tokio::test]
async fn an_unregistered_actor_refuses_the_pattern_import() {
    // The pre-parse gives up when this fails, leaving the serial pass to parse
    // in the actor. That fallback is only reachable if the failure is real, so
    // pin that an unregistered actor rejects the import rather than silently
    // dropping the entries.
    let dir = tempfile::tempdir().unwrap();
    let model = write(&dir.path().join("app/Models/User.php"), MODEL);

    let bare = laravel_lsp::salsa_impl::SalsaActor::spawn();
    let text = fs::read_to_string(&model).unwrap();
    let (data, _) = laravel_lsp::pattern_indexer::parse_owned_with_hierarchy(&model, &text);

    assert!(
        bare.bulk_import_patterns(vec![(model, data)])
            .await
            .is_err(),
        "no project registered means no published pattern cache to import into"
    );
}
