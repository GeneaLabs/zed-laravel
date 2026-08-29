//! Regression: `textDocument/didClose` must relinquish the external-PHP
//! loader ownership that `didOpen`/`didChange` acquired — without evicting
//! anything.
//!
//! `handle_update_file` stamps `ExternalPhpText::PushedByClient` on every push
//! so `ensure_external_php_source_loaded` never reads disk over an unsaved
//! edit. That acquire had no matching release. Close a backing component class
//! with its changes DISCARDED and the editor reverts its buffer in memory: no
//! filesystem write happens, so no `didChangeWatchedFiles` event fires either —
//! and the stamp, which is unconditional authority, went on serving text that
//! existed neither on disk nor in any open buffer, until the file happened to
//! be reopened.
//!
//! The release is deliberately NOT a `RemoveFile`. Eviction on close would
//! zero the resolved magic-member entries only warm/save passes rebuild, which
//! is exactly why `did_close` refuses to call it; the ownership downgrade and
//! the eviction question have to stay separate. Both halves of that claim are
//! pinned here — the downgrade happens, and nothing else does.
//!
//! The release is also counted, not flagged. tower-lsp runs notification
//! handlers concurrently, so a reopened buffer's `didOpen` can reach the Salsa
//! actor before the `didClose` it followed at the client — and a release that
//! fired there would strip a LIVE buffer's ownership, which is the same defect
//! reached from the other side. `did_open` counts its buffer in and `did_close`
//! counts it out, so the pair settles the same way whichever order it arrives
//! in.
//!
//! These drive the **real** `did_open` and `did_close` notification handlers on
//! the `LspService::new` harness (the seam `watched_files_magic.rs` uses), not
//! the `SalsaHandle` methods underneath them: a unit test of either edge would
//! prove nothing about whether the handlers call it.

use crate::LaravelLanguageServer;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, TextDocumentIdentifier,
    TextDocumentItem, Url,
};
use tower_lsp::{LanguageServer, LspService};

const COMPOSER: &str = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;

async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    // The actor keeps its OWN root, and the backing-class loader's containment
    // guard reads it (#364). Production always sets the two together — via
    // `register_config_with_salsa` or the cached-config request — so a fixture
    // that sets only `root_path` is under-registering, and every load below
    // would fail closed for want of a root rather than for the reason the test
    // is about.
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("actor registers the tempdir project root");
    backend
}

fn write_file(root: &Path, rel: &str, src: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, src).unwrap();
    path
}

/// Drive the real `textDocument/didOpen` handler for `path` with `content` as
/// the buffer — the acquire `did_close` has to undo.
///
/// Driving the handler rather than re-spelling its Salsa calls here is the
/// point: a helper that pushed the buffer itself would keep passing after
/// `did_open` stopped counting its buffer in, and the ownership count is half
/// of what these tests exist to pin.
async fn open_buffer(backend: &LaravelLanguageServer, path: &Path, content: &str) -> Url {
    let uri = Url::from_file_path(path).unwrap();
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "php".to_string(),
                version: 1,
                text: content.to_string(),
            },
        })
        .await;
    uri
}

/// Install `content` as `path`'s Salsa text with NO open document — what a
/// `didChangeWatchedFiles` push does. It takes the same `PushedByClient`
/// stamp any client push does, but no buffer is counted in behind it.
async fn push_without_open(backend: &LaravelLanguageServer, path: &Path, content: &str) {
    backend
        .salsa
        .update_file(path.to_path_buf(), 1, content.to_string())
        .await
        .expect("the watcher's own read reaches Salsa");
}

/// Drive the real `textDocument/didClose` handler for `uri`.
async fn close_document(backend: &LaravelLanguageServer, uri: Url) {
    backend
        .did_close(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        })
        .await;
}

// ---- the release edge ----------------------------------------------------

/// A Livewire v3 component whose class declares `increment()` on line 10.
const SAVED_CLASS: &str = "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n\n    public function increment(): void\n    {\n        $this->count++;\n    }\n}\n";

