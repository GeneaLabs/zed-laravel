//! End-to-end member resolution for the inline-class component shapes.
//!
//! A Livewire v4 single-file component and a class-based Volt component keep
//! their class in the template's own front matter — there is no standalone
//! `.php` file for `component_member_locator` to read. `blade_backing_class_
//! sources` therefore hands the `.blade.php` content itself to the locator,
//! so `$this->member`, bare-`$var` and `wire:` navigation land at the exact
//! member declaration INSIDE the blade file. These tests drive the private
//! async methods on a server built through the `tower_lsp::LspService`
//! harness against a tempdir (same pattern as the translation tests).

use crate::LaravelLanguageServer;
use laravel_lsp::component_member_locator::MemberKind;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::LspService;

async fn backend_with_root(root: &Path) -> LaravelLanguageServer {
    let (service, _socket) = LspService::new(LaravelLanguageServer::new);
    let backend = service.inner().clone();
    *backend.root_path.write().await = Some(root.to_path_buf());
    backend
}

const SFC_SOURCE: &str = r#"<?php

use Livewire\Component;

new class extends Component {
    public int $count = 0;

    public function increment(): void
    {
        $this->count++;
    }
};
?>

<div>
    <span>{{ $count }}</span>
    <button wire:click="increment">+</button>
</div>
"#;

fn write_sfc(root: &Path) -> PathBuf {
    let dir = root.join("resources/views/livewire");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("counter.blade.php");
    fs::write(&path, SFC_SOURCE).unwrap();
    path
}

#[tokio::test]
async fn sfc_member_resolves_into_the_blade_file_itself() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_sfc(dir.path());

    let (path, loc) = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("the inline class declares increment()");
    assert_eq!(path, blade, "an SFC's member lives in the blade file");
    assert_eq!(loc.kind, MemberKind::Method);
    assert_eq!(loc.line, 7, "0-based line of `public function increment`");

    let (_, count) = backend
        .locate_in_backing_class_files(&blade, "count")
        .await
        .expect("the inline class declares $count");
    assert_eq!(count.kind, MemberKind::Property);
    assert_eq!(count.line, 5);
}

#[tokio::test]
async fn volt_class_member_resolves_into_the_blade_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let pages = dir.path().join("resources/views/pages");
    fs::create_dir_all(&pages).unwrap();
    let blade = pages.join("search.blade.php");
    fs::write(
        &blade,
        r#"<?php

use Livewire\Volt\Component;

new class extends Component {
    public string $search = '';
}; ?>

<div><input wire:model="search"></div>
"#,
    )
    .unwrap();

    let (path, loc) = backend
        .locate_in_backing_class_files(&blade, "search")
        .await
        .expect("the Volt front-matter class declares $search");
    assert_eq!(path, blade);
    assert_eq!(loc.kind, MemberKind::Property);
    assert_eq!(loc.line, 5);
}

#[tokio::test]
async fn hover_card_carries_kind_class_and_signature() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_sfc(dir.path());

    let card = backend
        .this_member_hover_card(&blade, "increment")
        .await
        .expect("member exists, so a card must be emitted");
    assert!(
        // The header is markdown-escaped by `hover::render`, so the `::`
        // and the parens arrive backslashed; the rendered line is unchanged.
        card.contains(r"\:\:increment\(\)"),
        "header names the member as a method: {card}"
    );
    assert!(
        card.contains("public function increment(): void"),
        "card carries the full signature: {card}"
    );

    let card = backend
        .this_member_hover_card(&blade, "count")
        .await
        .expect("property card");
    assert!(card.contains(r"\:\:\$count"), "property header: {card}");
    assert!(
        card.contains("public int $count"),
        "card carries the declared type: {card}"
    );
}

