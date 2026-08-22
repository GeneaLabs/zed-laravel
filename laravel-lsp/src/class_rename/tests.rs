use super::{
    blade_use_spans, class_at_cursor, class_at_cursor_for, is_dependency_path, project_php_files,
    reference_spans, reference_spans_for, renamed_file_path,
};
use std::path::Path;

/// Replace every span (right-to-left so earlier offsets stay valid) — mimics
/// what the rename WorkspaceEdit does, so we can assert on the rewritten source.
fn apply(content: &str, spans: &[(usize, usize)], new: &str) -> String {
    let mut out = content.to_string();
    let mut spans = spans.to_vec();
    spans.sort_unstable();
    for (s, e) in spans.into_iter().rev() {
        out.replace_range(s..e, new);
    }
    out
}

fn rename(content: &str, fqcn: &str, old: &str, new: &str) -> String {
    apply(content, &reference_spans(content, fqcn, old), new)
}

// ---- declaration + same-file references -----------------------------------

#[test]
fn renames_declaration_and_self_references() {
    let src = r#"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model;
class User extends Model
{
    public function scopeRecent($q) { return $q; }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Customer");
    assert!(out.contains("class Customer extends Model"));
    // The base class `Model` is untouched.
    assert!(out.contains("extends Model"));
}

// ---- use imports + static calls + new + type hints ------------------------

const CONTROLLER: &str = r#"<?php
namespace App\Http\Controllers;

use App\Models\User;
use App\Models\Post;

class UserController extends Controller
{
    public function show(User $user): ?User
    {
        $u = new User();
        $found = User::where('id', 1)->first();
        $key = User::class;
        if ($u instanceof User) {
            return $u;
        }
        return $found;
    }

    public User $current;
}
"#;

#[test]
fn renames_all_reference_kinds_in_consumer_file() {
    let out = rename(CONTROLLER, "App\\Models\\User", "User", "Member");
    // use import
    assert!(
        out.contains("use App\\Models\\Member;"),
        "use import\n{out}"
    );
    // param + return type
    assert!(
        out.contains("show(Member $user): ?Member"),
        "type hints\n{out}"
    );
    // new
    assert!(out.contains("new Member()"), "new\n{out}");
    // static call
    assert!(out.contains("Member::where("), "static call\n{out}");
    // ::class
    assert!(out.contains("Member::class"), "::class\n{out}");
    // instanceof
    assert!(out.contains("instanceof Member"), "instanceof\n{out}");
    // property type
    assert!(
        out.contains("public Member $current"),
        "property type\n{out}"
    );
    // unrelated import untouched
    assert!(
        out.contains("use App\\Models\\Post;"),
        "Post untouched\n{out}"
    );
    // the controller class name itself is untouched
    assert!(out.contains("class UserController extends Controller"));
}

// ---- alias safety ---------------------------------------------------------

#[test]
fn aliased_import_rewrites_only_the_class_segment() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User as Account;
class C {
    public function f(Account $a): Account { return new Account(); }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    // The import's class segment changes; the alias `Account` stays everywhere.
    assert!(out.contains("use App\\Models\\Member as Account;"), "{out}");
    assert!(
        out.contains("f(Account $a): Account"),
        "alias usages unchanged\n{out}"
    );
    assert!(out.contains("new Account()"), "{out}");
    assert!(!out.contains("Member as Member"));
}

// ---- fully-qualified references -------------------------------------------

#[test]
fn renames_fully_qualified_references() {
    let src = r#"<?php
namespace App\Jobs;
class J {
    public function f() {
        return \App\Models\User::query()->get();
    }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("\\App\\Models\\Member::query()"), "{out}");
}

// ---- precision: same-named member must NOT be touched ---------------------

#[test]
fn does_not_touch_method_or_property_named_like_class() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User;
class C {
    public function f($obj) {
        $obj->User();       // method call named User
        $x = $obj->User;    // property named User
        return User::find(1); // real class ref
    }
}
"#;
    let spans = reference_spans(src, "App\\Models\\User", "User");
    // Exactly two: the `use` import and the `User::find` static call.
    assert_eq!(spans.len(), 2, "only real class refs: {spans:?}");
    let out = apply(src, &spans, "Member");
    assert!(out.contains("$obj->User()"), "method untouched\n{out}");
    assert!(
        out.contains("$x = $obj->User;"),
        "property untouched\n{out}"
    );
    assert!(out.contains("Member::find(1)"), "{out}");
}

