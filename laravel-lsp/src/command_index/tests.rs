//! Unit tests for the project-wide Artisan command index.

use super::*;
use std::path::Path;

/// A minimal Artisan command class declaring `signature`.
fn command_class(class: &str, signature: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Console\\Commands;\n\nuse Illuminate\\Console\\Command;\n\nclass {class} extends Command\n{{\n    protected $signature = '{signature}';\n\n    public function handle()\n    {{\n        //\n    }}\n}}\n"
    )
}

#[test]
fn class_name_extracted_from_declaration() {
    assert_eq!(
        class_name_from_content("<?php\nclass SendEmails extends Command {}").as_deref(),
        Some("SendEmails")
    );
    assert_eq!(class_name_from_content("<?php\n$x = 1;").as_deref(), None);
}

#[test]
fn priority_classifies_by_path() {
    assert_eq!(
        classify_priority(Path::new("/app/app/Console/Commands/SendEmails.php")),
        CommandPriority::App
    );
    assert_eq!(
        classify_priority(Path::new(
            "/app/vendor/spatie/backup/src/Commands/Backup.php"
        )),
        CommandPriority::Package
    );
    assert_eq!(
        classify_priority(Path::new(
            "/app/vendor/laravel/framework/src/Illuminate/Queue/Console/WorkCommand.php"
        )),
        CommandPriority::Framework
    );
}

#[test]
fn indexes_a_command_and_resolves_it() {
    let mut index = CommandIndex::default();
    let src = command_class("SendEmails", "emails:send {user} {--force}");
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/SendEmails.php"),
        &src,
    );

    let entry = index
        .resolve("emails:send")
        .expect("command should resolve");
    assert_eq!(entry.name, "emails:send");
    assert_eq!(entry.class_name, "SendEmails");
    assert_eq!(entry.raw_signature, "emails:send {user} {--force}");
    assert_eq!(entry.priority, CommandPriority::App);
    assert_eq!(index.len(), 1);
}

#[test]
fn non_command_files_are_ignored() {
    let mut index = CommandIndex::default();
    index_command_file(
        &mut index,
        Path::new("/app/app/Models/User.php"),
        "<?php\nclass User extends Model {\n    protected $table = 'users';\n}",
    );
    assert!(index.is_empty());
}

#[test]
fn command_without_signature_is_ignored() {
    let mut index = CommandIndex::default();
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/Dynamic.php"),
        "<?php\nclass Dynamic extends Command {\n    public function handle() {}\n}",
    );
    assert!(index.is_empty());
}

#[test]
fn app_command_overrides_package_with_same_name() {
    let mut index = CommandIndex::default();
    // Package declares queue:work first…
    index_command_file(
        &mut index,
        Path::new("/app/vendor/laravel/horizon/src/Console/WorkCommand.php"),
        &command_class("PackageWork", "queue:work"),
    );
    // …then the app overrides it.
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/WorkCommand.php"),
        &command_class("AppWork", "queue:work"),
    );

    let entry = index.resolve("queue:work").expect("should resolve");
    assert_eq!(entry.class_name, "AppWork");
    assert_eq!(entry.priority, CommandPriority::App);
}

#[test]
fn lower_priority_does_not_clobber_higher() {
    let mut index = CommandIndex::default();
    // App declared first…
    index_command_file(
        &mut index,
        Path::new("/app/app/Console/Commands/WorkCommand.php"),
        &command_class("AppWork", "queue:work"),
    );
    // …a later package declaration must NOT replace it.
    index_command_file(
        &mut index,
        Path::new("/app/vendor/laravel/framework/src/WorkCommand.php"),
        &command_class("FrameworkWork", "queue:work"),
    );

    let entry = index.resolve("queue:work").expect("should resolve");
    assert_eq!(entry.class_name, "AppWork");
    assert_eq!(entry.priority, CommandPriority::App);
}

