//! Tests for controller → Blade view-variable extraction.
//!
//! Variable types here resolve via flow tracking on a typed parameter, so no
//! on-disk model is needed — `view_renders_in_file` returns the view name and
//! the `var → fqcn` map for each render site.

use super::*;
use crate::class_hierarchy_index::ClassHierarchyIndex;
use std::path::Path;

fn renders(controller: &str) -> Vec<ViewRender> {
    view_renders_in_file(
        controller,
        &ClassHierarchyIndex::default(),
        &ClassViewCache::new(),
        Path::new("/proj"),
    )
}

const CTRL_HEADER: &str = "<?php
namespace App\\Http\\Controllers;
use App\\Models\\User;
class C {
    public function show(User $user) {
";

#[test]
fn extracts_array_data() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', ['user' => $user]);\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_compact() {
    let src =
        format!("{CTRL_HEADER}        return view('users.show', compact('user'));\n    }}\n}}\n");
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_key_value() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with('user', $user);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "users.show");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn extracts_with_array() {
    let src = format!(
        "{CTRL_HEADER}        return view('users.show')->with(['user' => $user]);\n    }}\n}}\n"
    );
    let r = renders(&src);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn unresolvable_value_is_omitted() {
    // `$mystery` has no type info → the var simply doesn't appear (vs. a wrong
    // guess). The view render is still recorded.
    let src = "<?php
function show($mystery) {
    return view('x', ['thing' => $mystery]);
}
";
    let r = renders(src);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_name, "x");
    assert!(r[0].vars.is_empty(), "got {:?}", r[0].vars);
}

#[test]
fn no_view_calls_yields_empty() {
    let r = renders("<?php\nfunction f() { return 1; }\n");
    assert!(r.is_empty());
}

// ---- Filament `$view` property render site --------------------------------
//
// `protected string $view = '…';` (Filament `Page`) / `protected static
// string $view = '…';` (Filament `Widget`) declares which Blade view the
// class renders — the class-property counterpart of a controller's
// `view('name', […])` call. The class's typed surface becomes that view's
// variables.

#[test]
fn view_property_typed_public_property_is_render_site() {
    let src = r#"<?php
namespace App\Filament\Pages;
use App\Models\User;
class ContractViewPage {
    public ?User $user = null;
    protected string $view = 'legal-contractmanagement::filament.pages.contract-edit-page';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].view_name,
        "legal-contractmanagement::filament.pages.contract-edit-page"
    );
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn view_property_computed_method_is_render_var() {
    let src = r#"<?php
use App\Models\User;
class ReportPage {
    protected string $view = 'pages.report';

    #[Computed]
    public function user(): User { return User::first(); }
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn view_property_mount_assignment_is_render_var() {
    let src = r#"<?php
use App\Models\User;
class ProfilePage {
    protected string $view = 'pages.profile';
    public $user;

    public function mount(User $injected) {
        $this->user = $injected;
    }
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn view_property_static_variant_is_render_site() {
    // Filament `Widget`s declare `$view` as `static`.
    let src = r#"<?php
class StatsWidget {
    protected static string $view = 'widgets.stats';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "widgets.stats");
}

#[test]
fn view_property_public_untyped_variant_is_render_site() {
    // Neither visibility nor a type hint gates detection — only the
    // property's NAME and a literal initializer.
    let src = r#"<?php
class BannerWidget {
    public $view = 'widgets.banner';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "widgets.banner");
}

#[test]
fn view_property_private_typed_variant_is_render_site() {
    let src = r#"<?php
class SecretPage {
    private string $view = 'pages.secret';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "pages.secret");
}

#[test]
fn view_property_static_untyped_variant_is_render_site() {
    let src = r#"<?php
class LegacyWidget {
    protected static $view = 'widgets.legacy';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(r[0].view_name, "widgets.legacy");
}

#[test]
fn view_property_get_view_data_is_render_var() {
    // `getViewData()` is Filament's `with()`-equivalent extension point on
    // `Page`/`Widget` — its returned array folds into the view's variables.
    let src = r#"<?php
use App\Models\User;
class ReportPage {
    protected string $view = 'pages.report';

    protected function getViewData(): array {
        return ['user' => User::first()];
    }
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn view_property_surface_is_scoped_to_owning_class() {
    // Two classes in one file, only one declares `$view`: the other class's
    // typed surface must never leak into this render site's vars.
    let src = r#"<?php
use App\Models\Order;
use App\Models\User;
class UnrelatedHelper {
    public Order $order;
}
class ProfilePage {
    public User $user;
    protected string $view = 'pages.profile';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert!(
        !r[0].vars.contains_key("order"),
        "the other class's property leaked: {:?}",
        r[0].vars
    );
}

#[test]
fn view_property_non_literal_is_skipped() {
    // `self::VIEW` isn't a string literal — no resolvable render site.
    let src = r#"<?php
class DynamicPage {
    const VIEW = 'pages.dynamic';
    protected string $view = self::VIEW;
}

"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn view_property_interpolated_string_is_skipped() {
    let src = r#"<?php
class DynamicPage {
    protected string $view = "pages.{$type}";
}
"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn view_property_call_initializer_is_skipped() {
    let src = r#"<?php
class DynamicPage {
    protected string $view = self::defaultView();
}
"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn view_property_concatenation_is_skipped() {
    let src = r#"<?php
class DynamicPage {
    protected string $view = 'pages.' . 'home';
}
"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn view_property_without_initializer_is_skipped() {
    let src = r#"<?php
class DynamicPage {
    protected string $view;
}
"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn view_property_constructor_promoted_is_skipped() {
    // A promoted parameter's default is a PARAMETER default — the actual
    // value depends on the caller, so it is not a resolvable render site.
    let src = r#"<?php
class DynamicPage {
    public function __construct(protected string $view = 'pages.injected') {}
}
"#;
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

// ---- `declared_view_literal_node` — position-aware sibling used by the
// pattern-capture site (`queries::extract_all_php_patterns`) to report the
// `$view` property as a ViewReferenceData for goto/hover/diagnostics -------

#[test]
fn declared_view_literal_node_points_at_literal_content() {
    // Line 2 (0-based):
    //     protected string $view = 'legal-contractmanagement::filament.pages.contract-edit-page';
    //     0         1         2         3         4         5         6         7         8
    //     0123456789012345678901234567890123456789012345678901234567890123456789012345678901234
    let src = "<?php\nclass ContractViewPage {\n    protected string $view = \
               'legal-contractmanagement::filament.pages.contract-edit-page';\n}\n";
    let tree = crate::parser::parse_php(src).unwrap();
    let bytes = src.as_bytes();

    let nodes = declared_view_literal_nodes(tree.root_node(), bytes);
    assert_eq!(nodes.len(), 1, "one declaring class, one node");
    let content = nodes[0];
    assert_eq!(
        content.utf8_text(bytes).unwrap(),
        "legal-contractmanagement::filament.pages.contract-edit-page"
    );
    // Position points at the literal's CONTENT, not the surrounding quotes.
    assert_eq!(content.start_position().row, 2);
    assert_eq!(content.start_position().column, 30);
    assert_eq!(content.end_position().column, 89);
}

#[test]
fn declared_view_literal_node_static_variant() {
    let src =
        "<?php\nclass StatsWidget {\n    protected static string $view = 'widgets.stats';\n}\n";
    let tree = crate::parser::parse_php(src).unwrap();
    let bytes = src.as_bytes();

    let nodes = declared_view_literal_nodes(tree.root_node(), bytes);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].utf8_text(bytes).unwrap(), "widgets.stats");
}

#[test]
fn declared_view_literal_node_non_literal_is_none() {
    let src = "<?php\nclass DynamicPage {\n    const VIEW = 'pages.dynamic';\n    protected string $view = self::VIEW;\n}\n";
    let tree = crate::parser::parse_php(src).unwrap();
    let bytes = src.as_bytes();

    assert!(declared_view_literal_nodes(tree.root_node(), bytes).is_empty());
}

#[test]
fn declared_view_literal_node_no_view_property_is_none() {
    let src = "<?php\nclass NoView {\n    public function render() {}\n}\n";
    let tree = crate::parser::parse_php(src).unwrap();
    let bytes = src.as_bytes();

    assert!(declared_view_literal_nodes(tree.root_node(), bytes).is_empty());
}

#[test]
fn captured_view_property_render_matches_live() {
    // Plan capture/eval (`capture_render_plans` + `evaluate_render_plans`) must
    // reproduce the live `view_renders_in_file` result exactly, across a typed
    // property AND a `#[Computed]` method on the same class.
    let p = blade_project();
    let controller = r#"<?php
namespace App\Filament\Pages;
use App\Models\User;
class ContractViewPage {
    public ?User $user = null;

    #[Computed]
    public function admin(): User { return User::first(); }

    protected string $view = 'legal-contractmanagement::filament.pages.contract-edit-page';
}
"#;
    let (live, captured) = render_both_ways(&p.index, &p.root, controller);
    assert_eq!(
        live, captured,
        "captured $view render plan diverged from live"
    );
    assert_eq!(live.len(), 1, "got {live:?}");
    assert_eq!(
        live[0].vars.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
    assert_eq!(
        live[0].vars.get("admin").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn view_property_real_world_filament_page_shape() {
    // Mirrors ContractViewPage.php's actual shape: a `#[Validate(...)]`
    // attribute directly above the typed property, an untyped `#[Locked]`
    // property, a non-class builtin-typed property, a protected (non-public)
    // typed property, and a namespace-qualified `$view` literal — none of
    // which should confuse the typed-property scan.
    let src = r#"<?php
namespace App\Legal\ContractManagement\Filament\Pages;

use Filament\Pages\Page;
use Livewire\Attributes\Locked;
use Livewire\Attributes\Validate;
use Livewire\Features\SupportFileUploads\TemporaryUploadedFile;

class ContractViewPage extends Page
{
    #[Locked]
    public ?string $contractId = null;

    public bool $isEditMode = false;

    #[Validate('file|max:10240|mimes:pdf', as: ['uploadedFile' => 'Contract document'], translate: true)]
    public ?TemporaryUploadedFile $uploadedFile = null;

    protected string $contractService;

    protected string $view = 'legal-contractmanagement::filament.pages.contract-edit-page';
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert_eq!(
        r[0].view_name,
        "legal-contractmanagement::filament.pages.contract-edit-page"
    );
    assert_eq!(
        r[0].vars.get("uploadedFile").map(String::as_str),
        Some("Livewire\\Features\\SupportFileUploads\\TemporaryUploadedFile")
    );
    // Builtins, `#[Locked]`-only, and non-public props contribute nothing.
    assert!(!r[0].vars.contains_key("contractId"));
    assert!(!r[0].vars.contains_key("isEditMode"));
    assert!(!r[0].vars.contains_key("contractService"));
}

// ---- ViewVarIndex --------------------------------------------------------

use std::collections::HashMap;
use std::path::PathBuf;

fn render(view: &str, vars: &[(&str, &str)]) -> ViewRender {
    ViewRender {
        view_name: view.to_string(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn index_returns_var_type() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    assert_eq!(
        idx.var_types("users.show", "user"),
        vec!["App\\Models\\User"]
    );
    assert!(idx.var_types("users.show", "missing").is_empty());
    assert!(idx.var_types("other.view", "user").is_empty());
}

#[test]
fn index_unions_types_across_files() {
    // Two controllers render the same view with different types for `user`.
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("dash", &[("user", "App\\Models\\User")])],
    );
    idx.insert_file(
        PathBuf::from("/proj/AdminController.php"),
        &[render("dash", &[("user", "App\\Models\\Admin")])],
    );
    // Union — both observed types are kept (sorted).
    assert_eq!(
        idx.var_types("dash", "user"),
        vec!["App\\Models\\Admin", "App\\Models\\User"]
    );
}

#[test]
fn index_evicts_on_reinsert() {
    let mut idx = ViewVarIndex::new();
    let path = PathBuf::from("/proj/UserController.php");
    idx.insert_file(path.clone(), &[render("v", &[("a", "App\\A")])]);
    // Re-parse of the same file now renders a different var — old one is gone.
    idx.insert_file(path, &[render("v", &[("b", "App\\B")])]);
    assert!(idx.var_types("v", "a").is_empty());
    assert_eq!(idx.var_types("v", "b"), vec!["App\\B"]);
}

#[test]
fn index_clear_empties() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/C.php"),
        &[render("v", &[("a", "App\\A")])],
    );
    assert!(!idx.is_empty());
    idx.clear();
    assert!(idx.is_empty());
    assert_eq!(idx.view_count(), 0);
}

#[test]
fn render_entries_carry_every_contributing_file() {
    let mut idx = ViewVarIndex::new();
    let controller = PathBuf::from("/proj/UserController.php");
    let page = PathBuf::from("/proj/Filament/UserPage.php");
    idx.insert_file(
        controller.clone(),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    idx.insert_file(
        page.clone(),
        &[render("users.show", &[("account", "App\\Models\\Account")])],
    );
    idx.insert_file(
        PathBuf::from("/proj/OtherController.php"),
        &[render("other.view", &[("x", "App\\X")])],
    );

    let entries = idx.render_entries();
    let mut sources: Vec<PathBuf> = entries
        .iter()
        .filter(|(view, _)| view == "users.show")
        .map(|(_, path)| path.clone())
        .collect();
    sources.sort();
    let mut expected = vec![controller, page];
    expected.sort();
    assert_eq!(sources, expected);
    assert!(!entries.iter().any(|(view, _)| view == "missing.view"));
}

#[test]
fn vars_for_view_returns_every_variable_sorted() {
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render(
            "dash",
            &[
                ("user", "App\\Models\\User"),
                ("posts", "App\\Models\\Post"),
            ],
        )],
    );
    idx.insert_file(
        PathBuf::from("/proj/AdminController.php"),
        &[render("dash", &[("user", "App\\Models\\Admin")])],
    );

    assert_eq!(
        idx.vars_for_view("dash"),
        vec![
            ("posts".to_string(), vec!["App\\Models\\Post".to_string()]),
            (
                "user".to_string(),
                vec![
                    "App\\Models\\Admin".to_string(),
                    "App\\Models\\User".to_string()
                ]
            ),
        ]
    );
    assert!(idx.vars_for_view("missing.view").is_empty());
}

// ---- view_name_for_path --------------------------------------------------

#[test]
fn view_name_strips_root_and_suffix() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/users/show.blade.php"),
            &roots
        ),
        Some("users.show".to_string())
    );
    assert_eq!(
        view_name_for_path(Path::new("/proj/resources/views/welcome.blade.php"), &roots),
        Some("welcome".to_string())
    );
}

#[test]
fn view_name_none_outside_roots() {
    let roots = vec![PathBuf::from("/proj/resources/views")];
    assert_eq!(
        view_name_for_path(Path::new("/proj/app/Models/User.php"), &roots),
        None
    );
}

#[test]
fn view_name_longest_root_wins() {
    // A package view root nested under the app's view root should win, yielding
    // the package-relative name rather than the deep app-relative one.
    let roots = vec![
        PathBuf::from("/proj/resources/views"),
        PathBuf::from("/proj/resources/views/vendor/pkg"),
    ];
    assert_eq!(
        view_name_for_path(
            Path::new("/proj/resources/views/vendor/pkg/button.blade.php"),
            &roots
        ),
        Some("button".to_string())
    );
}

// ---- view_name_for_path_namespaced ----------------------------------------

#[test]
fn view_name_namespaced_directory_maps_to_prefixed_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let ns_dir = dir
        .path()
        .join("modules/legal-contractmanagement/resources/views");
    let file = ns_dir.join("filament/pages/contract-edit-page.blade.php");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "").unwrap();

    let mut namespaces = HashMap::new();
    namespaces.insert("legal-contractmanagement".to_string(), ns_dir);
    let roots = vec![dir.path().join("resources/views")];

    assert_eq!(
        view_name_for_path_namespaced(&file, &roots, &namespaces),
        Some("legal-contractmanagement::filament.pages.contract-edit-page".to_string())
    );
}

#[test]
fn view_name_namespace_wins_over_plain_root_when_both_match() {
    // A namespace dir NESTED under a plain view root (e.g. a module's views
    // published under resources/views/modules/legal) must still key with the
    // namespace prefix, not the plain-root-relative name.
    let dir = tempfile::TempDir::new().unwrap();
    let plain_root = dir.path().join("resources/views");
    let ns_dir = plain_root.join("modules/legal");
    let file = ns_dir.join("show.blade.php");
    fs::create_dir_all(&ns_dir).unwrap();
    fs::write(&file, "").unwrap();

    let mut namespaces = HashMap::new();
    namespaces.insert("legal".to_string(), ns_dir);
    let roots = vec![plain_root];

    assert_eq!(
        view_name_for_path_namespaced(&file, &roots, &namespaces),
        Some("legal::show".to_string())
    );
}

#[test]
fn view_name_namespaced_falls_back_to_plain_roots() {
    // No namespace matches — falls through to `view_name_for_path`.
    let roots = vec![PathBuf::from("/proj/resources/views")];
    let namespaces: HashMap<String, PathBuf> = HashMap::new();
    assert_eq!(
        view_name_for_path_namespaced(
            Path::new("/proj/resources/views/users/show.blade.php"),
            &roots,
            &namespaces,
        ),
        Some("users.show".to_string())
    );
}

// ---- resolve_blade_member_accesses ---------------------------------------

use crate::salsa_impl::{Confidence, MemberAccessReferenceData};
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

const USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $fillable = ['email'];
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#;

/// A temp project with `app/Models/User.php` indexed, plus a ready resolver.
struct BladeProject {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
}

fn blade_project() -> BladeProject {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join("app/Models/User.php");
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(&model_path, USER_MODEL).unwrap();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    index.insert_file(
        &model_path,
        crate::class_hierarchy_index::classes_in_file(&model_path, USER_MODEL),
    );
    BladeProject {
        _dir: dir,
        index,
        root,
    }
}

/// A property-form member-access ref as the capture pass would emit it
/// (byte ranges unused for the Blade path — only receiver text + position).
fn member_ref(
    receiver: &str,
    member: &str,
    line: u32,
    column: u32,
) -> Arc<MemberAccessReferenceData> {
    Arc::new(MemberAccessReferenceData {
        member: member.to_string(),
        receiver: receiver.to_string(),
        receiver_byte_start: 0,
        receiver_byte_end: 0,
        is_nullsafe: false,
        form: AccessForm::Property,
        line,
        column,
        end_column: column + member.len() as u32,
        declaring_fqcn: None,
        kind: None,
        confidence: Confidence::Unresolved,
    })
}

#[test]
fn blade_var_resolves_via_view_index() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("app/Http/Controllers/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );

    // `{{ $user->email }}` captured at line 3, col 15.
    let refs = vec![member_ref("$user", "email", 3, 15)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &cache,
        &p.root,
        None,
    );

    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
    assert_eq!(entries[0].line, 3);
    assert_eq!(entries[0].column, 15);
}

#[test]
fn blade_unknown_member_is_dropped() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    // `nope` is not a column/accessor/relationship/property on User → dropped.
    let refs = vec![member_ref("$user", "nope", 1, 0)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &cache,
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

#[test]
fn blade_var_with_no_inferred_type_is_dropped() {
    let p = blade_project();
    let idx = ViewVarIndex::new(); // empty — nothing rendered this view
    let refs = vec![member_ref("$user", "email", 1, 0)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &cache,
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

/// A `SnapshotResolver` binding `currentUser` → the indexed `User` model.
fn user_bound_resolver(p: &BladeProject, key: &str) -> crate::member_resolver::SnapshotResolver {
    crate::member_resolver::SnapshotResolver {
        class_files: Arc::new(std::collections::HashMap::from([(
            "App\\Models\\User".to_string(),
            p.root.join("app/Models/User.php"),
        )])),
        bindings: Arc::new(std::collections::HashMap::from([(
            key.to_string(),
            "App\\Models\\User".to_string(),
        )])),
        facade_aliases: Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Default::default(),
        implementers: Default::default(),
    }
}

#[test]
fn blade_container_binding_receiver_resolves() {
    // `{{ app('currentUser')->email }}` — the receiver is a container
    // resolution, not a view variable, so it has no entry in the view-var index.
    // A binding-aware resolver types it to the bound model and the column
    // classifies. This is the path that lights up `app('currentTenant')->logo`.
    let p = blade_project();
    let idx = ViewVarIndex::new(); // no controller render — type comes from the binding
    let resolver = user_bound_resolver(&p, "currentUser");
    let refs = vec![member_ref("app('currentUser')", "email", 5, 12)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &resolver,
        &cache,
        &p.root,
        None,
    );
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
    assert_eq!(entries[0].line, 5);
}

#[test]
fn blade_container_binding_unknown_key_is_dropped() {
    // No binding registered for the accessed key → receiver unresolved → no
    // entry (the resolver is binding-aware but the registry is empty for it).
    let p = blade_project();
    let idx = ViewVarIndex::new();
    let resolver = user_bound_resolver(&p, "someOtherKey");
    let refs = vec![member_ref("app('currentUser')", "email", 1, 0)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &resolver,
        &cache,
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

#[test]
fn blade_relationship_resolves() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    // `{{ $user->posts }}` — relationship read as a property.
    let refs = vec![member_ref("$user", "posts", 2, 4)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &cache,
        &p.root,
        None,
    );
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "posts");
}

// ---- Volt: volt_property_types -------------------------------------------

/// Extract Volt prop types. Inline Eloquent values (`User::first()`) resolve via
/// the flow chain classifier, which needs only the `use` aliases — no on-disk
/// model — so a default resolver suffices here.
fn volt_types(src: &str) -> HashMap<String, String> {
    volt_property_types(
        src,
        &ClassHierarchyIndex::default(),
        &ClassViewCache::new(),
        Path::new("/proj"),
    )
}

#[test]
fn volt_typed_public_property() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;

new class extends Component {
    public User $user;
    public ?User $maybe;
    public int $count = 0;
};
?>
<div>{{ $this->user->email }}</div>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
    // Nullable still resolves.
    assert_eq!(
        types.get("maybe").map(String::as_str),
        Some("App\\Models\\User")
    );
    // Builtins are not classes — excluded.
    assert!(!types.contains_key("count"), "got {types:?}");
}

#[test]
fn volt_functional_mount_assignment() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{state, mount};

state(['user']);

mount(function (User $user) {
    $this->user = $user;
});
?>
<div>{{ $this->user->email }}</div>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_class_mount_assignment() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;

new class extends Component {
    public $user;
    public function mount(User $account) {
        $this->user = $account;
    }
};
?>
"#;
    let types = volt_types(src);
    assert_eq!(
        types.get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_untyped_state_yields_nothing() {
    let src = r#"<?php
use function Livewire\Volt\{state};
state(['count' => 0]);
?>
<div>{{ $count }}</div>
"#;
    assert!(volt_types(src).is_empty());
}

#[test]
fn volt_state_typed_initial_value() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{state};
state(['user' => User::find(1)]);
?>
<div>{{ $user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_computed_explicit_return_type() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{computed};
$user = computed(fn (): User => User::find(1));
?>
<div>{{ $this->user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_computed_inferred_from_body() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{computed};
$user = computed(fn () => User::firstWhere('id', 1));
?>
<div>{{ $this->user->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_with_closure_array() {
    let src = r#"<?php
use App\Models\User;
use function Livewire\Volt\{with};
with(fn () => ['account' => User::first()]);
?>
<div>{{ $account->email }}</div>
"#;
    assert_eq!(
        volt_types(src).get("account").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_class_render_view_data() {
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public function render() {
        return view('livewire.users', ['account' => User::first()]);
    }
};
?>
"#;
    assert_eq!(
        volt_types(src).get("account").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_typed_property_wins_over_state() {
    // A typed public prop must not be downgraded by a same-named state entry.
    let src = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public User $user;
};
?>
"#;
    assert_eq!(
        volt_types(src).get("user").map(String::as_str),
        Some("App\\Models\\User")
    );
}

// ---- Volt: resolve_volt_member_accesses ----------------------------------

#[test]
fn volt_resolves_this_property_access() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("user".to_string(), "App\\Models\\User".to_string());

    // `{{ $this->user->email }}` — receiver captured as `$this->user`.
    let refs = vec![member_ref("$this->user", "email", 5, 18)];
    let cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &cache, &p.root, None);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
}

#[test]
fn volt_resolves_bare_public_property_access() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("user".to_string(), "App\\Models\\User".to_string());

    // Public properties are also readable bare in the template: `{{ $user->email }}`.
    let refs = vec![member_ref("$user", "email", 1, 0)];
    let cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &cache, &p.root, None);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
}

#[test]
fn volt_unknown_property_is_dropped() {
    let p = blade_project();
    let types = HashMap::new(); // nothing inferred
    let refs = vec![member_ref("$this->user", "email", 1, 0)];
    let cache = ClassViewCache::new();
    let entries = resolve_volt_member_accesses(&refs, &types, &[], &p.index, &cache, &p.root, None);
    assert!(entries.is_empty(), "got {entries:?}");
}

// ---- MFC / @foreach loop-variable typing ---------------------------------

#[test]
fn volt_computed_attribute_collection_body_inference() {
    // `#[Computed] public function users(): Collection { return User::...->get(); }`
    // — declared return type is a bare Collection, so the element type must come
    // from the body's flow chain (→ App\Models\User).
    let src = r#"<?php
use App\Models\User;
use Illuminate\Database\Eloquent\Collection;
use Livewire\Volt\Component;
new class extends Component {
    #[Computed]
    public function users(): Collection {
        return User::with("roles")->orderBy("name")->get();
    }
};
?>
"#;
    assert_eq!(
        volt_types(src).get("users").map(String::as_str),
        Some("App\\Models\\User")
    );
}

#[test]
fn volt_foreach_loop_var_resolves_from_this_computed() {
    let p = blade_project();
    // `users` computed yields User elements.
    let mut types = HashMap::new();
    types.insert("users".to_string(), "App\\Models\\User".to_string());

    // `@foreach($this->users as $user)` on line 5; `{{ $user->email }}` on line 7.
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$this->users".to_string(),
        start_line: 5,
        end_line: 20,
    }];
    let refs = vec![member_ref("$user", "email", 7, 40)];
    let cache = ClassViewCache::new();
    let entries =
        resolve_volt_member_accesses(&refs, &types, &loops, &p.index, &cache, &p.root, None);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
    assert_eq!(entries[0].member, "email");
}

#[test]
fn blade_foreach_loop_var_resolves_from_view_var() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    // Controller passed `users` (a User collection → element type User).
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.index", &[("users", "App\\Models\\User")])],
    );
    // `@foreach($users as $user)` lines 2..10; `{{ $user->email }}` line 4.
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$users".to_string(),
        start_line: 2,
        end_line: 10,
    }];
    let refs = vec![member_ref("$user", "email", 4, 12)];
    let cache = ClassViewCache::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.index",
        &idx,
        &loops,
        &p.index,
        &cache,
        &p.root,
        None,
    );
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].fqcn, "App\\Models\\User");
}

#[test]
fn loop_var_outside_loop_range_is_dropped() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("users".to_string(), "App\\Models\\User".to_string());
    let loops = vec![crate::salsa_impl::BladeLoopVar {
        item_var: "user".to_string(),
        iterable: "$this->users".to_string(),
        start_line: 5,
        end_line: 20,
    }];
    // Access on line 30 — outside the loop body → no resolution.
    let refs = vec![member_ref("$user", "email", 30, 0)];
    let cache = ClassViewCache::new();
    let entries =
        resolve_volt_member_accesses(&refs, &types, &loops, &p.index, &cache, &p.root, None);
    assert!(entries.is_empty(), "got {entries:?}");
}

// ---- Livewire/Volt component member references ----------------------------

#[test]
fn component_member_this_access_keyed_under_synthetic_id() {
    // Volt SFC: anonymous class, so `$this->entities` keys under a synthetic
    // per-file component id. Only declared members are indexed (not framework
    // calls like `$this->dispatch`).
    let src = r#"<?php
use Illuminate\Database\Eloquent\Collection;
use Livewire\Volt\Component;
new class extends Component {
    public ?int $editingId = null;
    #[Computed]
    public function entities(): Collection { return Entity::all(); }
    public function loadStuff(): void {
        foreach ($this->entities as $e) {}
    }
};
"#;
    let path = Path::new("/proj/resources/views/pages/permissions.php");
    let refs = vec![
        member_ref("$this", "entities", 8, 30),
        member_ref("$this", "editingId", 5, 10),
        member_ref("$this", "dispatch", 9, 10), // framework method, not declared
    ];
    let entries = resolve_component_member_accesses(path, src, &refs);
    let key = format!("volt::{}", path.display());
    assert_eq!(entries.len(), 2, "got {entries:?}");
    assert!(entries.iter().all(|e| e.fqcn == key));
    assert!(entries.iter().any(|e| e.member == "entities"));
    assert!(entries.iter().any(|e| e.member == "editingId"));
    assert!(!entries.iter().any(|e| e.member == "dispatch"));
}

#[test]
fn non_component_php_yields_no_component_entries() {
    let src = "<?php\nclass UserController { public function show() { return $this->foo; } }\n";
    let refs = vec![member_ref("$this", "foo", 1, 10)];
    let entries = resolve_component_member_accesses(
        Path::new("/proj/app/Http/Controllers/UserController.php"),
        src,
        &refs,
    );
    assert!(entries.is_empty(), "got {entries:?}");
}

#[test]
fn volt_component_key_variants() {
    // .php SFC (inline anonymous component class)
    assert_eq!(
        volt_component_key(
            Path::new("/proj/x/users.php"),
            "<?php\nnew class extends Component {};\n"
        ),
        Some("volt::/proj/x/users.php".to_string())
    );
    // .blade SFC (own Volt signature)
    assert_eq!(
        volt_component_key(
            Path::new("/proj/x/page.blade.php"),
            "<?php\nuse Livewire\\Volt\\Component;\nnew class extends Component {};\n?>\n<div></div>"
        ),
        Some("volt::/proj/x/page.blade.php".to_string())
    );
    // plain model — not a component
    assert_eq!(
        volt_component_key(
            Path::new("/proj/app/Models/User.php"),
            "<?php\nclass User {}\n"
        ),
        None
    );
}

// ─── Dependency recording (incremental save, #80) ──────────────────────────

#[test]
fn blade_deps_record_attempted_var_types_even_on_unknown_member() {
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("app/Http/Controllers/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );

    // `notAColumn` doesn't classify — the dependency must still register.
    let refs = vec![member_ref("$user", "notAColumn", 3, 15)];
    let cache = ClassViewCache::new();
    let mut deps = HashSet::new();
    let entries = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &cache,
        &p.root,
        Some(&mut deps),
    );
    assert!(entries.is_empty(), "got {entries:?}");
    assert!(deps.contains("App\\Models\\User"), "{deps:?}");
}

#[test]
fn volt_deps_record_attempted_prop_types_even_on_unknown_member() {
    let p = blade_project();
    let mut types = HashMap::new();
    types.insert("user".to_string(), "App\\Models\\User".to_string());

    let refs = vec![member_ref("$this->user", "notAColumn", 1, 0)];
    let cache = ClassViewCache::new();
    let mut deps = HashSet::new();
    let entries = resolve_volt_member_accesses(
        &refs,
        &types,
        &[],
        &p.index,
        &cache,
        &p.root,
        Some(&mut deps),
    );
    assert!(entries.is_empty(), "got {entries:?}");
    assert!(deps.contains("App\\Models\\User"), "{deps:?}");
}

#[test]
fn remove_file_preserves_other_files_contributions() {
    let mut idx = ViewVarIndex::new();
    let a = PathBuf::from("/proj/app/Http/Controllers/A.php");
    let b = PathBuf::from("/proj/app/Http/Controllers/B.php");
    // Both controllers feed the same view with different types for `user`.
    idx.insert_file(
        a.clone(),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    idx.insert_file(
        b,
        &[render("users.show", &[("user", "App\\Models\\Admin")])],
    );

    idx.remove_file(&a);
    // B's contribution must survive A's eviction (the old implementation
    // dropped the whole view).
    let types = idx.var_types("users.show", "user");
    assert_eq!(types, vec!["App\\Models\\Admin".to_string()]);
}

#[test]
fn renders_for_returns_current_contribution() {
    let mut idx = ViewVarIndex::new();
    let a = PathBuf::from("/proj/app/Http/Controllers/A.php");
    assert!(idx.renders_for(&a).is_none());
    let r = render("users.show", &[("user", "App\\Models\\User")]);
    idx.insert_file(a.clone(), std::slice::from_ref(&r));
    assert_eq!(idx.renders_for(&a), Some(std::slice::from_ref(&r)));
}

// ─── M1 single-parse capture: view-render + Volt-surface equivalence ──────
//
// Evaluating captured plans must be byte-identical to the live re-parse path,
// across every render/Volt-surface shape the fixtures exercise.

/// Resolve a controller's view() renders BOTH ways (live `view_renders_in_file`
/// vs captured `evaluate_render_plans`), sorted for an order-independent
/// comparison.
fn render_both_ways(
    resolver: &impl ClassFileResolver,
    root: &Path,
    controller: &str,
) -> (Vec<ViewRender>, Vec<ViewRender>) {
    let sort = |mut v: Vec<ViewRender>| {
        v.sort_by(|a, b| a.view_name.cmp(&b.view_name));
        v
    };
    let live = view_renders_in_file(controller, resolver, &ClassViewCache::new(), root);
    let tree = crate::parser::parse_php(controller).unwrap();
    let aliases = crate::query_chain::use_aliases::extract_use_aliases(&tree, controller);
    let plans = capture_render_plans(controller, &tree, &aliases);
    let captured = evaluate_render_plans(&plans, &aliases, resolver, &ClassViewCache::new(), root);
    (sort(live), sort(captured))
}

#[test]
fn captured_render_plans_match_live_across_shapes() {
    let p = blade_project();
    let controller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        $u2 = User::first();
        view('a.array', ['user' => $user, 'other' => 42]);   // array (one typed, one not)
        view('a.compact', compact('user'));                  // compact
        view('a.with')->with('user', $u2);                   // ->with(k, v)
        view('a.witharr')->with(['user' => $user]);          // ->with([...])
        view('a.dynamic', ['x' => $undefinedVar]);           // value that won't type
        view($dynamicName, ['user' => $user]);               // dynamic view name → skipped by both
    }
}
"#;
    let (live, captured) = render_both_ways(&p.index, &p.root, controller);
    assert_eq!(live, captured, "captured render plans diverged from live");
    assert!(
        live.iter().any(|r| r.vars.contains_key("user")),
        "fixture typed no view vars — comparison would be near-vacuous"
    );
}