// ---- same-namespace bare references (no import needed) --------------------

#[test]
fn renames_same_namespace_bare_reference() {
    let src = r#"<?php
namespace App\Models;
class Post extends Model {
    public function author() { return $this->belongsTo(User::class); }
    public function owner(): User { return new User(); }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("belongsTo(Member::class)"), "{out}");
    assert!(out.contains("owner(): Member"), "{out}");
    assert!(out.contains("new Member()"), "{out}");
    // Post itself untouched.
    assert!(out.contains("class Post extends Model"));
}

// ---- docblocks ------------------------------------------------------------

#[test]
fn renames_docblock_type_references() {
    let src = r#"<?php
namespace App\Http;
use App\Models\User;
class C {
    /**
     * @param User $user
     * @return User|null
     */
    public function f($user) { return $user; }
}
"#;
    let out = rename(src, "App\\Models\\User", "User", "Member");
    assert!(out.contains("@param Member $user"), "docblock param\n{out}");
    assert!(
        out.contains("@return Member|null"),
        "docblock return\n{out}"
    );
}

// ---- class_at_cursor (prepare_rename) -------------------------------------

#[test]
fn cursor_on_static_call_resolves_class() {
    let src = "<?php\nnamespace App\\Http;\nuse App\\Models\\User;\n$x = User::where('id', 1);\n";
    let byte = src.find("User::").unwrap() + 1; // inside `User`
    let (fqcn, span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Models\\User");
    assert_eq!(&src[span.0..span.1], "User");
}

#[test]
fn cursor_on_declaration_resolves_class() {
    let src = "<?php\nnamespace App\\Models;\nclass User extends Model {}\n";
    let byte = src.find("class User").unwrap() + "class U".len();
    let (fqcn, _span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Models\\User");
}

#[test]
fn cursor_on_alias_returns_none() {
    // Cursor on the alias token `Account` — not a real class name, so no rename.
    let src = "<?php\nuse App\\Models\\User as Account;\n$x = Account::find(1);\n";
    let byte = src.find("Account::").unwrap() + 1;
    assert!(class_at_cursor(src, byte).is_none());
}

#[test]
fn cursor_off_any_class_returns_none() {
    let src = "<?php\n$x = 1 + 2;\n";
    assert!(class_at_cursor(src, 8).is_none());
}

// ---- project file enumeration ---------------------------------------------

#[test]
fn project_php_files_skips_vendor_and_finds_app() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("app/Models")).unwrap();
    std::fs::create_dir_all(root.join("routes")).unwrap();
    std::fs::create_dir_all(root.join("vendor/laravel/framework/src")).unwrap();
    std::fs::write(root.join("app/Models/User.php"), "<?php").unwrap();
    std::fs::write(root.join("routes/web.php"), "<?php").unwrap();
    std::fs::write(root.join("vendor/laravel/framework/src/Model.php"), "<?php").unwrap();

    let files = project_php_files(root);
    assert!(files.iter().any(|p| p.ends_with("app/Models/User.php")));
    assert!(
        !files.iter().any(|p| p.to_string_lossy().contains("vendor")),
        "vendor must be pruned: {files:?}"
    );
}

#[test]
fn is_dependency_path_flags_vendor() {
    assert!(is_dependency_path(Path::new(
        "/proj/vendor/laravel/framework/src/Model.php"
    )));
    assert!(!is_dependency_path(Path::new("/proj/app/Models/User.php")));
}

// ---- file move target -----------------------------------------------------

