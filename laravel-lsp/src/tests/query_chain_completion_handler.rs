//! End-to-end coverage for the relationship-hop wiring in the *completion
//! handler* (issue #219, follow-up to #211 / PR #215).
//!
//! The unit tests in `query_chain::eloquent_completion::tests` exercise
//! `apply_relation_method_hops` + `columns_for_collection` directly with a
//! hand-built `ChainContext`, and the diagnostics path is covered end-to-end
//! through `chain_diagnostics`. What was *not* covered is the glue in
//! `main.rs::try_query_chain_completion` that dispatches
//! `apply_relation_method_hops` inside the real `textDocument/completion`
//! flow. A bad re-wire of that block (wrong call order, missing dispatch, lost
//! `effective_model`) would slip past every existing test.
//!
//! These tests drive the real completion entry point with the property-access
//! receiver shape `$user->competitions->where('|')`, mirroring the
//! diagnostics-side end-to-end fixtures added in PR #215. They build a genuine
//! `LaravelLanguageServer` backend, prime its `initialized_root` with a tempdir
//! of `User`/`Competition` model files and its `database_schema` with a seeded
//! provider where `type` lives on `competitions` but not on `users`, then
//! assert the related table's columns are offered. Commenting out the
//! `apply_relation_method_hops` dispatch block in `main.rs` makes both tests
//! fail — they are regression guards for the wiring, not coverage theatre.

use crate::LaravelLanguageServer;
use laravel_lsp::database::{DatabaseSchema, DatabaseSchemaProvider};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use tempfile::TempDir;
use tower_lsp::lsp_types::{Position, Url};
use tower_lsp::LspService;

/// A `User` model with a single `competitions` hasMany relation — the issue
/// #211 shape, where the relation's table carries columns the parent's doesn't.
const USER_WITH_COMPETITIONS: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function competitions() { return $this->hasMany(Competition::class); }
}
"#;

