//! Secret-bearing `.env` values never reach a rendered surface (issue #344).
//!
//! #342 closed the *process*-environment leak. The project's own `.env` is a
//! milder but real second source: a Laravel `.env` routinely holds `APP_KEY`,
//! `DB_PASSWORD`, `MAIL_PASSWORD` and third-party tokens, and four surfaces
//! echoed them — env completion, `.env` hover, `config('…')` completion, and
//! the warm-start disk cache.
//!
//! A fifth was found in review: the server log. It consults the same predicate
//! and is covered next to the code that logs, in `database::tests` — these four
//! are the client-rendered ones.
//!
//! All four now consult the same two gates: `is_sensitive_env_name`, which
//! reads the variable's **name**, and `mask_url_credentials`, which reads the
//! value's **shape**. The second exists because the first cannot see
//! `DATABASE_URL=mysql://user:pass@host/db` — a name matching no sensitive
//! segment, filled by stock Laravel's `'url' => env('DATABASE_URL')`, whose
//! password sits inside the value. These tests drive the real entry points —
//! `completion()`, `hover_for_env`, `get_all_config_keys`, and a genuine
//! `CacheManager` save/load round trip — with two different matched keyword
//! categories (`DB_PASSWORD` and `API_TOKEN`) plus the URL shape crossing every
//! one of them, so a surface that re-implemented either check locally and
//! drifted is caught by observed behaviour rather than by reading the diff.
//!
//! Leak assertions run over the **whole serialized response**, not the fields
//! this change touches: `insertText`, `label`, `sortText` and friends are just
//! as visible in a screen-share as `detail` is. The cache assertion reads the
//! deserialized struct rather than the file's bytes, because a reversible
//! encoding of the value would pass a bytes-only check and still leak.
//!
//! Every leak assertion carries a positive control — the ordinary `APP_NAME`
//! renders exactly as it did before this change — so a fixture that never
//! reached the code under test cannot pass vacuously on an empty response.
//!
//! One test here is about a second property of the same surface rather than
//! about redaction: `a_value_spelling_a_markdown_link_renders_inert_in_the_panel`.
//! Redaction decides *whether* a value is displayed; that one decides whether
//! the displayed one can act, since a `.env` value has no charset restriction
//! and the completion panel is rendered as markdown. It lives here because it
//! shares this module's subject — what the LSP puts on screen from `.env`
//! content — and its fixture is the completion panel these helpers already
//! model. The hover card's matching property is pinned in `env_key_navigation`.

use crate::LaravelLanguageServer;
use laravel_lsp::cache_manager::CacheManager;
use laravel_lsp::completion_display::REDACTED_ENV_VALUE;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionParams, CompletionResponse, Documentation, MarkupContent, MarkupKind,
    PartialResultParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

/// Two matched names from two different keyword categories. One category alone
/// cannot tell a shared predicate from a hard-coded `contains("PASSWORD")`.
const PASSWORD_NAME: &str = "DB_PASSWORD";
const PASSWORD_VALUE: &str = "hunter2-issue-344";
const TOKEN_NAME: &str = "API_TOKEN";
const TOKEN_VALUE: &str = "tok-issue-344-only";

/// The unmatched control. Its value must survive every surface untouched.
const PLAIN_NAME: &str = "APP_NAME";
const PLAIN_VALUE: &str = "Example";

/// The name gate's blind spot: `DATABASE_URL` splits to `DATABASE` / `URL` and
/// matches no sensitive segment, yet stock Laravel's `config/database.php`
/// reads `'url' => env('DATABASE_URL')` and the value carries the password
/// inside itself. Caught by shape, not by name.
const URL_NAME: &str = "DATABASE_URL";
const URL_SECRET: &str = "url-hunter2-issue-344";

fn url_value() -> String {
    format!("mysql://sail:{URL_SECRET}@db.internal.example:3306/laravel")
}

/// What every surface must render instead: masked, not dropped. The host, port
/// and database are the whole reason a developer looks at this value.
fn url_masked() -> String {
    "mysql://sail:***@db.internal.example:3306/laravel".to_string()
}

/// A matched name that is *commented out* in the fixture. The client-visible
/// surfaces skip commented entries already; the cache-write loop never did.
const COMMENTED_NAME: &str = "MAIL_PASSWORD";
const COMMENTED_VALUE: &str = "mail-hunter2-issue-344";