#[test]
fn renamed_file_path_swaps_basename_same_dir_for_every_kind() {
    // The declaring file moves within its own directory, basename swapped,
    // `.php` preserved — identical rule for every class kind (PSR-4: one class
    // per file, basename == class basename).
    let cases = [
        (
            "/proj/app/Http/Controllers/UserController.php",
            "AdminController",
            "/proj/app/Http/Controllers/AdminController.php",
        ),
        (
            "/proj/app/Jobs/SendWelcomeEmail.php",
            "SendGreeting",
            "/proj/app/Jobs/SendGreeting.php",
        ),
        (
            "/proj/app/Services/PaymentService.php",
            "BillingService",
            "/proj/app/Services/BillingService.php",
        ),
        (
            "/proj/app/Http/Requests/StorePostRequest.php",
            "CreatePostRequest",
            "/proj/app/Http/Requests/CreatePostRequest.php",
        ),
        // Regression guard: models follow the very same rule.
        (
            "/proj/app/Models/User.php",
            "Customer",
            "/proj/app/Models/Customer.php",
        ),
    ];
    for (decl, new_basename, expected) in cases {
        assert_eq!(
            renamed_file_path(Path::new(decl), new_basename),
            Path::new(expected),
            "rename {decl} → {new_basename}"
        );
    }
}

// ---- controllers ----------------------------------------------------------