/// The same class as an UNSAVED buffer that renamed the method. The rename is
/// the whole test: with disk and buffer agreeing, both a released and an
/// unreleased path answer identically and the assertions below prove nothing.
const DISCARDED_BUFFER: &str = "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n\n    public function decrement(): void\n    {\n        $this->count--;\n    }\n}\n";

/// A third spelling of the same class, for the reopen in the race tests. Three
/// distinct method names keep the three states apart: `increment` is on disk,
/// `decrement` was the buffer that closed, `multiply` is the buffer that is
/// open now. Reusing one buffer constant for both opens could not tell "the
/// reopened buffer answers" from "the first push's text was never replaced".
const REOPENED_BUFFER: &str = "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n\n    public function multiply(): void\n    {\n        $this->count *= 2;\n    }\n}\n";

/// Write the v3 component pair and return `(class, blade)`.
fn write_v3_component(root: &Path) -> (PathBuf, PathBuf) {
    let class = write_file(root, "app/Livewire/Counter.php", SAVED_CLASS);
    let blade = write_file(
        root,
        "resources/views/livewire/counter.blade.php",
        "<div><button wire:click=\"increment\">+</button></div>\n",
    );
    (class, blade)
}

#[tokio::test]
async fn a_discarded_buffer_stops_answering_once_its_document_closes() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());

    // The rename exists only in the editor. Nothing is written to disk here,
    // and nothing is written to disk anywhere in this test after this point —
    // which is the sequence with no `didChangeWatchedFiles` to fall back on.
    let uri = open_buffer(&backend, &class, DISCARDED_BUFFER).await;

    // Seed. The buffer OWNS the path while it is open, so resolution must
    // answer out of it — `decrement` resolves and the saved `increment` does
    // not. Without this pair the closing assertions cannot tell a released
    // stamp from a buffer that was never installed.
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_some(),
        "seed: the open buffer's method must resolve — the push took ownership",
    );
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "increment")
            .await
            .is_none(),
        "seed: the saved method must NOT resolve while the buffer owns the path",
    );

    // The user discards the changes and closes the tab. No disk write.
    close_document(&backend, uri).await;

    let (path, loc) = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("with the buffer gone, the SAVED method must resolve again");
    assert_eq!(path, class, "resolution reaches the standalone class");
    assert_eq!(loc.line, 10, "the saved declaration's line");
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_none(),
        "the discarded buffer's method must stop resolving — it exists neither \
         on disk nor in any open buffer",
    );
}

/// The release is keyed on the closed path alone, so a SECOND file's live
/// buffer must survive its neighbour's close. Without this, a release that
/// cleared the whole map would pass the test above.
#[tokio::test]
async fn closing_one_document_leaves_another_buffers_ownership_intact() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());
    let bystander = write_file(dir.path(), "app/Livewire/Other.php", SAVED_CLASS);

    let closed = open_buffer(&backend, &bystander, DISCARDED_BUFFER).await;
    open_buffer(&backend, &class, DISCARDED_BUFFER).await;

    close_document(&backend, closed).await;

    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_some(),
        "the still-open buffer keeps its ownership — only the closed path is \
         handed back",
    );
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "increment")
            .await
            .is_none(),
        "and disk must not be read back over it",
    );
}

// ---- the release must not outrun a reopen -------------------------------

/// A close must not release a path that a LATER open has already claimed.
///
/// tower-lsp drives up to four notification handlers concurrently, so the
/// `didOpen` of a reopened buffer — a revert, a tab restored, Zed's
/// multibuffer lifecycle — can reach the Salsa actor before the `didClose`
/// that preceded it at the client. Calling the handlers in that order IS that
/// interleaving: the actor sees the reopen's messages first and the close
/// last.
///
/// Releasing there is not a stale answer, it is a write: an unowned path sends
/// the loader's next read into `set_text` on the shared `SourceFile` every
/// per-file query reads, so a `$this->member` hover in the Blade view would
/// revert the PHP buffer's text to disk.
#[tokio::test]
async fn a_reopen_that_overtakes_its_close_keeps_the_buffers_ownership() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());

    let closing = open_buffer(&backend, &class, DISCARDED_BUFFER).await;
    // The reopen's didOpen wins the race to the actor...
    open_buffer(&backend, &class, REOPENED_BUFFER).await;
    // ...and the close it should have followed arrives late.
    close_document(&backend, closing).await;

    assert!(
        backend
            .locate_in_backing_class_files(&blade, "multiply")
            .await
            .is_some(),
        "the reopened buffer is live and still owns the path — the late close \
         belongs to a buffer that no longer exists",
    );
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "increment")
            .await
            .is_none(),
        "disk must not be read back over the live buffer",
    );
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_none(),
        "and the closed buffer's own text is gone — the reopen replaced it",
    );
}

