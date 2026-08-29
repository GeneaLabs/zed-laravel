//! Env completion must never offer the language server's own process
//! environment (issue #342).
//!
//! The handler used to merge `std::env::vars()` into the env-completion list
//! and render each value inline in `detail` / `documentation`. The LSP process
//! inherits Zed's environment, which inherits the login shell's, so that list
//! routinely carried `AWS_SECRET_ACCESS_KEY`, `GITHUB_TOKEN` and friends —
//! rendered, with values, in a popup that is most likely to be on screen during
//! a screen-share or a recording. It was also wrong on its own terms: `${...}`
//! interpolation inside `.env` and `env()` at runtime both resolve against the
//! dotenv file set, never against the editor's environment.
//!
//! These tests drive the **real** `textDocument/completion` entry point — not a
//! hand-built `StringContext` — across all three contexts the env branch serves
//! (`.env` `${…}`, PHP/Blade `env('…')`, and PHPUnit XML in both spellings
//! `get_phpunit_env_context` accepts, `<env name="…">` and `<server name="…">`),
//! with a real process variable set via `std::env::set_var`.
//!
//! Every leak assertion carries a positive control — the project's own `.env`
//! variable IS offered — so a fixture that never reached the completion code
//! cannot pass vacuously on an empty response. The control checks the name
//! (`label`) and both fields that echo the value and its source: `detail`
//! (`"<value> (from <file>)"`) and the `documentation` panel, asserted whole so
//! that deleting any one of `completion()`'s three `CompletionDoc` builder calls
//! — `.header(…)`, `.summary(…)`, `.section("Source: …")` — is caught. The one
//! exception is
//! `prefix_matching_only_a_process_var_returns_no_completions`, whose subject is
//! the empty response itself; a positive control would contradict what it
//! asserts, and the siblings sharing its fixture helper establish reachability.
//!
//! Restoring the deleted `for (name, value) in std::env::vars()` loop in
//! `main.rs` turns nine of these ten tests red. The tenth,
//! `dotenv_declaration_shadowing_a_process_var_still_completes_from_the_file`,
//! stays green under that mutation by design: its fixture `.env` declares the
//! process variable's own name, so the deleted loop's own
//! `!seen_names.contains(&name)` guard skipped that variable even before this
//! fix. A second mutation discriminates it — that name is secret-bearing, so
//! dropping the redaction branch added for issue #344 changes its `detail` and
//! its documentation panel, and both assertions fail.

use crate::LaravelLanguageServer;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionParams, CompletionResponse, Documentation, MarkupContent, MarkupKind,
    PartialResultParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

/// A process variable shaped like the credentials the issue is about. Suffixed
/// `_TEST` so it can never collide with a real credential in the developer's
/// own shell, and never itself declared in the fixture `.env` (except in the
/// shadowing test, which is about exactly that case).
const SECRET_NAME: &str = "AWS_SECRET_ACCESS_KEY_TEST";

/// Distinctive enough that finding it anywhere in the serialized response is
/// proof of a leak rather than a coincidental substring match.
const SECRET_VALUE: &str = "s3cr3t-value-set-only-by-issue-342-tests";

/// The prefix the tests type. Matches both `SECRET_NAME` and the fixture's own
/// declared variable, so the process variable is a genuine candidate the filter
/// would admit — the fix is what excludes it, not the prefix.
const TYPED_PREFIX: &str = "AWS_SECRET";

/// A variable the *project* declares, sharing `TYPED_PREFIX` with the secret.
///
/// `SECRETARIAT` deliberately is not the segment `SECRET`: this control asserts
/// the untouched `.env` echo format, so its name must fall outside
/// `completion_display::is_sensitive_env_name` (issue #344) — which redacts by
/// whole `_`-delimited segment, and would otherwise blank the very value this
/// control is here to see.
const DECLARED_NAME: &str = "AWS_SECRETARIAT_REGION";
const DECLARED_VALUE: &str = "declared-in-dotenv";

/// `.env` fixture: one variable under the typed prefix, one outside it. Built
/// from the constants above so the fixture and the assertions cannot drift.
fn dotenv() -> String {
    format!("APP_NAME=Laravel\n{DECLARED_NAME}={DECLARED_VALUE}\n")
}