#[test]
fn renames_controller_class_declaration_and_route_references() {
    // Declaration site: `class UserController extends Controller`.
    let decl = r#"<?php
namespace App\Http\Controllers;
class UserController extends Controller
{
    public function index() {}
}
"#;
    let out = rename(
        decl,
        "App\\Http\\Controllers\\UserController",
        "UserController",
        "AdminController",
    );
    assert!(
        out.contains("class AdminController extends Controller"),
        "declaration\n{out}"
    );
    // The base `Controller` is untouched.
    assert!(out.contains("extends Controller"), "base class\n{out}");

    // Consumer site: a routes file referencing the controller via `::class`.
    let routes = r#"<?php
use App\Http\Controllers\UserController;
Route::get('/users', [UserController::class, 'index']);
Route::resource('users', UserController::class);
"#;
    let out = rename(
        routes,
        "App\\Http\\Controllers\\UserController",
        "UserController",
        "AdminController",
    );
    assert!(
        out.contains("use App\\Http\\Controllers\\AdminController;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("[AdminController::class, 'index']"),
        "route array action\n{out}"
    );
    assert!(
        out.contains("Route::resource('users', AdminController::class)"),
        "resource action\n{out}"
    );
}

#[test]
fn cursor_on_controller_declaration_resolves_class() {
    let src =
        "<?php\nnamespace App\\Http\\Controllers;\nclass UserController extends Controller {}\n";
    let byte = src.find("class UserController").unwrap() + "class User".len();
    let (fqcn, _span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Http\\Controllers\\UserController");
}

// ---- jobs -----------------------------------------------------------------

#[test]
fn renames_job_class_declaration_and_dispatch_references() {
    let decl = r#"<?php
namespace App\Jobs;
class SendWelcomeEmail implements ShouldQueue
{
    public function handle() {}
}
"#;
    let out = rename(
        decl,
        "App\\Jobs\\SendWelcomeEmail",
        "SendWelcomeEmail",
        "SendGreeting",
    );
    assert!(
        out.contains("class SendGreeting implements ShouldQueue"),
        "declaration\n{out}"
    );

    // Consumer: dispatch via static call, `new`, and `dispatch(new …)`.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Jobs\SendWelcomeEmail;
class RegisterController extends Controller {
    public function store() {
        SendWelcomeEmail::dispatch($user);
        dispatch(new SendWelcomeEmail($user));
        $job = new SendWelcomeEmail($user);
        return $job;
    }
}
"#;
    let out = rename(
        consumer,
        "App\\Jobs\\SendWelcomeEmail",
        "SendWelcomeEmail",
        "SendGreeting",
    );
    assert!(
        out.contains("use App\\Jobs\\SendGreeting;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("SendGreeting::dispatch($user)"),
        "static dispatch\n{out}"
    );
    assert_eq!(
        out.matches("new SendGreeting($user)").count(),
        2,
        "both `new` sites\n{out}"
    );
    // The enclosing controller is untouched.
    assert!(out.contains("class RegisterController extends Controller"));
}

// ---- services -------------------------------------------------------------

#[test]
fn renames_service_class_declaration_and_injection_references() {
    let decl = r#"<?php
namespace App\Services;
class PaymentService
{
    public function charge() {}
}
"#;
    let out = rename(
        decl,
        "App\\Services\\PaymentService",
        "PaymentService",
        "BillingService",
    );
    assert!(out.contains("class BillingService"), "declaration\n{out}");

    // Consumer: constructor-injected + method type-hint + return type + `new`.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Services\PaymentService;
class CheckoutController extends Controller {
    public function __construct(private PaymentService $payments) {}
    public function build(PaymentService $service): PaymentService {
        return new PaymentService();
    }
}
"#;
    let out = rename(
        consumer,
        "App\\Services\\PaymentService",
        "PaymentService",
        "BillingService",
    );
    assert!(
        out.contains("use App\\Services\\BillingService;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("__construct(private BillingService $payments)"),
        "constructor injection\n{out}"
    );
    assert!(
        out.contains("build(BillingService $service): BillingService"),
        "param + return type\n{out}"
    );
    assert!(out.contains("new BillingService()"), "new\n{out}");
    assert!(out.contains("class CheckoutController extends Controller"));
}

// ---- form requests --------------------------------------------------------

#[test]
fn renames_form_request_class_declaration_and_type_hint_references() {
    let decl = r#"<?php
namespace App\Http\Requests;
class StorePostRequest extends FormRequest
{
    public function rules(): array { return []; }
}
"#;
    let out = rename(
        decl,
        "App\\Http\\Requests\\StorePostRequest",
        "StorePostRequest",
        "CreatePostRequest",
    );
    assert!(
        out.contains("class CreatePostRequest extends FormRequest"),
        "declaration\n{out}"
    );

    // Consumer: a controller type-hinting the request as an action argument,
    // plus a docblock reference.
    let consumer = r#"<?php
namespace App\Http\Controllers;
use App\Http\Requests\StorePostRequest;
class PostController extends Controller {
    public function store(StorePostRequest $request) {
        return $request->validated();
    }
    /**
     * @param StorePostRequest $request
     */
    public function update(StorePostRequest $request) {}
}
"#;
    let out = rename(
        consumer,
        "App\\Http\\Requests\\StorePostRequest",
        "StorePostRequest",
        "CreatePostRequest",
    );
    assert!(
        out.contains("use App\\Http\\Requests\\CreatePostRequest;"),
        "use import\n{out}"
    );
    assert!(
        out.contains("store(CreatePostRequest $request)"),
        "action type hint\n{out}"
    );
    assert!(
        out.contains("update(CreatePostRequest $request)"),
        "second type hint\n{out}"
    );
    assert!(
        out.contains("@param CreatePostRequest $request"),
        "docblock\n{out}"
    );
    assert!(out.contains("class PostController extends Controller"));
}

#[test]
fn cursor_on_job_static_dispatch_resolves_class() {
    let src = "<?php\nnamespace App\\Http\\Controllers;\nuse App\\Jobs\\SendWelcomeEmail;\nSendWelcomeEmail::dispatch($u);\n";
    let byte = src.find("SendWelcomeEmail::dispatch").unwrap() + 1; // inside the class name
    let (fqcn, span) = class_at_cursor(src, byte).expect("class at cursor");
    assert_eq!(fqcn, "App\\Jobs\\SendWelcomeEmail");
    assert_eq!(&src[span.0..span.1], "SendWelcomeEmail");
}

// ---------------------------------------------------------------------------
// Blade `@use` imports — the class name lives in a string, invisible to the
// PHP class-name walk, so it needs its own span finder or a class rename
// leaves the template importing a class that no longer exists.
// ---------------------------------------------------------------------------

fn blade_rename(content: &str, fqcn: &str, old: &str, new: &str) -> String {
    apply(content, &blade_use_spans(content, fqcn, old), new)
}

#[test]
fn blade_use_import_is_rewritten_by_a_class_rename() {
    let src = "@use('App\\Models\\Flight')\n<div>{{ Flight::count() }}</div>\n";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        "@use('App\\Models\\Booking')\n<div>{{ Flight::count() }}</div>\n",
        "only the import string is this function's job; the echo is the PHP walk's"
    );
}

#[test]
fn blade_use_rewrite_touches_only_the_basename() {
    // A namespace segment that repeats the basename must not be rewritten.
    let src = "@use('App\\Flight\\Flight')";

    assert_eq!(
        blade_rename(src, r"App\Flight\Flight", "Flight", "Booking"),
        "@use('App\\Flight\\Booking')"
    );
}

#[test]
fn blade_use_for_a_different_class_is_untouched() {
    let src = "@use('App\\Models\\Airport')";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        src
    );
}

#[test]
fn blade_use_rewrites_every_matching_import_in_the_file() {
    let src = "@use('App\\Models\\Flight')\n@use('App\\Models\\Flight', 'F2')\n";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        "@use('App\\Models\\Booking')\n@use('App\\Models\\Booking', 'F2')\n"
    );
}

/// A group import names several classes in one string. Spans are computed from
/// the source, so the member that matches is rewritten and its siblings are not.
#[test]
fn blade_group_import_rewrites_only_the_matching_member() {
    let src = "@use('App\\Models\\{Flight, Airport}')";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        "@use('App\\Models\\{Booking, Airport}')"
    );
}