#[tokio::test]
async fn plain_template_without_inline_class_contributes_no_source() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let views = dir.path().join("resources/views");
    fs::create_dir_all(&views).unwrap();
    let blade = views.join("welcome.blade.php");
    fs::write(&blade, "<div>{{ $title }}</div>").unwrap();

    assert!(
        backend
            .locate_in_backing_class_files(&blade, "title")
            .await
            .is_none(),
        "no backing class, no navigation target"
    );
}

#[tokio::test]
async fn dollar_surface_offers_inline_class_properties_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_sfc(dir.path());
    let uri = tower_lsp::lsp_types::Url::from_file_path(&blade).unwrap();

    let vars = backend.get_blade_available_variables(&uri, Some(SFC_SOURCE), Some(16));
    let count_entries: Vec<_> = vars.iter().filter(|v| v.name == "count").collect();
    assert_eq!(
        count_entries.len(),
        1,
        "the inline class's $count is offered exactly once, got {vars:?}"
    );
    assert_eq!(count_entries[0].php_type, "int");
}

#[tokio::test]
async fn local_loop_binding_shadows_class_property_navigation() {
    // The backing class declares $count; the template also binds $count as a
    // @foreach loop variable. Inside the loop the LOCAL binding wins — no
    // class-property navigation.
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_sfc(dir.path());
    let shadowing = "@foreach ($items as $count)\n    {{ $count }}\n@endforeach\n";
    fs::write(&blade, format!("{SFC_SOURCE}\n{shadowing}")).unwrap();
    let uri = tower_lsp::lsp_types::Url::from_file_path(&blade).unwrap();
    let content = fs::read_to_string(&blade).unwrap();
    backend
        .documents
        .write()
        .await
        .insert(uri.clone(), (content.clone(), 0));

    let inside_loop_line = content
        .lines()
        .position(|l| l.trim() == "{{ $count }}")
        .unwrap() as u32;
    let col = content
        .lines()
        .nth(inside_loop_line as usize)
        .unwrap()
        .find("count")
        .unwrap() as u32;

    assert!(
        backend
            .blade_variable_goto_definition(
                &uri,
                tower_lsp::lsp_types::Position {
                    line: inside_loop_line,
                    character: col,
                },
            )
            .await
            .is_none(),
        "an enclosing @foreach binding must shadow the class property"
    );

    // Outside any loop the same $count still navigates to the class property.
    let outside_line = content
        .lines()
        .position(|l| l.contains("<span>{{ $count }}</span>"))
        .unwrap() as u32;
    let outside_col = content
        .lines()
        .nth(outside_line as usize)
        .unwrap()
        .find("count")
        .unwrap() as u32;
    assert!(
        backend
            .blade_variable_goto_definition(
                &uri,
                tower_lsp::lsp_types::Position {
                    line: outside_line,
                    character: outside_col,
                },
            )
            .await
            .is_some(),
        "outside the loop the class property is the target"
    );
}

// ─── issue #339 ───────────────────────────────────────────────────────────
//
// Items 2, 3, 4 and 5 driven through the real handlers: the goto entry point
// (`wire_attribute_goto_definition`), the hover-card entry point
// (`this_member_hover_card_of_kind`) and the shared member lookup — not the
// pure helpers alone.

use tower_lsp::lsp_types::{GotoDefinitionResponse, Position, Url};

/// Register `content` as the live editor buffer for `path`, the way
/// `did_open` would, so handlers reading `self.documents` see it.
async fn open_document(backend: &LaravelLanguageServer, path: &Path, content: &str) -> Url {
    let uri = Url::from_file_path(path).unwrap();
    backend
        .documents
        .write()
        .await
        .insert(uri.clone(), (content.to_string(), 1));
    uri
}

/// A component that declares BOTH a `save()` method and a `$save` property,
/// so a kind-blind lookup cannot tell which one a reference meant.
///
/// **The METHOD is declared first on purpose.** Every lookup here picks the
/// first candidate in document order, so with the property first a kind filter
/// that only reaches half a result — a hover header filtered while its body is
/// not — still shows the property and the tests still pass. Method-first makes
/// the unfiltered path answer `save()` and the filtered path answer `$save`,
/// which is what lets these assertions discriminate at all.
const KIND_CLASH: &str = r#"<?php