/// The count must not strand ownership: the reopened buffer's own close still
/// hands the path back. Without this, refusing the late close above could be
/// spelled as never releasing at all.
#[tokio::test]
async fn the_reopened_buffer_releases_when_it_closes_in_turn() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());

    let closing = open_buffer(&backend, &class, DISCARDED_BUFFER).await;
    let reopened = open_buffer(&backend, &class, REOPENED_BUFFER).await;
    close_document(&backend, closing).await;

    close_document(&backend, reopened).await;

    let (path, loc) = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("with the last buffer gone, the SAVED method must resolve again");
    assert_eq!(path, class, "resolution reaches the standalone class");
    assert_eq!(loc.line, 10, "the saved declaration's line");
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "multiply")
            .await
            .is_none(),
        "the reopened buffer's method must stop resolving once it closes too",
    );
}

/// A `didChangeWatchedFiles` push stamps ownership with no open buffer behind
/// it, so a close for that path finds nothing counted in. It must still hand
/// the path back: an absent count is zero, and the original defect — text
/// nothing holds, served forever — is what zero has to fall toward.
#[tokio::test]
async fn a_close_releases_a_path_no_open_ever_counted() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());

    push_without_open(&backend, &class, DISCARDED_BUFFER).await;
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_some(),
        "seed: the push owns the path, exactly as a buffer's would",
    );

    close_document(&backend, Url::from_file_path(&class).unwrap()).await;

    assert!(
        backend
            .locate_in_backing_class_files(&blade, "increment")
            .await
            .is_some(),
        "the uncounted push is still handed back on close — disk answers again",
    );
    assert!(
        backend
            .locate_in_backing_class_files(&blade, "decrement")
            .await
            .is_none(),
        "and its text stops answering",
    );
}

/// `didClose` arrives for every closed document, including ones with no file
/// path at all (an unsaved scratch buffer). The release must not assume one.
#[tokio::test]
async fn closing_a_document_with_no_file_path_is_a_no_op() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;

    close_document(&backend, Url::parse("untitled:Untitled-1").unwrap()).await;
}

// ---- what the release must NOT do ----------------------------------------

/// `Post` both declares an accessor and reads it via `$this->headline`, so it
/// carries magic-member entries and a magic-dependency contribution OF ITS
/// OWN — the state `RemoveFile` purges and `did_close` must not.
const POST: &str = "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass Post extends Model {\n    public function getHeadlineAttribute(): string { return ''; }\n    public function describe(): string { return $this->headline; }\n}\n";

/// A controller reading `$post->headline` off a typed `Post` param — resolvable
/// only while `Post` declares the accessor, which makes it a dependent.
const CONSUMER: &str = "<?php\nnamespace App\\Http\\Controllers;\nuse App\\Models\\Post;\nclass PostController {\n    public function show(Post $post) {\n        return [$post->headline];\n    }\n}\n";

/// Register a file in Salsa and run the per-file magic refresh — what the save
/// path does, so entries + receiver deps land in the live indexes.
async fn seed(backend: &LaravelLanguageServer, path: &Path, src: &str) {
    backend
        .salsa
        .update_file(path.to_path_buf(), 1, src.to_string())
        .await
        .unwrap();
    backend.refresh_file_magic(path, src).await;
}

/// The resolved magic-member *member names* the index holds for `path`.
async fn member_names(backend: &LaravelLanguageServer, path: &Path) -> HashSet<String> {
    backend
        .salsa
        .export_magic_members()
        .await
        .unwrap()
        .get(path)
        .map(|entries| entries.iter().map(|e| e.member.clone()).collect())
        .unwrap_or_default()
}

