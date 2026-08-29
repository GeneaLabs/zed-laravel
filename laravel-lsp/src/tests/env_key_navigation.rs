//! End-to-end coverage for hover and go-to-definition on key declarations in
//! `.env*` buffers (issue #341).
//!
//! Both handlers used to return before any dispatch on a `.env` buffer:
//! `goto_definition` gated on `uri.path().ends_with(".php")`, and `hover` on
//! `is_blade || is_php`. Four other env features already dispatched on the
//! shared `env_key_locator::is_env_file_name` gate (code lens, interpolation
//! completion, semantic tokens, Salsa ingestion); these two now join them.
//!
//! Every test drives the **real** `LanguageServer::hover` /
//! `LanguageServer::goto_definition` through a `tower_lsp::LspService` harness,
//! not the helpers underneath. That is the point: widening a boolean gate is
//! worth nothing if the admitted buffer then falls through the PHP pattern
//! index and dead-ends in `Ok(None)`, and only a handler-level call can tell
//! those two apart.
//!
//! The consumer index is primed for real — a `.php` file carrying `env('KEY')`
//! goes through `salsa.update_file`, exactly as `did_open` would — so the
//! counts and jump targets these tests read are the ones the reverse index
//! actually holds.

use crate::LaravelLanguageServer;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, PartialResultParams,
    Position, TextDocumentIdentifier, TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

/// Build a server instance for testing. `LspService::new` wires up a real
/// `Client` and a live Salsa actor; `inner()` hands back the
/// `LaravelLanguageServer` so the tests can prime its private `documents` map
/// and call the trait handlers.
fn test_server() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// The priority `execute_salsa_update` assigns each env file name — `.env` 2,
/// `.env.local` 1, everything else 0. Mirrored here so the fixtures register
/// sources under the same ladder the running server uses.
fn env_priority(file_name: &str) -> u8 {
    match file_name {
        ".env" => 2,
        ".env.local" => 1,
        _ => 0,
    }
}

/// A server with `files` (name → contents) registered as env sources and open
/// in the document cache, plus `consumers` (relative `.php` path → contents)
/// indexed for pattern extraction. Returns the server and the tempdir root,
/// which the caller must keep alive for the duration of the test.
async fn server_with(
    files: &[(&str, &str)],
    consumers: &[(&str, &str)],
) -> (LaravelLanguageServer, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let server = test_server();
    *server.root_path.write().await = Some(dir.path().to_path_buf());

    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("write env file");
        server
            .salsa
            .register_env_source(path.clone(), contents.to_string(), env_priority(name))
            .await
            .expect("register env source");
        server
            .documents
            .write()
            .await
            .insert(url_for(&path), (contents.to_string(), 1));
    }

    for (rel, contents) in consumers {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create consumer dir");
        }
        std::fs::write(&path, contents).expect("write consumer");
        server
            .salsa
            .update_file(path, 1, contents.to_string())
            .await
            .expect("index consumer");
    }

    (server, dir)
}

fn url_for(path: &Path) -> Url {
    Url::from_file_path(path).expect("absolute path")
}

fn position(line: u32, character: u32) -> Position {
    Position { line, character }
}

/// Drive the real `textDocument/hover` against `path` at `position`, returning
/// the rendered markdown (or `None` when the handler declines).
async fn hover_at(
    server: &LaravelLanguageServer,
    path: &Path,
    position: Position,
) -> Option<String> {
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: url_for(path) },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let hover = server.hover(params).await.expect("hover handler")?;
    match hover.contents {
        HoverContents::Markup(markup) => Some(markup.value),
        other => panic!("expected markup hover contents, got {other:?}"),
    }
}

/// Drive the real `textDocument/definition` against `path` at `position`.
async fn goto_at(
    server: &LaravelLanguageServer,
    path: &Path,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: url_for(path) },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    server.goto_definition(params).await.expect("goto handler")
}

/// The `(line, character)` pairs a goto response resolves to, sorted, so a test
/// can assert the whole set without depending on index ordering.
fn goto_positions(response: &GotoDefinitionResponse) -> Vec<(PathBuf, u32, u32)> {
    let locations = match response {
        GotoDefinitionResponse::Scalar(loc) => vec![loc.clone()],
        GotoDefinitionResponse::Array(locs) => locs.clone(),
        GotoDefinitionResponse::Link(links) => panic!("unexpected link response: {links:?}"),
    };
    let mut out: Vec<(PathBuf, u32, u32)> = locations
        .iter()
        .map(|loc| {
            (
                loc.uri.to_file_path().expect("file uri"),
                loc.range.start.line,
                loc.range.start.character,
            )
        })
        .collect();
    out.sort();
    out
}