use Livewire\Component;

new class extends Component {
    public function save(): void
    {
    }

    public $save = '';
};
?>

<div>
    <input wire:model="save">
    <button wire:click="save">Go</button>
    <span wire:target="save, reset"></span>
</div>
"#;

/// A hover header as it is RENDERED. `main` escapes inline markdown in card
/// headers, so `Counter::increment()` reaches the client as
/// `Counter\:\:increment\(\)`. Routing the expected text through the same
/// helper keeps these assertions honest without pinning their escaping.
fn rendered(text: &str) -> String {
    laravel_lsp::markdown_safety::escape_inline(text).into_owned()
}

fn write_blade(root: &Path, rel: &str, source: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
    path
}

/// A config whose only interesting field is the view root, so
/// `view_name_for_path` can turn a template path into a view name and reach
/// the render index.
fn config_with_view_root(root: &Path) -> laravel_lsp::salsa_impl::LaravelConfigData {
    use std::collections::HashMap;
    laravel_lsp::salsa_impl::LaravelConfigData {
        root: root.to_path_buf(),
        view_paths: vec![root.join("resources/views")],
        component_paths: Vec::new(),
        livewire_path: None,
        has_livewire: false,
        view_namespaces: HashMap::new(),
        component_namespaces: HashMap::new(),
        anonymous_component_paths: HashMap::new(),
        anonymous_component_namespaces: HashMap::new(),
        component_aliases: HashMap::new(),
        icon_aliases: HashMap::new(),
        class_component_files: HashMap::new(),
    }
}

/// The 0-based line the single goto target points at. `goto_link` answers
/// with a one-element `Link`, so that is the shape asserted here.
fn goto_line(response: &GotoDefinitionResponse) -> u32 {
    match response {
        GotoDefinitionResponse::Link(links) => {
            assert_eq!(links.len(), 1, "goto answers with exactly one target");
            links[0].target_range.start.line
        }
        GotoDefinitionResponse::Scalar(loc) => loc.range.start.line,
        other => panic!("expected a single location, got {other:?}"),
    }
}

#[tokio::test]
async fn wire_model_goto_resolves_the_property_not_the_same_named_method() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(dir.path(), "resources/views/clash.blade.php", KIND_CLASH);
    let uri = open_document(&backend, &blade, KIND_CLASH).await;

    let model_line = 14;
    let col = KIND_CLASH.lines().nth(model_line as usize).unwrap();
    let character = col.find("save").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: model_line,
                character,
            },
        )
        .await
        .expect("a data binding resolves to the property");
    assert_eq!(
        goto_line(&response),
        9,
        "wire:model binds `public $save`, declared on line 9 — not `save()` on line 5"
    );
}

#[tokio::test]
async fn wire_click_goto_resolves_the_method_not_the_same_named_property() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(dir.path(), "resources/views/clash.blade.php", KIND_CLASH);
    let uri = open_document(&backend, &blade, KIND_CLASH).await;

    let click_line = 15;
    let text = KIND_CLASH.lines().nth(click_line as usize).unwrap();
    let character = text.find("save").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: click_line,
                character,
            },
        )
        .await
        .expect("an action binding resolves to the method");
    assert_eq!(
        goto_line(&response),
        5,
        "wire:click calls `save()`, declared on line 5"
    );
}