#[test]
fn blade_group_import_rewrites_an_aliased_member_on_the_class_not_the_alias() {
    let src = "@use('App\\Models\\{Flight as F, Airport}')";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        "@use('App\\Models\\{Booking as F, Airport}')"
    );
}

#[test]
fn blade_group_import_for_a_different_class_is_untouched() {
    let src = "@use('App\\Models\\{Flight, Airport}')";

    assert_eq!(
        blade_rename(src, r"App\Models\Gate", "Gate", "Booking"),
        src
    );
}

#[test]
fn blade_use_in_a_comment_is_not_rewritten() {
    let src = "{{-- @use('App\\Models\\Flight') --}}";

    assert!(blade_use_spans(src, r"App\Models\Flight", "Flight").is_empty());
}

// --- the path-aware wrapper -------------------------------------------------

#[test]
fn reference_spans_for_merges_blade_imports_with_php_positions() {
    // A Volt-ish template: an `@use` import AND a `@php` block using the class.
    let src = "@use('App\\Models\\Flight')\n@php\n    $n = Flight::count();\n@endphp\n";
    let spans = reference_spans_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        r"App\Models\Flight",
        "Flight",
    );

    assert_eq!(
        apply(src, &spans, "Booking"),
        "@use('App\\Models\\Booking')\n@php\n    $n = Booking::count();\n@endphp\n",
        "both the import string and the PHP usage are rewritten"
    );
}

#[test]
fn reference_spans_for_a_php_file_ignores_use_directive_syntax() {
    // `@use(...)` inside a PHP string is not a Blade directive — a `.php` file
    // must behave exactly as `reference_spans` does.
    let src = "<?php\nuse App\\Models\\Flight;\n$s = \"@use('App\\Models\\Flight')\";\n";
    let path = Path::new("/p/app/Foo.php");

    assert_eq!(
        reference_spans_for(path, src, r"App\Models\Flight", "Flight"),
        reference_spans(src, r"App\Models\Flight", "Flight"),
        "the Blade branch must not fire for a .php file"
    );
}

// --- cursor resolution ------------------------------------------------------

