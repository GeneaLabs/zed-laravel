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
//! These drive the **real** `did_close` notification handler on the
//! `LspService::new` harness (the seam `watched_files_magic.rs` uses), not the
//! `SalsaHandle` method underneath it: a unit test of the release would prove
//! nothing about whether close calls it.

use crate::LaravelLanguageServer;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{DidCloseTextDocumentParams, TextDocumentIdentifier, Url};
use tower_lsp::{LanguageServer, LspService};

const COMPOSER: &str = r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#;

async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
}

fn write_file(root: &Path, rel: &str, src: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, src).unwrap();
    path
}

/// Install `content` as the live editor buffer for `path` — `self.documents`
/// plus the Salsa input, which is both halves of what `did_open` does with a
/// buffer, and the pair `did_close` has to undo.
async fn open_buffer(backend: &LaravelLanguageServer, path: &Path, content: &str) -> Url {
    let uri = Url::from_file_path(path).unwrap();
    backend
        .documents
        .write()
        .await
        .insert(uri.clone(), (content.to_string(), 1));
    backend
        .salsa
        .update_file(path.to_path_buf(), 1, content.to_string())
        .await
        .expect("the buffer reaches Salsa, exactly as did_open pushes it");
    uri
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