#[tokio::test]
async fn wire_target_navigates_the_entry_under_the_cursor() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(dir.path(), "resources/views/clash.blade.php", KIND_CLASH);
    let uri = open_document(&backend, &blade, KIND_CLASH).await;

    let target_line = 16;
    let text = KIND_CLASH.lines().nth(target_line as usize).unwrap();
    // `wire:target="save, reset"` — the first entry names the component's
    // member, and `wire:target` accepts EITHER kind, so the winner is simply
    // the first declaration in document order: `save()` on line 5.
    let character = text.find("save,").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: target_line,
                character,
            },
        )
        .await
        .expect("wire:target names a navigable member");
    assert_eq!(goto_line(&response), 5);

    // The second entry is declared nowhere, so it resolves to nothing rather
    // than falling back to the first — proof the cursor picks the segment.
    let character = text.find("reset").unwrap() as u32;
    assert!(
        backend
            .wire_attribute_goto_definition(
                &uri,
                Position {
                    line: target_line,
                    character,
                },
            )
            .await
            .is_none(),
        "the cursor's own entry decides what is resolved"
    );
}

/// `wire:target` also names a PROPERTY when it pairs with `wire:model`'s
/// loading state, which is the form item 5's kind filter could silently
/// break: classify `target` as a method and this reference resolves to
/// nothing. `$query` is declared as a property and nothing else, so only the
/// unfiltered classification can answer it.
const WIRE_TARGET_PROPERTY: &str = r#"<?php

use Livewire\Component;

new class extends Component {
    public function save(): void
    {
    }

    public $query = '';
};
?>

<div>
    <input wire:model.live="query">
    <span wire:target="query" wire:loading>Searching…</span>
</div>
"#;

#[tokio::test]
async fn wire_target_resolves_a_loading_state_property() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(
        dir.path(),
        "resources/views/loading.blade.php",
        WIRE_TARGET_PROPERTY,
    );
    let uri = open_document(&backend, &blade, WIRE_TARGET_PROPERTY).await;

    let target_line = 15;
    let text = WIRE_TARGET_PROPERTY
        .lines()
        .nth(target_line as usize)
        .unwrap();
    let character = text.find("query\"").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: target_line,
                character,
            },
        )
        .await
        .expect("wire:target resolves a property, not only an action");
    assert_eq!(
        goto_line(&response),
        9,
        "the target is the `public $query` declaration"
    );
}

#[tokio::test]
async fn hover_card_for_a_binding_ignores_the_same_named_method() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(dir.path(), "resources/views/clash.blade.php", KIND_CLASH);

    let card = backend
        .this_member_hover_card_of_kind(&blade, "save", Some(MemberKind::Property))
        .await
        .expect("the property is declared");
    assert!(
        card.contains(&rendered("::$save")),
        "a property card names the property: {card}"
    );
    assert!(
        !card.contains("::save()"),
        "the method must not answer a property reference: {card}"
    );
    // The BODY, not just the header. `member_declaration_summary` is a second,
    // independent lookup, and unfiltered it renders whichever declaration comes
    // first — the method, in this fixture. Asserting the absence of `::save()`
    // cannot catch that: the rendered method reads `public function save():
    // void`, which contains no `::` at all. Read raw, not through `rendered`:
    // the code block is fenced PHP, and fenced content is not inline-escaped.
    assert!(
        card.contains("public $save"),
        "the code block shows the property's own declaration: {card}"
    );
    assert!(
        !card.contains("function save"),
        "the code block must not show the method's signature: {card}"
    );
}

#[tokio::test]
async fn hover_card_of_a_kind_absent_from_the_component_is_not_emitted() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let source = "<?php\nuse Livewire\\Component;\nnew class extends Component {\n    public function save(): void {}\n};\n?>\n<div></div>\n";
    let blade = write_blade(dir.path(), "resources/views/only-method.blade.php", source);

    assert!(
        backend
            .this_member_hover_card_of_kind(&blade, "save", Some(MemberKind::Property))
            .await
            .is_none(),
        "no property is declared, so a binding gets no card at all"
    );
}

#[tokio::test]
async fn inline_class_hover_card_names_the_component_not_the_base_class() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_sfc(dir.path());

    let card = backend
        .this_member_hover_card(&blade, "increment")
        .await
        .expect("the inline class declares increment()");
    assert!(
        !card.contains(&rendered("Component::")),
        "an anonymous `new class extends Component` is not called `Component`: {card}"
    );
    assert!(
        card.contains(&rendered("counter::increment()")),
        "the card names the component: {card}"
    );
}