#[test]
fn cursor_inside_a_blade_use_import_resolves_the_class() {
    let src = "@use('App\\Models\\Flight')";
    let cursor = src.find("Models").expect("offset");

    let (fqcn, span) =
        class_at_cursor_for(Path::new("/p/resources/views/x.blade.php"), src, cursor)
            .expect("a Blade @use import is a class reference");

    assert_eq!(fqcn, r"App\Models\Flight");
    assert_eq!(
        &src[span.0..span.1],
        "Flight",
        "the rename range is the basename"
    );
}

#[test]
fn cursor_outside_a_blade_use_import_resolves_nothing() {
    let src = "@use('App\\Models\\Flight')\n<div>hello</div>";
    let cursor = src.find("hello").expect("offset");

    assert!(
        class_at_cursor_for(Path::new("/p/resources/views/x.blade.php"), src, cursor).is_none()
    );
}

/// The cursor picks out which member of a group import it sits on.
#[test]
fn cursor_in_a_blade_group_import_resolves_that_member() {
    let src = "@use('App\\Models\\{Flight, Airport}')";

    let (fqcn, span) = class_at_cursor_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        src.find("Flight").expect("offset"),
    )
    .expect("a group member is a class reference");
    assert_eq!(fqcn, r"App\Models\Flight");
    assert_eq!(&src[span.0..span.1], "Flight");

    let (other, _) = class_at_cursor_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        src.find("Airport").expect("offset"),
    )
    .expect("the sibling resolves independently");
    assert_eq!(other, r"App\Models\Airport");
}

/// The PHP branch is unchanged — same answer as `class_at_cursor`.
#[test]
fn cursor_in_a_php_file_still_resolves_through_the_php_walk() {
    let src = "<?php\nuse App\\Models\\Flight;\n";
    let cursor = src.find("Flight").expect("offset");

    assert_eq!(
        class_at_cursor_for(Path::new("/p/app/Foo.php"), src, cursor),
        class_at_cursor(src, cursor)
    );
}

/// Regression: a Blade template is not PHP, so tree-sitter-php collapses the
/// whole file to one inline-`text` node. Before regions were parsed
/// individually, a class rename found NOTHING in any template — it rewrote the
/// PHP side of the project and left every Blade usage pointing at a class that
/// no longer existed.
#[test]
fn class_usage_in_a_blade_echo_is_rewritten() {
    let src = "@use('App\\Models\\Flight')\n<p>{{ Flight::count() }}</p>\n";
    let spans = reference_spans_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        r"App\Models\Flight",
        "Flight",
    );

    assert_eq!(
        apply(src, &spans, "Booking"),
        "@use('App\\Models\\Booking')\n<p>{{ Booking::count() }}</p>\n"
    );
}

#[test]
fn class_usage_in_a_blade_template_without_the_php_walk_finds_nothing() {
    // Proves the region parsing is what does the work: the plain PHP walk over
    // the same template still sees a single text node.
    let src = "@php\n    $n = App\\Models\\Flight::count();\n@endphp\n";

    assert!(
        reference_spans(src, r"App\Models\Flight", "Flight").is_empty(),
        "the whole-file PHP walk cannot see into a template"
    );
    assert!(
        !reference_spans_for(
            Path::new("/p/resources/views/x.blade.php"),
            src,
            r"App\Models\Flight",
            "Flight"
        )
        .is_empty(),
        "the region walk can"
    );
}

/// An import the template aliases still resolves usages of the alias target,
/// and the alias itself is left alone (basename ≠ old basename).
#[test]
fn blade_aliased_import_rewrites_the_import_but_not_the_alias() {
    let src = "@use('App\\Models\\Flight', 'F')\n<p>{{ F::count() }}</p>\n";
    let spans = reference_spans_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        r"App\Models\Flight",
        "Flight",
    );

    assert_eq!(
        apply(src, &spans, "Booking"),
        "@use('App\\Models\\Booking', 'F')\n<p>{{ F::count() }}</p>\n",
        "the alias keeps pointing at the renamed class, so it must not change"
    );
}

