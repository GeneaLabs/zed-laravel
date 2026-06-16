//! Handler-level tests for the Blade variable rename wiring (issue #95).
//!
//! The pure scope/admissibility logic — `is_template_variable`,
//! `cursor_in_unresolved_loop`, `in_scope_spans`, `variable_spans` — is already
//! covered exhaustively by the `blade_var_rename` unit tests. What those tests
//! can't catch is a *wiring inversion* in the async `main.rs` handlers that
//! compose them: `prepare_blade_var_rename` and `blade_var_rename_edit` read the
//! document, extract the variable under the cursor, and gate on those pure
//! functions in a specific order. If the `is_template_variable` gate were
//! bypassed or reversed, or the unresolved-loop refusal dropped, every pure-
//! function test would still pass while the handler shipped a broken rename.
//!
//! These tests drive the real handlers on a server built through the same
//! `tower_lsp::LspService` harness `folio_rename.rs` / `folio_cursor_containment.rs`
//! use. Documents are left unopened, so each handler resolves its `file://` URI
//! through the filesystem-fallback path in `rename_document_text` — reading a
//! `tempfile` instead of an editor buffer.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tower_lsp::lsp_types::{Position, Url};
use tower_lsp::LspService;

/// Build a backend for handler-level tests: `LspService::new` wires up a real
/// `Client`, and `inner().clone()` hands back the `LaravelLanguageServer` so we
/// can call its private handlers. The document cache starts empty, so the rename
/// handlers take the filesystem-fallback path. This mirrors the existing
/// `test_server()` harness in `folio_rename.rs`; the Salsa actor it spawns is
/// inert here — the rename handlers under test never touch it.
fn minimal_backend() -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    service.inner().clone()
}

/// Write `content` to a `.blade.php` file under `dir` and return its `file://`
/// URL — the input the handlers receive, resolved back to a path and read off
/// disk by the filesystem-fallback path (the document cache is empty).
fn blade_file(dir: &Path, name: &str, content: &str) -> Url {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    Url::from_file_path(&path).expect("tempfile path is absolute")
}

#[tokio::test]
async fn prepare_rejects_php_only_variable() {
    // `$secret` lives only inside an `@php … @endphp` block — no `@foreach`/
    // `@forelse`, no controller binding context — so it never surfaces in
    // template markup. The `is_template_variable` gate must reject it and the
    // handler return `None`. A wiring inversion that bypassed or reversed that
    // gate would (wrongly) offer F2 here.
    let dir = TempDir::new().unwrap();
    let backend = minimal_backend();
    let src = "@php\n    $secret = compute();\n@endphp\n{{ $user }}\n";
    let uri = blade_file(dir.path(), "php-only.blade.php", src);

    // Cursor on `$secret` (line 1, mid-name).
    let pos = Position {
        line: 1,
        character: 8,
    };
    let result = backend.prepare_blade_var_rename(&uri, pos).await;
    assert_eq!(
        result, None,
        "a variable confined to @php…@endphp is not a renameable Blade variable"
    );
}

#[tokio::test]
async fn prepare_accepts_file_scoped_variable_and_excludes_the_sigil() {
    // `$name` is a plain file-scoped Blade variable (the kind a controller
    // passes into the view) echoed in markup — renameable. The returned range
    // must cover the bare name, never the leading `$` sigil.
    let dir = TempDir::new().unwrap();
    let backend = minimal_backend();
    let src = "<h1>{{ $name }}</h1>\n";
    let uri = blade_file(dir.path(), "file-scoped.blade.php", src);

    // Cursor on `$name` (line 0, on the `a`).
    let pos = Position {
        line: 0,
        character: 9,
    };
    let range = backend
        .prepare_blade_var_rename(&uri, pos)
        .await
        .expect("a file-scoped Blade variable is renameable");

    assert_eq!(range.start.line, 0);
    assert_eq!(range.end.line, 0);

    // The highlighted span is exactly the name…
    let line = src.lines().next().unwrap();
    let slice: String = line
        .chars()
        .skip(range.start.character as usize)
        .take((range.end.character - range.start.character) as usize)
        .collect();
    assert_eq!(slice, "name", "the rename range covers the bare name");

    // …and the character immediately before it is the `$` sigil, proving the
    // range excludes it.
    assert_eq!(
        line.chars().nth(range.start.character as usize - 1),
        Some('$'),
        "the `$` sigil sits just before the range and is not part of it"
    );
}

#[tokio::test]
async fn prepare_rejects_cursor_in_unresolved_loop() {
    // An opaque `@foreach`: the header has no resolvable ` as $var` binding, so
    // the loop's scope can't be determined. Even though `$user` *does* surface
    // in markup (so the `is_template_variable` gate passes), a cursor inside the
    // loop must be refused by the `cursor_in_unresolved_loop` gate — a
    // scope-local rename there risks a file-wide clobber. (Issue #166 reworked
    // this fixture off a comment-embedded `)`, which now parses correctly.)
    let dir = TempDir::new().unwrap();
    let backend = minimal_backend();
    let src = "{{ $user }}\n\
               @foreach ($users)\n\
               \x20   {{ $user }}\n\
               @endforeach\n\
               {{ $user }}\n";
    let uri = blade_file(dir.path(), "opaque-loop.blade.php", src);

    // Cursor inside the loop body (line 2) on `$user`.
    let pos = Position {
        line: 2,
        character: 8,
    };
    let result = backend.prepare_blade_var_rename(&uri, pos).await;
    assert_eq!(
        result, None,
        "rename must be refused inside an unresolved loop region"
    );
}

#[tokio::test]
async fn rename_edit_rewrites_in_scope_and_skips_loop_rebinding() {
    // A file-level `$item` (lines 0 and 4) plus a `@foreach` that re-binds
    // `$item` (lines 1–2, a distinct scope). Renaming from the file-level cursor
    // must rewrite ONLY the file-scoped occurrences and leave the loop's
    // shadowing `$item` untouched — the WorkspaceEdit carries TextEdits for
    // every in-scope occurrence and none outside scope.
    let dir = TempDir::new().unwrap();
    let backend = minimal_backend();
    let src = "{{ $item }}\n\
               @foreach ($items as $item)\n\
               \x20   {{ $item }}\n\
               @endforeach\n\
               {{ $item }}\n";
    let uri = blade_file(dir.path(), "file-scope-edit.blade.php", src);

    // Cursor on the file-level `$item` (line 0).
    let pos = Position {
        line: 0,
        character: 5,
    };
    let edit = backend
        .blade_var_rename_edit(&uri, pos, "thing")
        .await
        .expect("a valid identifier must not error")
        .expect("a file-scoped Blade rename produces a WorkspaceEdit");

    // A text-only rename populates the legacy `changes` map, keyed by URI.
    let changes = edit
        .changes
        .expect("a text-only rename populates the `changes` map");
    let file_edits = changes
        .get(&uri)
        .expect("edits target the blade file under the cursor");

    let mut lines: Vec<u32> = file_edits.iter().map(|e| e.range.start.line).collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![0, 4],
        "only the file-scoped occurrences rewrite; the loop's re-bound $item is skipped"
    );
    assert!(
        file_edits.iter().all(|e| e.new_text == "thing"),
        "every in-scope edit writes the new name"
    );
}