#[tokio::test]
async fn a_named_backing_class_still_shows_its_fqcn() {
    use laravel_lsp::view_var_index::ViewRender;

    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let class_path = dir.path().join("app/Livewire/Counter.php");
    fs::create_dir_all(class_path.parent().unwrap()).unwrap();
    fs::write(
        &class_path,
        "<?php\n\nnamespace App\\Livewire;\n\nclass Counter extends Component\n{\n    public function increment(): void {}\n}\n",
    )
    .unwrap();
    let blade = write_blade(
        dir.path(),
        "resources/views/counter-page.blade.php",
        "<div wire:click=\"increment\"></div>\n",
    );

    // Point the render index at the class, the way a `view('counter-page')`
    // call site would.
    {
        *backend.cached_config.write().await =
            Some(std::sync::Arc::new(config_with_view_root(dir.path())));
    }
    backend.view_vars.write().unwrap().insert_file(
        class_path.clone(),
        &[ViewRender {
            view_name: "counter-page".to_string(),
            vars: Default::default(),
        }],
    );

    let card = backend
        .this_member_hover_card(&blade, "increment")
        .await
        .expect("the backing class declares increment()");
    assert!(
        card.contains(&rendered("App\\Livewire\\Counter::increment()")),
        "a NAMED class keeps its FQCN — the component-name fallback is only \
         for anonymous and functional shapes: {card}"
    );
}

/// The card's class name has to come from the text the user is looking at.
///
/// **The divergence is the whole test.** Disk keeps `OldName`; the editor's
/// unsaved buffer renames the class to `NewName`, and only the buffer is
/// pushed into Salsa — so a card reading the live source says `NewName` and a
/// card re-reading the path off disk says `OldName`. Without that split the
/// assertion passes either way, which is exactly how the first version of
/// this test went green with the fix reverted.
#[tokio::test]
async fn the_hover_card_reads_the_live_buffer_not_the_saved_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let saved = "<?php\n\nnamespace App\\Livewire;\n\nclass OldName extends Component\n{\n    public function increment(): void {}\n}\n";
    let buffer = "<?php\n\nnamespace App\\Livewire;\n\nclass NewName extends Component\n{\n    public function increment(): void {}\n}\n";
    let class_path = dir.path().join("app/Livewire/Counter.php");
    fs::create_dir_all(class_path.parent().unwrap()).unwrap();
    fs::write(&class_path, saved).unwrap();
    let blade = write_blade(
        dir.path(),
        "resources/views/counter-page.blade.php",
        "<div wire:click=\"increment\"></div>\n",
    );
    *backend.cached_config.write().await =
        Some(std::sync::Arc::new(config_with_view_root(dir.path())));
    backend.view_vars.write().unwrap().insert_file(
        class_path.clone(),
        &[laravel_lsp::view_var_index::ViewRender {
            view_name: "counter-page".to_string(),
            vars: Default::default(),
        }],
    );

    // The rename exists only in the editor. `did_open`/`did_change` push the
    // buffer to both places, so the test does too.
    open_document(&backend, &class_path, buffer).await;
    backend
        .salsa
        .update_file(class_path.clone(), 2, buffer.to_string())
        .await
        .expect("the editor's buffer reaches Salsa");

    let card = backend
        .this_member_hover_card(&blade, "increment")
        .await
        .expect("the backing class declares increment()");
    assert!(
        card.contains(&rendered("App\\Livewire\\NewName::increment()")),
        "the card must name the class in the live buffer: {card}"
    );
    assert!(
        !card.contains("OldName"),
        "the last-saved class name must not reach the card: {card}"
    );
}

// ---- functional Volt (item 3) --------------------------------------------

const VOLT_FUNCTIONAL: &str = r#"<?php
use function Livewire\Volt\{state, action};

state(['count' => 0]);