/// Type a Volt SFC's front-matter BOTH ways (live `volt_property_types` vs
/// captured `evaluate_volt_surface`).
fn volt_both_ways(
    resolver: &impl ClassFileResolver,
    root: &Path,
    src: &str,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let live = volt_property_types(src, resolver, &ClassViewCache::new(), root);
    let surface = capture_volt_surface(src).expect("volt surface captured");
    let captured = evaluate_volt_surface(&surface, resolver, &ClassViewCache::new(), root);
    (live, captured)
}

#[test]
fn captured_volt_surface_matches_live_across_shapes() {
    // Typed prop (authoritative), computed (body-inferred), state, with,
    // mount-assignment, and `$x = computed(...)` — the whole surface zoo.
    let src = r#"<?php
use App\Models\User;
use Illuminate\Support\Collection;
use Livewire\Volt\Component;
use function Livewire\Volt\{state, computed, mount, with};

new class extends Component {
    public User $user;               // typed prop — authoritative
    public int $count = 0;           // non-class, dropped

    public function mount(User $injected) {
        $this->user = $injected;     // or_insert, loses to the typed prop
    }

    #[Computed]
    public function latest(): Collection { return User::query()->latest()->get(); }

    public function with(): array {
        return ['viaWith' => User::first()];
    }
};

$posts = computed(fn (): User => User::first());
state(['seed' => User::first()]);
?>
<div>{{ $this->user->email }}</div>
"#;
    let (live, captured) = volt_both_ways(&ClassHierarchyIndex::default(), Path::new("/proj"), src);
    assert_eq!(live, captured, "captured Volt surface diverged from live");
    assert_eq!(
        live.get("user").map(String::as_str),
        Some("App\\Models\\User"),
        "typed prop must stay authoritative"
    );
}

