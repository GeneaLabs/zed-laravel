//! End-to-end coverage for factory goto-definition through the real Backend
//! handler `LaravelLanguageServer::create_magic_member_location` (issue #30
//! item 3).
//!
//! The actor-side classification (`resolve_and_classify` → `kind == Factory`
//! with the resolved factory FQCN) is covered in `member_resolver/tests.rs`.
//! What this covers is the goto-target *producer*: the
//! `MagicMemberKind::Factory | Pivot` arm of `create_magic_member_location`
//! in `main.rs`, which must land on the FACTORY CLASS's declaration line —
//! there is no member-named method to narrow to (`factory()` is vendor-trait
//! magic). Mirrors `macro_goto_def_handler.rs`: prime the live Salsa actor
//! exactly as the server does during indexing, then drive the real handler.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{AccessForm, Confidence, MemberAccessReferenceData};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::LspService;

/// Build a server instance for testing (same shape as the macro handler test).
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

const MODEL_SRC: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#;

/// The factory class declaration sits on line 3 (0-based): `<?php`=0,
/// `namespace`=1, `use Factory`=2, `class UserFactory`=3.
const FACTORY_SRC: &str = r#"<?php
namespace Database\Factories;
use Illuminate\Database\Eloquent\Factories\Factory;
class UserFactory extends Factory {
    public function suspended(): static { return $this->state(['active' => false]); }
}
"#;

const CALLER_SRC: &str = r#"<?php
namespace Database\Seeders;
use App\Models\User;
class UserSeeder {
    public function run(): void { User::factory()->create(); }
}
"#;

/// Find the (line, column) of `needle` in `src`, 0-based — repo convention.
fn position_of(src: &str, needle: &str) -> (u32, u32) {
    for (row, line) in src.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return (row as u32, col as u32);
        }
    }
    panic!("{needle} not found in fixture");
}

/// A `MemberAccessReferenceData` for the `User::factory()` call site. The
/// handler re-resolves from the file's parsed patterns using `line`/`column`,
/// so only the member position drives resolution.
fn factory_ref() -> MemberAccessReferenceData {
    let (line, column) = position_of(CALLER_SRC, "factory");
    let receiver_byte_start = CALLER_SRC.find("User::factory").expect("call in fixture");
    MemberAccessReferenceData {
        member: "factory".to_string(),
        receiver: "User".to_string(),
        receiver_byte_start,
        receiver_byte_end: receiver_byte_start + "User".len(),
        is_nullsafe: false,
        form: AccessForm::StaticCall,
        line,
        column,
        end_column: column + "factory".len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    }
}

/// Prime `server`'s live Salsa actor with a project containing the model, its
/// conventional factory, and the caller — registering each file and forcing
/// its parse so the class-hierarchy index knows the factory's declaration
/// line, exactly as indexing would. Returns the caller path.
async fn prime_factory_project(server: &LaravelLanguageServer, root: &Path) -> PathBuf {
    let files = [
        (root.join("app/Models/User.php"), MODEL_SRC),
        (root.join("database/factories/UserFactory.php"), FACTORY_SRC),
        (root.join("database/seeders/UserSeeder.php"), CALLER_SRC),
    ];
    std::fs::write(
        root.join("composer.json"),
        r#"{
  "autoload": { "psr-4": { "App\\": "app/" } },
  "autoload-dev": { "psr-4": { "Database\\Factories\\": "database/factories/" } }
}"#,
    )
    .unwrap();

    server
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .unwrap();
    for (path, src) in &files {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
        server
            .salsa
            .update_file(path.clone(), 1, src.to_string())
            .await
            .unwrap();
        server.salsa.get_patterns(path.clone()).await.unwrap();
    }
    files[2].0.clone()
}

/// Pull the single `LocationLink` out of a goto-def response.
fn single_link(resp: GotoDefinitionResponse) -> tower_lsp::lsp_types::LocationLink {
    let links = match resp {
        GotoDefinitionResponse::Link(links) => links,
        other => panic!("expected GotoDefinitionResponse::Link, got {other:?}"),
    };
    assert_eq!(
        links.len(),
        1,
        "exactly one LocationLink for a resolved factory"
    );
    links.into_iter().next().unwrap()
}

#[tokio::test]
async fn factory_call_goto_lands_on_factory_class_line() {
    // `User::factory()` → the conventional `UserFactory` class declaration.
    // The handler must return a `Link` targeting the factory file at the
    // class's line (3, 0-based) with a zero-width caret — no `factory`
    // method exists on the factory class to narrow to.
    let dir = TempDir::new().unwrap();
    let server = test_server();
    let caller = prime_factory_project(&server, dir.path()).await;
    let factory = dir.path().join("database/factories/UserFactory.php");

    let resp = server
        .create_magic_member_location(&caller, &factory_ref())
        .await
        .expect("a model with a conventional factory resolves to a goto Link");

    let link = single_link(resp);
    assert_eq!(
        link.target_uri.to_file_path().expect("file:// target"),
        factory,
        "goto lands on the factory class file"
    );
    assert_eq!(
        link.target_range.start.line, 3,
        "goto lands on the factory class's 0-based declaration line"
    );
    assert_eq!(
        link.target_range.start.character, 0,
        "zero-width caret — no member-named method on the factory to narrow to"
    );
}

#[tokio::test]
async fn factory_call_without_factory_file_does_not_resolve() {
    // Same project WITHOUT the factory file: the conventional candidate
    // doesn't resolve, nothing else claims `factory`, so the handler degrades
    // to `None` — no panic, no bogus location.
    let dir = TempDir::new().unwrap();
    let server = test_server();
    let root = dir.path();
    let model = root.join("app/Models/User.php");
    let caller = root.join("database/seeders/UserSeeder.php");
    std::fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();

    server
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .unwrap();
    for (path, src) in [(&model, MODEL_SRC), (&caller, CALLER_SRC)] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
        server
            .salsa
            .update_file(path.clone(), 1, src.to_string())
            .await
            .unwrap();
        server.salsa.get_patterns(path.clone()).await.unwrap();
    }

    let resp = server
        .create_magic_member_location(&caller, &factory_ref())
        .await;
    assert!(
        resp.is_none(),
        "a model with no factory must not produce a goto target; got {resp:?}"
    );
}