$increment = fn () => $this->count++;
$reset = action(fn () => $this->count = 0);
?>

<div wire:click="increment">{{ $count }}</div>
<button wire:click="reset">reset</button>
"#;

#[tokio::test]
async fn functional_volt_action_goto_lands_on_the_assignment() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(
        dir.path(),
        "resources/views/pages/counter.blade.php",
        VOLT_FUNCTIONAL,
    );
    let uri = open_document(&backend, &blade, VOLT_FUNCTIONAL).await;

    let click_line = 9;
    let text = VOLT_FUNCTIONAL.lines().nth(click_line as usize).unwrap();
    let character = text.find("increment").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: click_line,
                character,
            },
        )
        .await
        .expect("a functional Volt action is navigable");
    assert_eq!(
        goto_line(&response),
        5,
        "goto lands on the `$increment = fn () => …` assignment"
    );
}

#[tokio::test]
async fn functional_volt_wrapped_action_form_resolves_too() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(
        dir.path(),
        "resources/views/pages/counter.blade.php",
        VOLT_FUNCTIONAL,
    );
    let uri = open_document(&backend, &blade, VOLT_FUNCTIONAL).await;

    let click_line = 10;
    let text = VOLT_FUNCTIONAL.lines().nth(click_line as usize).unwrap();
    let character = text.find("reset").unwrap() as u32;
    let response = backend
        .wire_attribute_goto_definition(
            &uri,
            Position {
                line: click_line,
                character,
            },
        )
        .await
        .expect("`action(fn () => …)` is an action too");
    assert_eq!(goto_line(&response), 6);
}

#[tokio::test]
async fn functional_volt_state_key_is_a_property() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(
        dir.path(),
        "resources/views/pages/counter.blade.php",
        VOLT_FUNCTIONAL,
    );

    let (path, loc) = backend
        .locate_in_backing_class_files_of_kind(&blade, "count", Some(MemberKind::Property))
        .await
        .expect("`state(['count' => 0])` declares a property");
    assert_eq!(
        path, blade,
        "the state lives in the template's front matter"
    );
    assert_eq!(loc.line, 3, "the `state([...])` call's own line");
    assert!(
        backend
            .locate_in_backing_class_files_of_kind(&blade, "count", Some(MemberKind::Method))
            .await
            .is_none(),
        "state is never an action"
    );
}

#[tokio::test]
async fn functional_volt_hover_card_names_the_component() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let blade = write_blade(
        dir.path(),
        "resources/views/pages/counter.blade.php",
        VOLT_FUNCTIONAL,
    );

    let card = backend
        .this_member_hover_card(&blade, "increment")
        .await
        .expect("a functional Volt action gets a card");
    assert!(
        !card.contains(&rendered("Component::")),
        "a functional Volt file declares no class called `Component`: {card}"
    );
    assert!(
        card.contains(&rendered("counter::increment()")),
        "the card names the component: {card}"
    );
}

// ─── Backing-class resolution is memoized, and content-tracked (#339, item 7) ─
//
// `blade_backing_class_sources` used to re-scan the whole render index and
// re-read every backing class from disk on EVERY keystroke inside a `wire:`
// value. These two tests drive the real lookup handler and read the Salsa
// database's own body-execution counters, because the return value is
// identical whether the answer was memoized or recomputed.

/// A stock Livewire v3 component: a class under `app/Livewire`, and its view
/// under `resources/views/livewire`. The class is a STANDALONE `.php` file, so
/// the backing-class query has real file content to track.
fn write_v3_component(root: &Path) -> (PathBuf, PathBuf) {
    let class_dir = root.join("app/Livewire");
    fs::create_dir_all(&class_dir).unwrap();
    let class = class_dir.join("Counter.php");
    fs::write(
        &class,
        "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n\n    public function increment(): void\n    {\n        $this->count++;\n    }\n}\n",
    )
    .unwrap();

    let view_dir = root.join("resources/views/livewire");
    fs::create_dir_all(&view_dir).unwrap();
    let blade = view_dir.join("counter.blade.php");
    fs::write(
        &blade,
        "<div><button wire:click=\"increment\">+</button></div>\n",
    )
    .unwrap();

    (class, blade)
}