/// The related `Competition` model. Empty body — its only job is to exist on
/// disk so the relation hop resolves `User::competitions` → `Competition`.
const COMPETITION_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Competition extends Model {
}
"#;

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can call
/// its private `try_query_chain_completion` directly and prime its fields.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Tempdir with `User.php` + `Competition.php` under the standard Laravel
/// `app/Models/` location. Returns the tempdir (hold it to keep the dir alive)
/// and the project root.
fn project_with_models(models: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let models_dir = dir.path().join("app").join("Models");
    std::fs::create_dir_all(&models_dir).expect("create models dir");
    for (name, body) in models {
        std::fs::write(models_dir.join(format!("{name}.php")), body).expect("write model");
    }
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// Seed a `DatabaseSchemaProvider` directly from in-memory fixtures — no live
/// DB. Uses the `set_test_schema` seam (reachable cross-crate from this bin
/// test, hence `#[doc(hidden)]` rather than `#[cfg(test)]` on the lib side).
async fn provider_with(
    root: PathBuf,
    tables: &[(&str, &[(&str, &str)])],
) -> DatabaseSchemaProvider {
    let mut columns = HashMap::new();
    let mut columns_with_types = HashMap::new();
    let mut table_names = Vec::new();
    for (table, cols) in tables {
        table_names.push(table.to_string());
        columns.insert(
            table.to_string(),
            cols.iter().map(|(n, _)| n.to_string()).collect(),
        );
        columns_with_types.insert(
            table.to_string(),
            cols.iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
        );
    }
    let schema = DatabaseSchema {
        tables: table_names,
        columns,
        columns_with_types,
        cached_at: Instant::now(),
    };
    let provider = DatabaseSchemaProvider::new(root);
    provider.set_test_schema(schema).await;
    provider
}

/// Build a backend primed for the issue #211 fixtures: `User`/`Competition`
/// models on disk, and a schema where `type` lives on `competitions` but not on
/// `users` (which only carries `email`). The asymmetry is what lets the tests
/// prove the hop advanced `effective_model` from `User` to `Competition`.
async fn competitions_backend() -> (TempDir, LaravelLanguageServer) {
    let (dir, root) = project_with_models(&[
        ("User", USER_WITH_COMPETITIONS),
        ("Competition", COMPETITION_MODEL),
    ]);
    let db = provider_with(
        root.clone(),
        &[
            ("users", &[("id", "int"), ("email", "string")]),
            ("competitions", &[("id", "int"), ("type", "string")]),
        ],
    )
    .await;

    let server = test_server();
    *server.initialized_root.write().await = Some(root);
    *server.database_schema.write().await = Some(db);
    (dir, server)
}

/// Build `(content, cursor position)` from a template carrying a single `◊`
/// marker at the desired cursor. The marker is stripped from the returned
/// content; the position is its 0-based line + code-point column, which is what
/// `try_query_chain_completion` expects.
fn cursor_at(template: &str) -> (String, Position) {
    const MARK: char = '◊';
    let byte_idx = template
        .find(MARK)
        .expect("template must contain the ◊ cursor marker");
    let before = &template[..byte_idx];
    let line = before.matches('\n').count() as u32;
    let character = before.rsplit('\n').next().unwrap_or(before).chars().count() as u32;
    (template.replace(MARK, ""), Position { line, character })
}

/// `$user->competitions->where('|')` — `$user` typed as `User` via the `@var`
/// docblock, the cursor inside the (empty) `where` column argument. This is the
/// shape that, after the relation hop, must complete the *competitions* table's
/// columns.
const PROPERTY_RECEIVER_HOP: &str = "<?php\n\
     use App\\Models\\User;\n\
     /** @var User $user */\n\
     $user->competitions->where('◊')->get();\n";

/// Run the real completion entry point and return the offered labels.
async fn completion_labels(server: &LaravelLanguageServer, template: &str) -> Vec<String> {
    let (content, position) = cursor_at(template);
    let uri = Url::from_file_path("/tmp/watson-219-fixture/Controller.php")
        .expect("absolute path → file uri");
    let items = server
        .try_query_chain_completion(&content, position, &uri)
        .await
        .expect("completion should resolve a chain-completion result, not None");
    items.into_iter().map(|i| i.label).collect()
}

#[tokio::test]
async fn property_receiver_hop_offers_related_table_columns() {
    // `$user->competitions->where('|')` resolves to a Collection over
    // Competition once the relation hop advances `effective_model` from User to
    // Competition. The completion must then offer the *competitions* table's
    // columns. `type` lives only on `competitions`, so its presence proves the
    // hop fired through the real `main.rs` dispatch. With the dispatch block
    // commented out, `effective_model` stays `User`, the users table is queried,
    // and `type` never appears — this assertion fails, as a regression guard
    // should.
    let (_dir, server) = competitions_backend().await;
    let labels = completion_labels(&server, PROPERTY_RECEIVER_HOP).await;

    assert!(
        labels.iter().any(|l| l == "type"),
        "completion after the relation hop must offer the competitions column \
         `type`; got {labels:?}"
    );
}

#[tokio::test]
async fn property_receiver_hop_excludes_parent_only_columns() {
    // The mirror assertion: `email` lives only on `users`. Once the hop advances
    // `effective_model` to Competition, the users table is no longer the one
    // being completed, so `email` must NOT appear. If the
    // `apply_relation_method_hops` dispatch block is commented out, the model
    // stays `User`, the users table is queried, and `email` leaks into the
    // results — this assertion fails, proving it guards the wiring rather than
    // just exercising it.
    let (_dir, server) = competitions_backend().await;
    let labels = completion_labels(&server, PROPERTY_RECEIVER_HOP).await;

    assert!(
        !labels.iter().any(|l| l == "email"),
        "a column present only on the parent `users` table must not survive the \
         hop to Competition; `email` leaked: {labels:?}"
    );
}
