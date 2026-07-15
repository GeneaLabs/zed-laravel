//! Tests for the magic-member classifier (`classify_member`).
//!
//! Each test builds a real model on disk (via `tempfile`), runs the existing
//! `chain::analyze` to get an inheritance-resolved `ClassView`, then asserts
//! the classification of a member access against it.

use super::*;
use crate::laravel_introspector::chain::analyze;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Write a single model into a PSR-4 temp project and analyze it.
/// Returns the temp dir (keep alive) + the resolved `ClassView`.
fn model(model_php: &str) -> (TempDir, ClassView) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, model_php).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let view = analyze(&path, dir.path()).expect("should analyze model");
    (dir, view)
}

/// Write a model plus an extra PSR-4 file (e.g. a trait) and analyze the model.
fn model_with_extra(model_php: &str, extra_rel: &str, extra_php: &str) -> (TempDir, ClassView) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, model_php).unwrap();
    let extra_path: PathBuf = dir.path().join(extra_rel);
    fs::create_dir_all(extra_path.parent().unwrap()).unwrap();
    fs::write(&extra_path, extra_php).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let view = analyze(&path, dir.path()).expect("should analyze model");
    (dir, view)
}

#[test]
fn classifies_scope_as_call() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#,
    );
    let c = classify_member(&view, "active", AccessForm::StaticCall).expect("scope");
    assert_eq!(c.kind, MagicMemberKind::Scope);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");

    // A scope is not reachable via property read.
    assert!(classify_member(&view, "active", AccessForm::Property).is_none());
}

#[test]
fn classifies_old_style_accessor_as_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function getFullNameAttribute(): string { return 'x'; }
}
"#,
    );
    let c = classify_member(&view, "full_name", AccessForm::Property).expect("accessor");
    assert_eq!(c.kind, MagicMemberKind::Accessor);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_relationship_both_forms() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#,
    );
    let as_prop = classify_member(&view, "posts", AccessForm::Property).expect("rel prop");
    assert_eq!(as_prop.kind, MagicMemberKind::Relationship);

    let as_call = classify_member(&view, "posts", AccessForm::InstanceCall).expect("rel call");
    assert_eq!(as_call.kind, MagicMemberKind::Relationship);
    assert_eq!(as_call.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_column_as_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#,
    );
    let c = classify_member(&view, "email", AccessForm::Property).expect("column");
    assert_eq!(c.kind, MagicMemberKind::Column);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn classifies_dynamic_finder_as_call() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email_address'];
}
"#,
    );
    let c = classify_member(&view, "whereEmailAddress", AccessForm::StaticCall)
        .expect("dynamic finder");
    assert_eq!(c.kind, MagicMemberKind::DynamicFinder);
    assert_eq!(c.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn dynamic_finder_requires_known_column() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#,
    );
    // `whereNonexistent` is not backed by a column → not a dynamic finder.
    assert!(classify_member(&view, "whereNonexistent", AccessForm::StaticCall).is_none());
    // `whereabouts` (lowercase remainder) must not be mistaken for a finder.
    assert!(classify_member(&view, "whereabouts", AccessForm::StaticCall).is_none());
}

#[test]
fn accessor_shadows_column_of_same_name() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['name'];
    public function getNameAttribute(): string { return 'x'; }
}
"#,
    );
    let c = classify_member(&view, "name", AccessForm::Property).expect("accessor wins");
    assert_eq!(
        c.kind,
        MagicMemberKind::Accessor,
        "an accessor must win over a raw column of the same name"
    );
}

#[test]
fn classifies_plain_method_and_property() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public $nickname;
    public function doThing(): void {}
}
"#,
    );
    let method = classify_member(&view, "doThing", AccessForm::InstanceCall).expect("method");
    assert_eq!(method.kind, MagicMemberKind::PlainMember);

    let prop = classify_member(&view, "nickname", AccessForm::Property).expect("property");
    assert_eq!(prop.kind, MagicMemberKind::PlainMember);
}

#[test]
fn unknown_member_classifies_to_none() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {}
"#,
    );
    assert!(classify_member(&view, "totallyUnknown", AccessForm::Property).is_none());
    assert!(classify_member(&view, "totallyUnknown", AccessForm::InstanceCall).is_none());
}

#[test]
fn trait_shared_scope_attributes_to_the_trait() {
    // The plan keys magic members by their *declaring* FQCN so a trait-shared
    // scope keys once. Here the scope is declared on a trait the model uses;
    // the declaring class must be the trait, not the model.
    let (_d, view) = model_with_extra(
        r#"<?php
namespace App\Models;
use App\Concerns\Activatable;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    use Activatable;
}
"#,
        "app/Concerns/Activatable.php",
        r#"<?php
namespace App\Concerns;
use Illuminate\Database\Eloquent\Builder;
trait Activatable {
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#,
    );
    let c = classify_member(&view, "active", AccessForm::StaticCall).expect("trait scope");
    assert_eq!(c.kind, MagicMemberKind::Scope);
    assert_eq!(
        c.declaring_fqcn, "App\\Concerns\\Activatable",
        "a trait-shared scope must attribute to the trait, not the using model"
    );
}

#[test]
fn inherited_column_resolves_through_parent_model() {
    // A child model extending a base model inherits the base's columns.
    let (_d, view) = model_with_extra(
        r#"<?php
namespace App\Models;
class User extends BaseModel {
    protected $fillable = ['email'];
}
"#,
        "app/Models/BaseModel.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class BaseModel extends Model {
    protected $fillable = ['uuid'];
}
"#,
    );
    // Own column.
    let own = classify_member(&view, "email", AccessForm::Property).expect("own column");
    assert_eq!(own.kind, MagicMemberKind::Column);
    // Inherited column from the parent.
    let inherited = classify_member(&view, "uuid", AccessForm::Property).expect("inherited column");
    assert_eq!(inherited.kind, MagicMemberKind::Column);
}

// ─── Orchestration: resolve_and_classify (M3) ────────────────────────────

use crate::class_hierarchy_index::{classes_in_file, ClassHierarchyIndex};
use crate::parser::parse_php;
use crate::query_chain::use_aliases::extract_use_aliases;

/// A temp project: a model file indexed in a `ClassHierarchyIndex`, plus the
/// project root for `analyze`. Caller source is parsed per-test.
struct Project {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
}

/// Build a project with a model at `model_rel` and index it.
fn project(model_rel: &str, model_php: &str) -> Project {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let model_path = root.join(model_rel);
    fs::create_dir_all(model_path.parent().unwrap()).unwrap();
    fs::write(&model_path, model_php).unwrap();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    index.insert_file(&model_path, classes_in_file(&model_path, model_php));
    Project {
        _dir: dir,
        index,
        root,
    }
}

/// Find the receiver (object) node of the first `$x->{member}` access.
fn receiver_of<'t>(
    tree: &'t tree_sitter::Tree,
    bytes: &[u8],
    member: &str,
) -> Option<tree_sitter::Node<'t>> {
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "member_access_expression" | "nullsafe_member_access_expression"
        ) {
            if let Some(name) = n.child_by_field_name("name") {
                if name.utf8_text(bytes).ok() == Some(member) {
                    return n.child_by_field_name("object");
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

const USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $fillable = ['email'];
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#;

/// Resolve `$x->{member}` in `caller` against project `p`.
fn resolve_in(p: &Project, caller: &str, member: &str) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    let receiver = receiver_of(&tree, bytes, member)?;
    let cache = ClassViewCache::new();
    resolve_and_classify(
        receiver,
        member,
        AccessForm::Property,
        bytes,
        &aliases,
        &p.index,
        &cache,
        &p.root,
        None,
    )
}

// ─── Container-resolution receivers: app('key')->member ───────────────────
//
// `app('currentTenant')->logo` types its receiver through the binding registry
// (key → concrete FQCN) instead of variable flow. `WithBindings` is the test
// analogue of the salsa/main `ContainerAwareResolver`: a class index that also
// answers `binding_concrete`.

use std::collections::HashMap;

const TENANT_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Tenant extends Model {
    protected $fillable = ['name'];
    public function getLogoAttribute(): string { return ''; }
}
"#;

/// A resolver that pairs a class index with a container-binding map, so a test
/// can exercise `app('key')` receivers without standing up the salsa actor.
struct WithBindings<'a> {
    index: &'a ClassHierarchyIndex,
    bindings: HashMap<String, String>,
}

impl ClassFileResolver for WithBindings<'_> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.index.class_file(fqcn)
    }
    fn binding_concrete(&self, key: &str) -> Option<String> {
        self.bindings.get(key).cloned()
    }
}

/// Resolve `$x->{member}` in `caller` against an arbitrary resolver.
fn resolve_with(
    resolver: &impl ClassFileResolver,
    root: &std::path::Path,
    caller: &str,
    member: &str,
) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    let receiver = receiver_of(&tree, bytes, member)?;
    let cache = ClassViewCache::new();
    resolve_and_classify(
        receiver,
        member,
        AccessForm::Property,
        bytes,
        &aliases,
        resolver,
        &cache,
        root,
        None,
    )
}

/// Build a `WithBindings` resolver mapping `currentTenant` → the given concrete.
fn tenant_bound_to<'a>(p: &'a Project, concrete: &str) -> WithBindings<'a> {
    WithBindings {
        index: &p.index,
        bindings: HashMap::from([("currentTenant".to_string(), concrete.to_string())]),
    }
}

#[test]
fn resolves_app_string_binding_to_accessor() {
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = tenant_bound_to(&p, "App\\Models\\Tenant");
    let caller = "<?php $x = app('currentTenant')->logo;";
    let r = resolve_with(&resolver, &p.root, caller, "logo").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Accessor);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Tenant");
}

#[test]
fn resolves_resolve_helper_string_binding() {
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = tenant_bound_to(&p, "App\\Models\\Tenant");
    let caller = r#"<?php $x = resolve("currentTenant")->logo;"#;
    let r = resolve_with(&resolver, &p.root, caller, "logo").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Accessor);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Tenant");
}

#[test]
fn resolves_app_string_binding_to_column() {
    // The bridge feeds the whole classifier, not just accessors: a fillable
    // column on the bound model resolves too.
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = tenant_bound_to(&p, "App\\Models\\Tenant");
    let caller = "<?php $x = app('currentTenant')->name;";
    let r = resolve_with(&resolver, &p.root, caller, "name").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Tenant");
}

#[test]
fn unbound_container_key_does_not_resolve() {
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = WithBindings {
        index: &p.index,
        bindings: HashMap::new(),
    };
    let caller = "<?php $x = app('currentTenant')->logo;";
    assert!(resolve_with(&resolver, &p.root, caller, "logo").is_none());
}

#[test]
fn closure_bound_key_does_not_resolve() {
    // A binding registered with a closure has no concrete class the index knows;
    // the receiver stays unresolved rather than resolving to a phantom "Closure".
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = tenant_bound_to(&p, "Closure");
    let caller = "<?php $x = app('currentTenant')->logo;";
    assert!(resolve_with(&resolver, &p.root, caller, "logo").is_none());
}

#[test]
fn class_const_argument_is_not_a_string_key() {
    // `app(Tenant::class)` is a separate (later) receiver shape — the string-key
    // path must not misfire on it.
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let resolver = tenant_bound_to(&p, "App\\Models\\Tenant");
    let caller = "<?php $x = app(Tenant::class)->logo;";
    assert!(resolve_with(&resolver, &p.root, caller, "logo").is_none());
}

// ─── SnapshotResolver: the build-pass resolver over owned snapshots ────────