/// Serializes the process-global env mutation these tests perform. `set_var`
/// mutates state shared by every thread in the process, and `cargo test` runs
/// tests in parallel — without this, two env-mutating tests could interleave
/// their set and restore. A tokio mutex (not `std::sync::Mutex`) because the
/// guard is held across `.await` points.
static ENV_MUTATION_LOCK: Mutex<()> = Mutex::const_new(());

/// Sets a process environment variable for the lifetime of the guard and
/// restores the previous state on drop — including on panic, so a failing
/// assertion cannot leak the variable into the rest of the suite.
struct EnvVarGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var(self.name, previous),
            None => std::env::remove_var(self.name),
        }
    }
}

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client`; `inner()` hands back the `LaravelLanguageServer` so its real
/// `completion()` handler can be driven directly.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Register `text` as the project's `.env` with Salsa (priority 2, matching
/// `register_env_files_with_salsa`), open `file_name` as the buffer the cursor
/// sits in, and return the tempdir (hold it) plus the buffer's URL.
async fn open_project(
    server: &LaravelLanguageServer,
    dotenv: &str,
    file_name: &str,
    buffer: &str,
) -> (TempDir, Url) {
    let dir = TempDir::new().expect("tempdir");
    let env_path: PathBuf = dir.path().join(".env");
    std::fs::write(&env_path, dotenv).expect("write .env");
    server
        .salsa
        .register_env_source(env_path, dotenv.to_string(), 2)
        .await
        .expect("register .env with salsa");

    let buffer_path = dir.path().join(file_name);
    let uri = Url::from_file_path(&buffer_path).expect("file url");
    server
        .documents
        .write()
        .await
        .insert(uri.clone(), (buffer.to_string(), 1));
    (dir, uri)
}

/// Drive the real completion handler with the cursor at the end of `line` (the
/// buffer is always a single line in these fixtures).
async fn complete_at_end_of_line(
    server: &LaravelLanguageServer,
    uri: Url,
    line: &str,
) -> Option<CompletionResponse> {
    server
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: line.chars().count() as u32,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .expect("completion handler must not error")
}

/// Split a response into its items and its serialized form.
///
/// Both response shapes are handled deliberately. `Ok(None)` is a real outcome
/// now that the system fallback is gone — a prefix no `.env` variable matches
/// produces an empty item list, which the handler reports as `None` — so it is
/// treated as "zero items" and still runs every assertion below, rather than
/// being skipped as "nothing to inspect".
fn dissect(response: Option<CompletionResponse>) -> (Vec<CompletionItem>, String) {
    match response {
        None => (Vec::new(), String::new()),
        Some(CompletionResponse::Array(items)) => {
            let json = serde_json::to_string(&items).expect("serialize items");
            (items, json)
        }
        Some(CompletionResponse::List(list)) => {
            let json = serde_json::to_string(&list).expect("serialize list");
            (list.items, json)
        }
    }
}

/// Assert the process variable left no trace — by value, in any field of any
/// item, which no relabelling can dodge — and that no item is derived from its
/// name either. Returns the items so callers can make positive assertions.
fn assert_no_process_var_leak(
    response: Option<CompletionResponse>,
    context: &str,
) -> Vec<CompletionItem> {
    let (items, json) = dissect(response);
    assert!(
        !json.contains(SECRET_VALUE),
        "{context}: the process variable's value leaked into the completion response: {json}"
    );
    assert!(
        !items.iter().any(|i| i.label == SECRET_NAME),
        "{context}: an item was derived from the process variable {SECRET_NAME}"
    );
    items
}

/// The documentation panel's markdown, or a failure naming what arrived instead.
/// `Source: <file>` lives in this field, not in `detail`, so the assertions read
/// the field itself rather than the `detail` line that merely resembles it.
fn documentation_markdown(item: &CompletionItem, context: &str) -> String {
    match item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        })) => value.clone(),
        other => panic!("{context}: expected markdown documentation, got {other:?}"),
    }
}

/// The panel `completion()` builds for a `.env`-declared variable: the name as
/// the bolded header, the value as the summary, `Source: <file>` as the trailing
/// section. Asserted whole rather than by substring, so dropping any one of the
/// three builder calls fails — not only the `.section(...)` this fix is about.
/// The header arrives markdown-escaped (`markdown_safety::escape_inline`),
/// because a `.env` key has no charset and the panel is rendered as markdown.
/// Building the expectation with the same helper is not circular: the escaping
/// itself is pinned by `markdown_safety`'s own literal-expectation tests and by
/// the link/image fixtures in `env_key_navigation.rs`. What *this* helper
/// asserts is the panel's structure and its redaction, and those must not
/// become unreadable to spell an underscore.
fn expected_documentation(name: &str, value: &str, source_file: &str) -> String {
    let name = laravel_lsp::markdown_safety::escape_inline(name);
    format!("**{name}**\n\n{value}\n\nSource: {source_file}")
}

/// Assert the project's own `.env` variable is still offered, with its value and
/// source file in both fields that report them — `detail` and the `documentation`
/// panel. The guard against a fix that empties the list entirely, and the proof
/// that the fixture really reached the env-completion code.
fn assert_declared_var_offered(items: &[CompletionItem], context: &str) {
    let declared = items
        .iter()
        .find(|i| i.label == DECLARED_NAME)
        .unwrap_or_else(|| {
            panic!(
                "{context}: expected the .env-declared {DECLARED_NAME} to be offered, got {:?}",
                items.iter().map(|i| &i.label).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        declared.detail.as_deref(),
        Some(format!("{DECLARED_VALUE} (from .env)").as_str()),
        "{context}: the .env echo path must be untouched by this fix"
    );
    assert_eq!(
        documentation_markdown(declared, context),
        expected_documentation(DECLARED_NAME, DECLARED_VALUE, ".env"),
        "{context}: the .env documentation panel must be untouched by this fix"
    );
}

// ============================================================================
// A typed prefix that matches the secret — all three contexts
// ============================================================================

#[tokio::test]
async fn env_file_interpolation_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = format!("NEW_VAR=${{{TYPED_PREFIX}");
    let (_dir, uri) = open_project(&server, &dotenv(), ".env", &line).await;

    let response = complete_at_end_of_line(&server, uri, &line).await;
    let items = assert_no_process_var_leak(response, ".env ${…} interpolation");
    assert_declared_var_offered(&items, ".env ${…} interpolation");
}

#[tokio::test]
async fn php_env_call_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = format!("<?php $key = env('{TYPED_PREFIX}");
    let (_dir, uri) = open_project(&server, &dotenv(), "probe.php", &line).await;

    let response = complete_at_end_of_line(&server, uri, &line).await;
    let items = assert_no_process_var_leak(response, "PHP env('…') call");
    assert_declared_var_offered(&items, "PHP env('…') call");
}

#[tokio::test]
async fn phpunit_xml_env_attribute_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = format!("    <env name=\"{TYPED_PREFIX}");
    let (_dir, uri) = open_project(&server, &dotenv(), "phpunit.xml", &line).await;

    let response = complete_at_end_of_line(&server, uri, &line).await;
    let items = assert_no_process_var_leak(response, "PHPUnit <env name=\"…\">");
    assert_declared_var_offered(&items, "PHPUnit <env name=\"…\">");
}

/// `<server name="…">` is the second spelling `get_phpunit_env_context` accepts
/// (`main.rs`), and it reaches the completion code through its own arm of the
/// `(env_pattern, server_pattern)` match — a different literal, a different
/// offset (`s + 14`, against `e + 11`). The leak filtering below the parse is
/// shared, so this test also pins that arm's offset arithmetic: get it wrong and
/// the prefix is mis-sliced, and the declared-variable control fails.
#[tokio::test]
async fn phpunit_xml_server_attribute_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = format!("    <server name=\"{TYPED_PREFIX}");
    let (_dir, uri) = open_project(&server, &dotenv(), "phpunit.xml", &line).await;

    let response = complete_at_end_of_line(&server, uri, &line).await;
    let items = assert_no_process_var_leak(response, "PHPUnit <server name=\"…\">");
    assert_declared_var_offered(&items, "PHPUnit <server name=\"…\">");
}

// ============================================================================
// The empty-prefix repro from the issue — `FOO=${` with nothing typed yet, the
// shape that returned 57 items because an empty prefix matches every process
// variable.
// ============================================================================

#[tokio::test]
async fn empty_prefix_in_env_file_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = "FOO=${";
    let (_dir, uri) = open_project(&server, &dotenv(), ".env", line).await;

    let response = complete_at_end_of_line(&server, uri, line).await;
    let items = assert_no_process_var_leak(response, "empty prefix in .env");
    assert_declared_var_offered(&items, "empty prefix in .env");
    // Every offered item comes from the fixture `.env`, which declares two.
    assert_eq!(
        items.len(),
        2,
        "empty prefix in .env: only the two declared variables may be offered, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn empty_prefix_in_php_env_call_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = "<?php $key = env('";
    let (_dir, uri) = open_project(&server, &dotenv(), "probe.php", line).await;

    let response = complete_at_end_of_line(&server, uri, line).await;
    let items = assert_no_process_var_leak(response, "empty prefix in env('…')");
    assert_declared_var_offered(&items, "empty prefix in env('…')");
    assert_eq!(
        items.len(),
        2,
        "empty prefix in env('…'): only the two declared variables may be offered, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn empty_prefix_in_phpunit_xml_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = "    <env name=\"";
    let (_dir, uri) = open_project(&server, &dotenv(), "phpunit.xml", line).await;

    let response = complete_at_end_of_line(&server, uri, line).await;
    let items = assert_no_process_var_leak(response, "empty prefix in <env name=\"…\">");
    assert_declared_var_offered(&items, "empty prefix in <env name=\"…\">");
    assert_eq!(
        items.len(),
        2,
        "empty prefix in <env name=\"…\">: only the two declared variables may be offered, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn empty_prefix_in_phpunit_xml_server_attribute_never_offers_process_vars() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let line = "    <server name=\"";
    let (_dir, uri) = open_project(&server, &dotenv(), "phpunit.xml", line).await;

    let response = complete_at_end_of_line(&server, uri, line).await;
    let items = assert_no_process_var_leak(response, "empty prefix in <server name=\"…\">");
    assert_declared_var_offered(&items, "empty prefix in <server name=\"…\">");
    assert_eq!(
        items.len(),
        2,
        "empty prefix in <server name=\"…\">: only the two declared variables may be offered, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}

// ============================================================================
// A prefix that matches nothing the project declares — the `Ok(None)` shape,
// which is now the common case and used to be masked by the process-env merge.
// ============================================================================

#[tokio::test]
async fn prefix_matching_only_a_process_var_returns_no_completions() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    // `AWS_SECRET_ACCESS` matches the process variable and nothing in the
    // fixture `.env`, so the item list ends up empty and the handler answers
    // `Ok(None)` — the shape a leak would replace with a one-item list.
    let line = "NEW_VAR=${AWS_SECRET_ACCESS";
    let (_dir, uri) = open_project(&server, &dotenv(), ".env", line).await;

    let response = complete_at_end_of_line(&server, uri, line).await;
    assert!(
        response.is_none(),
        "a prefix only the process environment matches must produce no completions, got {response:?}"
    );
}

// ============================================================================
// Shadowing: a `.env` declaration whose name is also a real process variable.
// ============================================================================

#[tokio::test]
async fn dotenv_declaration_shadowing_a_process_var_still_completes_from_the_file() {
    let _serial = ENV_MUTATION_LOCK.lock().await;
    let _secret = EnvVarGuard::set(SECRET_NAME, SECRET_VALUE);

    let server = test_server();
    let shadowing_dotenv = format!("APP_NAME=Laravel\n{SECRET_NAME}=dotenv-owns-this-name\n");
    let line = format!("NEW_VAR=${{{TYPED_PREFIX}");
    let (_dir, uri) = open_project(&server, &shadowing_dotenv, ".env", &line).await;

    let (items, json) = dissect(complete_at_end_of_line(&server, uri, &line).await);
    assert!(
        !json.contains(SECRET_VALUE),
        "the process value must not surface even when the .env declares the same name: {json}"
    );

    let shadowing: Vec<_> = items.iter().filter(|i| i.label == SECRET_NAME).collect();
    assert_eq!(
        shadowing.len(),
        1,
        "the declared variable must be offered exactly once, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
    // `AWS_SECRET_ACCESS_KEY_TEST` is secret-bearing, so issue #344 redacts its
    // value on this surface. That is orthogonal to what this test is about —
    // the declaration is still the one that wins, and it is still offered
    // exactly once — but the rendering it asserts is now the redacted one.
    assert_eq!(
        shadowing[0].detail.as_deref(),
        Some("(from .env)"),
        "the source file must still be reported, with the value redacted"
    );
    assert_eq!(
        documentation_markdown(shadowing[0], "shadowing"),
        expected_documentation(
            SECRET_NAME,
            laravel_lsp::completion_display::REDACTED_ENV_VALUE,
            ".env"
        ),
        "the documentation panel must report the redaction string and the source"
    );
}