/// Spans from several regions must not overlap or double-apply.
#[test]
fn multiple_blade_regions_each_rewrite_once() {
    let src = "@use('App\\Models\\Flight')\n@php\n    $a = Flight::first();\n@endphp\n<p>{{ Flight::count() }}</p>\n";
    let spans = reference_spans_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        r"App\Models\Flight",
        "Flight",
    );

    let out = apply(src, &spans, "Booking");
    assert_eq!(out.matches("Booking").count(), 3, "got {out:?}");
    assert!(!out.contains("Flight"), "got {out:?}");
}

/// Padding inside the quotes is located, not treated as untrustworthy: the name
/// is found at its real offset, so only the name is rewritten and the padding
/// survives.
#[test]
fn blade_use_with_padding_inside_the_quotes_is_rewritten() {
    let src = "@use(' App\\Models\\Flight ')";

    assert_eq!(
        blade_rename(src, r"App\Models\Flight", "Flight", "Booking"),
        "@use(' App\\Models\\Booking ')"
    );
    let (fqcn, span) = class_at_cursor_for(
        Path::new("/p/resources/views/x.blade.php"),
        src,
        src.find("Models").expect("offset"),
    )
    .expect("a padded import is still an import");
    assert_eq!(fqcn, r"App\Models\Flight");
    assert_eq!(&src[span.0..span.1], "Flight");
}

/// A Volt single-file component keeps its class body in `<?php … ?>` front
/// matter. Those are real PHP `use` statements and real class references — a
/// rename that skipped them would leave the component importing a class that no
/// longer exists.
#[test]
fn volt_front_matter_is_rewritten_by_a_class_rename() {
    let src = "<?php\n\nuse App\\Models\\Flight;\nuse Livewire\\Volt\\Component;\n\nnew class extends Component {\n    public function count(): int\n    {\n        return Flight::query()->count();\n    }\n};\n\n?>\n\n<div>{{ $this->count() }}</div>\n";
    let spans = reference_spans_for(
        Path::new("/p/resources/views/livewire/counter.blade.php"),
        src,
        r"App\Models\Flight",
        "Flight",
    );

    let out = apply(src, &spans, "Booking");
    assert!(
        out.contains("use App\\Models\\Booking;"),
        "the import must be rewritten, got:\n{out}"
    );
    assert!(
        out.contains("return Booking::query()->count();"),
        "the usage must be rewritten too, got:\n{out}"
    );
    assert!(
        out.contains("use Livewire\\Volt\\Component;"),
        "an unrelated import must be untouched, got:\n{out}"
    );
    assert!(!out.contains("Flight"), "got:\n{out}");
}

/// And the rename can be *started* from the front matter, not only applied to
/// it — otherwise F2 on a Volt component's own import would do nothing.
#[test]
fn cursor_in_volt_front_matter_resolves_the_class() {
    let src = "<?php\nuse App\\Models\\Flight;\nnew class {};\n?>\n<div></div>\n";
    let cursor = src.find("Flight").expect("offset");

    let (fqcn, span) = class_at_cursor_for(
        Path::new("/p/resources/views/livewire/counter.blade.php"),
        src,
        cursor,
    )
    .expect("front matter is PHP like any other");

    assert_eq!(fqcn, r"App\Models\Flight");
    assert_eq!(&src[span.0..span.1], "Flight");
}

/// A cursor on a class used inside a `@php` block resolves through the same
/// region walk, with the template's `@use` seeding the alias map.
#[test]
fn cursor_on_a_class_in_a_php_block_resolves_via_the_blade_import() {
    let src = "@use('App\\Models\\Flight')\n@php\n    $n = Flight::count();\n@endphp\n";
    let cursor = src.rfind("Flight").expect("offset");

    let (fqcn, span) =
        class_at_cursor_for(Path::new("/p/resources/views/x.blade.php"), src, cursor)
            .expect("a short name bound by @use is a class reference");

    assert_eq!(fqcn, r"App\Models\Flight");
    assert_eq!(&src[span.0..span.1], "Flight");
}