#[tokio::test]
async fn repeating_a_lookup_serves_the_backing_class_from_the_memo() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());
    *backend.cached_config.write().await =
        Some(std::sync::Arc::new(config_with_view_root(dir.path())));

    // Both hot inputs live: an OPEN document (so the live-buffer path runs on
    // every lookup) and a non-empty render index (so the render-index query is
    // actually consulted). Without either, the counters below would be
    // vacuously equal.
    open_document(&backend, &blade, &fs::read_to_string(&blade).unwrap()).await;
    backend.view_vars.write().unwrap().insert_file(
        dir.path()
            .join("app/Http/Controllers/CounterController.php"),
        &[laravel_lsp::view_var_index::ViewRender {
            view_name: "livewire.counter".to_string(),
            vars: std::collections::HashMap::new(),
        }],
    );

    let first = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("the component class backs its own view");
    assert_eq!(first.0, class, "resolution must reach the standalone class");

    let after_first = backend.salsa.query_run_counts().await.unwrap();
    assert!(
        after_first.render_source_files > 0 && after_first.blade_backing_class_sources > 0,
        "both queries must have run at least once, or the deltas below prove nothing",
    );
    let second = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("the second lookup resolves too");
    let after_second = backend.salsa.query_run_counts().await.unwrap();

    assert_eq!(first, second, "both lookups land on the same declaration");
    assert_eq!(
        after_second.blade_backing_class_sources, after_first.blade_backing_class_sources,
        "a repeat lookup with nothing edited must not re-read the backing class",
    );
    assert_eq!(
        after_second.render_source_files, after_first.render_source_files,
        "nor re-scan the render index",
    );
}

#[tokio::test]
async fn editing_the_backing_class_on_disk_invalidates_the_cached_source() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend = backend_with_root(dir.path()).await;
    let (class, blade) = write_v3_component(dir.path());

    let before = backend
        .locate_in_backing_class_files(&blade, "increment")
        .await
        .expect("the original method resolves");
    assert_eq!(before.1.line, 10, "the original declaration line");

    // Rewrite the BACKING CLASS — the blade file is untouched. A cache keyed on
    // the template alone would keep serving the stale source and resolve
    // `decrement` to nothing.
    //
    // The re-read is gated on mtime, whose resolution is only one second on
    // some filesystems; push the timestamp forward explicitly rather than
    // sleeping, so the test is neither slow nor flaky.
    fs::write(
        &class,
        "<?php\n\nnamespace App\\Livewire;\n\nuse Livewire\\Component;\n\nclass Counter extends Component\n{\n    public int $count = 0;\n\n    public function decrement(): void\n    {\n        $this->count--;\n    }\n}\n",
    )
    .unwrap();
    let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    fs::OpenOptions::new()
        .write(true)
        .open(&class)
        .unwrap()
        .set_modified(bumped)
        .unwrap();

    assert!(
        backend
            .locate_in_backing_class_files(&blade, "increment")
            .await
            .is_none(),
        "the removed method must stop resolving",
    );
    let after = backend
        .locate_in_backing_class_files(&blade, "decrement")
        .await
        .expect("the newly added method must resolve");
    assert_eq!(after.0, class);
    assert_eq!(
        after.1.line, 10,
        "the replacement method sits where the old one did",
    );
}

// ---- items 6 and 8, through their real handler ---------------------------
//
// Both fixes were pinned by unit tests over `locate_member` and
// `is_template_local_binding` alone. A unit test of a predicate proves nothing
// about whether the handler consults it, and `blade_variable_goto_definition`
// is the only consumer either one has — so it is what these drive.