#[test]
fn snapshot_resolver_answers_class_file_and_binding() {
    let class_files = Arc::new(HashMap::from([(
        "App\\Models\\Tenant".to_string(),
        PathBuf::from("/x/Tenant.php"),
    )]));
    let bindings = Arc::new(HashMap::from([(
        "currentTenant".to_string(),
        "App\\Models\\Tenant".to_string(),
    )]));
    let r = SnapshotResolver {
        class_files,
        bindings,
        facade_aliases: Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Default::default(),
        implementers: Default::default(),
    };
    assert_eq!(
        r.class_file("App\\Models\\Tenant"),
        Some(PathBuf::from("/x/Tenant.php"))
    );
    assert_eq!(
        r.binding_concrete("currentTenant"),
        Some("App\\Models\\Tenant".to_string())
    );
    assert_eq!(r.binding_concrete("missing"), None);
}

#[test]
fn snapshot_resolver_resolves_app_binding_through_engine() {
    // The production resolver type, driven through the real resolution path
    // (not just the `WithBindings` stub) — proves the build pass will resolve
    // `app('currentTenant')->logo` to the bound model's accessor.
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let class_files = Arc::new(HashMap::from([(
        "App\\Models\\Tenant".to_string(),
        p.index.class_file("App\\Models\\Tenant").expect("indexed"),
    )]));
    let bindings = Arc::new(HashMap::from([(
        "currentTenant".to_string(),
        "App\\Models\\Tenant".to_string(),
    )]));
    let resolver = SnapshotResolver {
        class_files,
        bindings,
        facade_aliases: Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Default::default(),
        implementers: Default::default(),
    };
    let caller = "<?php $x = app('currentTenant')->logo;";
    let r = resolve_with(&resolver, &p.root, caller, "logo").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Accessor);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Tenant");
}

// ─── Facade receivers: Auth::check() → AuthManager (all three forms) ───────
//
// A facade is a thin static proxy whose own class carries only `@method`
// docblocks. The interception in the `name`/`qualified_name` arm walks the
// facade to its real implementation — facade FQCN → accessor key → bound
// concrete — and returns that concrete as the receiver type so the member
// classifies against the real class. These tests prove the whole chain
// end-to-end (token → FQCN → accessor → priority-0 vendor binding → concrete →
// member classification), not merely the seam, for each facade-call form.

/// Laravel's `AuthManager` as it lives in vendor — the concrete the framework's
/// `auth` container binding resolves to, with a `check()` the facade forwards.
const AUTH_MANAGER: &str = r#"<?php
namespace Illuminate\Auth;
class AuthManager {
    public function check() { return true; }
    public function user() { return null; }
}
"#;

/// Project with the vendor `AuthManager` indexed, plus a `SnapshotResolver`
/// binding the framework's `auth` key to it (the priority-0 vendor binding the
/// `AuthServiceProvider` registers at runtime). `facade_aliases` is the built-in
/// seed — `Auth` is a default facade, no user config needed.
fn auth_facade_project() -> (Project, SnapshotResolver) {
    let p = project(
        "vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php",
        AUTH_MANAGER,
    );
    let class_files = Arc::new(HashMap::from([(
        "Illuminate\\Auth\\AuthManager".to_string(),
        p.index
            .class_file("Illuminate\\Auth\\AuthManager")
            .expect("AuthManager indexed"),
    )]));
    let bindings = Arc::new(HashMap::from([(
        "auth".to_string(),
        "Illuminate\\Auth\\AuthManager".to_string(),
    )]));
    let resolver = SnapshotResolver {
        class_files,
        bindings,
        facade_aliases: Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Default::default(),
        implementers: Default::default(),
    };
    (p, resolver)
}

/// Resolve a static call `<scope>::{member}()` in `caller` against `resolver`,
/// classifying as a `StaticCall`. Mirrors [`resolve_with`] but targets the
/// `scope` of a `scoped_call_expression` (the facade token) rather than a
/// `->`-access object.
fn resolve_static_call(
    resolver: &impl ClassFileResolver,
    root: &std::path::Path,
    caller: &str,
    member: &str,
) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    // Find the `scoped_call_expression` whose name is `member`, return its scope.
    let mut stack = vec![tree.root_node()];
    let mut scope = None;
    while let Some(n) = stack.pop() {
        if n.kind() == "scoped_call_expression"
            && n.child_by_field_name("name")
                .and_then(|nm| nm.utf8_text(bytes).ok())
                == Some(member)
        {
            scope = n.child_by_field_name("scope");
            break;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    let cache = ClassViewCache::new();
    resolve_and_classify(
        scope?,
        member,
        AccessForm::StaticCall,
        bytes,
        &aliases,
        resolver,
        &cache,
        root,
        None,
    )
}

#[test]
fn facade_imported_resolves_to_concrete_through_binding() {
    // Imported / inline form: `use …\Facades\Auth; Auth::check();`. The receiver
    // resolves into the Facades namespace via the `use`, so it's a facade — we
    // walk to `AuthManager` (the `auth` binding's concrete) and classify
    // `check()` against IT, not the empty proxy.
    let (p, resolver) = auth_facade_project();
    let caller = r#"<?php
use Illuminate\Support\Facades\Auth;
Auth::check();
"#;
    let r = resolve_static_call(&resolver, &p.root, caller, "check").expect("resolves");
    // `check()` IS declared on `AuthManager` here, so it's a facade method
    // pointing at that declaration (not a plain method the consumers drop).
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
}

#[test]
fn facade_global_alias_resolves_to_concrete() {
    // Root-namespace alias form: `\Auth::check()`. No `use` import — PHP's
    // global `Facade::defaultAliases()` registration makes the leading-`\` token
    // resolve, and our seed alias maps it to the facade FQCN, then on to the
    // concrete. Intelephense resolves this nowhere; we own it.
    let (p, resolver) = auth_facade_project();
    let caller = "<?php \\Auth::check();";
    let r = resolve_static_call(&resolver, &p.root, caller, "check").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
}

#[test]
fn facade_inline_fully_qualified_resolves_to_concrete() {
    // Inline fully-qualified form: `\Illuminate\Support\Facades\Auth::check()`
    // with no `use`. The written-out path lands directly in the Facades
    // namespace, so it's recognized as a facade and walked to the concrete.
    let (p, resolver) = auth_facade_project();
    let caller = "<?php \\Illuminate\\Support\\Facades\\Auth::check();";
    let r = resolve_static_call(&resolver, &p.root, caller, "check").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
}

#[test]
fn facade_bare_token_in_namespaced_file_without_import_does_not_resolve() {
    // Bare `Auth::check()` inside a namespaced file with NO facade import: PHP's
    // class-name rule resolves `Auth` against the CURRENT namespace
    // (`App\Http\Auth`), not the global facade alias. We must not emit a wrong
    // goto — resolution drops to None (the would-be `App\Http\Auth` isn't a
    // facade and isn't indexed).
    let (p, resolver) = auth_facade_project();
    let caller = r#"<?php
namespace App\Http;
Auth::check();
"#;
    assert!(resolve_static_call(&resolver, &p.root, caller, "check").is_none());
}

#[test]
fn facade_method_not_declared_on_concrete_degrades_to_class() {
    // `Auth::guard()` — `guard` is NOT declared on this `AuthManager` stub (it's
    // forwarded via `__call`/`@method` in real Laravel). We must NOT drop to None
    // and we must NOT chase the forwarding chain: DEGRADE to the concrete CLASS
    // (`AuthManager`) as the goto target. Still a useful jump — and exactly the
    // case Intelephense can't resolve.
    let (p, resolver) = auth_facade_project();
    let caller = r#"<?php
use Illuminate\Support\Facades\Auth;
Auth::guard();
"#;
    let r = resolve_static_call(&resolver, &p.root, caller, "guard").expect("degrades, not None");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    // The declaring FQCN is the concrete itself — the consumer falls back to its
    // class line since there's no `guard` method token to narrow to.
    assert_eq!(r.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
    assert_eq!(r.confidence, Confidence::High);
}

// ─── Helper-chain receivers: view()->make(), cache()->get() (#253) ────────
//
// A zero-arg Laravel helper (`view()`, `cache()`, `session()`, …) returns a
// container service. `resolve_helper_receiver` maps the helper name to the
// SAME container binding key its facade proxies, resolves it to the concrete
// FQCN through the binding registry, and (mirroring the facade path) tags the
// chained method `FacadeMethod` so goto/hover chase the real declaration. These
// tests mirror the `app('key')` / facade tests "one indirection over".

/// Vendor `Illuminate\View\Factory`, the concrete the `view` binding resolves
/// to. `make()` returns the `View` *contract* (the real Laravel shape), so the
/// second hop must fall through the implementors scan to the concrete `View`.
const VIEW_FACTORY: &str = r#"<?php
namespace Illuminate\View;
use Illuminate\Contracts\View\View as ViewContract;
class Factory {
    public function make($view): ViewContract { }
    public function exists($view): bool { return true; }
}
"#;

/// The `View` contract `Factory::make()` declares as its return type.
const VIEW_CONTRACT: &str = r#"<?php
namespace Illuminate\Contracts\View;
interface View {
    public function render(): string;
}
"#;

/// The concrete `Illuminate\View\View` implementing the contract — the
/// implementors scan's target for `make()->render()`.
const VIEW_CONCRETE: &str = r#"<?php
namespace Illuminate\View;
use Illuminate\Contracts\View\View as ViewContract;
class View implements ViewContract {
    public function render(): string { return ''; }
}
"#;

/// A multi-file project: writes each `(rel_path, src)` and indexes it.
struct MultiProject {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
}

fn multi_project(files: &[(&str, &str)]) -> MultiProject {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    for (rel, src) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, src).unwrap();
        index.insert_file(&path, classes_in_file(&path, src));
    }
    MultiProject {
        _dir: dir,
        index,
        root,
    }
}

/// A resolver pairing a multi-file index with a binding map — the helper-chain
/// analogue of `WithBindings`, also wiring `implementers_of` through to the
/// index so contract→concrete resolution works.
struct WithBindingsMulti<'a> {
    index: &'a ClassHierarchyIndex,
    bindings: HashMap<String, String>,
}

impl ClassFileResolver for WithBindingsMulti<'_> {
    fn class_file(&self, fqcn: &str) -> Option<PathBuf> {
        self.index.class_file(fqcn)
    }
    fn binding_concrete(&self, key: &str) -> Option<String> {
        self.bindings.get(key).cloned()
    }
    fn implementers_of(&self, interface_fqcn: &str) -> Vec<String> {
        ClassHierarchyIndex::implementers_of(self.index, interface_fqcn).to_vec()
    }
}

