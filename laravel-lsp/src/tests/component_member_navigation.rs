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