/// Wire a Blade template to an external backing class, the way the hover-card
/// tests do: the config supplies the view root, and the render index says this
/// `.php` renders that view.
async fn backend_rendering(
    root: &Path,
    view_name: &str,
    class_rel: &str,
    class_source: &str,
) -> (LaravelLanguageServer, PathBuf) {
    let backend = backend_with_root(root).await;
    let class_path = root.join(class_rel);
    fs::create_dir_all(class_path.parent().unwrap()).unwrap();
    fs::write(&class_path, class_source).unwrap();
    *backend.cached_config.write().await = Some(std::sync::Arc::new(config_with_view_root(root)));
    backend.view_vars.write().unwrap().insert_file(
        class_path.clone(),
        &[laravel_lsp::view_var_index::ViewRender {
            view_name: view_name.to_string(),
            vars: Default::default(),
        }],
    );
    (backend, class_path)
}

/// `@props(['color' => 'blue'])` binds the KEY. The value `blue`, and any
/// unrelated quoted text sharing the line, are not bindings — so `$blue` and
/// `$literal` must still navigate to the backing class while `$color` is
/// correctly shadowed.
const PROPS_LINE: &str = r#"@props(['color' => 'blue']) <div title="literal">
{{ $color }}
{{ $blue }}
{{ $literal }}
</div>
"#;

const PROPS_BACKING_CLASS: &str = r#"<?php

namespace App\Livewire;

class Card extends Component
{
    public $color = '';
    public $blue = '';
    public $literal = '';
}
"#;

#[tokio::test]
async fn props_goto_binds_the_key_and_leaves_the_value_navigable() {
    let dir = tempfile::TempDir::new().unwrap();
    let (backend, _class) = backend_rendering(
        dir.path(),
        "card",
        "app/Livewire/Card.php",
        PROPS_BACKING_CLASS,
    )
    .await;
    let blade = write_blade(dir.path(), "resources/views/card.blade.php", PROPS_LINE);
    let uri = open_document(&backend, &blade, PROPS_LINE).await;

    let goto = |line: u32, character: u32| {
        backend.blade_variable_goto_definition(&uri, Position { line, character })
    };

    assert!(
        goto(1, 6).await.is_none(),
        "`color` is the @props KEY, so it is template-local and shadows the class"
    );

    let blue = goto(2, 6)
        .await
        .expect("`blue` is a default VALUE, not a binding — it still navigates");
    assert_eq!(
        goto_line(&blue),
        7,
        "goto lands on `public $blue` in the backing class"
    );

    let literal = goto(3, 6)
        .await
        .expect("unrelated quoted text on the @props line binds nothing");
    assert_eq!(
        goto_line(&literal),
        8,
        "goto lands on `public $literal` in the backing class"
    );
}

/// The candidate declared FIRST in the file sits one level deeper in the tree
/// than the one declared second, so a queue-based traversal reports the
/// shallow class and a true document-order sort reports the trait. Goto is a
/// location question over the whole file, which is why the trait wins here
/// even though completion (a surface question) excludes it.
const DOCUMENT_ORDER_CLASS: &str =
    "<?php\ntrait Helper {\n    public $dup = 1;\n}\nclass Card extends Component { public $dup = 2; }\n";

#[tokio::test]
async fn goto_takes_the_first_declaration_in_document_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let (backend, _class) = backend_rendering(
        dir.path(),
        "card",
        "app/Livewire/Card.php",
        DOCUMENT_ORDER_CLASS,
    )
    .await;
    let source = "{{ $dup }}\n";
    let blade = write_blade(dir.path(), "resources/views/card.blade.php", source);
    let uri = open_document(&backend, &blade, source).await;

    let response = backend
        .blade_variable_goto_definition(
            &uri,
            Position {
                line: 0,
                character: 6,
            },
        )
        .await
        .expect("`$dup` is declared in the backing class file");
    assert_eq!(
        goto_line(&response),
        2,
        "the trait's `$dup` is declared first, on line 2; the class's is line 4"
    );
}