/// Does the magic-dependency index still hold a `by_file` entry for `path`?
fn magic_deps_has(backend: &LaravelLanguageServer, path: &Path) -> bool {
    backend
        .magic_deps
        .read()
        .unwrap()
        .export()
        .iter()
        .any(|(p, _)| p == path)
}

#[tokio::test]
async fn closing_a_document_keeps_its_resolved_magic_members() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post = write_file(root, "app/Models/Post.php", POST);
    let consumer = write_file(root, "app/Http/Controllers/PostController.php", CONSUMER);
    seed(&backend, &post, POST).await;
    seed(&backend, &consumer, CONSUMER).await;

    assert!(
        member_names(&backend, &post).await.contains("headline"),
        "seed: Post resolves its own `$this->headline`",
    );
    assert!(
        member_names(&backend, &consumer).await.contains("headline"),
        "seed: the dependent resolves `$post->headline`",
    );
    assert!(
        magic_deps_has(&backend, &post),
        "seed: Post has a magic-dependency contribution",
    );

    // The model buffer closes — what happens when the references multibuffer
    // opens over it. `RemoveFile` here would zero every assertion below, and
    // only a warm or a save would ever rebuild them.
    let uri = open_buffer(&backend, &post, POST).await;
    close_document(&backend, uri).await;

    assert!(
        member_names(&backend, &post).await.contains("headline"),
        "the closed file's own resolved entries must survive — close is not \
         a delete",
    );
    assert!(
        member_names(&backend, &consumer).await.contains("headline"),
        "and the dependent must still resolve through it",
    );
    assert!(
        magic_deps_has(&backend, &post),
        "the closed file's magic-dependency contribution must survive too",
    );
}

// ---- the text the release leaves behind ----------------------------------

/// The same class again, distinguished by the view name its `render()` returns
/// rather than by a method name: `handle_get_patterns` reports `view(...)`
/// call sites, so a view name is what makes the two texts tell apart through
/// the pattern path rather than the backing-class path.
const SAVED_CLASS_RENDERING: &str = "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public function render()\n    {\n        return view('livewire.from-disk');\n    }\n}\n";

const DISCARDED_BUFFER_RENDERING: &str = "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public function render()\n    {\n        return view('livewire.from-buffer');\n    }\n}\n";

/// The view names `get_patterns` currently reports for `path`.
async fn pattern_view_names(backend: &LaravelLanguageServer, path: &Path) -> HashSet<String> {
    backend
        .salsa
        .get_patterns(path.to_path_buf())
        .await
        .unwrap()
        .map(|data| data.views.iter().map(|v| v.name.clone()).collect())
        .unwrap_or_default()
}

/// Releasing ownership un-blocks the LOADER, but the loader is not the only
/// reader of the text a discarded buffer installed. `handle_get_patterns`,
/// `handle_get_document_symbols`, `handle_get_loop_blocks` and
/// `handle_get_php_assignments` all read `files[path]` directly, and the
/// pattern cache is checked with no version comparison at all — so a
/// buffer-derived entry there is served until something else evicts it.
/// Find-references answering out of that entry names a symbol that exists in
/// no file at all.
///
/// The release therefore drops the text it hands back, not merely the claim
/// on it: every reader re-derives from disk on its next question. Still lazy —
/// nothing is read at close time.
#[tokio::test]
async fn closing_a_document_stops_every_reader_serving_the_discarded_text() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let class = write_file(
        dir.path(),
        "app/Livewire/Counter.php",
        SAVED_CLASS_RENDERING,
    );

    let uri = open_buffer(&backend, &class, DISCARDED_BUFFER_RENDERING).await;

    // Populate the pattern cache from the buffer, exactly as a diagnostics or
    // find-references pass would while the document is open.
    assert!(
        pattern_view_names(&backend, &class)
            .await
            .contains("livewire.from-buffer"),
        "precondition: the open buffer's text is what the pattern path reports",
    );

    // Changes discarded, document closed. Nothing is written to disk, so no
    // `didChangeWatchedFiles` arrives to correct anything.
    close_document(&backend, uri).await;

    let names = pattern_view_names(&backend, &class).await;
    assert!(
        names.contains("livewire.from-disk"),
        "after the close every reader must see the file on disk, got {names:?}",
    );
    assert!(
        !names.contains("livewire.from-buffer"),
        "the discarded buffer's text must not survive its document, got {names:?}",
    );
}

