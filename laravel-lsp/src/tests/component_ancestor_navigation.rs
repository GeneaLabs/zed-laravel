//! Member navigation from inside an anonymous Blade partial (#339, item 1).
//!
//! `resources/views/components/save-button.blade.php` has no backing class of
//! its own: nobody renders it through `view('components.save-button')`, it does
//! not live under Livewire's view path, and it declares no inline class. Its
//! `wire:click="save"` nevertheless resolves at runtime, against the component
//! that rendered it — so the resolver climbs the usage graph (`<x-…>` and
//! `<livewire:…>` tags alike) and answers with the nearest ancestor that HAS a
//! backing class.
//!
//! Every test drives a real LSP entry point — `wire_attribute_goto_definition`,
//! `this_member_hover_card`, `locate_in_backing_class_files` — against a
//! tempdir project registered with the Salsa actor, and asserts the exact
//! declaration line rather than merely that something resolved.

use crate::LaravelLanguageServer;
use laravel_lsp::salsa_impl::LaravelConfigData;
use laravel_lsp::view_var_index::ViewRender;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower_lsp::lsp_types::{GotoDefinitionResponse, Position, Url};
use tower_lsp::LspService;

/// A Livewire component's template, as a v4 single-file component: the class
/// lives in the blade's own front matter, so this file resolves DIRECTLY and
/// is a valid walk destination.
const PARENT_SFC: &str = r#"<?php

use Livewire\Component;

new class extends Component {
    public int $count = 0;

    public function save(): void
    {
        $this->count++;
    }
};
?>

<div>
    <x-save-button />
</div>
"#;

/// The anonymous partial. No front matter, no backing class, no render site —
/// only a `wire:` value that has to resolve through whoever rendered it.
const PARTIAL: &str = "<button wire:click=\"save\">Save</button>\n";

fn config_for(root: &Path) -> LaravelConfigData {
    LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: vec![("".to_string(), root.join("resources/views/components"))],
        livewire_path: Some(root.join("resources/views/livewire")),
        has_livewire: true,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// A server wired to `root`: config cached (the walk needs `view_paths` to
/// name the partial) and the project registered with the Salsa actor, which
/// queues each view file into the reverse component-usage index the walk
/// reads.
async fn backend_for(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    *backend.cached_config.write().await = Some(Arc::new(config_for(root)));
    // The actor's own `config_root`, which the backing-class loader's
    // containment guard reads (#364). Production registers it before the
    // project walk; caching the config on the backend alone does not.
    backend
        .salsa
        .register_config_files(root.to_path_buf(), None, None, None)
        .await
        .expect("actor registers the tempdir project root");
    backend
        .salsa
        .register_project_files(
            root.to_path_buf(),
            vec![PathBuf::from("app/Http/Controllers")],
            vec![root.join("resources/views")],
            Some(root.join("resources/views/livewire")),
            PathBuf::from("routes"),
        )
        .await
        .expect("actor registers the tempdir project");
    backend
}

/// A hover header as it is RENDERED. `main` escapes inline markdown in card
/// headers, so `Counter::increment()` reaches the client as
/// `Counter\:\:increment\(\)`. Routing the expected text through the same
/// helper keeps these assertions honest without pinning their escaping.
fn rendered(text: &str) -> String {
    laravel_lsp::markdown_safety::escape_inline(text).into_owned()
}

fn write(path: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

/// Write the partial at `resources/views/components/<name>.blade.php`.
fn write_partial(root: &Path, name: &str, contents: &str) -> PathBuf {
    write(
        &root
            .join("resources/views/components")
            .join(format!("{name}.blade.php")),
        contents,
    )
}

/// Goto-definition on the `save` inside the partial's `wire:click="save"`,
/// through the real handler. The document is opened first, because
/// `wire_attribute_goto_definition` reads the live buffer.
async fn goto_wire_click(
    backend: &LaravelLanguageServer,
    partial: &Path,
) -> Option<(PathBuf, u32)> {
    let uri = Url::from_file_path(partial).unwrap();
    let content = fs::read_to_string(partial).unwrap();
    let line = content
        .lines()
        .position(|l| l.contains("wire:click="))
        .expect("fixture carries a wire:click") as u32;
    let character = content
        .lines()
        .nth(line as usize)
        .unwrap()
        .find("save\"")
        .expect("fixture carries the member name") as u32
        + 1;
    backend
        .documents
        .write()
        .await
        .insert(uri.clone(), (content, 0));

    let response = backend
        .wire_attribute_goto_definition(&uri, Position { line, character })
        .await?;
    match response {
        // A member declaration is ONE place, so the handler answers with a
        // single link. Anything else means the walk stopped modelling
        // Livewire's single `$this` and is a failure, not a shape to tolerate.
        GotoDefinitionResponse::Link(links) if links.len() == 1 => Some((
            links[0].target_uri.to_file_path().unwrap(),
            links[0].target_range.start.line,
        )),
        other => panic!("expected exactly one target, got {other:?}"),
    }
}

#[tokio::test]
async fn x_tag_ancestor_backs_the_partials_wire_value() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        PARENT_SFC,
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((parent, 7)),
        "`<x-save-button />` in the SFC makes its save() the partial's target"
    );
}