/// The project `.env` every test registers with Salsa.
fn dotenv() -> String {
    format!(
        "{PLAIN_NAME}={PLAIN_VALUE}\n{PASSWORD_NAME}={PASSWORD_VALUE}\n{TOKEN_NAME}={TOKEN_VALUE}\n{URL_NAME}={}\n# {COMMENTED_NAME}={COMMENTED_VALUE}\n",
        url_value()
    )
}

fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// A project on disk with `dotenv` registered with Salsa (priority 2, matching
/// `register_env_files_with_salsa`) and `root_path` primed, so both the env and
/// the `config('…')` completion branches are reachable.
async fn project(dotenv: &str) -> (TempDir, PathBuf, LaravelLanguageServer) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().to_path_buf();
    let env_path = root.join(".env");
    std::fs::write(&env_path, dotenv).expect("write .env");

    let server = test_server();
    *server.root_path.write().await = Some(root.clone());
    *server.auto_complete_debounce_ms.write().await = 0;
    server
        .salsa
        .register_env_source(env_path, dotenv.to_string(), 2)
        .await
        .expect("register .env with salsa");
    (dir, root, server)
}

/// Open `file_name` holding the single line `buffer`, and drive the real
/// completion handler with the cursor at the end of it.
async fn complete_in(
    server: &LaravelLanguageServer,
    root: &Path,
    file_name: &str,
    buffer: &str,
) -> Option<CompletionResponse> {
    let uri = Url::from_file_path(root.join(file_name)).expect("file url");
    server
        .documents
        .write()
        .await
        .insert(uri.clone(), (buffer.to_string(), 1));
    server
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: buffer.chars().count() as u32,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .expect("completion handler must not error")
}

/// Split a response into its items and its serialized form. `None` is a real
/// outcome (an empty item list is reported as `None`), so it becomes zero items
/// and still runs every assertion rather than being skipped.
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

/// No secret survives anywhere in the serialized response — any field, any
/// item. Returns the items so callers can add positive assertions.
fn assert_no_secret_leak(
    response: Option<CompletionResponse>,
    context: &str,
) -> Vec<CompletionItem> {
    let (items, json) = dissect(response);
    for secret in [PASSWORD_VALUE, TOKEN_VALUE, COMMENTED_VALUE, URL_SECRET] {
        assert!(
            !json.contains(secret),
            "{context}: {secret} leaked into the completion response: {json}"
        );
    }
    items
}

fn item<'a>(items: &'a [CompletionItem], label: &str, context: &str) -> &'a CompletionItem {
    items.iter().find(|i| i.label == label).unwrap_or_else(|| {
        panic!(
            "{context}: expected {label} to be offered, got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        )
    })
}

fn documentation_markdown(item: &CompletionItem, context: &str) -> String {
    match item.documentation.as_ref() {
        Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        })) => value.clone(),
        other => panic!("{context}: expected markdown documentation, got {other:?}"),
    }
}

/// The panel `completion()` builds for a `.env` variable: name, summary,
/// `Source: <file>`. Asserted whole, so dropping any builder call fails.
///
/// Both the name and the value arrive markdown-escaped
/// (`markdown_safety::escape_inline`), because neither a `.env` key nor a
/// `.env` value has a charset and the panel is rendered as markdown. The two
/// are escaped in different places — `hover::render` and
/// `CompletionDoc::render` escape the header for every caller, while
/// `summary` is markdown-bearing by contract and leaves it to the call site —
/// but the panel text is the same either way, which is what this models.
///
/// Building the expectation with the same helper is not circular: the escaping
/// itself is pinned by `markdown_safety`'s own literal-expectation tests, by
/// the link/image fixtures in `env_key_navigation.rs`, and by
/// `a_value_spelling_a_markdown_link_renders_inert_in_the_panel` below, which
/// asserts against literal escaped text rather than through this helper. What
/// *this* helper asserts is the panel's structure and its redaction, and those
/// must not become unreadable to spell an underscore.
fn expected_panel(name: &str, summary: &str) -> String {
    let name = laravel_lsp::markdown_safety::escape_inline(name);
    let summary = laravel_lsp::markdown_safety::escape_inline(summary);
    format!("**{name}**\n\n{summary}\n\nSource: .env")
}

