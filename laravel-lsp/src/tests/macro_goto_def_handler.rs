//! End-to-end coverage for macro goto-definition through the real Backend
//! handler `LaravelLanguageServer::create_magic_member_location` (the headline
//! macro feature on `feat/facade-resolution`).
//!
//! The actor-side classification (`resolve_magic_member_at` → `kind == Macro`
//! with decl_file/decl_line from the macro registry) is covered in
//! `salsa_impl/tests.rs`. What was NOT covered is the goto-target *producer*:
//! the `MagicMemberKind::Macro` arm of `create_magic_member_location` in
//! `main.rs`, which turns the resolved `MagicMemberHoverData` into the actual
//! `GotoDefinitionResponse::Link` the editor jumps to. A regression there — a
//! dropped `Link` wrap, the wrong line, or a `None` for a macro that *does*
//! resolve — would slip past every lower-level test.
//!
//! Unlike the Flux/view goto tests, the macro path has no `cached_config`
//! short-circuit: `create_magic_member_location` always goes through the
//! Backend's live Salsa actor. So these tests prime that actor directly
//! (registering config + provider source + the caller file, exactly as the
//! server does during indexing) and then drive the real handler.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::{AccessForm, Confidence, MemberAccessReferenceData};
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::GotoDefinitionResponse;
use tower_lsp::LspService;

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so we can prime
/// its (live) Salsa actor and call its private `create_magic_member_location`.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A provider registering `Str::macro('uuid7', fn () => …)`. The closure sits on
/// line 6 (0-based): `<?php`=0, `namespace`=1, `use Str`=2, `use SP`=3,
/// `class`=4, `boot`=5, `Str::macro(...)`=6.
const PROVIDER_SRC: &str = r#"<?php
namespace App\Providers;
use Illuminate\Support\Str;
use Illuminate\Support\ServiceProvider;
class AppServiceProvider extends ServiceProvider {
    public function boot(): void {
        Str::macro('uuid7', fn () => 'x');
    }
}
"#;

/// A caller invoking the registered macro. `Str` is imported so it qualifies to
/// the framework Macroable host.
const CALLER_SRC: &str = r#"<?php
namespace App\Support;
use Illuminate\Support\Str;
class Ids {
    public function make(): string { return Str::uuid7(); }
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

/// Byte range of `needle` in `src` — for the receiver byte range the handler
/// re-locates in the live tree.
fn byte_range_of(src: &str, needle: &str) -> (usize, usize) {
    let start = src.find(needle).expect("needle in fixture");
    (start, start + needle.len())
}

/// A `MemberAccessReferenceData` for the `Str::uuid7()` call site. The handler
/// re-resolves from the file's parsed patterns using `line`/`column`, so only
/// the member position drives resolution; the receiver byte range is filled for
/// completeness, mirroring what the parser emits.
fn uuid7_ref() -> MemberAccessReferenceData {
    let (line, column) = position_of(CALLER_SRC, "uuid7");
    let (receiver_byte_start, receiver_byte_end) = byte_range_of(CALLER_SRC, "Str::uuid7");
    MemberAccessReferenceData {
        member: "uuid7".to_string(),
        receiver: "Str".to_string(),
        receiver_byte_start,
        receiver_byte_end,
        is_nullsafe: false,
        form: AccessForm::StaticCall,
        line,
        column,
        end_column: column + "uuid7".len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    }
}

/// Prime `server`'s live Salsa actor with the config, the provider source (so
/// the macro registry knows `Str::uuid7`), and the caller file — exactly the
/// registration the server performs while indexing. Returns the caller path.
async fn prime_macro_project(server: &LaravelLanguageServer, root: &Path) -> std::path::PathBuf {
    let provider = root.join("app/Providers/AppServiceProvider.php");
    let caller = root.join("app/Support/Ids.php");
    std::fs::create_dir_all(caller.parent().unwrap()).unwrap();
    std::fs::write(&caller, CALLER_SRC).unwrap();

    server
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None, None)
        .await
        .unwrap();
    server
        .salsa
        .register_service_provider_source(provider, PROVIDER_SRC.to_string(), 2, root.to_path_buf())
        .await
        .unwrap();
    server
        .salsa
        .update_file(caller.clone(), 1, CALLER_SRC.to_string())
        .await
        .unwrap();
    server.salsa.get_patterns(caller.clone()).await.unwrap();
    caller
}