/// Build a Blade `MemberContextData` as the parse-time capture does (per-site
/// recipes from receiver-text snippets; empty file-level aliases).
fn blade_context(refs: &[Arc<MemberAccessReferenceData>]) -> crate::salsa_impl::MemberContextData {
    let sites = refs
        .iter()
        .map(|m| crate::member_resolver::compile_blade_site(m.receiver.trim()))
        .collect();
    crate::salsa_impl::MemberContextData {
        aliases: Default::default(),
        sites,
        view_renders: Vec::new(),
        volt_surface: None,
        component: None,
    }
}

#[test]
fn captured_blade_member_accesses_match_live() {
    // A bare `$user` (view-var path — recipe unused) and a chain receiver
    // `app('currentUser')->email` (recipe path) resolved both ways.
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    let resolver = user_bound_resolver(&p, "currentUser");
    let refs = vec![
        member_ref("$user", "posts", 2, 4),
        member_ref("app('currentUser')", "email", 3, 8),
    ];

    let live = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &resolver,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    let ctx = blade_context(&refs);
    let captured = resolve_blade_member_accesses_with_context(
        &ctx,
        &refs,
        "users.show",
        &idx,
        &[],
        &resolver,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert_eq!(live, captured, "captured Blade accesses diverged from live");
    assert_eq!(captured.len(), 2, "both sites should resolve: {captured:?}");
}

// ─── M1 review follow-ups: Bug 1 & Bug 2 regressions + coverage ───────────

#[test]
fn captured_volt_duplicate_key_within_handler_is_last_wins() {
    // Bug 1: within ONE render() handler the tree engine merges the array data
    // and the chained ->with into a single temp map with `insert` (LAST-wins),
    // then or_inserts across handlers. A flat first-wins replay wrongly kept the
    // array value. Trigger: array `account => User` shadowed by `->with('account',
    // Admin)` in the same handler → must be Admin, with the Admin dep.
    let src = r#"<?php
use App\Models\User;
use App\Models\Admin;
use Livewire\Volt\Component;
new class extends Component {
    public function render() {
        return view('x', ['account' => User::first()])->with('account', Admin::first());
    }
};
?>
<div>{{ $this->account->id }}</div>
"#;
    let (live, captured) = volt_both_ways(&ClassHierarchyIndex::default(), Path::new("/proj"), src);
    assert_eq!(
        live, captured,
        "duplicate-key-within-handler diverged from live"
    );
    assert_eq!(
        live.get("account").map(String::as_str),
        Some("App\\Models\\Admin"),
        "within-handler last-wins: ->with('account', Admin) must beat the array value"
    );
}

#[test]
fn captured_volt_first_handler_wins_across_handlers() {
    // The other half of the precedence: across DISTINCT handlers a key is
    // FIRST-VISITED-handler-wins (`fold_or_insert` → `or_insert`). BOTH engines
    // walk the front-matter with the SAME stack DFS, which pops sibling
    // statements last-to-first — so of two top-level handlers the SOURCE-LAST one
    // (`with` here) is visited first and wins. The point of this test is that
    // live == captured for the cross-handler fold AND the winner is a specific,
    // non-empty value (so it can't pass vacuously) — not the source position.
    let src = r#"<?php
use App\Models\User;
use App\Models\Admin;
use Livewire\Volt\Component;
use function Livewire\Volt\{state, with};
new class extends Component {};
state(['a' => User::first()]);
with(fn () => ['a' => Admin::first()]);
?>
<div>{{ $this->a }}</div>
"#;
    let (live, captured) = volt_both_ways(&ClassHierarchyIndex::default(), Path::new("/proj"), src);
    assert_eq!(
        live, captured,
        "cross-handler precedence diverged from live"
    );
    // First-VISITED handler wins: the DFS reaches `with` (source-last) before
    // `state`, so `a = Admin` in both engines.
    assert_eq!(
        live.get("a").map(String::as_str),
        Some("App\\Models\\Admin"),
        "cross-handler or_insert: the first-VISITED handler (with) must win"
    );
    assert!(
        !live.is_empty(),
        "surface resolved nothing — test would be vacuous"
    );
}

#[test]
fn captured_volt_within_handler_earlier_resolved_beats_unresolvable_later() {
    // The precise over-correction guard for Bug 1's fix: WITHIN one handler the
    // tree engine inserts ONLY when the value resolves (resolvability is
    // cross-file / eval-time), so a LATER UNRESOLVABLE duplicate must NOT clobber
    // an EARLIER resolved one. A capture-time last-occurrence dedup would wrongly
    // keep the unresolvable later value and drop `account` entirely. (`$mystery`
    // is an undefined local → unresolvable at eval.) Both the array-literal
    // duplicate and the view([...])->with('same', $mystery) chain form.
    let array_form = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
use function Livewire\Volt\with;
new class extends Component {};
with(fn () => ['account' => User::first(), 'account' => $mystery]);
?>
<div>{{ $this->account }}</div>
"#;
    let chain_form = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public function render() {
        return view('x', ['account' => User::first()])->with('account', $mystery);
    }
};
?>
<div>{{ $this->account }}</div>
"#;
    for src in [array_form, chain_form] {
        let (live, captured) =
            volt_both_ways(&ClassHierarchyIndex::default(), Path::new("/proj"), src);
        assert_eq!(
            live, captured,
            "earlier-resolved-vs-later-unresolvable diverged from live:\n{src}"
        );
        assert_eq!(
            live.get("account").map(String::as_str),
            Some("App\\Models\\User"),
            "earlier RESOLVED value must win over a later unresolvable one:\n{src}"
        );
    }
}