#[tokio::test]
async fn livewire_tag_ancestor_backs_the_partials_wire_value() {
    // Same walk, reached through `<livewire:…>` instead of `<x-…>`. The
    // partial lives under Livewire's view path, so it resolves as an anonymous
    // Volt component by NAME — but has no class of its own, so navigation
    // still has to climb to the parent.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        &PARENT_SFC.replace("<x-save-button />", "<livewire:save-button />"),
    );
    let partial = write(
        &root.join("resources/views/livewire/save-button.blade.php"),
        PARTIAL,
    );
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((parent, 7)),
        "a `<livewire:…>` usage is a rendering edge too, not just `<x-…>`"
    );
}

#[tokio::test]
async fn hover_card_for_a_partial_member_names_the_ancestor_component() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        PARENT_SFC,
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    let card = backend
        .this_member_hover_card(&partial, "save")
        .await
        .expect("the ancestor declares save(), so a card must be emitted");
    assert!(
        card.contains(&rendered("dashboard::save()")),
        "the card names the ancestor component, not the partial: {card}"
    );
    assert!(
        card.contains("public function save(): void"),
        "the card carries the ancestor's signature: {card}"
    );
}

#[tokio::test]
async fn walk_is_transitive_across_five_levels_of_nesting() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        &PARENT_SFC.replace("<x-save-button />", "<x-level-1 />"),
    );
    // level-1 → level-2 → … → level-5 → save-button, none of them backed.
    for level in 1..=4 {
        write_partial(
            root,
            &format!("level-{level}"),
            &format!("<div><x-level-{} /></div>\n", level + 1),
        );
    }
    write_partial(root, "level-5", "<div><x-save-button /></div>\n");
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((parent, 7)),
        "six hops up an acyclic chain still reach the component"
    );
}

#[tokio::test]
async fn a_cycle_terminates_instead_of_hanging() {
    // `alpha` renders `beta`, `beta` renders `alpha`, and `alpha` renders the
    // partial. Nothing in the loop is backed, so the answer is None — the
    // point of the test is that it ARRIVES, rather than looping forever.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write_partial(root, "alpha", "<div><x-beta /><x-save-button /></div>\n");
    write_partial(root, "beta", "<div><x-alpha /></div>\n");
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            goto_wire_click(&backend, &partial),
        )
        .await
        .expect("the visited set terminates the walk"),
        None,
        "an unbacked cycle resolves to nothing"
    );
}

#[tokio::test]
async fn a_cycle_on_one_branch_does_not_hide_a_resolvable_branch() {
    // Two independent parents render the partial: `alpha` sits in a cycle with
    // `beta` and is never backed; `zulu` is a plain unbacked partial rendered
    // by the SFC. Pruning the cyclic branch must not prune the other one.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        &PARENT_SFC.replace("<x-save-button />", "<x-zulu />"),
    );
    write_partial(root, "alpha", "<div><x-beta /><x-save-button /></div>\n");
    write_partial(root, "beta", "<div><x-alpha /></div>\n");
    write_partial(root, "zulu", "<div><x-save-button /></div>\n");
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((parent, 7)),
        "the cyclic branch is pruned; the acyclic one still answers"
    );
}