const APP_NAME_CONSUMER: &str = "<?php\nreturn ['name' => env('APP_NAME', 'Laravel')];\n";

// ── the gate actually routes to a result ──────────────────────────────────

#[tokio::test]
async fn hover_on_env_key_renders_value_source_and_consumer_count() {
    // The headline: a `.env` buffer is admitted AND reaches a real card. A
    // widened boolean that still fell through to the PHP pattern index would
    // return `None` here and pass a gate-only unit test.
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=Acme\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let card = hover_at(&server, &dir.path().join(".env"), position(0, 2))
        .await
        .expect("hover on a key must render a card");

    assert!(
        card.contains("APP_NAME"),
        "card must name the key: {card:?}"
    );
    assert!(
        card.contains("Acme"),
        "card must carry the effective value: {card:?}"
    );
    assert!(
        card.contains(".env"),
        "card must link the declaring file: {card:?}"
    );
    assert!(
        card.contains("1 reference") && !card.contains("1 references"),
        "card must carry the singular consumer count: {card:?}"
    );
}

#[tokio::test]
async fn goto_definition_on_env_key_jumps_to_its_consumer() {
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=Acme\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let response = goto_at(&server, &dir.path().join(".env"), position(0, 2))
        .await
        .expect("goto on a consumed key must resolve");

    assert!(
        matches!(response, GotoDefinitionResponse::Scalar(_)),
        "exactly one consumer must return a Scalar, got {response:?}"
    );
    let hits = goto_positions(&response);
    assert_eq!(hits.len(), 1, "expected one jump target, got {hits:?}");
    assert_eq!(
        hits[0].0,
        dir.path().join("config/app.php"),
        "must jump into the consuming file, got {hits:?}"
    );
    assert_eq!(hits[0].1, 1, "must jump to the `env()` call's line");
}

// ── cursor placement ──────────────────────────────────────────────────────

#[tokio::test]
async fn cursor_off_the_key_resolves_nothing() {
    // `APP_NAME=Acme` — column 8 is the `=`, column 10 sits in the value, and
    // line 1 is blank. The key span is half-open, so none of the three is on
    // key text and both handlers must decline.
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=Acme\n\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let env = dir.path().join(".env");

    for (line, character, what) in [
        (0, 8, "the `=`"),
        (0, 10, "the value"),
        (1, 0, "a blank line"),
    ] {
        assert!(
            hover_at(&server, &env, position(line, character))
                .await
                .is_none(),
            "hover on {what} must resolve nothing"
        );
        assert!(
            goto_at(&server, &env, position(line, character))
                .await
                .is_none(),
            "goto on {what} must resolve nothing"
        );
    }

    // Control: the same fixture DOES resolve on key text, so the assertions
    // above are discriminating rather than vacuously satisfied.
    assert!(
        hover_at(&server, &env, position(0, 0)).await.is_some(),
        "the fixture must resolve on key text"
    );
}

#[tokio::test]
async fn second_declaration_of_the_same_key_resolves_nothing() {
    // `enumerate_keys_in_source` is first-match-wins per key, so only the first
    // `APP_NAME=` line is a declaration. A cursor on the second one is on text
    // the enumeration never emitted.
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=first\nAPP_NAME=second\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let env = dir.path().join(".env");

    assert!(
        hover_at(&server, &env, position(0, 2)).await.is_some(),
        "the first declaration must resolve"
    );
    assert!(
        hover_at(&server, &env, position(1, 2)).await.is_none(),
        "the re-declared line must resolve nothing"
    );
    assert!(
        goto_at(&server, &env, position(1, 2)).await.is_none(),
        "the re-declared line must resolve nothing for goto either"
    );
}

// ── value resolution ──────────────────────────────────────────────────────