#[test]
fn is_volt_needle_without_frontmatter_routes_to_volt_not_blade() {
    // Bug 2: `source_contains_volt_signature` is a raw-byte needle scan (no
    // `<?php` required), so a plain template with a stray `computed(` and no
    // front-matter is is_volt=true / volt_surface=None. The OLD engine and the
    // save-refresh route such a file to the VOLT resolver (empty props); the
    // build path must too — never to the Blade resolver, whose view-var index
    // would resolve `$user->email` and whose `view_name_for_path?` could drop
    // the whole file.
    let p = blade_project();
    let mut idx = ViewVarIndex::new();
    idx.insert_file(
        p.root.join("C.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );

    let source = "<div x-data=\"{ get computed() { return 1 } }\">{{ $user->email }}</div>\n";
    assert!(
        crate::livewire_resolver::source_contains_volt_signature(source),
        "test needs the volt needle to fire — signature function changed?"
    );
    let path = p.root.join("resources/views/users/show.blade.php");
    let refs = vec![member_ref("$user", "email", 0, 20)];
    let ctx = crate::member_capture::capture_member_context(&path, source, None, &refs, true)
        .expect("is_volt file with refs captures context");
    assert!(
        ctx.volt_surface.is_none(),
        "a needle without a <?php block must have no captured Volt surface"
    );

    // What BOTH build and save now call: the Volt resolver with an empty prop
    // map. It must match the OLD tree Volt path (also an empty map).
    let volt_ctx = resolve_volt_member_accesses_with_context(
        &ctx,
        &refs,
        &HashMap::new(),
        &[],
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    let volt_tree = resolve_volt_member_accesses(
        &refs,
        &volt_property_types(source, &p.index, &ClassViewCache::new(), &p.root),
        &[],
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert_eq!(
        volt_ctx, volt_tree,
        "Volt build path diverged from Volt save/old"
    );

    // And the routing genuinely matters: the Blade path WOULD resolve
    // `$user->email` via the view-var index — the bug's misroute. The fix picks
    // Volt (empty), so the two paths differ.
    let blade = resolve_blade_member_accesses(
        &refs,
        "users.show",
        &idx,
        &[],
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert_ne!(
        volt_ctx, blade,
        "Volt vs Blade must differ here, else the routing fix is untested"
    );
    assert!(volt_ctx.is_empty() && !blade.is_empty());
}

#[test]
fn captured_render_multiple_sites_same_view_name() {
    // Two `view('x', …)` sites for the SAME view name must both survive in
    // order, and match live (the view-var index unions them downstream).
    let p = blade_project();
    let controller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function a(User $user) { view('shared', ['user' => $user]); }
    public function b(User $user) { view('shared', ['also' => $user]); }
}
"#;
    let (live, captured) = render_both_ways(&p.index, &p.root, controller);
    assert_eq!(live, captured, "multi-site same-name renders diverged");
    assert_eq!(
        live.iter().filter(|r| r.view_name == "shared").count(),
        2,
        "both same-name render sites must be present: {live:?}"
    );
}

#[test]
fn captured_render_duplicate_array_key_last_wins() {
    // A duplicate key inside ONE `view()` array is last-wins (the tree's
    // `vars.insert`); the controller render path already uses `insert`, so
    // capture must agree.
    let p = blade_project();
    let controller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
use App\Models\Admin;
class C {
    public function show(User $user, Admin $admin) {
        view('x', ['who' => $user, 'who' => $admin]);
    }
}
"#;
    let (live, captured) = render_both_ways(&p.index, &p.root, controller);
    assert_eq!(live, captured, "duplicate array key diverged");
    assert_eq!(
        live[0].vars.get("who").map(String::as_str),
        Some("App\\Models\\Admin"),
        "last array entry wins"
    );
}

#[test]
fn captured_component_member_accesses_match_live() {
    // resolve_component_member_accesses_with_context (captured member names for
    // a Volt SFC) must match the live re-parse for the component's `$this->…`
    // reads.
    let p = blade_project();
    let source = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public User $user;
    public int $count = 0;
    public function increment() { $this->count++; }
};
?>
<div>{{ $this->user }} {{ $this->count }}</div>
"#;
    let path = p.root.join("resources/views/livewire/counter.blade.php");
    let refs = vec![
        member_ref("$this", "user", 10, 4),
        member_ref("$this", "count", 10, 20),
        member_ref("$this", "increment", 11, 4),
        member_ref("$this", "notAMember", 12, 4),
    ];

    let live = resolve_component_member_accesses(&path, source, &refs);
    let comp = capture_component(&path, source, true).expect("volt SFC → component context");
    let captured = resolve_component_member_accesses_with_context(&comp, &path, &refs);

    assert_eq!(
        live, captured,
        "component member accesses diverged from live"
    );
    assert_eq!(
        captured.len(),
        3,
        "user/count/increment index; notAMember drops"
    );
    assert!(captured.iter().all(|e| e.fqcn.starts_with("volt::")));
}

#[test]
fn every_class_in_a_file_gets_its_own_render_site() {
    // Two classes, two render sites — neither drops, in document order.
    let src = r#"<?php
class PageA { protected string $view = 'pages.a'; }
class PageB { protected string $view = 'pages.b'; }
"#;
    let r = renders(src);
    let names: Vec<&str> = r.iter().map(|v| v.view_name.as_str()).collect();
    assert_eq!(names, vec!["pages.a", "pages.b"]);
}

#[test]
fn anonymous_class_and_host_never_absorb_each_other() {
    // BOTH directions: the anonymous class nested in a method gets its OWN
    // render site without the host's members, AND the host's surface walk
    // stops at the class boundary instead of descending through the method
    // body into the anonymous class — each site's vars are EXACTLY its own
    // class's members.
    let src = r#"<?php
use App\Models\Secret;
use App\Models\User;
class SettingsPage {
    public User $user;
    protected string $view = 'pages.settings';
    public function modal() {
        return new class {
            public Secret $secret;
            protected string $view = 'pages.modal';
        };
    }
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 2, "got {r:?}");
    let settings = r
        .iter()
        .find(|v| v.view_name == "pages.settings")
        .expect("the host page keeps its render site");
    let mut settings_vars: Vec<&str> = settings.vars.keys().map(String::as_str).collect();
    settings_vars.sort_unstable();
    assert_eq!(
        settings_vars,
        vec!["user"],
        "the host's surface is exactly its own members — the anonymous \
         class's $secret must not fold in: {:?}",
        settings.vars
    );
    let modal = r
        .iter()
        .find(|v| v.view_name == "pages.modal")
        .expect("the anonymous class contributes its own site");
    let mut modal_vars: Vec<&str> = modal.vars.keys().map(String::as_str).collect();
    modal_vars.sort_unstable();
    assert_eq!(
        modal_vars,
        vec!["secret"],
        "…and vice versa: the host's $user stays out: {:?}",
        modal.vars
    );
}

#[test]
fn captured_plan_surface_stops_at_the_class_boundary_too() {
    // The plan path (`capture_class_surface_items`) shares the bounded
    // descent — parity with the live walk on the two-directional fixture.
    let src = r#"<?php
use App\Models\Secret;
use App\Models\User;
class SettingsPage {
    public User $user;
    protected string $view = 'pages.settings';
    public function modal() {
        return new class {
            public Secret $secret;
            protected string $view = 'pages.modal';
        };
    }
}
"#;
    let p = blade_project();
    let (live, captured) = render_both_ways(&p.index, &p.root, src);
    assert_eq!(
        live, captured,
        "captured $view render plans diverged from live"
    );
    let settings = live
        .iter()
        .find(|v| v.view_name == "pages.settings")
        .expect("host site");
    assert!(
        !settings.vars.contains_key("secret"),
        "plan path leaked the anonymous class's member: {:?}",
        settings.vars
    );
}

#[test]
fn view_property_heredoc_is_skipped() {
    let src = "<?php\nclass DynamicPage {\n    protected string $view = <<<'VIEW'\npages.dynamic\nVIEW;\n}\n";
    let r = renders(src);
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn global_with_helper_does_not_pollute_the_surface() {
    // Laravel's global `with($value, $callback)` is NOT Volt's functional
    // `with(fn () => [...])` — only a LEADING closure argument contributes.
    let src = r#"<?php
use App\Models\User;
class ReportPage {
    protected string $view = 'pages.report';

    public function boot() {
        return with(User::first(), fn () => ['leaked' => User::first()]);
    }
}
"#;
    let r = renders(src);
    assert_eq!(r.len(), 1, "got {r:?}");
    assert!(
        !r[0].vars.contains_key("leaked"),
        "global with() helper leaked into the surface: {:?}",
        r[0].vars
    );
}

// ---- view_names_for_path_namespaced ----------------------------------------

#[test]
fn namespace_selection_is_deterministic_across_map_instances() {
    // Two prefixes registered against the SAME directory: the winner must
    // not depend on HashMap iteration order. Alphabetical tiebreak, checked
    // across many fresh maps.
    let dir = PathBuf::from("/proj/modules/shop/resources/views");
    let file = dir.join("index.blade.php");
    for _ in 0..100 {
        let mut namespaces = std::collections::HashMap::new();
        namespaces.insert("shop".to_string(), dir.clone());
        namespaces.insert("alpha".to_string(), dir.clone());
        assert_eq!(
            view_name_for_path_namespaced(&file, &[], &namespaces).as_deref(),
            Some("alpha::index"),
            "same inputs, same answer, every run"
        );
    }
}

#[test]
fn published_vendor_override_maps_to_the_namespaced_name() {
    // `resolve_view_path("ns::x")` probes the published override at
    // `{view_root}/vendor/ns/x.blade.php` — the reverse direction must
    // agree, since the published copy is the file actually being edited.
    let root = PathBuf::from("/proj/resources/views");
    let mut namespaces = std::collections::HashMap::new();
    namespaces.insert(
        "filament-panels".to_string(),
        PathBuf::from("/proj/vendor/filament/panels/resources/views"),
    );
    let published = root.join("vendor/filament-panels/pages/auth/login.blade.php");
    assert_eq!(
        view_name_for_path_namespaced(&published, std::slice::from_ref(&root), &namespaces)
            .as_deref(),
        Some("filament-panels::pages.auth.login")
    );
    // An unregistered directory under vendor/ is NOT a namespace.
    let stray = root.join("vendor/unregistered/x.blade.php");
    assert_eq!(
        view_name_for_path_namespaced(&stray, &[root], &namespaces).as_deref(),
        Some("vendor.unregistered.x"),
        "falls through to the plain dotted name"
    );
}

#[test]
fn namespace_dir_inside_a_view_root_keeps_both_names() {
    // A namespace registered INSIDE a plain view root: the file answers to
    // both its `ns::` name and its plain dotted name, namespaced first —
    // a controller's view('admin.dashboard') render site stays connected.
    let root = PathBuf::from("/proj/resources/views");
    let mut namespaces = std::collections::HashMap::new();
    namespaces.insert("admin".to_string(), root.join("admin"));
    let file = root.join("admin/dashboard.blade.php");
    assert_eq!(
        view_names_for_path_namespaced(&file, std::slice::from_ref(&root), &namespaces),
        vec![
            "admin::dashboard".to_string(),
            "admin.dashboard".to_string()
        ]
    );
}

#[test]
fn render_entries_are_sorted_for_deterministic_first_match() {
    let mut idx = ViewVarIndex::new();
    for name in ["zeta", "alpha", "midway"] {
        idx.insert_file(
            PathBuf::from(format!("/proj/{name}/Controller.php")),
            &[render("users.show", &[("user", "App\\Models\\User")])],
        );
    }
    let entries = idx.render_entries();
    let mut sorted = entries.clone();
    sorted.sort();
    assert_eq!(entries, sorted, "HashMap order must not leak to callers");
}

#[test]
fn generation_advances_on_every_mutation_and_holds_otherwise() {
    let mut idx = ViewVarIndex::new();
    assert_eq!(idx.generation(), 0, "a fresh index is generation 0");

    idx.insert_file(
        PathBuf::from("/proj/UserController.php"),
        &[render("users.show", &[("user", "App\\Models\\User")])],
    );
    let after_insert = idx.generation();
    assert_ne!(after_insert, 0, "insert_file must advance the generation");

    // A read is not a mutation: the Salsa push is skipped on an unchanged
    // generation, so a read that bumped it would invalidate the memo per call.
    let _ = idx.render_entries();
    let _ = idx.var_types("users.show", "user");
    assert_eq!(idx.generation(), after_insert, "reads must not advance it");

    idx.remove_file(Path::new("/proj/UserController.php"));
    let after_remove = idx.generation();
    assert_ne!(
        after_remove, after_insert,
        "remove_file must advance the generation"
    );

    // Removing a file that contributed nothing changes no state, so it must
    // not advance either — otherwise every no-op eviction costs a re-push.
    idx.remove_file(Path::new("/proj/NeverIndexed.php"));
    assert_eq!(
        idx.generation(),
        after_remove,
        "a no-op remove must not advance it"
    );

    idx.clear();
    assert_ne!(idx.generation(), after_remove, "clear must advance it");
}