// ========================================================================
// Surface 1 — env completion
// ========================================================================

#[tokio::test]
async fn env_completion_redacts_every_matched_category_and_leaves_the_rest_alone() {
    let (_dir, root, server) = project(&dotenv()).await;
    let line = "<?php $v = env('";
    let response = complete_in(&server, &root, "probe.php", line).await;
    let items = assert_no_secret_leak(response, "env completion");

    for name in [PASSWORD_NAME, TOKEN_NAME] {
        let redacted = item(&items, name, "env completion");
        assert_eq!(
            redacted.detail.as_deref(),
            Some("(from .env)"),
            "{name}: the detail line must drop the value but keep the source"
        );
        assert_eq!(
            documentation_markdown(redacted, "env completion"),
            expected_panel(name, REDACTED_ENV_VALUE),
            "{name}: the panel must show the redaction string, not the value"
        );
    }

    // Matched by shape rather than by name: the credential goes, the rest of
    // the URL stays — dropping the whole value would be a different, worse bug.
    let url = item(&items, URL_NAME, "env completion");
    assert_eq!(
        url.detail.as_deref(),
        Some(format!("{} (from .env)", url_masked()).as_str()),
        "{URL_NAME}: the detail line must show the URL with its password masked"
    );
    assert_eq!(
        documentation_markdown(url, "env completion"),
        expected_panel(URL_NAME, &url_masked()),
        "{URL_NAME}: the panel must show the masked URL, not the credential"
    );

    // Positive control: the unmatched variable is byte-for-byte what it was.
    let plain = item(&items, PLAIN_NAME, "env completion");
    assert_eq!(
        plain.detail.as_deref(),
        Some(format!("{PLAIN_VALUE} (from .env)").as_str())
    );
    assert_eq!(
        documentation_markdown(plain, "env completion"),
        expected_panel(PLAIN_NAME, PLAIN_VALUE)
    );
}

/// The `.env` buffer's own `${…}` interpolation is a second caller of the same
/// item builder, and it is the surface most likely to be open while the file
/// full of credentials is on screen.
#[tokio::test]
async fn env_file_interpolation_redacts_too() {
    let (_dir, root, server) = project(&dotenv()).await;
    let line = "NEW_VAR=${";
    let response = complete_in(&server, &root, ".env", line).await;
    let items = assert_no_secret_leak(response, ".env ${…} interpolation");

    assert_eq!(
        documentation_markdown(
            item(&items, PASSWORD_NAME, "interpolation"),
            "interpolation"
        ),
        expected_panel(PASSWORD_NAME, REDACTED_ENV_VALUE)
    );
    assert_eq!(
        documentation_markdown(item(&items, PLAIN_NAME, "interpolation"), "interpolation"),
        expected_panel(PLAIN_NAME, PLAIN_VALUE)
    );
}

/// Precedence: redaction runs before the existing empty-value display, so a
/// declared-but-blank credential reads as redacted rather than as `(empty)` —
/// which would otherwise tell a reader "this one is not set".
///
/// This doubles as the warm-start parity guard. A matched name read back from
/// the disk cache arrives with an empty value (surface 4), so "identical
/// whether cached or live" reduces to exactly this fixture: an empty value
/// under a matched name renders the redaction string either way.
#[tokio::test]
async fn an_empty_sensitive_value_redacts_rather_than_reading_empty() {
    let fixture = format!("{PASSWORD_NAME}=\nAPP_DEBUG=\n");
    let (_dir, root, server) = project(&fixture).await;
    let line = "<?php $v = env('";
    let items = dissect(complete_in(&server, &root, "probe.php", line).await).0;

    assert_eq!(
        documentation_markdown(item(&items, PASSWORD_NAME, "empty value"), "empty value"),
        expected_panel(PASSWORD_NAME, REDACTED_ENV_VALUE),
        "an empty sensitive value must not fall through to the (empty) display"
    );
    // Positive control for the branch this one jumps ahead of: an ordinary
    // blank variable still reads `(empty)`.
    assert_eq!(
        documentation_markdown(item(&items, "APP_DEBUG", "empty value"), "empty value"),
        expected_panel("APP_DEBUG", "(empty)")
    );
}