#[tokio::test]
async fn equidistant_ancestors_resolve_lexicographically_and_stably() {
    // Two components at the same distance both declare save(). The winner is
    // the lexicographically smaller PATH, on every call — not whichever the
    // file walk happened to yield first.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let alpha = write(
        &root.join("resources/views/livewire/alpha.blade.php"),
        PARENT_SFC,
    );
    write(
        &root.join("resources/views/livewire/zulu.blade.php"),
        PARENT_SFC,
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    for attempt in 0..3 {
        assert_eq!(
            goto_wire_click(&backend, &partial).await,
            Some((alpha.clone(), 7)),
            "attempt {attempt}: the lexicographically first parent wins every time"
        );
    }
}

#[tokio::test]
async fn a_nearer_ancestor_outranks_a_further_one() {
    // `zulu-near` renders the partial directly; `alpha-far` renders it one hop
    // further away, through the unbacked `mid`. Both declare save(). Distance
    // decides, so the further-but-lexicographically-earlier `alpha-far` loses
    // — which a depth-first walk (it would descend `mid` before finishing the
    // level) or a flat lexicographic pick would both get wrong.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let near = write(
        &root.join("resources/views/livewire/zulu-near.blade.php"),
        PARENT_SFC,
    );
    write(
        &root.join("resources/views/livewire/alpha-far.blade.php"),
        &PARENT_SFC.replace("<x-save-button />", "<x-mid />"),
    );
    write_partial(root, "mid", "<div><x-save-button /></div>\n");
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((near, 7)),
        "the nearest rendering component wins, not the first one reachable"
    );
}

#[tokio::test]
async fn a_direct_render_site_outranks_a_component_ancestor() {
    // The partial has BOTH: a controller that renders it by view name, and an
    // SFC that renders it as `<x-save-button />`. The direct render site is
    // the runtime truth for `view('components.save-button')`, so it wins.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        PARENT_SFC,
    );
    let controller = write(
        &root.join("app/Http/Controllers/SaveButtonController.php"),
        "<?php\n\nclass SaveButtonController\n{\n    public function save(): void\n    {\n    }\n}\n",
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;
    backend.view_vars.write().unwrap().insert_file(
        controller.clone(),
        &[ViewRender {
            view_name: "components.save-button".to_string(),
            vars: HashMap::new(),
        }],
    );

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((controller, 4)),
        "the render site answers; the walk never runs"
    );
}

#[tokio::test]
async fn a_plain_php_file_under_the_view_root_is_not_a_rendering_ancestor() {
    // The watcher classifies a created file into `view_files` by PATH PREFIX
    // alone, so a plain `.php` under `resources/views` lands there — and its
    // `'<x-save-button />'` string literal IS indexed as a component reference.
    // It still isn't a rendering ancestor: the partial's `$this` belongs to the
    // component whose TEMPLATE renders the tag. Sorting first must not let it
    // answer ahead of the real parent.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let parent = write(
        &root.join("resources/views/livewire/dashboard.blade.php"),
        PARENT_SFC,
    );
    let legacy = write(
        &root.join("resources/views/aaa-legacy.php"),
        "<?php\n\nuse Livewire\\Component;\n\n$markup = '<x-save-button />';\n\nnew class extends Component {\n    public function save(): void\n    {\n    }\n};\n",
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;
    backend
        .salsa
        .update_project_file_list(legacy, laravel_lsp::salsa_impl::FileListOp::Add)
        .await
        .expect("the watcher files it under the view root");

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        Some((parent, 7)),
        "the Blade parent answers, not the `.php` file that merely mentions the tag"
    );
}

#[tokio::test]
async fn a_partial_with_no_livewire_ancestor_resolves_to_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    // Rendered, but only by templates that are themselves unbacked.
    write(
        &root.join("resources/views/pages/home.blade.php"),
        "<div><x-save-button /></div>\n",
    );
    let partial = write_partial(root, "save-button", PARTIAL);
    let backend = backend_for(root).await;

    assert_eq!(
        goto_wire_click(&backend, &partial).await,
        None,
        "no ancestor has a backing class, so there is nothing to navigate to"
    );
    assert!(
        backend
            .locate_in_backing_class_files(&partial, "save")
            .await
            .is_none(),
        "and the underlying resolution is empty, not a wrong guess"
    );
}
