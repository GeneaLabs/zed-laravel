use super::*;
use std::path::PathBuf;

#[test]
fn parses_php_file_view_calls() {
    let path = PathBuf::from("/fixture/app/Http/Controllers/HomeController.php");
    let src = r#"<?php
class HomeController {
    public function index() {
        return view('home');
    }
}
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.views.iter().map(|v| v.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected view 'home', got {:?}",
        names
    );
}

#[test]
fn parses_php_file_route_calls() {
    let path = PathBuf::from("/fixture/routes/web.php");
    let src = r#"<?php
Route::get('/', [HomeController::class, 'index'])->name('home');
$url = route('home');
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected route 'home' call site, got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_components() {
    let path = PathBuf::from("/fixture/resources/views/layout.blade.php");
    let src = r#"<div>
    <x-button>Click me</x-button>
</div>
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.components.iter().map(|c| c.name.clone()).collect();
    assert!(
        names.contains(&"button".to_string()),
        "expected component 'button', got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_route_calls_in_echo() {
    let path = PathBuf::from("/fixture/resources/views/nav.blade.php");
    let src = r#"<nav>
    <a href="{{ route('home') }}">Home</a>
    <a href="{{ route('users.index') }}">Users</a>
</nav>
"#;
    let data = parse_owned(&path, src);
    let names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    assert!(
        names.contains(&"home".to_string()),
        "expected 'home' from {{ route('home') }}, got {:?}",
        names
    );
    assert!(
        names.contains(&"users.index".to_string()),
        "expected 'users.index', got {:?}",
        names
    );
}

#[test]
fn parses_blade_file_php_block() {
    let path = PathBuf::from("/fixture/resources/views/dashboard.blade.php");
    let src = r#"@php
    $url = route('home');
    $title = config('app.name');
@endphp
<h1>{{ $title }}</h1>
"#;
    let data = parse_owned(&path, src);
    let route_names: Vec<String> = data.route_refs.iter().map(|r| r.name.clone()).collect();
    let config_keys: Vec<String> = data.config_refs.iter().map(|c| c.key.clone()).collect();
    assert!(
        route_names.contains(&"home".to_string()),
        "expected 'home' route in @php block, got {:?}",
        route_names
    );
    assert!(
        config_keys.contains(&"app.name".to_string()),
        "expected 'app.name' config in @php block, got {:?}",
        config_keys
    );
}

#[test]
fn builds_position_index_for_find_at_position() {
    let path = PathBuf::from("/fixture/routes/web.php");
    let src = r#"<?php
$url = route('home');
"#;
    let data = parse_owned(&path, src);
    // The route 'home' starts at line 1, after `route('` (which is 7 chars).
    let line_text = src.lines().nth(1).unwrap();
    let start_col = line_text.find("home").unwrap() as u32;
    let pat = data
        .find_at_position(1, start_col + 1)
        .expect("find_at_position should locate the route");
    match pat {
        crate::salsa_impl::PatternAtPosition::Route(r) => {
            assert_eq!(r.name, "home");
        }
        other => panic!("expected Route pattern, got {:?}", other),
    }
}

#[test]
fn returns_empty_for_unparseable_garbage() {
    let path = PathBuf::from("/fixture/garbage.php");
    let data = parse_owned(&path, "this is not valid PHP at all <<>>");
    // tree-sitter is error-tolerant; expect no captured patterns from garbage.
    assert!(data.views.is_empty());
    assert!(data.route_refs.is_empty());
    assert!(data.config_refs.is_empty());
}

#[test]
fn warming_path_populates_member_access_refs() {
    // Regression: the warming path (`parse_owned`/`parse_owned_with_hierarchy`)
    // must capture property-form member accesses, like the lazy
    // `handle_get_patterns` path does. Without this, the magic-member index
    // (M4) builds empty and find-references on `$this->email` finds nothing.
    let path = PathBuf::from("/fixture/app/Models/User.php");
    let src = r#"<?php
namespace App\Models;
class User {
    public function gravatar(): string {
        return md5($this->email);
    }
}
"#;
    let data = parse_owned(&path, src);
    let members: Vec<&str> = data
        .member_access_refs
        .iter()
        .map(|m| m.member.as_str())
        .collect();
    assert!(
        members.contains(&"email"),
        "warming path must capture `$this->email`, got {members:?}"
    );
}

#[test]
fn blade_embedded_member_access_is_captured_with_outer_positions() {
    // Blade view-var inference (phase 1): `{{ $user->email }}` is now captured,
    // with the member-name position mapped back into outer-file coordinates.
    let path = PathBuf::from("/fixture/resources/views/show.blade.php");
    let src = "<div>{{ $user->email }}</div>\n";
    let data = parse_owned(&path, src);
    let email = data
        .member_access_refs
        .iter()
        .find(|m| m.member == "email")
        .expect("Blade-embedded $user->email should be captured");
    assert_eq!(email.receiver, "$user");
    // Outer-file row 0; `email` sits at column 15 in `<div>{{ $user->email }}`.
    assert_eq!(email.line, 0);
    assert_eq!(email.column, 15);
}

#[test]
fn blade_bound_attribute_member_access_is_captured() {
    // PHP inside a bound attribute (`:tooltip="$post->is_published ? …"`) is a
    // real member access the echo/@php passes don't reach.
    let path = PathBuf::from("/fixture/resources/views/x.blade.php");
    let src = "<flux:button :tooltip=\"$post->is_published ? 'a' : 'b'\">Go</flux:button>\n";
    let data = parse_owned(&path, src);
    let m = data
        .member_access_refs
        .iter()
        .find(|m| m.member == "is_published")
        .expect("bound-attribute $post->is_published should be captured");
    assert_eq!(m.receiver, "$post");
    assert_eq!(m.line, 0);
}

#[test]
fn blade_directive_attribute_param_member_access_is_captured() {
    // `@class(['x' => $post->active])` — the directive parameter is PHP.
    let path = PathBuf::from("/fixture/resources/views/y.blade.php");
    let src = "<div @class(['on' => $post->active])>x</div>\n";
    let data = parse_owned(&path, src);
    assert!(
        data.member_access_refs
            .iter()
            .any(|m| m.member == "active" && m.receiver == "$post"),
        "directive-attribute param member access captured, got {:?}",
        data.member_access_refs
    );
}

// ─── M1 single-parse capture: member_context gating via THIS constructor ──
//
// The vendor gate + zero-cost `None` behaviour must hold through the real
// `parse_owned_with_hierarchy` path (the parallel warm constructor), not just
// the `capture_member_context` helper. The v11 disk invariant depends on
// vendor files carrying no context.

#[test]
fn parse_owned_captures_context_for_a_member_reader() {
    let path = PathBuf::from("/proj/app/Http/Controllers/C.php");
    let src = "<?php\nnamespace App;\nclass C { public function f(\\App\\Models\\User $u) { return $u->email; } }\n";
    let (data, _) = parse_owned_with_hierarchy(&path, src);
    let ctx = data
        .member_context
        .as_ref()
        .expect("a non-vendor member reader must capture context");
    assert_eq!(
        ctx.sites.len(),
        data.member_access_refs.len(),
        "sites must stay positionally parallel to member_access_refs"
    );
}

#[test]
fn parse_owned_skips_capture_for_vendor() {
    // Same source, under a `vendor/` path → NO context (the build passes skip
    // vendor, so capturing there is wasted; the disk cache relies on this).
    let path = PathBuf::from("/proj/vendor/acme/pkg/src/C.php");
    let src = "<?php\nnamespace Acme;\nclass C { public function f(\\App\\Models\\User $u) { return $u->email; } }\n";
    let (data, _) = parse_owned_with_hierarchy(&path, src);
    assert!(
        !data.member_access_refs.is_empty(),
        "the file DOES have a member access (proving the gate, not the parse, drops context)"
    );
    assert!(
        data.member_context.is_none(),
        "a vendor file must capture no context"
    );
}

#[test]
fn parse_owned_no_context_for_pattern_free_file() {
    let path = PathBuf::from("/proj/app/Widget.php");
    let src = "<?php\nnamespace App;\nclass Widget { public function noop() {} }\n";
    let (data, _) = parse_owned_with_hierarchy(&path, src);
    assert!(data.member_context.is_none());
}

/// A component whose only usages are Blade markup built as PHP strings — the
/// shape that made the unused-symbol diagnostic (#59) call a live component
/// "possibly dead". The tag must land in the same `components` bucket a
/// `.blade.php` tag would, so the index, lens, and warning all see it.
#[test]
fn parses_component_tags_built_as_php_strings() {
    let path = PathBuf::from("/fixture/app/Jobs/ProcessCrossReferenceBatch.php");
    let src = r#"<?php
class ProcessCrossReferenceBatch {
    public function replace(int $id, string $reference): string
    {
        $direct = "<x-reader.cross-reference :id=\"{$id}\" />";
        $grouped = "<x-reader.cross-reference reference=\"{$reference}\" />";
        return $direct . $grouped;
    }
}
"#;
    let data = parse_owned(&path, src);

    let names: Vec<String> = data.components.iter().map(|c| c.name.clone()).collect();
    assert_eq!(
        names,
        ["reader.cross-reference", "reader.cross-reference"],
        "both string-built tags count as references"
    );
    assert_eq!(data.components[0].tag_name, "x-reader.cross-reference");
}

#[test]
fn parses_livewire_tags_built_as_php_strings() {
    let path = PathBuf::from("/fixture/app/Jobs/RenderCounter.php");
    let src = r#"<?php
class RenderCounter {
    public function html(): string
    {
        return '<livewire:counter />';
    }
}
"#;
    let data = parse_owned(&path, src);

    let names: Vec<String> = data.livewire_refs.iter().map(|l| l.name.clone()).collect();
    assert_eq!(names, ["counter"]);
    assert!(
        data.components.is_empty(),
        "a Livewire tag must not also land in the component bucket"
    );
}

/// The scan is literal-only: a `.php` file that merely mentions a tag in a
/// comment gains no reference. Guards against the diagnostic flipping from
/// false-positive to false-negative.
#[test]
fn php_comment_mentioning_a_tag_is_not_a_reference() {
    let path = PathBuf::from("/fixture/app/Support/Doc.php");
    let src = r#"<?php
/** Renders <x-reader.cross-reference /> somewhere else. */
class Doc {}
"#;
    let data = parse_owned(&path, src);

    assert!(
        data.components.is_empty(),
        "got {:?}",
        data.components.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

/// A Blade file's tags still come from the Blade extractor only — the PHP
/// string scan never runs there (tree-sitter-php on Blade is the pathological
/// case the full-file parse is gated against).
///
/// The Blade extractor tags both halves of a paired component, so `<x-card>…
/// </x-card>` yields two entries. That is pre-existing behaviour, pinned here
/// so the PHP-string scan can be shown not to have perturbed it.
#[test]
fn blade_file_component_extraction_is_unchanged() {
    let path = PathBuf::from("/fixture/resources/views/page.blade.php");
    let src = "<x-card>body</x-card>\n";
    let data = parse_owned(&path, src);

    let names: Vec<String> = data.components.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, ["card", "card"], "opening and closing tag");
}

// ---------------------------------------------------------------------------
// Blade `@use` class imports.
// ---------------------------------------------------------------------------

/// End-to-end for the `@use` path: the directive's in-quote span must be
/// captured (the `string_column` whitelist) and derived into a class reference
/// (the `class_refs` derivation). Slicing the source by the reported span is
/// what proves both — a whole-directive span would slice back `@use("App\…")`.
#[test]
fn parses_class_import_from_a_blade_use_directive() {
    let path =
        PathBuf::from("/fixture/resources/views/components/reader/cross-reference.blade.php");
    let src = "@use(\"App\\Support\\Reader\\VerseMarkerResolver\")\n\n<div></div>\n";
    let data = parse_owned(&path, src);

    assert_eq!(data.class_refs.len(), 1, "got {:?}", data.class_refs);
    let c = &data.class_refs[0];
    assert_eq!(c.name, r"App\Support\Reader\VerseMarkerResolver");
    assert_eq!(c.line, 0);

    let line = src.lines().next().unwrap();
    assert_eq!(
        &line[c.column as usize..c.end_column as usize],
        r"App\Support\Reader\VerseMarkerResolver",
        "the span must cover the FQCN inside the quotes, not the whole directive"
    );
}

#[test]
fn class_import_span_follows_the_alias_form() {
    let path = PathBuf::from("/fixture/resources/views/page.blade.php");
    let src = "@use('App\\Models\\Flight', 'FlightModel')\n";
    let data = parse_owned(&path, src);

    assert_eq!(data.class_refs.len(), 1, "got {:?}", data.class_refs);
    let c = &data.class_refs[0];
    assert_eq!(c.name, r"App\Models\Flight");
    assert_eq!(
        &src.lines().next().unwrap()[c.column as usize..c.end_column as usize],
        r"App\Models\Flight",
        "the span covers the imported class, not the alias"
    );
}

/// A Blade group import names several classes in one string — each is its own
/// reference, positioned on its own member inside the braces.
#[test]
fn blade_group_import_yields_one_class_ref_per_member() {
    let path = PathBuf::from("/fixture/resources/views/page.blade.php");
    let src = "@use('App\\Models\\{Flight, Airport as Field}')\n";
    let data = parse_owned(&path, src);

    assert_eq!(
        data.class_refs
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        [r"App\Models\Flight", r"App\Models\Airport"],
        "the prefix applies to each member; the alias names a binding, not a class"
    );

    let line = src.lines().next().unwrap();
    let spans: Vec<&str> = data
        .class_refs
        .iter()
        .map(|c| &line[c.column as usize..c.end_column as usize])
        .collect();
    assert_eq!(spans, ["Flight", "Airport"]);
}

/// Padding inside the quotes is located rather than skipped.
#[test]
fn padded_blade_import_still_yields_a_class_ref() {
    let path = PathBuf::from("/fixture/resources/views/page.blade.php");
    let src = "@use(' App\\Models\\Flight ')\n";
    let data = parse_owned(&path, src);

    assert_eq!(data.class_refs.len(), 1, "got {:?}", data.class_refs);
    assert_eq!(data.class_refs[0].name, r"App\Models\Flight");
    let line = src.lines().next().unwrap();
    assert_eq!(
        &line[data.class_refs[0].column as usize..data.class_refs[0].end_column as usize],
        r"App\Models\Flight",
        "the span skips the padding"
    );
}

/// `function` / `const` imports bind no class, so they stay out of the index.
#[test]
fn function_and_const_blade_imports_produce_no_class_reference() {
    let path = PathBuf::from("/fixture/resources/views/page.blade.php");

    let func = parse_owned(&path, "@use('function App\\Helpers\\fmt')\n");
    assert!(func.class_refs.is_empty(), "got {:?}", func.class_refs);

    let konst = parse_owned(&path, "@use('const App\\Constants\\MAX')\n");
    assert!(konst.class_refs.is_empty(), "got {:?}", konst.class_refs);
}

/// A Volt single-file component's `<?php … ?>` front matter carries real PHP
/// `use` statements. They are class references like any other, and a rename
/// that skipped them would leave the component importing a class that no
/// longer exists.
#[test]
fn volt_front_matter_use_statements_are_class_refs() {
    let path = PathBuf::from("/fixture/resources/views/livewire/counter.blade.php");
    let src = "<?php\n\nuse App\\Models\\Flight;\nuse Livewire\\Volt\\Component;\n\nnew class extends Component {};\n?>\n\n<div>{{ $count }}</div>\n";
    let data = parse_owned(&path, src);

    assert_eq!(
        data.class_refs
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        [r"App\Models\Flight", r"Livewire\Volt\Component"]
    );

    let lines: Vec<&str> = src.lines().collect();
    let first = &data.class_refs[0];
    assert_eq!(
        &lines[first.line as usize][first.column as usize..first.end_column as usize],
        r"App\Models\Flight",
        "front-matter positions map back into the outer file"
    );
}

#[test]
fn parses_class_imports_from_php_use_statements() {
    let path = PathBuf::from("/fixture/app/Http/Controllers/FlightController.php");
    let src = "<?php\nnamespace App\\Http\\Controllers;\n\nuse App\\Models\\Flight;\nuse App\\Models\\{Airport, Gate as G};\nuse function App\\Helpers\\fmt;\n\nclass FlightController {}\n";
    let data = parse_owned(&path, src);

    assert_eq!(
        data.class_refs
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        [
            r"App\Models\Flight",
            r"App\Models\Airport",
            r"App\Models\Gate"
        ],
        "grouped clauses expand; the `function` import binds no class"
    );

    // Each span must slice back to the name as written at that site.
    let lines: Vec<&str> = src.lines().collect();
    let spans: Vec<&str> = data
        .class_refs
        .iter()
        .map(|c| &lines[c.line as usize][c.column as usize..c.end_column as usize])
        .collect();
    assert_eq!(spans, [r"App\Models\Flight", "Airport", "Gate"]);
}

/// A trait `use` inside a class body imports no class — indexing it would make
/// every trait consumer a phantom reference to a class of the same name.
#[test]
fn trait_use_in_a_class_body_is_not_a_class_ref() {
    let path = PathBuf::from("/fixture/app/Models/Flight.php");
    let src = "<?php\nnamespace App\\Models;\n\nclass Flight\n{\n    use HasFactory;\n}\n";
    let data = parse_owned(&path, src);

    assert!(data.class_refs.is_empty(), "got {:?}", data.class_refs);
}