/// Redaction decides *whether* a value is shown; this decides whether the shown
/// one can act. A `.env` value is everything after the first `=`, with no
/// charset restriction, and the panel is `MarkupKind::Markdown` — so a value
/// spelling a link renders a live clickable one, and the image variant is
/// fetched with no click at all.
///
/// The hover card's key had the identical property through its header
/// (`env_key_navigation`); `hover::render` escapes that field for its callers.
/// `CompletionDoc::summary` cannot do the same — a PHPDoc summary legitimately
/// carries markdown — so its contract puts the escaping on the call site, and
/// this is the call site that hands it untrusted text.
#[tokio::test]
async fn a_value_spelling_a_markdown_link_renders_inert_in_the_panel() {
    // Neither name matches a sensitive segment and neither value has the
    // `user:pass@host` shape, so both reach the panel unredacted and unmasked —
    // this test is about the value that *is* displayed, not the ones that
    // aren't.
    const LINK_NAME: &str = "SUPPORT_NOTICE";
    const IMAGE_NAME: &str = "BANNER";
    let link = "[Update your credentials here](https://evil.example/harvest)";
    let image = "![](https://evil.example/pixel)";
    let fixture = format!("{LINK_NAME}={link}\n{IMAGE_NAME}={image}\n");
    let (_dir, root, server) = project(&fixture).await;

    // The `.env` buffer's own `${…}` interpolation: the popup most likely to be
    // open while the file holding these lines is the one on screen.
    let items = dissect(complete_in(&server, &root, ".env", "NEW_VAR=${").await).0;

    let panel = documentation_markdown(item(&items, LINK_NAME, "link value"), "link value");
    assert_eq!(
        panel,
        expected_panel(LINK_NAME, link),
        "the value must render as itself, not as a live link"
    );
    // Literal, not routed through `expected_panel` — that helper escapes with
    // the same function production does, so on its own it would still pass if
    // both sides stopped escaping together.
    assert!(
        !panel.contains("](https://evil.example/harvest)"),
        "the link's target must not survive unescaped: {panel}"
    );
    assert!(
        panel.contains(r"\[Update your credentials here\]"),
        "the value's brackets must arrive escaped: {panel}"
    );

    let panel = documentation_markdown(item(&items, IMAGE_NAME, "image value"), "image value");
    assert_eq!(
        panel,
        expected_panel(IMAGE_NAME, image),
        "the value must render as itself, not as an inline image"
    );
    // The image variant needs no click — the client fetches the URL to render
    // it — so the leading `!` is the load-bearing character here.
    assert!(
        !panel.contains("!["),
        "the image marker must not survive unescaped: {panel}"
    );
    assert!(
        panel.contains(r"\!\["),
        "the image marker must arrive escaped: {panel}"
    );
}

// ========================================================================
// Surface 2 — hover on `.env` keys
// ========================================================================

#[tokio::test]
async fn hover_redacts_every_matched_category() {
    let (_dir, _root, server) = project(&dotenv()).await;

    for (name, value) in [(PASSWORD_NAME, PASSWORD_VALUE), (TOKEN_NAME, TOKEN_VALUE)] {
        let markdown = server.hover_for_env(name).await;
        assert!(
            !markdown.contains(value),
            "{name}: the value leaked into the hover markdown: {markdown}"
        );
        assert!(
            markdown.contains(REDACTED_ENV_VALUE),
            "{name}: the hover must say why it shows nothing: {markdown}"
        );
        assert!(
            markdown.contains(".env"),
            "{name}: the source link must survive redaction: {markdown}"
        );
    }
}

/// The shape gate on the hover surface. The name clears the first gate, so a
/// hover on `DATABASE_URL` is the one place a developer reads that value —
/// masked, with the host and database still legible.
#[tokio::test]
async fn hover_masks_a_credential_carried_inside_the_value() {
    let (_dir, _root, server) = project(&dotenv()).await;
    let markdown = server.hover_for_env(URL_NAME).await;

    assert!(
        !markdown.contains(URL_SECRET),
        "the password inside {URL_NAME} leaked into the hover markdown: {markdown}"
    );
    assert!(
        markdown.contains(&format!("```\n{}\n```", url_masked())),
        "the masked URL must still render as a code block: {markdown}"
    );
    assert!(
        !markdown.contains(REDACTED_ENV_VALUE),
        "the shape gate masks, it does not fall back to the name gate's \
         whole-value redaction: {markdown}"
    );
}