/// Find the receiver (object) node of the first call-form `…->{member}(...)`.
fn call_receiver_of<'t>(
    tree: &'t tree_sitter::Tree,
    bytes: &[u8],
    member: &str,
) -> Option<tree_sitter::Node<'t>> {
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "member_call_expression" | "nullsafe_member_call_expression"
        ) {
            if let Some(name) = n.child_by_field_name("name") {
                if name.utf8_text(bytes).ok() == Some(member) {
                    return n.child_by_field_name("object");
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

/// Resolve a call-form `…->{member}()` in `caller` against `resolver`.
fn resolve_call_member(
    resolver: &impl ClassFileResolver,
    root: &std::path::Path,
    caller: &str,
    member: &str,
) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    let receiver = call_receiver_of(&tree, bytes, member)?;
    let cache = ClassViewCache::new();
    resolve_and_classify(
        receiver,
        member,
        AccessForm::InstanceCall,
        bytes,
        &aliases,
        resolver,
        &cache,
        root,
        None,
    )
}

#[test]
fn helper_view_make_resolves_to_concrete_factory() {
    // `view()->make(...)` — `make` is declared on the concrete `Factory` the
    // `view` binding resolves to. The helper resolves the receiver to that
    // concrete, and `make` classifies as a `FacadeMethod` pointing at it.
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/View/Factory.php",
        VIEW_FACTORY,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([("view".to_string(), "Illuminate\\View\\Factory".to_string())]),
    };
    let caller = "<?php view()->make('welcome');";
    let r = resolve_call_member(&resolver, &p.root, caller, "make").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\View\\Factory");
    assert_eq!(r.confidence, Confidence::High);
}

#[test]
fn helper_cache_get_resolves_to_concrete() {
    // `cache()->get(...)` resolves through the `cache` binding to its concrete.
    const CACHE_REPO: &str = r#"<?php
namespace Illuminate\Cache;
class Repository {
    public function get($key) { return null; }
}
"#;
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/Cache/Repository.php",
        CACHE_REPO,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([(
            "cache".to_string(),
            "Illuminate\\Cache\\Repository".to_string(),
        )]),
    };
    let caller = "<?php cache()->get('k');";
    let r = resolve_call_member(&resolver, &p.root, caller, "get").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Cache\\Repository");
}

#[test]
fn helper_session_put_resolves_to_concrete() {
    const SESSION_STORE: &str = r#"<?php
namespace Illuminate\Session;
class Store {
    public function put($key, $value) { }
}
"#;
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/Session/Store.php",
        SESSION_STORE,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([(
            "session".to_string(),
            "Illuminate\\Session\\Store".to_string(),
        )]),
    };
    let caller = "<?php session()->put('k', 1);";
    let r = resolve_call_member(&resolver, &p.root, caller, "put").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Session\\Store");
}

#[test]
fn unmapped_helper_does_not_resolve() {
    // A function that isn't a modeled helper falls through cleanly — no false
    // target, no panic.
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/View/Factory.php",
        VIEW_FACTORY,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([("view".to_string(), "Illuminate\\View\\Factory".to_string())]),
    };
    let caller = "<?php totallyNotAHelper()->make('x');";
    assert!(resolve_call_member(&resolver, &p.root, caller, "make").is_none());
}

#[test]
fn mapped_helper_with_no_concrete_returns_none() {
    // `view` is a modeled helper, but with no binding registered its key has no
    // concrete — resolution drops to None rather than a phantom target.
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/View/Factory.php",
        VIEW_FACTORY,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::new(),
    };
    let caller = "<?php view()->make('x');";
    assert!(resolve_call_member(&resolver, &p.root, caller, "make").is_none());
}

#[test]
fn helper_auth_check_resolves_through_helper_map() {
    // `auth()->check()` (a non-`user()` chain) resolves through the `auth`
    // binding, NOT the special-cased user-model exit `auth()->user()` takes.
    const AUTH_MGR: &str = r#"<?php
namespace Illuminate\Auth;
class AuthManager {
    public function check() { return true; }
}
"#;
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/Auth/AuthManager.php",
        AUTH_MGR,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([(
            "auth".to_string(),
            "Illuminate\\Auth\\AuthManager".to_string(),
        )]),
    };
    let caller = "<?php auth()->check();";
    let r = resolve_call_member(&resolver, &p.root, caller, "check").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FacadeMethod);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Auth\\AuthManager");
}

// ─── Part B: deep chains + contract→concrete (resolve_method_return) ───────

#[test]
fn method_return_surfaces_concrete_for_second_hop() {
    // `view()->make()->render()` — TWO hops. `make()` returns the `View`
    // contract; the implementors scan lands the receiver of `render` on the
    // concrete `Illuminate\View\View`, where `render` is declared.
    let p = multi_project(&[
        (
            "vendor/laravel/framework/src/Illuminate/View/Factory.php",
            VIEW_FACTORY,
        ),
        (
            "vendor/laravel/framework/src/Illuminate/Contracts/View/View.php",
            VIEW_CONTRACT,
        ),
        (
            "vendor/laravel/framework/src/Illuminate/View/View.php",
            VIEW_CONCRETE,
        ),
    ]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([("view".to_string(), "Illuminate\\View\\Factory".to_string())]),
    };
    let caller = "<?php view()->make('welcome')->render();";
    let r = resolve_call_member(&resolver, &p.root, caller, "render").expect("resolves");
    // `render` IS declared on the concrete `View`, so it classifies as a plain
    // member pointing at THAT declaration — the AC's "lands on concrete
    // `Illuminate\View\View::render()`, not the contract" is the declaring_fqcn.
    assert_eq!(r.declaring_fqcn, "Illuminate\\View\\View");
    assert_eq!(r.kind, MagicMemberKind::PlainMember);
}

#[test]
fn interface_return_with_single_implementer_resolves_to_concrete() {
    // A method declaring a concrete (non-contract) return type surfaces it
    // directly: `Factory::exists()` returns `bool` (no class) → None, but a
    // method returning an indexed concrete resolves. Here we prove the
    // implementors-scan branch: a contract with exactly one implementer.
    let p = multi_project(&[
        (
            "vendor/laravel/framework/src/Illuminate/Contracts/View/View.php",
            VIEW_CONTRACT,
        ),
        (
            "vendor/laravel/framework/src/Illuminate/View/View.php",
            VIEW_CONCRETE,
        ),
    ]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::new(),
    };
    assert_eq!(
        resolver.implementers_of("Illuminate\\Contracts\\View\\View"),
        vec!["Illuminate\\View\\View".to_string()]
    );
}

#[test]
fn deep_chain_unresolvable_return_yields_none() {
    // `view()->exists()->whatever()` — `exists` returns `bool`, not a class, so
    // the second-hop receiver can't be typed: None, no crash, no false target.
    let p = multi_project(&[(
        "vendor/laravel/framework/src/Illuminate/View/Factory.php",
        VIEW_FACTORY,
    )]);
    let resolver = WithBindingsMulti {
        index: &p.index,
        bindings: HashMap::from([("view".to_string(), "Illuminate\\View\\Factory".to_string())]),
    };
    let caller = "<?php view()->exists('x')->whatever();";
    assert!(resolve_call_member(&resolver, &p.root, caller, "whatever").is_none());
}

// ─── Macro receivers: Str::macro('foo', …) classification (commit 1) ──────
//
// A runtime-registered macro on a Macroable host (the dominant host being the
// vendor `Illuminate\Support\Str`, which the project class index does NOT carry)
// classifies a static call as `MagicMemberKind::Macro` via the macro registry,
// keyed on the resolved receiver FQCN. `macros_resolver` is the test analogue of
// the `ContainerAwareResolver`/`SnapshotResolver` macro path: a `SnapshotResolver`
// whose `macros` map holds one `(host, name) → (decl_file, line)` entry, with no
// indexed class file for the host (proving the host need not be indexed).

/// A `SnapshotResolver` with a single macro `(host, name)` registered at a
/// definition site — and crucially no class file for `host`, so it exercises the
/// "Macroable host the index doesn't carry" path.
fn macros_resolver(host: &str, name: &str, decl: (PathBuf, u32)) -> SnapshotResolver {
    SnapshotResolver {
        class_files: Default::default(),
        bindings: Default::default(),
        facade_aliases: Arc::new(crate::facade_resolver::default_facade_aliases()),
        macros: Arc::new(HashMap::from([(
            (host.to_string(), name.to_string()),
            decl,
        )])),
        implementers: Default::default(),
    }
}

#[test]
fn registered_macro_on_imported_host_classifies_as_macro() {
    // `use Illuminate\Support\Str; Str::uuid7();` with `uuid7` registered as a
    // macro on `Illuminate\Support\Str`. The receiver resolves to the host FQCN
    // through the `use` import even though the host isn't indexed (the macro
    // registry vouches for it), and the member classifies as a Macro.
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    let resolver = macros_resolver("Illuminate\\Support\\Str", "uuid7", (provider.clone(), 17));
    let caller = r#"<?php
use Illuminate\Support\Str;
Str::uuid7();
"#;
    let r = resolve_static_call(&resolver, dir.path(), caller, "uuid7").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Macro);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Support\\Str");
    // The registry carries the true definition site (the closure) for goto/hover.
    assert_eq!(
        resolver.macro_target("Illuminate\\Support\\Str", "uuid7"),
        Some((provider, 17))
    );
}

#[test]
fn unregistered_member_on_macro_host_does_not_resolve() {
    // The host has *a* macro (`uuid7`), so it's a known Macroable host the static
    // arm will yield — but a different, unregistered member (`notAMacro`) matches
    // no real surface and no registry entry, so classification drops to None
    // rather than guessing.
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    let resolver = macros_resolver("Illuminate\\Support\\Str", "uuid7", (provider, 17));
    let caller = r#"<?php
use Illuminate\Support\Str;
Str::notAMacro();
"#;
    assert!(resolve_static_call(&resolver, dir.path(), caller, "notAMacro").is_none());
}

#[test]
fn macro_lookup_keys_on_resolved_fqcn_not_token() {
    // A bare `\Str::uuid7()` (root-qualified, no import) must resolve to the same
    // `Illuminate\Support\Str` key the import form uses — proving registry keys
    // and lookup keys agree on the resolved FQCN, not the raw token. Here `\Str`
    // has no import and isn't in the Facades namespace, so it resolves to the
    // global `Str` — which is NOT the registered host, so it must NOT resolve.
    // (The faithful agreement case is the imported form above; this guards the
    // negative: a token that resolves elsewhere doesn't collide.)
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    let resolver = macros_resolver("Illuminate\\Support\\Str", "uuid7", (provider, 17));
    let caller = "<?php \\Str::uuid7();";
    assert!(resolve_static_call(&resolver, dir.path(), caller, "uuid7").is_none());
}

#[test]
fn mixin_method_classifies_as_macro_and_targets_mixin_file() {
    // A mixin-expanded member behaves identically to a scalar macro at the
    // classification surface — `(host, method)` is a registry entry whose
    // definition site is the mixin method's own file/line. `Str::shout()` with
    // `shout` registered (host = Str, target = the mixin file) classifies as a
    // Macro and goto lands on the mixin method.
    let dir = TempDir::new().unwrap();
    let mixin = dir.path().join("app/Mixins/StrMixin.php");
    let resolver = macros_resolver("Illuminate\\Support\\Str", "shout", (mixin.clone(), 4));
    let caller = r#"<?php
use Illuminate\Support\Str;
Str::shout();
"#;
    let r = resolve_static_call(&resolver, dir.path(), caller, "shout").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Macro);
    assert_eq!(r.declaring_fqcn, "Illuminate\\Support\\Str");
    assert_eq!(
        resolver.macro_target("Illuminate\\Support\\Str", "shout"),
        Some((mixin, 4))
    );
}

#[test]
fn resolves_typed_param_property_to_column_high() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn resolves_typed_param_relationship() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->posts;
    }
}
"#;
    let r = resolve_in(&p, caller, "posts").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Relationship);
}

