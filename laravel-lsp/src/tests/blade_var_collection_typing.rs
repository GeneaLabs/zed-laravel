//! Blade-variable query-builder inference honors a model's custom
//! `$collectionClass` (issue #271).
//!
//! `find_variable_type_in_content` types a variable assigned from a
//! query-builder terminal (`Model::all()`, `Model::where(...)->get()`, …).
//! Before #271 it hardcoded `Collection<Model>`; now it resolves the model's
//! file and swaps in the model's custom collection when one is declared —
//! matching the relationship-completion path (#30 item 4). These tests drive
//! the associated function directly with a temp project root.

use crate::LaravelLanguageServer;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Temp project with `app/Models/{name}.php` = `body`. No composer.json —
/// `collection_class_for` resolves the model file via the FQCN heuristic
/// (`App\Models\User` → `app/Models/User.php`) / basename-walk fallback,
/// mirroring the `project_with_model` helper the eloquent-completion tests use.
fn project_with_model(name: &str, body: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let models = dir.path().join("app/Models");
    fs::create_dir_all(&models).expect("create models dir");
    fs::write(models.join(format!("{name}.php")), body).expect("write model");
    let root = dir.path().to_path_buf();
    (dir, root)
}

/// A User model that hydrates into a custom `UserCollection`.
const USER_WITH_CUSTOM_COLLECTION: &str = r#"<?php
namespace App\Models;

use App\Support\UserCollection;
use Illuminate\Database\Eloquent\Model;

class User extends Model
{
    protected $collectionClass = UserCollection::class;
}
"#;

/// A plain User model — no `$collectionClass`, so the default applies.
const USER_PLAIN: &str = r#"<?php
namespace App\Models;

use Illuminate\Database\Eloquent\Model;

class User extends Model
{
}
"#;

/// A controller assigning `$users` from `User::all()`.
fn controller_with(assignment: &str) -> String {
    format!(
        r#"<?php
namespace App\Http\Controllers;

use App\Models\User;

class UserController
{{
    public function index()
    {{
        {assignment}
        return view('users.index', compact('users'));
    }}
}}
"#
    )
}

#[test]
fn all_terminal_uses_models_custom_collection() {
    // Pattern 6: `$users = User::all()` on a model with `$collectionClass`
    // must surface the custom collection, not the default `Collection`.
    let (_dir, root) = project_with_model("User", USER_WITH_CUSTOM_COLLECTION);
    let content = controller_with("$users = User::all();");
    assert_eq!(
        LaravelLanguageServer::find_variable_type_in_content(&content, "users", &root),
        Some("UserCollection<User>".to_string()),
    );
}

#[test]
fn query_chain_get_terminal_uses_models_custom_collection() {
    // Pattern 8: `$users = User::where(...)->get()` — same custom-collection
    // resolution as the bare static terminal.
    let (_dir, root) = project_with_model("User", USER_WITH_CUSTOM_COLLECTION);
    let content = controller_with("$users = User::where('active', 1)->get();");
    assert_eq!(
        LaravelLanguageServer::find_variable_type_in_content(&content, "users", &root),
        Some("UserCollection<User>".to_string()),
    );
}

#[test]
fn absence_of_override_keeps_default_collection() {
    // AC #3: a model that declares no `$collectionClass` still types as the
    // default `Collection<User>` — the override is additive, not a rename.
    let (_dir, root) = project_with_model("User", USER_PLAIN);
    let content = controller_with("$users = User::all();");
    assert_eq!(
        LaravelLanguageServer::find_variable_type_in_content(&content, "users", &root),
        Some("Collection<User>".to_string()),
    );
}

#[test]
fn single_model_terminal_unaffected_by_root_threading() {
    // AC #5: threading the project root must not change the other inference
    // patterns. A single-model terminal (`::find`) still types as the bare
    // model, custom collection or not.
    let (_dir, root) = project_with_model("User", USER_WITH_CUSTOM_COLLECTION);
    let content = controller_with("$user = User::find(1);");
    assert_eq!(
        LaravelLanguageServer::find_variable_type_in_content(&content, "user", &root),
        Some("User".to_string()),
    );
}