// ---- the inverted index is a reader too ----------------------------------

/// The files the inverted symbol index reports as referencing view `name`.
async fn view_reference_files(backend: &LaravelLanguageServer, name: &str) -> HashSet<PathBuf> {
    backend
        .salsa
        .find_references(
            laravel_lsp::salsa_impl::SymbolRefData::View(name.to_string()),
            false,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|loc| loc.file_path)
        .collect()
}

/// `find_references` does not read `files[path]`; it answers from
/// `symbol_index`, refreshed lazily from the paths `mark_dirty` queued. A
/// query run while the buffer is open DRAINS that queue, so the index ends up
/// holding the buffer's literals with the dirty flag cleared — and dropping
/// the Salsa input on close cannot reach it. The view name the discarded
/// buffer introduced then keeps answering find-references and code lenses
/// forever, pointing at a file that never contained it.
///
/// The release therefore re-queues the path on the three deferred indexes,
/// exactly as `handle_update_file` does for any other text change. Queueing is
/// not eviction: the drain runs `remove_literal_entries` + `insert_file`, which
/// deliberately preserves the resolved magic-member entries `did_close` exists
/// to protect.
#[tokio::test]
async fn closing_a_document_stops_the_index_reporting_the_discarded_views() {
    let dir = TempDir::new().unwrap();
    let backend = backend_for(dir.path()).await;
    let class = write_file(
        dir.path(),
        "app/Livewire/Counter.php",
        SAVED_CLASS_RENDERING,
    );

    let uri = open_buffer(&backend, &class, DISCARDED_BUFFER_RENDERING).await;

    // Drain the dirty queue WHILE the buffer is open — a find-references or a
    // code lens during the edit session. Without this the release would look
    // correct for the wrong reason: the still-queued path would be refreshed
    // on the first post-close query whether or not the release re-queued it.
    assert!(
        view_reference_files(&backend, "livewire.from-buffer")
            .await
            .contains(&class),
        "precondition: a query during the edit indexes the buffer's literals",
    );

    close_document(&backend, uri).await;

    let phantom = view_reference_files(&backend, "livewire.from-buffer").await;
    assert!(
        phantom.is_empty(),
        "the discarded buffer's view reference must stop answering, got {phantom:?}",
    );
    assert!(
        view_reference_files(&backend, "livewire.from-disk")
            .await
            .contains(&class),
        "and the file on disk must answer in its place",
    );
}

/// The other half of the same edge: re-queueing must not cost the resolved
/// magic-member entries, which no re-parse can restore. The drain runs
/// `remove_literal_entries` + `insert_file`, never `remove_file`, so the two
/// requirements coexist — but only a test that actually FORCES the drain after
/// the close can say so.
#[tokio::test]
async fn the_index_requeue_on_close_keeps_resolved_magic_members() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let backend = backend_for(root).await;
    write_file(root, "composer.json", COMPOSER);

    let post = write_file(root, "app/Models/Post.php", POST);
    seed(&backend, &post, POST).await;
    let before = member_names(&backend, &post).await;
    assert!(
        before.contains("headline"),
        "precondition: Post has resolved magic-member entries",
    );

    let uri = open_buffer(&backend, &post, POST).await;
    close_document(&backend, uri).await;

    // Force the deferred drain the release just queued. A destructive refresh
    // would zero the entries here, where the sibling test above would still
    // pass — it never makes the drain run.
    let _ = view_reference_files(&backend, "no.such.view").await;

    assert_eq!(
        member_names(&backend, &post).await,
        before,
        "re-queueing the path must not zero what only warm/save can rebuild",
    );
}