/// Pull the single `LocationLink` out of a goto-def response, asserting the
/// `Link` shape the handler must return for a resolved macro.
fn single_link(resp: GotoDefinitionResponse) -> tower_lsp::lsp_types::LocationLink {
    let links = match resp {
        GotoDefinitionResponse::Link(links) => links,
        other => panic!("expected GotoDefinitionResponse::Link, got {other:?}"),
    };
    assert_eq!(
        links.len(),
        1,
        "exactly one LocationLink for a resolved macro"
    );
    links.into_iter().next().unwrap()
}

#[tokio::test]
async fn macro_call_goto_lands_on_closure_line() {
    // `Str::uuid7()` → the registered closure in the provider. The handler must
    // return a `Link` whose target is the provider file at the closure's line
    // (6, 0-based), with a zero-width caret (no method token to narrow to on the
    // vendor host).
    let dir = TempDir::new().unwrap();
    let server = test_server();
    let _caller = prime_macro_project(&server, dir.path()).await;
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");

    let resp = server
        .create_magic_member_location(&_caller, &uuid7_ref())
        .await
        .expect("a registered macro call resolves to a goto Link");

    let link = single_link(resp);
    assert_eq!(
        link.target_uri.to_file_path().expect("file:// target"),
        provider,
        "goto lands on the provider file declaring the macro"
    );
    assert_eq!(
        link.target_range.start.line, 6,
        "goto lands on the closure's 0-based declaration line"
    );
    assert_eq!(
        link.target_range.start.character, 0,
        "zero-width caret — no method token on the vendor host to narrow to"
    );
}

#[tokio::test]
async fn unregistered_macro_call_does_not_resolve() {
    // A `Str::notAMacro()` call site, with `Str::uuid7` registered so `Str` IS a
    // known Macroable host (`has_macro_host`) but THIS member is absent from the
    // registry. The member fails to classify as a macro (the registry lookup
    // misses), so the handler degrades to `None` gracefully — no panic, no bogus
    // location. This is the negative side of the `Macro` arm: a macro that
    // doesn't exist must not produce a goto target.
    let dir = TempDir::new().unwrap();
    let server = test_server();
    let root = dir.path();

    // Caller invokes an UNREGISTERED macro on the same host.
    let caller_src = r#"<?php
namespace App\Support;
use Illuminate\Support\Str;
class Ids {
    public function make(): string { return Str::notAMacro(); }
}
"#;
    let provider = root.join("app/Providers/AppServiceProvider.php");
    let caller = root.join("app/Support/Ids.php");
    std::fs::create_dir_all(caller.parent().unwrap()).unwrap();
    std::fs::write(&caller, caller_src).unwrap();

    server
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None, None)
        .await
        .unwrap();
    server
        .salsa
        .register_service_provider_source(provider, PROVIDER_SRC.to_string(), 2, root.to_path_buf())
        .await
        .unwrap();
    server
        .salsa
        .update_file(caller.clone(), 1, caller_src.to_string())
        .await
        .unwrap();
    server.salsa.get_patterns(caller.clone()).await.unwrap();

    let (line, column) = position_of(caller_src, "notAMacro");
    let (rs, re) = byte_range_of(caller_src, "Str::notAMacro");
    let member = MemberAccessReferenceData {
        member: "notAMacro".to_string(),
        receiver: "Str".to_string(),
        receiver_byte_start: rs,
        receiver_byte_end: re,
        is_nullsafe: false,
        form: AccessForm::StaticCall,
        line,
        column,
        end_column: column + "notAMacro".len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    };

    let resp = server.create_magic_member_location(&caller, &member).await;
    assert!(
        resp.is_none(),
        "an unregistered macro must not produce a goto target; got {resp:?}"
    );
}