#[tokio::test]
async fn hover_in_a_lower_priority_file_shows_the_winning_value() {
    // The whole ladder, not just its ends: `.env` (2) outranks `.env.local`
    // (1) outranks `.env.example` (0). Hovering the key in the example file
    // must report the value the application actually runs with, and name
    // `.env` as the declaring file — the same ladder `hover_for_env` reads for
    // the reverse direction.
    let (server, dir) = server_with(
        &[
            (".env", "APP_NAME=Acme\n"),
            (".env.local", "APP_NAME=local-only\n"),
            (".env.example", "APP_NAME=placeholder\n"),
        ],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let card = hover_at(&server, &dir.path().join(".env.example"), position(0, 2))
        .await
        .expect("hover in .env.example must render a card");

    assert!(
        card.contains("Acme"),
        "must render the winning `.env` value: {card:?}"
    );
    assert!(
        !card.contains("local-only"),
        "must not render the middle tier's value: {card:?}"
    );
    assert!(
        !card.contains("placeholder"),
        "must not render the outranked value: {card:?}"
    );
}

#[tokio::test]
async fn env_local_outranks_env_example_when_env_is_silent() {
    // The middle rung of the same ladder, exercised on its own: with no `.env`
    // declaration to win, `.env.local` (1) must still beat `.env.example` (0).
    // Without this the priority tests only ever span the two extremes, and a
    // ladder collapsed to "`.env` or anything else" would pass them both.
    let (server, dir) = server_with(
        &[
            (".env", "APP_NAME=Acme\n"),
            (".env.local", "MAIL_FROM=local@example.test\n"),
            (".env.example", "MAIL_FROM=placeholder@example.test\n"),
        ],
        &[],
    )
    .await;
    let card = hover_at(&server, &dir.path().join(".env.example"), position(0, 2))
        .await
        .expect("hover in .env.example must render a card");

    assert!(
        card.contains("local@example.test"),
        "must render the `.env.local` value: {card:?}"
    );
    assert!(
        !card.contains("placeholder@example.test"),
        "must not render the outranked `.env.example` value: {card:?}"
    );
    assert!(
        card.contains(".env.local"),
        "must name `.env.local` as the declaring file: {card:?}"
    );
}

#[tokio::test]
async fn zero_consumer_key_still_hovers_but_has_nowhere_to_jump() {
    // The two handlers diverge here by design: a card is still useful, a jump
    // to nothing is not.
    // Not `*_KEY`: that name matches the sensitive-name pattern, so its value
    // is redacted out of the card and a value assertion could never hold.
    let (server, dir) = server_with(&[(".env", "IDLE_SETTING=idle-value\n")], &[]).await;
    let env = dir.path().join(".env");

    let card = hover_at(&server, &env, position(0, 2))
        .await
        .expect("a key with no consumers must still render a card");
    assert!(
        card.contains("IDLE_SETTING"),
        "card must name the key: {card:?}"
    );
    // A *full* card, not a bare count: dropping the value or the source link
    // in this branch must redden the test.
    assert!(
        card.contains("idle-value"),
        "card must carry the value even with no consumers: {card:?}"
    );
    assert!(
        card.contains(".env"),
        "card must link the declaring file even with no consumers: {card:?}"
    );
    assert!(
        card.contains("0 references"),
        "card must report zero consumers: {card:?}"
    );
    assert!(
        goto_at(&server, &env, position(0, 2)).await.is_none(),
        "a key with no consumers must not resolve a jump target"
    );
}

#[tokio::test]
async fn undefined_key_hovers_with_the_consumer_count() {
    // The buffer declares the key but no env source registers it (an unsaved
    // or unregistered file). The count is the whole reason to hover a key the
    // project cannot resolve, so the not-defined trailer must keep it.
    let dir = TempDir::new().expect("tempdir");
    let server = test_server();
    *server.root_path.write().await = Some(dir.path().to_path_buf());

    let env = dir.path().join(".env");
    std::fs::write(&env, "GHOST_KEY=1\n").expect("write env");
    server
        .documents
        .write()
        .await
        .insert(url_for(&env), ("GHOST_KEY=1\n".to_string(), 1));

    let consumer = dir.path().join("config/app.php");
    std::fs::create_dir_all(consumer.parent().unwrap()).expect("mkdir");
    let php = "<?php\nreturn [env('GHOST_KEY'), env('GHOST_KEY')];\n";
    std::fs::write(&consumer, php).expect("write consumer");
    server
        .salsa
        .update_file(consumer, 1, php.to_string())
        .await
        .expect("index consumer");

    let card = hover_at(&server, &env, position(0, 2))
        .await
        .expect("an unresolvable key must still render a card");
    assert!(
        card.contains("not defined in .env"),
        "must carry the not-defined state: {card:?}"
    );
    assert!(
        card.contains("2 references"),
        "must keep the consumer count on the not-defined card: {card:?}"
    );
}

#[tokio::test]
async fn commented_key_hovers_with_the_commented_out_state() {
    // `enumerate_keys_in_source` classifies a `#` line as "not a declaration",
    // so the commented enumeration is what finds this key. Mirrors how
    // `hover_for_env` reports a commented declaration in the reverse direction.
    let (server, dir) = server_with(
        &[(".env", "# APP_NAME=Acme\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let card = hover_at(&server, &dir.path().join(".env"), position(0, 4))
        .await
        .expect("a commented key must render a card");

    assert!(
        card.contains("APP_NAME"),
        "card must name the key: {card:?}"
    );
    assert!(
        card.contains("commented out"),
        "card must carry the commented-out state: {card:?}"
    );
    assert!(
        !card.contains("Acme"),
        "a commented declaration has no value in effect, so none may be shown: {card:?}"
    );
    assert!(
        card.contains(".env"),
        "card must link the file the commented declaration lives in: {card:?}"
    );
    assert!(
        card.contains("1 reference"),
        "card must still carry the consumer count: {card:?}"
    );
}

#[tokio::test]
async fn goto_definition_on_a_commented_key_still_jumps() {
    // `find_references(SymbolRefData::Env(key), false)` is name-keyed and does
    // not distinguish a commented declaration from an active one, so goto
    // behaves identically on both.
    let (server, dir) = server_with(
        &[(".env", "# APP_NAME=Acme\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let response = goto_at(&server, &dir.path().join(".env"), position(0, 4))
        .await
        .expect("a commented key with a consumer must resolve");
    assert_eq!(goto_positions(&response).len(), 1);
}

// ── two declarations of one key in one file ───────────────────────────────

#[tokio::test]
async fn a_commented_line_above_the_active_one_does_not_answer_for_it() {
    // Commenting the old value out directly above the new one is ordinary
    // `.env` editing. Both declarations carry the same file priority, so no
    // name-keyed lookup can tell them apart — the card must be built from the
    // declaration the cursor is on.
    let (server, dir) = server_with(
        &[(".env", "# APP_NAME=retired-value\nAPP_NAME=current-value\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let env = dir.path().join(".env");

    let active = hover_at(&server, &env, position(1, 2))
        .await
        .expect("the active declaration must render a card");
    assert!(
        !active.contains("commented out"),
        "a live declaration must not be reported as disabled: {active:?}"
    );
    assert!(
        active.contains("current-value"),
        "the active card must carry the active value: {active:?}"
    );
    assert!(
        !active.contains("retired-value"),
        "the commented line's value must not answer for the active one: {active:?}"
    );

    let commented = hover_at(&server, &env, position(0, 4))
        .await
        .expect("the commented declaration must render a card");
    assert!(
        commented.contains("commented out"),
        "the commented line must carry the commented-out state: {commented:?}"
    );
    assert!(
        !commented.contains("current-value") && !commented.contains("retired-value"),
        "a commented declaration has no value in effect: {commented:?}"
    );
}

#[tokio::test]
async fn a_commented_line_below_the_active_one_still_reads_as_commented() {
    // The same file in the other order. Here the *active* line is the one a
    // first-match-wins merge keeps, so hovering the comment below it used to
    // render the active value with no commented-out marker at all — the same
    // defect pointing the other way.
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=current-value\n# APP_NAME=retired-value\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let env = dir.path().join(".env");

    let commented = hover_at(&server, &env, position(1, 4))
        .await
        .expect("the commented declaration must render a card");
    assert!(
        commented.contains("commented out"),
        "the commented line must carry the commented-out state: {commented:?}"
    );
    assert!(
        !commented.contains("current-value"),
        "the active line's value must not answer for the commented one: {commented:?}"
    );

    let active = hover_at(&server, &env, position(0, 2))
        .await
        .expect("the active declaration must render a card");
    assert!(
        !active.contains("commented out"),
        "a live declaration must not be reported as disabled: {active:?}"
    );
    assert!(
        active.contains("current-value"),
        "the active card must carry the active value: {active:?}"
    );
}

#[tokio::test]
async fn a_commented_card_links_the_file_the_comment_is_in() {
    // The commented declaration lives in `.env.example`; a live one of the same
    // key lives in `.env` and outranks it. The card describes the line under
    // the cursor, so it must link the file that line is in — a name-keyed
    // lookup would have linked `.env` and quoted its value.
    let (server, dir) = server_with(
        &[
            (".env", "APP_NAME=live-value\n"),
            (".env.example", "# APP_NAME=placeholder\n"),
        ],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;

    let card = hover_at(&server, &dir.path().join(".env.example"), position(0, 4))
        .await
        .expect("the commented declaration must render a card");
    assert!(
        card.contains("commented out"),
        "the commented line must carry the commented-out state: {card:?}"
    );
    assert!(
        card.contains(".env.example"),
        "the card must link the file the comment is in: {card:?}"
    );
    assert!(
        !card.contains("live-value") && !card.contains("placeholder"),
        "a commented declaration has no value in effect: {card:?}"
    );
}

#[tokio::test]
async fn the_reverse_direction_reads_the_same_pair_the_same_way() {
    // `env('APP_NAME')` in PHP resolves through the same merge. A commented
    // line above the active one must not make the reverse card report the key
    // as switched off either — one tie-break, both directions.
    let (server, _dir) = server_with(
        &[(".env", "# APP_NAME=retired-value\nAPP_NAME=current-value\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;

    let card = server.hover_for_env("APP_NAME").await;
    assert!(
        !card.contains("commented out"),
        "the PHP-side card must not report a live key as disabled: {card:?}"
    );
    assert!(
        card.contains("current-value"),
        "the PHP-side card must carry the active value: {card:?}"
    );
}

#[tokio::test]
async fn the_whole_table_merge_keeps_the_active_declaration_too() {
    // The by-name lookup is not the only merge: `get_all_parsed_env_vars`
    // builds the table completion and the disk cache read, and it resolves
    // ties with the same rule. Driven through the real Salsa actor so this
    // pins the call site, not the rule in isolation.
    let (server, _dir) = server_with(
        &[(".env", "# APP_NAME=retired-value\nAPP_NAME=current-value\n")],
        &[],
    )
    .await;

    let vars = server
        .salsa
        .get_all_parsed_env_vars()
        .await
        .expect("the merged env table");
    let app_name: Vec<_> = vars.iter().filter(|v| v.name == "APP_NAME").collect();
    assert_eq!(app_name.len(), 1, "the table merges by name: {app_name:?}");
    assert!(
        !app_name[0].is_commented,
        "the comment must not answer for the key: {:?}",
        app_name[0]
    );
    assert_eq!(app_name[0].value, "current-value");
}

#[tokio::test]
async fn a_key_commented_out_in_the_winning_file_has_no_value_in_effect() {
    // `.env` (2) outranks `.env.local` (1), and `.env` comments the key out —
    // switching a key off in `.env` is how a project disables it. The active
    // declaration under the cursor is real, so the card must not claim it is
    // commented; but the outranked value is not what the application runs
    // with, so it must not be presented as the effective one either.
    let (server, dir) = server_with(
        &[
            (".env", "# APP_NAME=disabled-in-env\n"),
            (".env.local", "APP_NAME=local-value\n"),
        ],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;

    let card = hover_at(&server, &dir.path().join(".env.local"), position(0, 2))
        .await
        .expect("the active declaration must still render a card");
    assert!(
        card.contains("not defined in .env"),
        "no declaration is in effect, so the card must say so: {card:?}"
    );
    assert!(
        !card.contains("local-value"),
        "an outranked value must not be shown as the effective one: {card:?}"
    );
    assert!(
        !card.contains("commented out"),
        "the line under the cursor is active: {card:?}"
    );
    assert!(
        card.contains("1 reference"),
        "the consumer count must survive the not-defined state: {card:?}"
    );
}

// ── multiple consumers ────────────────────────────────────────────────────

#[tokio::test]
async fn several_consumers_return_every_location_not_just_the_first() {
    let (server, dir) = server_with(
        &[(".env", "APP_NAME=Acme\n")],
        &[
            ("config/app.php", APP_NAME_CONSUMER),
            (
                "config/mail.php",
                "<?php\nreturn [\n    'from' => env('APP_NAME'),\n    'alt' => env('APP_NAME'),\n];\n",
            ),
        ],
    )
    .await;
    let response = goto_at(&server, &dir.path().join(".env"), position(0, 2))
        .await
        .expect("goto must resolve");

    assert!(
        matches!(response, GotoDefinitionResponse::Array(_)),
        "more than one consumer must return an Array, got {response:?}"
    );
    let hits = goto_positions(&response);
    assert_eq!(
        hits.len(),
        3,
        "every consumer must be returned, not just the first: {hits:?}"
    );
    assert_eq!(
        hits.iter().filter(|h| h.0.ends_with("mail.php")).count(),
        2,
        "both `mail.php` call sites must appear: {hits:?}"
    );

    // The hover count is sourced from the same call, so it must agree.
    let card = hover_at(&server, &dir.path().join(".env"), position(0, 2))
        .await
        .expect("hover must render");
    assert!(
        card.contains("3 references"),
        "hover count must match the goto target count: {card:?}"
    );
}

// ── the gate must not widen too far ───────────────────────────────────────

#[tokio::test]
async fn non_laravel_env_shaped_files_are_still_declined() {
    // `is_env_file_name` matches `.env` exactly or a `.env.` variant prefix.
    // These three are what a looser `starts_with(".env")` / `contains(".env")`
    // spelling would have admitted; each holds a syntactically valid
    // declaration, so only the gate can be what turns them away.
    let (server, dir) = server_with(&[(".env", "APP_NAME=Acme\n")], &[]).await;

    for name in [".envrc", ".environment", "my.env.local"] {
        let path = dir.path().join(name);
        let contents = "APP_NAME=Acme\n";
        std::fs::write(&path, contents).expect("write fixture");
        server
            .documents
            .write()
            .await
            .insert(url_for(&path), (contents.to_string(), 1));

        assert!(
            hover_at(&server, &path, position(0, 2)).await.is_none(),
            "{name} must not be admitted by the env hover gate"
        );
        assert!(
            goto_at(&server, &path, position(0, 2)).await.is_none(),
            "{name} must not be admitted by the env goto gate"
        );
    }
}

// ── explicitly out of scope ───────────────────────────────────────────────

#[tokio::test]
async fn find_all_references_stays_php_only_on_env_buffers() {
    // The reference code lens (`editor.action.showReferences`) remains the only
    // "find consumers" entry point for `.env` keys, consistent with how config
    // and translation keys already work. Pinned so widening `references` later
    // is a deliberate decision rather than a silent side effect.
    use tower_lsp::lsp_types::{ReferenceContext, ReferenceParams};

    let (server, dir) = server_with(
        &[(".env", "APP_NAME=Acme\n")],
        &[("config/app.php", APP_NAME_CONSUMER)],
    )
    .await;
    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: url_for(&dir.path().join(".env")),
            },
            position: position(0, 2),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: false,
        },
    };
    assert!(
        server
            .references(params)
            .await
            .expect("references handler")
            .is_none(),
        "textDocument/references must stay `.php`-only on env buffers"
    );
}

// ── secrets stay redacted here too ────────────────────────────────────────

#[tokio::test]
async fn sensitive_values_are_redacted_in_the_env_buffer_card() {
    // The value is already on screen in the buffer, which is exactly the
    // argument that would leave this card as the one un-redacted env surface.
    // It is LSP output like any other (issues #344, #348), so it applies both
    // of `hover_for_env`'s guards: the name match drops the value outright,
    // and an unmatched name still gets its URL credentials masked.
    let (server, dir) = server_with(
        &[(
            ".env",
            "DB_PASSWORD=hunter2\nDATABASE_URL=mysql://root:hunter2@localhost/app\n",
        )],
        &[],
    )
    .await;
    let env = dir.path().join(".env");

    let by_name = hover_at(&server, &env, position(0, 2))
        .await
        .expect("a secret-named key must still render a card");
    assert!(
        !by_name.contains("hunter2"),
        "the name-matched secret must not reach the card: {by_name:?}"
    );
    assert!(
        by_name.contains(laravel_lsp::completion_display::REDACTED_ENV_VALUE),
        "the card must say why the value is missing: {by_name:?}"
    );

    let by_shape = hover_at(&server, &env, position(1, 2))
        .await
        .expect("DATABASE_URL must still render a card");
    assert!(
        !by_shape.contains("hunter2"),
        "an unmatched name must still have its URL password masked: {by_shape:?}"
    );
    assert!(
        by_shape.contains("mysql://"),
        "masking must keep the rest of the URL readable: {by_shape:?}"
    );
}