/// Positive control and regression guard: an unmatched variable still renders
/// its value in a plain code block.
#[tokio::test]
async fn hover_on_an_ordinary_variable_is_unchanged() {
    let (_dir, _root, server) = project(&dotenv()).await;
    let markdown = server.hover_for_env(PLAIN_NAME).await;
    assert!(
        markdown.contains(&format!("```\n{PLAIN_VALUE}\n```")),
        "the ordinary hover must keep its code block: {markdown}"
    );
    assert!(!markdown.contains(REDACTED_ENV_VALUE));
}

/// The two paths this change must not disturb: a commented-out entry keeps its
/// note, and an undefined name keeps its trailer.
#[tokio::test]
async fn the_commented_and_not_found_hover_paths_are_untouched() {
    let (_dir, _root, server) = project(&dotenv()).await;

    let commented = server.hover_for_env(COMMENTED_NAME).await;
    assert!(
        commented.contains("*(commented out)*"),
        "commented hover changed: {commented}"
    );
    assert!(!commented.contains(COMMENTED_VALUE));

    let missing = server.hover_for_env("NOT_DECLARED_ANYWHERE").await;
    assert_eq!(missing, "*(not defined in .env)*");
}

// ========================================================================
// Surface 3 — `config('…')` completion
// ========================================================================