#[test]
fn build_index_walks_project_and_vendor() {
    let dir = std::env::temp_dir().join(format!("cmd-index-test-{}", std::process::id()));
    let app_cmds = dir.join("app/Console/Commands");
    let vendor_cmds = dir.join("vendor/acme/pkg/src/Commands");
    std::fs::create_dir_all(&app_cmds).unwrap();
    std::fs::create_dir_all(&vendor_cmds).unwrap();
    std::fs::write(
        app_cmds.join("SendEmails.php"),
        command_class("SendEmails", "emails:send"),
    )
    .unwrap();
    std::fs::write(
        vendor_cmds.join("Backup.php"),
        command_class("Backup", "backup:run"),
    )
    .unwrap();
    // A non-command file should be skipped.
    std::fs::write(
        app_cmds.join("NotACommand.php"),
        "<?php\nclass NotACommand {}",
    )
    .unwrap();

    let index = build_command_index(&dir);

    assert_eq!(index.len(), 2);
    assert_eq!(
        index.resolve("emails:send").unwrap().priority,
        CommandPriority::App
    );
    assert_eq!(
        index.resolve("backup:run").unwrap().priority,
        CommandPriority::Package
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Shared vendor walk equivalence (issue #371)
// ---------------------------------------------------------------------------

/// The command index as it was built before the shared vendor walk: ONE
/// `WalkDir` over `<root>`, pruning [`SKIP_DIRS`], reading every `*.php`.
///
/// Kept here as an executable oracle rather than as a comment. The refactor's
/// whole claim is that splitting this into a project leg plus a shared vendor
/// leg changes nothing, and the only way to test a claim about the old
/// implementation is to keep the old implementation.
fn build_command_index_single_walk(root: &Path) -> CommandIndex {
    let mut index = CommandIndex::default();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "php") {
            if let Ok(content) = std::fs::read_to_string(path) {
                index_command_file(&mut index, path, &content);
            }
        }
    }
    index
}

/// Compare two indexes by everything a consumer can observe.
fn as_pairs(index: &CommandIndex) -> Vec<(String, PathBuf, CommandPriority)> {
    let mut v: Vec<_> = index
        .entries()
        .map(|e| (e.name.clone(), e.file.clone(), e.priority))
        .collect();
    v.sort();
    v
}

/// A project holding the collisions and exclusions the split could break:
/// an app command shadowing a package one, two packages colliding at the same
/// tier, a framework command, a command inside a pruned `vendor/**/public/`
/// subtree, one in a NESTED `app/vendor/` (walked before, not in the shared
/// index), and one very deep in vendor.
fn seed_command_project(root: &Path) {
    let write = |rel: &str, class: &str, sig: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, command_class(class, sig)).unwrap();
    };
    write("app/Console/Commands/Send.php", "AppSend", "mail:send");
    write("app/vendor/Nested/Deep.php", "NestedCmd", "nested:run");
    write("vendor/acme/pkg/src/Send.php", "PkgSend", "mail:send");
    write("vendor/acme/pkg/src/Only.php", "PkgOnly", "pkg:only");
    write("vendor/beta/pkg/src/Only.php", "BetaOnly", "pkg:only");
    write(
        "vendor/laravel/framework/src/Illuminate/Foundation/Console/Work.php",
        "WorkCmd",
        "queue:work",
    );
    write(
        "vendor/acme/pkg/public/Hidden.php",
        "HiddenCmd",
        "hidden:cmd",
    );
    write(
        "vendor/acme/pkg/a/b/c/d/e/f/g/Deep.php",
        "DeepCmd",
        "deep:cmd",
    );
}

#[test]
fn shared_vendor_walk_builds_the_same_index_as_the_single_walk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_command_project(root);

    let shared = build_command_index_with_vendor(root, &VendorIndex::build(root));
    let oracle = build_command_index_single_walk(root);

    assert_eq!(
        as_pairs(&shared),
        as_pairs(&oracle),
        "splitting the walk must not change a single resolved command"
    );

    // Fixture checks — the equality above is worthless if the project is too
    // bland to exercise the rules the split could break.
    assert_eq!(
        shared.resolve("mail:send").map(|e| e.priority),
        Some(CommandPriority::App),
        "an app command must still beat the package command of the same name, \
         even though they are now found in two separate legs"
    );
    assert!(
        shared.resolve("hidden:cmd").is_none(),
        "a command under a pruned vendor/**/public/ subtree stays excluded"
    );
    assert!(
        shared.resolve("nested:run").is_some(),
        "a NESTED app/vendor/ is not the shared index's vendor root — it must \
         still be walked by the project leg"
    );
    assert!(
        shared.resolve("deep:cmd").is_some(),
        "the command leg has no depth budget"
    );
}

#[test]
fn shared_vendor_walk_keeps_the_same_winner_on_a_same_tier_collision() {
    // `insert_entry` keeps the FIRST file walked when two declarations share a
    // name AND a tier, so the vendor leg has to preserve walk order. Two
    // packages both declaring `pkg:only` is that case.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_command_project(root);

    let shared = build_command_index_with_vendor(root, &VendorIndex::build(root));
    let oracle = build_command_index_single_walk(root);

    let winner = |i: &CommandIndex| i.resolve("pkg:only").map(|e| e.file.clone());
    assert_eq!(
        winner(&shared),
        winner(&oracle),
        "the same-tier collision must resolve to the same file as before"
    );
    assert_eq!(
        shared.resolve("pkg:only").map(|e| e.priority),
        Some(CommandPriority::Package),
        "fixture check — both colliding declarations must really be same-tier, \
         or this test proves nothing about order"
    );
}