#[test]
fn resolves_this_to_enclosing_class() {
    // `$this->email` inside a User method resolves to the User model.
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function greeting() {
        return $this->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves $this");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn resolves_typed_property_in_anonymous_volt_class() {
    // `$this->user->email` inside a Volt SFC (anonymous `new class extends
    // Component`) must resolve `$this->user` via its typed property declaration,
    // even though the class itself has no FQCN.
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
use App\Models\User;
use Livewire\Volt\Component;
new class extends Component {
    public ?User $user = null;
    public function render() {
        return $this->user->email;
    }
};
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves $this->user in anon class");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn multi_hop_receiver_lowers_confidence() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$q` is seeded one hop from the typed `$user` → MEDIUM.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        $q = $user->newQuery();
        return $q->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("resolves multi-hop");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::Medium);
}

#[test]
fn unresolvable_receiver_yields_none() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$mystery` has no type info anywhere.
    let caller = r#"<?php
function show($mystery) {
    return $mystery->email;
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

#[test]
fn receiver_class_absent_from_index_yields_none() {
    // Empty index — even a perfectly typed receiver can't be classified.
    let p = Project {
        _dir: TempDir::new().unwrap(),
        index: ClassHierarchyIndex::default(),
        root: PathBuf::from("/nonexistent"),
    };
    let caller = r#"<?php
use App\Models\User;
function show(User $user) {
    return $user->email;
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

#[test]
fn classview_cache_reuses_built_view() {
    // Two resolutions against the same FQCN must reuse one ClassView build.
    let p = project("app/Models/User.php", USER_MODEL);
    let cache = ClassViewCache::new();
    let node = p.index.get("App\\Models\\User").expect("indexed");
    let v1 = cache.get_or_build("App\\Models\\User", &node.file_path, &p.root);
    let v2 = cache.get_or_build("App\\Models\\User", &node.file_path, &p.root);
    assert!(v1.is_some());
    assert!(std::sync::Arc::ptr_eq(
        v1.as_ref().unwrap(),
        v2.as_ref().unwrap()
    ));
}

// ─── Widening: typed properties ($this->prop) ────────────────────────────

/// Build a project from several PSR-4 files, indexing every one.
fn project_files(files: &[(&str, &str)]) -> Project {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let mut index = ClassHierarchyIndex::default();
    for (rel, src) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, src).unwrap();
        index.insert_file(&path, classes_in_file(&path, src));
    }
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    Project {
        _dir: dir,
        index,
        root,
    }
}

const PROFILE_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Profile extends Model {
    protected $fillable = ['bio'];
}
"#;

#[test]
fn widens_typed_property_this_prop() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private Profile $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    // Receiver of `bio` is `$this->profile`.
    let r = resolve_in(&p, user, "bio").expect("typed property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn widens_nullable_typed_property() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected ?Profile $profile = null;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let r = resolve_in(&p, user, "bio").expect("nullable typed property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn widens_promoted_constructor_property() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function __construct(private Profile $profile) {}
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let r = resolve_in(&p, user, "bio").expect("promoted property resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn untyped_property_does_not_resolve() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    assert!(
        resolve_in(&p, user, "bio").is_none(),
        "an untyped property gives the resolver nothing to go on"
    );
}

#[test]
fn union_typed_property_is_ambiguous() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    private Profile|Account $profile;
    public function bio() {
        return $this->profile->bio;
    }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    assert!(
        resolve_in(&p, user, "bio").is_none(),
        "a union-typed property is ambiguous and must not resolve"
    );
}

// ─── Widening: foreach iterator vars ─────────────────────────────────────

#[test]
fn widens_foreach_over_collection_variable() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        $users = User::all();
        foreach ($users as $user) {
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("foreach element resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(
        r.confidence,
        Confidence::Medium,
        "an inferred foreach element type is MEDIUM"
    );
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn widens_foreach_with_key_value_pair() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        $users = User::all();
        foreach ($users as $i => $user) {
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("foreach pair element resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::Medium);
}

#[test]
fn foreach_over_unresolvable_collection_yields_none() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
function index($users) {
    foreach ($users as $user) {
        echo $user->email;
    }
}
"#;
    assert!(
        resolve_in(&p, caller, "email").is_none(),
        "an untyped collection gives the foreach widening nothing"
    );
}

#[test]
fn foreach_docblock_var_is_high_via_flow() {
    // A `@var` on the loop body is found by flow directly (HIGH), before the
    // foreach fallback runs.
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index($rows) {
        foreach ($rows as $user) {
            /** @var User $user */
            echo $user->email;
        }
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("docblock resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.confidence, Confidence::High);
}

// ─── Widening: method return-type chains ($obj->m()->...) ────────────────

#[test]
fn widens_static_return_type_chain() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function activated(): static { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->activated()->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("static return chain resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(
        r.confidence,
        Confidence::Medium,
        "return-type inference is indirect"
    );
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn widens_self_return_type_chain() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function refreshed(): self { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->refreshed()->email;
    }
}
"#;
    let r = resolve_in(&p, caller, "email").expect("self return chain resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\User");
}

#[test]
fn explicit_class_return_type_surfaces_concrete() {
    // #253 Part B: a concrete class return type IS surfaced now. The return
    // type is written in the DECLARING file's namespace (`App\Models`), so
    // `makeProfile(): Profile` re-qualifies to `App\Models\Profile`, and the
    // second-hop `->bio` classifies as a column on it.
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function makeProfile(): Profile { return new Profile(); }
}
"#;
    let p = project_files(&[
        ("app/Models/User.php", user),
        ("app/Models/Profile.php", PROFILE_MODEL),
    ]);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->makeProfile()->bio;
    }
}
"#;
    let r = resolve_in(&p, caller, "bio").expect("concrete return type resolves");
    assert_eq!(r.kind, MagicMemberKind::Column);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Profile");
}

#[test]
fn untyped_return_does_not_resolve() {
    let user = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function thing() { return $this; }
}
"#;
    let p = project("app/Models/User.php", user);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->thing()->email;
    }
}
"#;
    assert!(resolve_in(&p, caller, "email").is_none());
}

// ─── Population helper: resolve_member_access_entries (M4) ────────────────

use crate::parser::language_php;
use crate::queries::extract_all_php_patterns;
use crate::salsa_impl::MemberAccessReferenceData;
use crate::symbol_index::MagicMemberEntry;
use std::sync::Arc;

/// Capture a caller's property-form member accesses as `MemberAccessReferenceData`
/// (mirrors what `handle_get_patterns` stores), so we can feed the real capture
/// shape into `resolve_member_access_entries`.
fn member_refs_of(source: &str) -> Vec<Arc<MemberAccessReferenceData>> {
    let tree = parse_php(source).expect("parse");
    let lang = language_php();
    extract_all_php_patterns(&tree, source, &lang)
        .expect("extract")
        .member_accesses
        .iter()
        .map(|m| {
            Arc::new(MemberAccessReferenceData {
                member: m.member.to_string(),
                receiver: m.receiver.to_string(),
                receiver_byte_start: m.receiver_byte_start,
                receiver_byte_end: m.receiver_byte_end,
                is_nullsafe: m.is_nullsafe,
                form: m.form,
                line: m.row as u32,
                column: m.column as u32,
                end_column: m.end_column as u32,
                declaring_fqcn: None,
                kind: None,
                confidence: Confidence::Unresolved,
            })
        })
        .collect()
}

fn has_entry(entries: &[MagicMemberEntry], fqcn: &str, member: &str) -> bool {
    entries.iter().any(|e| e.fqcn == fqcn && e.member == member)
}

#[test]
fn population_resolves_typed_param_member_accesses() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        $a = $user->email;
        $b = $user->posts;
        return [$a, $b];
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "email"),
        "{entries:?}"
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "posts"),
        "{entries:?}"
    );
    // Position is carried from the capture (member-name span), not the receiver.
    let email = entries.iter().find(|e| e.member == "email").unwrap();
    assert!(email.end_column > email.column);
}

#[test]
fn population_drops_unresolvable_receivers() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
function show($mystery) {
    return $mystery->email;
}
"#;
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        entries.is_empty(),
        "unresolvable receiver must not produce an index entry, got {entries:?}"
    );
}

#[test]
fn population_drops_unknown_members_on_resolved_receiver() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `$user` resolves to User, but `notAColumn` isn't a known member.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->notAColumn;
    }
}
"#;
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn population_empty_refs_is_empty() {
    let p = project("app/Models/User.php", USER_MODEL);
    let entries =
        resolve_member_access_entries("", &[], &p.index, &ClassViewCache::new(), &p.root, None);
    assert!(entries.is_empty());
}

#[test]
fn end_to_end_warming_flow_resolves_this_email() {
    // Mirror the real warming → magic-build flow exactly:
    // parse_owned_with_hierarchy → build index → fqcn_file_map snapshot →
    // resolve_member_access_entries against the snapshot. This is what the
    // warming pass does; the other tests use the index directly + re-extract.
    use crate::class_hierarchy_index::ClassHierarchyIndex;
    use crate::pattern_indexer::parse_owned_with_hierarchy;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function gravatar(): string { return md5($this->email); }
}
"#;
    fs::write(&path, src).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();

    let (data, nodes) = parse_owned_with_hierarchy(&path, src);
    assert!(
        !data.member_access_refs.is_empty(),
        "warming parse must capture $this->email"
    );

    let mut index = ClassHierarchyIndex::default();
    index.insert_file(&path, nodes);
    let snapshot = index.fqcn_file_map();
    assert!(
        snapshot.contains_key("App\\Models\\User"),
        "snapshot must map the model fqcn; keys: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );

    let entries = resolve_member_access_entries(
        src,
        &data.member_access_refs,
        &snapshot,
        &ClassViewCache::new(),
        dir.path(),
        None,
    );
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "end-to-end warming flow should resolve $this->email; got {entries:?}"
    );
}

#[test]
fn realistic_user_resolves_through_full_warm_restart_cycle() {
    // Mirror the production warm-restart path end to end: parse → save
    // (patterns + hierarchy nodes) → load → rebuild hierarchy from the
    // restored nodes → fqcn_file_map snapshot → resolve the restored
    // patterns' member accesses. Uses a realistic User (extends an aliased
    // vendor base, uses a trait) to match the real model shape.
    use crate::class_hierarchy_index::ClassHierarchyIndex;
    use crate::pattern_indexer::parse_owned_with_hierarchy;
    use dashmap::DashMap;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let src = r#"<?php
namespace App\Models;
use Illuminate\Foundation\Auth\User as Authenticatable;
use Illuminate\Notifications\Notifiable;
class User extends Authenticatable {
    use Notifiable;
    protected $fillable = ['name', 'email', 'password'];
    public function getGravatarAttribute(): string {
        return md5(strtolower($this->email));
    }
}
"#;
    fs::write(&path, src).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();

    let (data, nodes) = parse_owned_with_hierarchy(&path, src);

    // Save (patterns + hierarchy) then restore — the warm-restart cycle.
    let cache = Arc::new(DashMap::new());
    cache.insert(path.clone(), (0, data));
    let mut hierarchy_by_file = std::collections::HashMap::new();
    hierarchy_by_file.insert(path.clone(), nodes);
    crate::pattern_disk_cache::save_from(&cache, &hierarchy_by_file, dir.path()).unwrap();

    let restored_cache = Arc::new(DashMap::new());
    let lr = crate::pattern_disk_cache::load_into(&restored_cache, dir.path());

    let mut index = ClassHierarchyIndex::default();
    for (p, ns) in lr.hierarchy {
        index.insert_file(&p, ns);
    }
    let snapshot = index.fqcn_file_map();
    assert!(
        snapshot.contains_key("App\\Models\\User"),
        "restored snapshot must contain the app model; keys: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );

    let restored = restored_cache.get(&path).unwrap();
    let refs = restored.value().1.member_access_refs.clone();
    assert!(
        !refs.is_empty(),
        "restored patterns must carry member accesses"
    );

    let entries = resolve_member_access_entries(
        src,
        &refs,
        &snapshot,
        &ClassViewCache::new(),
        dir.path(),
        None,
    );
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "warm-restart cycle should resolve $this->email; got {entries:?}"
    );
}

// ─── Auth-aware receiver resolution ──────────────────────────────────────

#[test]
fn parse_auth_model_resolves_via_use_alias() {
    let content = r#"<?php
use App\Models\User;
return ['providers' => ['users' => ['driver' => 'eloquent', 'model' => User::class]]];
"#;
    assert_eq!(
        parse_auth_model(content).as_deref(),
        Some("App\\Models\\User")
    );
}

#[test]
fn parse_auth_model_handles_env_default() {
    let content = r#"<?php
use App\Models\User;
return ['providers' => ['users' => ['model' => env('AUTH_MODEL', User::class)]]];
"#;
    assert_eq!(
        parse_auth_model(content).as_deref(),
        Some("App\\Models\\User")
    );
}

#[test]
fn parse_auth_model_handles_fully_qualified() {
    let content = r#"<?php
return ['providers' => ['users' => ['model' => \App\Models\Account::class]]];
"#;
    assert_eq!(
        parse_auth_model(content).as_deref(),
        Some("App\\Models\\Account")
    );
}

#[test]
fn parse_auth_model_ignores_commented_provider() {
    let content = r#"<?php
use App\Models\User;
return ['providers' => [
    'users' => ['model' => User::class],
    // 'admins' => ['model' => Admin::class],
]];
"#;
    assert_eq!(
        parse_auth_model(content).as_deref(),
        Some("App\\Models\\User")
    );
}

#[test]
fn parse_auth_model_none_when_absent() {
    assert!(parse_auth_model("<?php return ['guards' => []];").is_none());
}

/// Temp project with config/auth.php + a User model, indexed. Returns the
/// dir + index for auth-receiver resolution tests.
fn auth_project(model_fqcn_class: &str) -> (TempDir, ClassHierarchyIndex) {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("config")).unwrap();
    fs::write(
        dir.path().join("config/auth.php"),
        "<?php\nuse App\\Models\\User;\nreturn ['providers' => ['users' => ['model' => User::class]]];\n",
    )
    .unwrap();
    let user_path = dir.path().join("app/Models/User.php");
    fs::create_dir_all(user_path.parent().unwrap()).unwrap();
    let src = format!(
        "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\nclass {} extends Model {{ protected $fillable = ['email']; }}\n",
        model_fqcn_class
    );
    fs::write(&user_path, &src).unwrap();
    fs::write(
        dir.path().join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();
    index.insert_file(&user_path, classes_in_file(&user_path, &src));
    (dir, index)
}

fn resolve_auth_caller(
    dir: &TempDir,
    index: &ClassHierarchyIndex,
    caller: &str,
) -> Vec<MagicMemberEntry> {
    let refs = member_refs_of(caller);
    resolve_member_access_entries(
        caller,
        &refs,
        index,
        &ClassViewCache::new(),
        dir.path(),
        None,
    )
}

#[test]
fn resolves_auth_helper_chain() {
    let (dir, index) = auth_project("User");
    let entries = resolve_auth_caller(&dir, &index, "<?php\n$x = auth()->user()->email;\n");
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "auth()->user()->email should resolve to the auth model; got {entries:?}"
    );
}

#[test]
fn resolves_auth_facade_chain() {
    let (dir, index) = auth_project("User");
    let caller = r#"<?php
use Illuminate\Support\Facades\Auth;
$x = Auth::user()->email;
"#;
    let entries = resolve_auth_caller(&dir, &index, caller);
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "Auth::user()->email should resolve; got {entries:?}"
    );
}

#[test]
fn resolves_request_user_helper_chain() {
    let (dir, index) = auth_project("User");
    let entries = resolve_auth_caller(&dir, &index, "<?php\n$x = request()->user()->email;\n");
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "request()->user()->email should resolve; got {entries:?}"
    );
}

#[test]
fn resolves_gate_define_closure_user() {
    // The HorizonServiceProvider::gate() shape: untyped $user in a Gate::define
    // closure is the authenticatable.
    let (dir, index) = auth_project("User");
    let caller = r#"<?php
use Illuminate\Support\Facades\Gate;
Gate::define('viewHorizon', function ($user) {
    return in_array($user->email, ['admin@example.com']);
});
"#;
    let entries = resolve_auth_caller(&dir, &index, caller);
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "Gate::define closure $user should resolve to the auth model; got {entries:?}"
    );
}

#[test]
fn resolves_gate_before_closure_user() {
    let (dir, index) = auth_project("User");
    let caller = r#"<?php
use Illuminate\Support\Facades\Gate;
Gate::before(function ($user, $ability) {
    return $user->email === 'root@example.com' ? true : null;
});
"#;
    let entries = resolve_auth_caller(&dir, &index, caller);
    assert!(
        entries
            .iter()
            .any(|e| e.member == "email" && e.fqcn == "App\\Models\\User"),
        "Gate::before closure $user should resolve; got {entries:?}"
    );
}

#[test]
fn non_gate_closure_user_does_not_resolve() {
    // An untyped $user in an ordinary closure (not a Gate ability) must NOT be
    // assumed to be the auth model — that would be a false positive.
    let (dir, index) = auth_project("User");
    let caller = r#"<?php
$users->map(function ($user) {
    return $user->email;
});
"#;
    let entries = resolve_auth_caller(&dir, &index, caller);
    assert!(
        entries.is_empty(),
        "an ordinary closure's untyped $user must not resolve to the auth model; got {entries:?}"
    );
}

// ─── Call-form magic members (#77): scopes, finders, relationship calls ───

const SCOPED_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $casts = ['email' => 'string'];
    public function posts(): HasMany { return $this->hasMany(Post::class); }
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#;

#[test]
fn dynamic_finder_column_maps_finder_names() {
    assert_eq!(
        dynamic_finder_column("whereEmail").as_deref(),
        Some("email")
    );
    assert_eq!(
        dynamic_finder_column("orWhereEmailAddress").as_deref(),
        Some("email_address")
    );
    // Not finder-shaped: bare `where`, lowercase remainder, unrelated word.
    assert!(dynamic_finder_column("where").is_none());
    assert!(dynamic_finder_column("whereabouts").is_none());
    assert!(dynamic_finder_column("posts").is_none());
}

#[test]
fn population_indexes_static_scope_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index() {
        return User::active()->get();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "static scope call should index under its usage name; got {entries:?}"
    );
}

#[test]
fn population_indexes_instance_scope_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index(User $user) {
        return $user->active()->get();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "instance scope call should index; got {entries:?}"
    );
}

#[test]
fn population_indexes_relationship_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index(User $user) {
        return $user->posts()->count();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "posts"),
        "relationship call should index under the same key as property reads; got {entries:?}"
    );
}

#[test]
fn population_indexes_dynamic_finder_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function find() {
        return User::whereEmail('a@b.test')->first();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "whereEmail"),
        "dynamic finder call should index; got {entries:?}"
    );
}

#[test]
fn population_prunes_plain_method_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function index(User $user) {
        return $user->posts()->count();
    }
    public function helper(User $user) { return $user->helperMethod(); }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    // `count()` (unresolvable/plain) and `helperMethod()` (not on the model)
    // must not index; only the relationship call survives.
    assert!(
        !has_entry(&entries, "App\\Models\\User", "count"),
        "{entries:?}"
    );
    assert!(
        !has_entry(&entries, "App\\Models\\User", "helperMethod"),
        "{entries:?}"
    );
    assert!(has_entry(&entries, "App\\Models\\User", "posts"));
}

#[test]
fn static_receiver_resolves_via_use_alias() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    // Aliased import: `use App\Models\User as Account;`.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User as Account;
class C {
    public function index() {
        return Account::active()->get();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "aliased static receiver should resolve through use-imports; got {entries:?}"
    );
}

#[test]
fn unknown_static_receiver_does_not_index() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    // `Str` is not in the class graph — the call must drop, not guess.
    let caller = r#"<?php
use Illuminate\Support\Str;
$slug = Str::slug('Laravel');
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn static_receiver_resolves_same_namespace_without_import() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    // A sibling model in the same namespace needs no `use` import — PHP
    // resolves the bare name to the current namespace. Regression for the
    // PR #76 review: the static-receiver arm must qualify bare names with
    // the file's namespace, not just expand use-aliases.
    let caller = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Order extends Model {
    public function activeUsers() { return User::active()->get(); }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "same-namespace unimported static receiver should resolve; got {entries:?}"
    );
}

// ─── Chain-aware call-form resolution (#77, review round 2) ────────────────
//
// Mid-chain scope calls are the canonical builder shapes — they MUST index,
// or scope rename rewrites the declaration + direct sites while silently
// leaving chained call sites behind (broken code).

#[test]
fn population_indexes_static_rooted_chain_scope_calls() {
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function a() { return User::query()->active()->get(); }
    public function b() { return User::where('email', 'x')->active()->get(); }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    let active_count = entries
        .iter()
        .filter(|e| e.fqcn == "App\\Models\\User" && e.member == "active")
        .count();
    assert_eq!(
        active_count, 2,
        "both static-rooted chain shapes must index `active`; got {entries:?}"
    );
    // The chain links themselves stay out of the index (plain methods).
    assert!(!has_entry(&entries, "App\\Models\\User", "where"));
    assert!(!has_entry(&entries, "App\\Models\\User", "get"));
}

#[test]
fn population_indexes_builder_param_scope_body_calls() {
    // `$query->active()` inside another scope's body — the receiver types as
    // the Eloquent Builder (no scopes declared there); resolution retries
    // against the enclosing model.
    let model = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
    public function scopeRecent(Builder $query): Builder {
        return $query->active()->where('created_at', '>', now());
    }
}
"#;
    let p = project("app/Models/User.php", model);
    let refs = member_refs_of(model);
    let entries = resolve_member_access_entries(
        model,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "builder-typed `$query->active()` must resolve via the enclosing model; got {entries:?}"
    );
}

#[test]
fn population_indexes_multi_link_builder_param_chains() {
    // `$query->where(…)->active()` — variable-rooted chain: the root `$query`
    // types as Builder, classification retries against the enclosing model.
    let model = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
    public function scopeFresh(Builder $query): Builder {
        return $query->where('x', 1)->active();
    }
}
"#;
    let p = project("app/Models/User.php", model);
    let refs = member_refs_of(model);
    let entries = resolve_member_access_entries(
        model,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        has_entry(&entries, "App\\Models\\User", "active"),
        "mid-chain `$query->where(…)->active()` must index; got {entries:?}"
    );
}

#[test]
fn population_static_rooted_chain_ignores_non_model_roots() {
    // `Str::of(…)->upper()` — the chain root resolves only if the class graph
    // knows it, and classification only matches magic surfaces. Neither holds
    // for a non-model helper: no entry.
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
use Illuminate\Support\Str;
$x = Str::of('laravel')->upper();
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn population_chain_plain_terminals_stay_pruned() {
    // `User::query()->first()` — the chain root resolves to the model, but
    // `first` matches no magic surface: pruned, not indexed.
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function f() { return User::query()->first(); }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        !has_entry(&entries, "App\\Models\\User", "first"),
        "plain chain terminals must not index; got {entries:?}"
    );
}

// ─── Round-2 review gates: factory states, relative scopes, closures ───────

#[test]
fn population_skips_factory_state_calls() {
    // `User::factory()->active()` — `active` here is a FACTORY STATE, not the
    // scope, and factory states routinely share scope names. The first-link
    // gate must refuse (`factory` is no chain starter, scope, or finder), or
    // a scope rename would rewrite factory-state calls (review round 2,
    // finding 1 — demonstrated failure before the gate).
    let p = project("app/Models/User.php", SCOPED_MODEL);
    let caller = r#"<?php
namespace Tests\Feature;
use App\Models\User;
class UserTest {
    public function test_active() {
        $user = User::factory()->active()->create();
    }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        !has_entry(&entries, "App\\Models\\User", "active"),
        "factory-state calls must not index as scope references; got {entries:?}"
    );
}

#[test]
fn population_indexes_relative_scope_calls() {
    // `self::active()` and `static::query()->active()` inside the model —
    // common in model statics; both must index or scope rename leaves them
    // behind (review round 2, finding 5).
    let model = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopeActive(Builder $query): Builder { return $query; }
    public static function activeCount(): int { return self::active()->count(); }
    public static function freshActive() { return static::query()->active()->get(); }
}
"#;
    let p = project("app/Models/User.php", model);
    let refs = member_refs_of(model);
    let entries = resolve_member_access_entries(
        model,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    let active_count = entries
        .iter()
        .filter(|e| e.fqcn == "App\\Models\\User" && e.member == "active")
        .count();
    assert_eq!(
        active_count, 2,
        "self:: and static::query() scope calls must both index; got {entries:?}"
    );
}

#[test]
fn population_skips_where_has_closure_builder_calls() {
    // A Builder-typed param of a `whereHas` CLOSURE is the related model's
    // builder — retrying it against the enclosing model would misattribute a
    // same-named scope (review round 2, finding 2). The builder retry is
    // gated to the enclosing scope method's own parameter.
    let model = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    public function scopePublished(Builder $query): Builder { return $query; }
    public function scopeRecent(Builder $query): Builder {
        return $query->whereHas('posts', function (Builder $q) {
            $q->published();
        });
    }
}
"#;
    let p = project("app/Models/User.php", model);
    let refs = member_refs_of(model);
    let entries = resolve_member_access_entries(
        model,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        !has_entry(&entries, "App\\Models\\User", "published"),
        "a whereHas-closure builder call must not attribute to the enclosing model; got {entries:?}"
    );
    // The legitimate `$query->whereHas(...)` receiver still flows: `whereHas`
    // itself is a plain builder method and stays pruned.
    assert!(!has_entry(&entries, "App\\Models\\User", "whereHas"));
}

#[test]
fn population_skips_relation_hop_chains() {
    // `$user->posts()->active()` — the chain's subject re-targets to Post at
    // the relationship link; attributing `active` to User would be wrong (and
    // rename-hazardous). Conservatively dropped (review round 2, finding 2).
    let model = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    public function posts(): HasMany { return $this->hasMany(Post::class); }
    public function scopeActive(Builder $query): Builder { return $query; }
}
"#;
    let p = project("app/Models/User.php", model);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function x(User $user) { return $user->posts()->active()->get(); }
}
"#;
    let refs = member_refs_of(caller);
    let entries = resolve_member_access_entries(
        caller,
        &refs,
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        None,
    );
    assert!(
        !has_entry(&entries, "App\\Models\\User", "active"),
        "a relation-hopped scope call must not attribute to the root model; got {entries:?}"
    );
    // The relationship CALL itself still indexes (it is User's member).
    assert!(has_entry(&entries, "App\\Models\\User", "posts"));
}

// ─── Dependency recording (incremental save, #80) ──────────────────────────

#[test]
fn deps_record_receiver_fqcn_on_successful_classification() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->email;
    }
}
"#;
    let mut deps = std::collections::HashSet::new();
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        Some(&mut deps),
    );
    assert!(!entries.is_empty());
    assert!(deps.contains("App\\Models\\User"), "{deps:?}");
}

#[test]
fn deps_record_receiver_fqcn_even_when_member_classification_fails() {
    let p = project("app/Models/User.php", USER_MODEL);
    // `notAColumn` doesn't exist on User → no index entry, but the file
    // still depends on User: adding the member later must re-resolve it.
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $user) {
        return $user->notAColumn;
    }
}
"#;
    let mut deps = std::collections::HashSet::new();
    let entries = resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        Some(&mut deps),
    );
    assert!(entries.is_empty(), "{entries:?}");
    assert!(
        deps.contains("App\\Models\\User"),
        "failed classification must still record the receiver dependency, got {deps:?}"
    );
}

#[test]
fn deps_record_nothing_for_unresolvable_receivers() {
    let p = project("app/Models/User.php", USER_MODEL);
    let caller = r#"<?php
function show($mystery) {
    return $mystery->email;
}
"#;
    let mut deps = std::collections::HashSet::new();
    resolve_member_access_entries(
        caller,
        &member_refs_of(caller),
        &p.index,
        &ClassViewCache::new(),
        &p.root,
        Some(&mut deps),
    );
    assert!(deps.is_empty(), "{deps:?}");
}

#[test]
fn deps_record_macro_decl_file_on_macro_classification() {
    // #255: a site that classifies as a Macro records the macro's declaration
    // file — the registering provider for an inline `::macro()` — so the
    // provider-path ripple key emitted by a provider-body save reaches it.
    // Both resolution paths (live re-parse and captured recipe) must record
    // identically; `resolve_both_ways` drives both chokepoints.
    let dir = TempDir::new().unwrap();
    let provider = dir.path().join("app/Providers/AppServiceProvider.php");
    let resolver = macros_resolver("Illuminate\\Support\\Str", "uuid7", (provider.clone(), 17));
    let caller = r#"<?php
use Illuminate\Support\Str;
class Ids {
    public function make(): string { return Str::uuid7(); }
}
"#;
    let ((tree_entries, tree_deps), (recipe_entries, recipe_deps)) =
        resolve_both_ways(&resolver, dir.path(), caller);
    let provider_dep = provider.to_string_lossy().into_owned();
    assert!(
        !tree_entries.is_empty() && !recipe_entries.is_empty(),
        "the macro call site must classify on both paths",
    );
    assert!(
        tree_deps.contains(&provider_dep),
        "live path must record the macro decl file (the provider); got {tree_deps:?}",
    );
    assert!(
        recipe_deps.contains(&provider_dep),
        "captured-recipe path must record the macro decl file (the provider); got {recipe_deps:?}",
    );
}

#[test]
fn deps_record_binding_attempt_key_even_when_unresolved() {
    // #255: a string-keyed container site records `binding:<key>` resolved or
    // not — a brand-new binding's call sites resolved to NOTHING before the
    // provider save, so this attempt key is the only dependency the
    // registration diff can reach them through. Both paths must record it.
    let dir = TempDir::new().unwrap();
    let resolver = macros_resolver(
        "Illuminate\\Support\\Str",
        "unused",
        (dir.path().join("p.php"), 1),
    );
    let caller = r#"<?php
function f() {
    return app('tenant')->name;
}
"#;
    let ((_, tree_deps), (_, recipe_deps)) = resolve_both_ways(&resolver, dir.path(), caller);
    assert!(
        tree_deps.contains(&"binding:tenant".to_string()),
        "live path must record the binding attempt key; got {tree_deps:?}",
    );
    assert!(
        recipe_deps.contains(&"binding:tenant".to_string()),
        "captured-recipe path must record the binding attempt key; got {recipe_deps:?}",
    );
}

// ─── End-to-end: a closure-bound key resolves app('key')->member ───────────
//
// Proves the new tree-sitter binding walker feeds the SAME `concrete_class`
// the class-concrete path does. A closure-derived concrete is a plain FQCN,
// byte-identical to a class-const one, and PHP, Blade, and Volt converge on the
// same `resolve_container_receiver` leaf that consumes it — so the PHP caller
// below is representative of all three surfaces.

#[test]
fn closure_singleton_resolves_app_member_end_to_end() {
    let p = project("app/Models/Tenant.php", TENANT_MODEL);
    let provider = r#"<?php
namespace App\Providers;
use Illuminate\Support\ServiceProvider;
use App\Models\Tenant;
class AppServiceProvider extends ServiceProvider {
    public function register(): void {
        $this->app->singleton('currentTenant', fn () => Tenant::where('domain', request()->host())->first());
    }
}
"#;
    // Parse the provider exactly as the actor does, and read back the concrete
    // the walker resolved for the closure binding.
    let db = crate::salsa_impl::LaravelDatabase::default();
    let file = crate::salsa_impl::ServiceProviderFile::new(
        &db,
        p.root.join("app/Providers/AppServiceProvider.php"),
        0,
        provider.to_string(),
        2,
    );
    let parsed = crate::salsa_impl::parse_service_provider_source(&db, file, p.root.clone());
    let concrete = parsed
        .bindings(&db)
        .iter()
        .find(|b| b.abstract_name(&db).name(&db) == "currentTenant")
        .map(|b| b.concrete_class(&db).clone())
        .expect("currentTenant closure binding parsed");
    assert_eq!(concrete, "App\\Models\\Tenant");

    // Feed that concrete through the resolution engine the live container-aware
    // resolver uses: `app('currentTenant')->logo` types to the bound model.
    let resolver = tenant_bound_to(&p, &concrete);
    let caller = "<?php $x = app('currentTenant')->logo;";
    let r = resolve_with(&resolver, &p.root, caller, "logo").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Accessor);
    assert_eq!(r.declaring_fqcn, "App\\Models\\Tenant");
}

// ─── Shared ClassViewCache: correctness + once-per-FQCN ───────────────────
//
// The whole-project build shares ONE `ClassViewCache` across every parallel
// worker so each class is analyzed once total (not once per referencing file).
// These prove the two invariants that keep that safe and worth it:
//   1. equivalence — a cold cache-per-file run and a shared-cache run resolve
//      byte-identical entries AND deps (sharing is pure memoization);
//   2. once-per-FQCN — N files referencing the same model analyze it once.

/// A project with a shared base model + shared trait and N caller files, each
/// reading members off a typed receiver. Callers reuse a small model pool so
/// the shared cache has something to collapse.
struct SharedProject {
    _dir: TempDir,
    index: ClassHierarchyIndex,
    root: PathBuf,
    /// (source, captured refs) per caller — ready to feed into resolution.
    callers: Vec<(String, Vec<Arc<MemberAccessReferenceData>>)>,
}

/// Build a shared-ancestor project: `BaseModel` (extends Eloquent Model, uses a
/// trait) + `Auditable` trait + `pool` concrete models, and `n_callers` callers
/// each targeting `Model{i % pool}`.
fn shared_project(pool: usize, n_callers: usize) -> SharedProject {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    let mut index = ClassHierarchyIndex::default();

    let write = |index: &mut ClassHierarchyIndex, rel: &str, body: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        index.insert_file(&path, classes_in_file(&path, body));
    };

    // Shared ancestors — the classes the sharing must analyze once, not N times.
    write(
        &mut index,
        "app/Concerns/Auditable.php",
        r#"<?php
namespace App\Concerns;
trait Auditable {
    public function scopeAudited($query) { return $query; }
    public function getAuditedAtAttribute() { return $this->updated_at; }
}
"#,
    );
    write(
        &mut index,
        "app/Models/BaseModel.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use App\Concerns\Auditable;
class BaseModel extends Model {
    use Auditable;
    protected $fillable = ['id'];
}
"#,
    );
    for i in 0..pool {
        write(
            &mut index,
            &format!("app/Models/Model{i}.php"),
            &format!(
                r#"<?php
namespace App\Models;
class Model{i} extends BaseModel {{
    protected $fillable = ['name{i}'];
    public function scopeActive{i}($query) {{ return $query; }}
}}
"#
            ),
        );
    }

    let callers = (0..n_callers)
        .map(|c| {
            let m = c % pool;
            // Read one own column, one own scope, one inherited-trait accessor,
            // and one inherited-trait scope — so the full receiver chain
            // (Model{m} → BaseModel → Auditable) must be built to classify all.
            let source = format!(
                r#"<?php
namespace App\Http\Controllers;
use App\Models\Model{m};
class Controller{c} {{
    public function show(Model{m} $x) {{
        $a = $x->name{m};
        $b = $x->active{m}();
        $d = $x->auditedAt;
        $e = $x->audited();
        return [$a, $b, $d, $e];
    }}
}}
"#
            );
            let refs = member_refs_of(&source);
            (source, refs)
        })
        .collect();

    SharedProject {
        _dir: dir,
        index,
        root,
        callers,
    }
}

/// Resolve every caller, sorting each file's entries so the comparison is order-
/// independent (worker order differs between the two regimes). Also collects the
/// per-file dep sets. `shared` picks one cache for all files vs a fresh one each.
fn resolve_all(p: &SharedProject, shared: bool) -> (Vec<Vec<MagicMemberEntry>>, Vec<Vec<String>>) {
    let one = ClassViewCache::new();
    let mut all_entries = Vec::new();
    let mut all_deps = Vec::new();
    for (source, refs) in &p.callers {
        let fresh = ClassViewCache::new();
        let cache = if shared { &one } else { &fresh };
        let mut deps = HashSet::new();
        let mut entries =
            resolve_member_access_entries(source, refs, &p.index, cache, &p.root, Some(&mut deps));
        // Deterministic order for comparison — the resolver's output order can
        // depend on capture order, which is stable, but sorting is belt-and-braces.
        entries.sort_by(|a, b| {
            (a.fqcn.as_str(), a.member.as_str(), a.line, a.column).cmp(&(
                b.fqcn.as_str(),
                b.member.as_str(),
                b.line,
                b.column,
            ))
        });
        let mut dep_vec: Vec<String> = deps.into_iter().collect();
        dep_vec.sort();
        all_entries.push(entries);
        all_deps.push(dep_vec);
    }
    (all_entries, all_deps)
}

#[test]
fn shared_cache_resolves_identically_to_cold_cache() {
    // The correctness gate: sharing one cache across all files must produce
    // byte-identical resolved entries AND deps as a fresh-cache-per-file run.
    let p = shared_project(4, 20);
    let (cold_entries, cold_deps) = resolve_all(&p, false);
    let (shared_entries, shared_deps) = resolve_all(&p, true);
    assert_eq!(
        cold_entries, shared_entries,
        "shared-cache entries diverged from cold-cache — sharing must be pure memoization"
    );
    assert_eq!(
        cold_deps, shared_deps,
        "shared-cache deps diverged from cold-cache — sharing must be pure memoization"
    );
    // Sanity: the fixture actually resolved something (a green all-empty run
    // would pass the equivalence trivially).
    assert!(
        cold_entries.iter().any(|e| !e.is_empty()),
        "fixture resolved no entries — test would be vacuous"
    );
}

#[test]
fn shared_cache_analyzes_each_receiver_once() {
    // 12 callers funnel through 3 distinct models. With ONE shared cache, each
    // referenced FQCN is analyzed exactly once — misses == distinct classes,
    // and the remaining lookups are all hits.
    let p = shared_project(3, 12);
    let cache = ClassViewCache::new();
    for (source, refs) in &p.callers {
        resolve_member_access_entries(source, refs, &p.index, &cache, &p.root, None);
    }

    // Only the 3 distinct Model FQCNs are analyzed — every other lookup for the
    // same model across the 12 callers is a cache hit. (Ancestors BaseModel /
    // Auditable are walked *inside* analyze(Model{i}), not keyed here, so the
    // cache miss count is exactly the distinct receiver classes.)
    assert_eq!(
        cache.misses(),
        3,
        "expected 3 distinct receiver classes analyzed once each, got {} misses / {} hits",
        cache.misses(),
        cache.hits(),
    );
    // 12 callers × the same 3 models → 9 of them are repeat lookups (hits).
    assert!(
        cache.hits() >= 9,
        "expected the repeated receivers to hit the cache, got {} hits",
        cache.hits(),
    );
}

#[test]
fn shared_cache_single_receiver_analyzed_exactly_once() {
    // The tightest statement of the win: N callers all referencing the SAME
    // model analyze it exactly once total.
    let p = shared_project(1, 25);
    let cache = ClassViewCache::new();
    for (source, refs) in &p.callers {
        resolve_member_access_entries(source, refs, &p.index, &cache, &p.root, None);
    }
    assert_eq!(
        cache.misses(),
        1,
        "one model referenced by 25 callers must analyze once, got {} misses",
        cache.misses(),
    );
}

// ─── M1 single-parse capture: capture-vs-live equivalence ─────────────────
//
// The hard M1 guarantee: resolving PHP member accesses from context captured at
// parse must produce byte-identical entries AND deps to today's re-parse path,
// on a fixture that exercises every receiver shape — aliased imports, `$this`,
// typed + constructor-promoted props, multi-hop flow, foreach, `app('key')`
// container bindings, `auth()->user()`, static + `::query()->…` chains,
// self/static, and a multi-class file (per-site enclosing class).

use crate::salsa_impl::MemberContextData;

/// Build a `MemberContextData` for a `.php` file exactly as the parse-time
/// capture does (aliases + per-site recipes off one parse).
fn php_member_context(source: &str, refs: &[Arc<MemberAccessReferenceData>]) -> MemberContextData {
    let tree = parse_php(source).expect("parse");
    let aliases = extract_use_aliases(&tree, source);
    let sites = super::capture_php_sites(source, &tree, refs, &aliases);
    MemberContextData {
        aliases,
        sites,
        view_renders: Vec::new(),
        volt_surface: None,
        component: None,
    }
}

/// Resolve a caller's member accesses BOTH ways (tree path, captured-context
/// path) against `resolver`/`root`, returning `(entries, deps)` sorted for an
/// order-independent comparison.
fn resolve_both_ways(
    resolver: &impl ClassFileResolver,
    root: &std::path::Path,
    caller: &str,
) -> (
    (Vec<MagicMemberEntry>, Vec<String>),
    (Vec<MagicMemberEntry>, Vec<String>),
) {
    let refs = member_refs_of(caller);

    let sort_entries = |mut e: Vec<MagicMemberEntry>| {
        e.sort_by(|a, b| {
            (a.fqcn.as_str(), a.member.as_str(), a.line, a.column).cmp(&(
                b.fqcn.as_str(),
                b.member.as_str(),
                b.line,
                b.column,
            ))
        });
        e
    };
    let sort_deps = |d: HashSet<String>| {
        let mut v: Vec<String> = d.into_iter().collect();
        v.sort();
        v
    };

    let tree = {
        let cache = ClassViewCache::new();
        let mut deps = HashSet::new();
        let e =
            resolve_member_access_entries(caller, &refs, resolver, &cache, root, Some(&mut deps));
        (sort_entries(e), sort_deps(deps))
    };
    let captured = {
        let ctx = php_member_context(caller, &refs);
        let cache = ClassViewCache::new();
        let mut deps = HashSet::new();
        let e = resolve_member_access_entries_with_context(
            &ctx,
            &refs,
            resolver,
            &cache,
            root,
            Some(&mut deps),
        );
        (sort_entries(e), sort_deps(deps))
    };
    (tree, captured)
}

/// A class index + auth-configured project root for the rich equivalence
/// fixture. Each test builds its own `WithBindings` resolver borrowing the
/// index (with a `tenant` container binding).
fn rich_equivalence_project() -> (ClassHierarchyIndex, PathBuf, TempDir) {
    let mut index = ClassHierarchyIndex::default();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/auth.php"),
        r#"<?php
use App\Models\User;
return ['providers' => ['users' => ['model' => User::class]]];
"#,
    )
    .unwrap();

    let write = |index: &mut ClassHierarchyIndex, rel: &str, body: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        index.insert_file(&path, classes_in_file(&path, body));
    };
    write(
        &mut index,
        "app/Models/User.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
class User extends Model {
    protected $fillable = ['email', 'name'];
    public function scopeActive($query) { return $query->where('active', true); }
    public function getFullNameAttribute(): string { return ''; }
    public function posts(): HasMany { return $this->hasMany(Post::class); }
}
"#,
    );
    write(
        &mut index,
        "app/Models/Post.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Post extends Model {
    protected $fillable = ['title'];
}
"#,
    );
    write(
        &mut index,
        "app/Models/Tenant.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class Tenant extends Model {
    protected $fillable = ['name'];
    public function getLogoAttribute(): string { return ''; }
}
"#,
    );

    (index, root, dir)
}

/// A `WithBindings` resolver borrowing `index`, with a `tenant` container
/// binding (matching the fixture's `app('tenant')`).
fn rich_resolver(index: &ClassHierarchyIndex) -> WithBindings<'_> {
    let mut bindings = HashMap::new();
    bindings.insert("tenant".to_string(), "App\\Models\\Tenant".to_string());
    WithBindings { index, bindings }
}

#[test]
fn captured_context_resolves_identically_to_tree_path() {
    let (index, root, _dir) = rich_equivalence_project();
    let resolver = rich_resolver(&index);

    // A controller-shaped caller exercising the receiver-shape zoo, plus a
    // SECOND class in the same file so per-site enclosing-class capture is
    // tested. Aliases: `User as U`, plain `Post`, `Tenant`.
    let caller = r#"<?php
namespace App\Http\Controllers;

use App\Models\User as U;
use App\Models\Post;
use App\Models\Tenant;

class ShowController {
    private U $currentUser;

    public function __construct(private Tenant $tenant) {}

    public function show(U $u) {
        $email = $u->email;            // typed param → column
        $name = $u->fullName;          // accessor
        $posts = $u->posts;            // relationship (property)
        $scope = $u->active();         // scope (instance call)
        $finder = $u->whereName('x');  // dynamic finder (instance call)

        $mine = $this->currentUser->email;   // typed prop → column
        $logo = $this->tenant->logo;         // promoted prop → accessor
        $nope = $this->missingThing;         // $this = controller, no member → dropped

        $s1 = U::active();                    // static scope
        $s2 = U::query()->active();           // static builder chain
        $s3 = U::whereEmail('a@b.c');         // static dynamic finder

        $all = U::all();
        foreach ($all as $one) {
            $e2 = $one->email;                // foreach element → column
        }

        $auth = auth()->user()->email;        // auth model → column
        $t = app('tenant')->name;             // container binding → column

        return [$email, $name, $posts, $scope, $finder, $mine, $logo, $nope,
                $s1, $s2, $s3, $e2, $auth, $t];
    }
}

class Helper {
    public function go(U $u) {
        return $u->email;   // second class: per-site enclosing class
    }
}
"#;

    let (tree, captured) = resolve_both_ways(&resolver, &root, caller);
    assert_eq!(
        tree.0, captured.0,
        "captured-context entries diverged from the tree path"
    );
    assert_eq!(
        tree.1, captured.1,
        "captured-context deps diverged from the tree path"
    );
    // Guard against a vacuous green: the fixture must resolve real entries.
    assert!(
        tree.0.len() >= 8,
        "fixture under-resolved ({} entries) — equivalence would be near-vacuous",
        tree.0.len()
    );
}

#[test]
fn captured_context_member_serde_round_trips() {
    // The captured context must survive the bincode disk-cache round trip and
    // re-resolve identically — the property the pattern-cache v11 bump relies on.
    let (index, root, _dir) = rich_equivalence_project();
    let resolver = rich_resolver(&index);
    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
class C {
    public function show(User $u) {
        $a = $u->email;
        $b = $u->active();
        $c = auth()->user()->email;
        $d = app('tenant')->name;
        return [$a, $b, $c, $d];
    }
}
"#;
    let refs = member_refs_of(caller);
    let ctx = php_member_context(caller, &refs);

    let bytes = bincode::serde::encode_to_vec(&ctx, bincode::config::standard()).unwrap();
    let (decoded, _): (MemberContextData, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    assert_eq!(
        ctx, decoded,
        "context did not survive the bincode round trip"
    );

    let resolve = |c: &MemberContextData| {
        let cache = ClassViewCache::new();
        let mut deps = HashSet::new();
        let mut e = resolve_member_access_entries_with_context(
            c,
            &refs,
            &resolver,
            &cache,
            &root,
            Some(&mut deps),
        );
        e.sort_by(|a, b| a.member.cmp(&b.member));
        e
    };
    assert_eq!(
        resolve(&ctx),
        resolve(&decoded),
        "re-resolving the decoded context diverged from the original"
    );
    assert!(!resolve(&ctx).is_empty(), "fixture resolved nothing");
}

/// A project exercising the four recipe/chain variants the first fixture left
/// uncovered: `GateClosureUser`, `HelperBinding`-as-receiver, `MethodReturn`,
/// and `ChainRootData::Var`. Auth model + a `cache` helper binding are set up.
fn variant_project() -> (ClassHierarchyIndex, PathBuf, TempDir) {
    let mut index = ClassHierarchyIndex::default();
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("composer.json"),
        r#"{ "autoload": { "psr-4": { "App\\": "app/" } } }"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/auth.php"),
        r#"<?php
use App\Models\User;
return ['providers' => ['users' => ['model' => User::class]]];
"#,
    )
    .unwrap();
    let write = |index: &mut ClassHierarchyIndex, rel: &str, body: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        index.insert_file(&path, classes_in_file(&path, body));
    };
    write(
        &mut index,
        "app/Models/User.php",
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
    public function scopeActive($query) { return $query->where('active', true); }
}
"#,
    );
    write(
        &mut index,
        "app/Repositories/Repo.php",
        r#"<?php
namespace App\Repositories;
use App\Models\User;
class Repo {
    public function currentUser(): User { return new User(); }
}
"#,
    );
    write(
        &mut index,
        "app/Support/Cache.php",
        r#"<?php
namespace App\Support;
class Cache {
    public string $prefix = '';
}
"#,
    );
    (index, root, dir)
}

#[test]
fn captured_context_covers_remaining_receiver_variants() {
    let (index, root, _dir) = variant_project();
    let mut bindings = HashMap::new();
    bindings.insert("cache".to_string(), "App\\Support\\Cache".to_string());
    let resolver = WithBindings {
        index: &index,
        bindings,
    };

    let caller = r#"<?php
namespace App\Http\Controllers;
use App\Models\User;
use App\Repositories\Repo;
class C {
    public function show(User $u, Repo $repo) {
        // ChainRootData::Var — $u->fresh() has no in-view return type, so the
        // direct method-return fails and the chain fallback roots at the $u var.
        $a = $u->fresh()->active();
        // MethodReturn — currentUser(): User, then a column read on the result.
        $b = $repo->currentUser()->email;
        // HelperBinding as a receiver — cache() → the bound Cache concrete.
        $c = cache()->prefix;
        // GateClosureUser — the Gate ability closure's first param is the model.
        \Gate::define('viewThing', function ($user) { return $user->email; });
        return [$a, $b, $c];
    }
}
"#;

    let (tree, captured) = resolve_both_ways(&resolver, &root, caller);
    assert_eq!(
        tree.0, captured.0,
        "captured-context entries diverged on the remaining variants"
    );
    assert_eq!(
        tree.1, captured.1,
        "captured-context deps diverged on the remaining variants"
    );
    // Each of the four variants must resolve a real entry (not just agree on
    // dropping everything) — assert the members that only these paths produce.
    let members: Vec<&str> = tree.0.iter().map(|e| e.member.as_str()).collect();
    for expected in ["active", "email", "prefix"] {
        assert!(
            members.contains(&expected),
            "variant fixture missing `{expected}` — got {members:?}"
        );
    }
}

// ─── Factory resolution: `Model::factory()` + factory chains (#30) ─────────

const FACTORY_USER_MODEL: &str = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#;

const USER_FACTORY_SRC: &str = r#"<?php
namespace Database\Factories;
use Illuminate\Database\Eloquent\Factories\Factory;
class UserFactory extends Factory {
    public function suspended(): static { return $this->state(['active' => false]); }
}
"#;

const FACTORY_CALLER: &str = r#"<?php
namespace Tests\Feature;
use App\Models\User;
class UserTest {
    public function seed() { return User::factory(); }
}
"#;

/// Resolve an instance call `…->{member}()` in `caller` against `resolver` —
/// the chain-link counterpart of [`resolve_static_call`].
fn resolve_instance_call(
    resolver: &impl ClassFileResolver,
    root: &std::path::Path,
    caller: &str,
    member: &str,
) -> Option<ResolvedMemberAccess> {
    let tree = parse_php(caller).expect("parse caller");
    let bytes = caller.as_bytes();
    let aliases = extract_use_aliases(&tree, caller);
    let mut stack = vec![tree.root_node()];
    let mut object = None;
    while let Some(n) = stack.pop() {
        if n.kind() == "member_call_expression"
            && n.child_by_field_name("name")
                .and_then(|nm| nm.utf8_text(bytes).ok())
                == Some(member)
        {
            object = n.child_by_field_name("object");
            break;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    let cache = ClassViewCache::new();
    resolve_and_classify(
        object?,
        member,
        AccessForm::InstanceCall,
        bytes,
        &aliases,
        resolver,
        &cache,
        root,
        None,
    )
}

#[test]
fn factory_call_resolves_to_conventional_factory() {
    // `User::factory()` → `Database\Factories\UserFactory` by Laravel's
    // convention, gated on the factory class actually resolving.
    let p = project_files(&[
        ("app/Models/User.php", FACTORY_USER_MODEL),
        ("database/factories/UserFactory.php", USER_FACTORY_SRC),
    ]);
    let r = resolve_static_call(&p.index, &p.root, FACTORY_CALLER, "factory").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Factory);
    assert_eq!(r.declaring_fqcn, "Database\\Factories\\UserFactory");
}

#[test]
fn factory_call_with_fully_qualified_receiver_resolves() {
    // `\App\Models\User::factory()` — no `use` import; the written-out
    // receiver must resolve identically (AC: FQ receiver form).
    let p = project_files(&[
        ("app/Models/User.php", FACTORY_USER_MODEL),
        ("database/factories/UserFactory.php", USER_FACTORY_SRC),
    ]);
    let caller = "<?php \\App\\Models\\User::factory();";
    let r = resolve_static_call(&p.index, &p.root, caller, "factory").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Factory);
    assert_eq!(r.declaring_fqcn, "Database\\Factories\\UserFactory");
}

#[test]
fn factory_call_honors_new_factory_override() {
    // A declared `newFactory()` names the factory directly and beats the
    // conventional candidate.
    let override_model = r#"<?php
namespace App\Models;
use Database\Factories\Custom\AdminFactory;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected static function newFactory(): AdminFactory { return AdminFactory::new(); }
}
"#;
    let admin_factory = r#"<?php
namespace Database\Factories\Custom;
use Illuminate\Database\Eloquent\Factories\Factory;
class AdminFactory extends Factory {}
"#;
    let p = project_files(&[
        ("app/Models/User.php", override_model),
        ("database/factories/Custom/AdminFactory.php", admin_factory),
    ]);
    let r = resolve_static_call(&p.index, &p.root, FACTORY_CALLER, "factory").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::Factory);
    assert_eq!(
        r.declaring_fqcn,
        "Database\\Factories\\Custom\\AdminFactory"
    );
}

#[test]
fn factory_call_without_factory_file_does_not_classify() {
    // No factory class resolves → no Factory tag, and nothing else on the
    // model claims `factory` — a dead goto target is worse than none.
    let p = project_files(&[("app/Models/User.php", FACTORY_USER_MODEL)]);
    let r = resolve_static_call(&p.index, &p.root, FACTORY_CALLER, "factory");
    assert!(
        r.is_none(),
        "a model with no resolvable factory must not classify; got {r:?}"
    );
}

#[test]
fn factory_chain_declared_state_classifies_as_factory_method() {
    // `User::factory()->suspended()` — the chain's subject re-targets to the
    // factory, and a state the factory DECLARES tags FactoryMethod (not a
    // droppable PlainMember).
    let p = project_files(&[
        ("app/Models/User.php", FACTORY_USER_MODEL),
        ("database/factories/UserFactory.php", USER_FACTORY_SRC),
    ]);
    let caller = r#"<?php
namespace Tests\Feature;
use App\Models\User;
class UserTest {
    public function t() { return User::factory()->suspended(); }
}
"#;
    let r = resolve_instance_call(&p.index, &p.root, caller, "suspended").expect("resolves");
    assert_eq!(r.kind, MagicMemberKind::FactoryMethod);
    assert_eq!(r.declaring_fqcn, "Database\\Factories\\UserFactory");
}

#[test]
fn factory_chain_undeclared_member_never_degrades_to_class_line() {
    // `User::factory()->create()` — `create` lives on the vendor base the
    // index can't see. Unlike the facade signal, a factory chain never
    // degrades an undeclared member to the class line: no target beats a
    // wrong-class target.
    let p = project_files(&[
        ("app/Models/User.php", FACTORY_USER_MODEL),
        ("database/factories/UserFactory.php", USER_FACTORY_SRC),
    ]);
    let caller = r#"<?php
namespace Tests\Feature;
use App\Models\User;
class UserTest {
    public function t() { return User::factory()->create(); }
}
"#;
    let r = resolve_instance_call(&p.index, &p.root, caller, "create");
    assert!(
        r.is_none(),
        "an undeclared factory-chain member must not classify; got {r:?}"
    );
}

// ─── Custom pivot classification: `->pivot` (#30 item 4) ───────────────────

#[test]
fn classifies_pivot_property_with_declared_pivot_class() {
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use App\Models\Pivots\Membership;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $pivotClass = Membership::class;
}
"#,
    );
    let c = classify_member(&view, "pivot", AccessForm::Property).expect("pivot");
    assert_eq!(c.kind, MagicMemberKind::Pivot);
    assert_eq!(c.declaring_fqcn, "App\\Models\\Pivots\\Membership");
}

#[test]
fn pivot_without_declared_class_does_not_classify() {
    // The default `Relations\Pivot` is vendor territory — no card, no goto.
    let (_d, view) = model(
        r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model {
    protected $fillable = ['email'];
}
"#,
    );
    assert!(classify_member(&view, "pivot", AccessForm::Property).is_none());
}