/// `config/app.php` exercising every input shape the resolver can take: a
/// dotenv hit through the plain `env()` spelling, a dotenv hit written with the
/// `(bool) env()` cast, a dotenv hit caught by value shape rather than by name,
/// an unmatched dotenv hit, a literal default, the not-found placeholder, and a
/// plain literal whose *config key* would match the predicate if the check were
/// attached to the wrong name.
fn config_php() -> String {
    format!(
        "<?php\nreturn [\n    'password' => env('{PASSWORD_NAME}'),\n    'token' => (bool) env('{TOKEN_NAME}'),\n    'url' => env('{URL_NAME}'),\n    'name' => env('{PLAIN_NAME}', 'Laravel'),\n    'fallback' => env('MISSING_PASSWORD', 'fallback-default'),\n    'absent' => env('ABSENT_TOKEN'),\n    'secret_key' => 'plain-literal-value',\n];\n"
    )
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn config_completion_redacts_only_the_dotenv_sourced_sensitive_values() {
    let (_dir, root, server) = project(&dotenv()).await;
    write(&root, "config/app.php", &config_php());

    let keys = server.get_all_config_keys().await;
    let value_of = |key: &str| {
        keys.iter()
            .find(|c| c.key == key)
            .unwrap_or_else(|| {
                panic!(
                    "expected {key}, got {:?}",
                    keys.iter().map(|c| &c.key).collect::<Vec<_>>()
                )
            })
            .value
            .clone()
    };

    // Both `env()` spellings redact, and they do so through the *same* arm of
    // the resolver: `env_pattern` is unanchored, so it matches the
    // `env('API_TOKEN')` substring of the cast and the `(bool)` prefix never
    // reaches a branch of its own. The fixture is here to pin that the cast
    // spelling resolves and redacts at all — not to claim a second code path
    // exists, which is what the deleted `bool_env_pattern` arm falsely implied.
    assert_eq!(value_of("app.password"), REDACTED_ENV_VALUE);
    assert_eq!(value_of("app.token"), REDACTED_ENV_VALUE);

    // Caught by shape, not by name: `DATABASE_URL` clears the predicate.
    assert_eq!(
        value_of("app.url"),
        url_masked(),
        "a credential inside the value is masked even though the name is not sensitive"
    );

    // The three exemptions, each of which has nothing to leak.
    assert_eq!(
        value_of("app.name"),
        PLAIN_VALUE,
        "an unmatched dotenv value is untouched"
    );
    assert_eq!(
        value_of("app.fallback"),
        "fallback-default",
        "a literal default is already visible in the PHP file being edited"
    );
    assert_eq!(
        value_of("app.absent"),
        "${ABSENT_TOKEN}",
        "the not-found placeholder carries no value to redact"
    );
    assert_eq!(
        value_of("app.secret_key"),
        "plain-literal-value",
        "the predicate reads the env var's name, never the config key's"
    );
}

/// The project-relative label `config_source_label` renders beside a config
/// value, spelled with a forward slash on **every** platform.
///
/// Typed as a literal on purpose. The label is user-visible text, and
/// `config_source_label` normalizes separators precisely so it does not change
/// shape with the host OS. An expectation built with `Path::join(...).display()`
/// would mirror that production logic instead of pinning it: were the
/// normalization dropped, the built expectation would pick up the native
/// separator too and the test would stay green on the one platform — Windows —
/// where the regression is visible.
const CONFIG_APP_LABEL: &str = "config/app.php";

/// End-to-end through the real handler, so the render sites are covered too:
/// `completion_detail` and `config_documentation` both receive the redacted
/// value, and nothing else in the response carries the secret.
#[tokio::test]
async fn the_config_completion_response_carries_no_dotenv_secret() {
    let (_dir, root, server) = project(&dotenv()).await;
    write(&root, "config/app.php", &config_php());

    let line = "<?php $v = config('app.";
    let response = complete_in(&server, &root, "probe.php", line).await;
    let items = assert_no_secret_leak(response, "config completion");

    let password = item(&items, "app.password", "config completion");
    assert_eq!(
        password.detail.as_deref(),
        Some(format!("{REDACTED_ENV_VALUE} ({CONFIG_APP_LABEL})").as_str())
    );
    assert!(
        documentation_markdown(password, "config completion").contains(REDACTED_ENV_VALUE),
        "the config panel must show the redaction string"
    );

    // Positive control: an unmatched key still shows its resolved value.
    let name = item(&items, "app.name", "config completion");
    assert_eq!(
        name.detail.as_deref(),
        Some(format!("{PLAIN_VALUE} ({CONFIG_APP_LABEL})").as_str())
    );
}

// ========================================================================
// Surface 4 — the on-disk warm-start cache
// ========================================================================

/// Run the real cache-population path and persist it, then load it back with a
/// fresh `CacheManager`. Returns the reloaded manager.
async fn round_trip_cache(server: &LaravelLanguageServer, root: &Path) -> CacheManager {
    *server.cache.write().await = Some(CacheManager::load(root));
    server.populate_cache_from_salsa().await;
    server
        .cache
        .read()
        .await
        .as_ref()
        .expect("cache present")
        .save()
        .expect("cache should persist");
    CacheManager::load(root)
}

#[tokio::test]
async fn the_disk_cache_keeps_sensitive_names_but_never_their_values() {
    let (_dir, root, server) = project(&dotenv()).await;
    let reloaded = round_trip_cache(&server, &root).await;
    let variables = &reloaded
        .get_env_vars()
        .expect("env vars should round-trip")
        .variables;

    // Asserted on the deserialized struct, not on the file's bytes: a
    // reversible encoding would pass a substring check and still leak.
    for name in [PASSWORD_NAME, TOKEN_NAME, COMMENTED_NAME] {
        assert_eq!(
            variables.get(name).map(String::as_str),
            Some(""),
            "{name} must be cached by name with an empty value — present, never plaintext"
        );
    }
    // Not name-matched, so not emptied — but the credential inside it must not
    // reach the file either. This is the worst of the four surfaces to get
    // wrong: the cache is long-lived, sits outside the project, and no one
    // reads it before it leaks.
    assert_eq!(
        variables.get(URL_NAME).map(String::as_str),
        Some(url_masked().as_str()),
        "{URL_NAME} must be cached with its password masked, never in plaintext"
    );
    assert_eq!(
        variables.get(PLAIN_NAME).map(String::as_str),
        Some(PLAIN_VALUE),
        "an unmatched variable must round-trip unchanged"
    );
}

/// A cache entry survives the round trip byte-for-byte for an unmatched name,
/// and registering the reloaded map back into a cold server is accepted — the
/// path `load_cache_data` takes on a warm start.
///
/// It stops there deliberately. `register_cached_env_vars` writes the Salsa
/// actor's `env_variables` map, and **nothing renders from that map**:
/// `get_env_variable` / `get_env_variable_names` have no caller outside
/// `salsa_impl`, and all four surfaces read `get_all_parsed_env_vars` /
/// `get_parsed_env_var`, which walk the registered `.env` *sources* only. So
/// redaction in the cache is about the plaintext sitting on disk, not about
/// what a warm start displays; asserting a rendering difference here would be
/// asserting about code that never runs. The rendering half of the parity
/// claim is pinned instead by
/// `an_empty_sensitive_value_redacts_rather_than_reading_empty`, whose fixture
/// is exactly the shape a cache read produces: a matched name with an empty
/// value.
#[tokio::test]
async fn the_reloaded_cache_registers_cleanly_on_a_cold_server() {
    let (_dir, root, server) = project(&dotenv()).await;
    let reloaded = round_trip_cache(&server, &root).await;
    let cached = reloaded.get_env_vars().expect("env vars").variables.clone();
    assert_eq!(
        cached.get(PLAIN_NAME).map(String::as_str),
        Some(PLAIN_VALUE),
        "the unmatched value must survive the round trip unchanged"
    );

    let warm = test_server();
    warm.salsa
        .register_cached_env_vars(cached)
        .await
        .expect("a redacted cache must still register on warm start");
}

/// A cache file written by a pre-fix binary already holds plaintext secrets.
/// The `CACHE_VERSION` bump is what stops it being trusted and served — and the
/// rescan it forces must leave an ordinary variable exactly as it was.
///
/// Both halves live in one test on purpose: "the old file is dropped" and "the
/// replacement is correct" are one behaviour, and splitting them let AC #7's
/// regression guard be satisfied by combining two fixtures neither of which
/// contained both variables.
#[tokio::test]
async fn a_pre_bump_cache_holding_plaintext_is_rejected_and_rescanned() {
    let (_dir, root, server) = project(&dotenv()).await;

    // Write a well-formed current-version cache carrying the plaintext, then
    // rewind only its version field. Hand-writing the JSON instead would risk
    // the test passing on a parse error rather than on the version check.
    let mut cache = CacheManager::load(&root);
    cache.set_env_vars(laravel_lsp::cache_manager::CachedEnvVars {
        variables: [
            (PASSWORD_NAME.to_string(), PASSWORD_VALUE.to_string()),
            // The regression guard rides in the same planted file: an ordinary
            // variable is dropped along with the secret, and has to come back
            // unchanged from the rescan.
            (PLAIN_NAME.to_string(), PLAIN_VALUE.to_string()),
        ]
        .into_iter()
        .collect(),
    });
    cache.save().expect("cache should persist");
    let path = cache.cache_path().expect("cache path").to_path_buf();

    let current = std::fs::read_to_string(&path).expect("read cache");
    let mut json: serde_json::Value = serde_json::from_str(&current).expect("cache json");
    // Derived from the file rather than written as a literal, so the next
    // version bump doesn't quietly turn this into a test of nothing.
    let version = json["version"].as_u64().expect("version field");
    json["version"] = serde_json::json!(version - 1);
    let previous = serde_json::to_string_pretty(&json).expect("serialize cache");
    assert!(
        previous.contains(PASSWORD_VALUE),
        "the planted cache must actually hold the plaintext this test is about"
    );
    std::fs::write(&path, previous).expect("plant pre-bump cache");

    // `get_env_vars()` is the assertion that discriminates: `has_cached_data()`
    // reads only the vendor/app/config sections and would answer the same with
    // the version check deleted.
    assert!(
        CacheManager::load(&root).get_env_vars().is_none(),
        "a pre-bump cache must be dropped, not served — it holds plaintext secrets"
    );

    // The rescan the rejection forces, through the real populate/save/load path.
    let rescanned = round_trip_cache(&server, &root).await;
    let variables = &rescanned
        .get_env_vars()
        .expect("the rescan must repopulate the cache")
        .variables;
    assert_eq!(
        variables.get(PLAIN_NAME).map(String::as_str),
        Some(PLAIN_VALUE),
        "an ordinary variable must survive the forced rescan unchanged"
    );
    assert_eq!(
        variables.get(PASSWORD_NAME).map(String::as_str),
        Some(""),
        "the rescan must rewrite the secret as an empty value, not restore it"
    );
    assert!(
        !std::fs::read_to_string(&path)
            .expect("read rescanned cache")
            .contains(PASSWORD_VALUE),
        "the rescanned file must not carry the plaintext the planted one did"
    );
}
