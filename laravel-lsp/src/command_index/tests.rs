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

// ---------------------------------------------------------------------------
// Per-file mtime skip (issue #371)
// ---------------------------------------------------------------------------
//
// `build_command_index` read and regex-scanned every `*.php` file in the
// project AND `vendor/` on every startup, and on every watched change under a
// `Commands/` directory — 16,202 files on this repo's `test-project/`. The disk
// cache existed but only accelerated the cold-start display: the full walk ran
// afterwards unconditionally, because a cache holding only DECLARATIONS cannot
// say "these 16,000 other files still declare nothing".
//
// The observable for "was this file read?" is a file whose on-disk contents no
// longer match the cached verdict. If the scan reads it, the new contents win;
// if it trusts the cache, the old verdict survives. That discriminates a real
// skip from a scan that merely produces the right answer by re-reading.

use crate::command_disk_cache::{load_scan, save_scan, CommandScanCache};
use crate::vendor_index::VendorIndex;

/// Scan `root` twice: once cold, then once with the first scan's verdicts.
fn scan_twice(root: &Path) -> (CommandScan, CommandScanCache) {
    let vendor = VendorIndex::build(root);
    let first = scan_commands(root, &vendor, None);
    save_scan(&first.files, root).unwrap();
    let cache = load_scan(root).expect("the scan we just saved must load");
    (first, cache)
}

/// The file's mtime in the `(secs, nanos)` form the cache stores.
fn mtime_of(path: &Path) -> (u64, u32) {
    let d = std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    (d.as_secs(), d.subsec_nanos())
}

#[test]
fn an_unchanged_file_is_not_reread() {
    // Discriminator: the cache is given a verdict that DISAGREES with the
    // file's contents, stamped with the file's real current mtime. A scan that
    // re-reads reports what is on disk; one that honours the cache reports the
    // planted verdict. Built through the public save/load API rather than by
    // rewinding the clock, so it needs no extra dependency.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    let cmd = root.join("app/Console/Commands/Send.php");
    std::fs::write(&cmd, command_class("Send", "mail:ondisk")).unwrap();

    let (secs, nanos) = mtime_of(&cmd);
    let planted = crate::command_disk_cache::ScannedFile {
        mtime_secs: secs,
        mtime_nanos: nanos,
        path: cmd.clone(),
        entry: Some(CommandEntry {
            name: "mail:cached".to_string(),
            class_name: "Send".to_string(),
            raw_signature: "mail:cached".to_string(),
            file: cmd.clone(),
            line: 0,
            start_column: 0,
            end_column: 0,
            priority: CommandPriority::App,
        }),
    };
    save_scan(&[planted], root).unwrap();
    let cache = load_scan(root).expect("the planted scan must load");

    let vendor = VendorIndex::build(root);
    let scan = scan_commands(root, &vendor, Some(&cache));

    assert!(
        scan.index.resolve("mail:cached").is_some(),
        "an unchanged mtime must be answered from the cache"
    );
    assert!(
        scan.index.resolve("mail:ondisk").is_none(),
        "the file was never read, so its on-disk signature cannot have been seen          — if this resolves, the scan is still reading every file"
    );
}

#[test]
fn a_changed_file_is_reread() {
    // The other half: the skip must not survive a real edit, or the index goes
    // permanently stale.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    let cmd = root.join("app/Console/Commands/Send.php");
    std::fs::write(&cmd, command_class("Send", "mail:send")).unwrap();

    let (_, cache) = scan_twice(root);

    // A genuine edit: contents AND mtime move.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&cmd, command_class("Send", "mail:changed")).unwrap();

    let vendor = VendorIndex::build(root);
    let second = scan_commands(root, &vendor, Some(&cache));

    assert!(
        second.index.resolve("mail:changed").is_some(),
        "a changed mtime must force a re-read"
    );
    assert!(second.index.resolve("mail:send").is_none());
}

#[test]
fn a_new_file_is_read_even_though_the_cache_is_fresh() {
    // A cache of declarations could never authorise skipping a file it had
    // never seen. Neither may this one.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    std::fs::write(
        root.join("app/Console/Commands/Send.php"),
        command_class("Send", "mail:send"),
    )
    .unwrap();

    let (_, cache) = scan_twice(root);

    std::fs::write(
        root.join("app/Console/Commands/Prune.php"),
        command_class("Prune", "db:prune"),
    )
    .unwrap();

    let vendor = VendorIndex::build(root);
    let second = scan_commands(root, &vendor, Some(&cache));

    assert!(
        second.index.resolve("db:prune").is_some(),
        "a file with no cached verdict must be read"
    );
    assert!(
        second.index.resolve("mail:send").is_some(),
        "and the cached one is still served"
    );
}

#[test]
fn a_deleted_file_leaves_the_index() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    let cmd = root.join("app/Console/Commands/Send.php");
    std::fs::write(&cmd, command_class("Send", "mail:send")).unwrap();

    let (_, cache) = scan_twice(root);
    std::fs::remove_file(&cmd).unwrap();

    let vendor = VendorIndex::build(root);
    let second = scan_commands(root, &vendor, Some(&cache));

    assert!(
        second.index.resolve("mail:send").is_none(),
        "a deleted file is never walked, so its cached verdict is never replayed"
    );
}

#[test]
fn the_scan_records_a_verdict_for_every_file_including_non_commands() {
    // The negative verdicts ARE the feature. Recording only declarations is
    // what made the old cache unable to skip anything.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    std::fs::write(
        root.join("app/Console/Commands/Send.php"),
        command_class("Send", "mail:send"),
    )
    .unwrap();
    std::fs::write(root.join("app/Plain.php"), "<?php\nclass Plain {}\n").unwrap();

    let vendor = VendorIndex::build(root);
    let scan = scan_commands(root, &vendor, None);

    assert_eq!(
        scan.files.len(),
        2,
        "both files get a verdict: {:?}",
        scan.files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>()
    );
    let plain = scan
        .files
        .iter()
        .find(|f| f.path.ends_with("Plain.php"))
        .expect("the non-command file must be recorded");
    assert!(
        plain.entry.is_none(),
        "its verdict is `declares nothing` — a real answer, not an absence"
    );
}

#[test]
fn an_unusable_cache_degrades_to_a_full_scan() {
    // Missing, stale or schema-mismatched: every one must fall back to reading,
    // never to a wrong index.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/Console/Commands")).unwrap();
    std::fs::write(
        root.join("app/Console/Commands/Send.php"),
        command_class("Send", "mail:send"),
    )
    .unwrap();

    let vendor = VendorIndex::build(root);
    assert!(
        scan_commands(root, &vendor, None)
            .index
            .resolve("mail:send")
            .is_some(),
        "no cache at all must still find the command"
    );
    assert!(
        load_scan(root).is_none(),
        "fixture check — nothing has been saved for this project yet"
    );
}

// === the mtime hand-off between try_cached and record_source (issue #373) ===
//
// `record_source` used to stat the file itself, so every file that needed a
// read was stat'd twice: once by `try_cached` deciding it needed reading, and
// again by `record_source` stamping the entry. That second stat was 225 ms of
// the shared vendor pass's 962 ms on a Windows CI runner, and roughly a fifth
// of it on Linux and macOS. It is now passed across instead.

/// The value `record_source` stamps must be the one it is HANDED. Fabricating
/// an mtime the file cannot possibly have is what makes this discriminate: a
/// `record_source` that stats again would record the real one and go red.
#[test]
fn record_source_stamps_the_mtime_it_is_given_rather_than_statting_again() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("MakeThing.php");
    let src = command_class("MakeThing", "make:thing");
    std::fs::write(&path, &src).unwrap();

    // Far in the past, and a nanosecond value a real filesystem stamp would
    // not land on by chance.
    let fabricated = (1_234_567_890u64, 424_242u32);
    let real = file_mtime(&path).expect("the fixture is stattable");
    assert_ne!(
        real, fabricated,
        "fixture check — the fabricated mtime must differ from the real one"
    );

    let mut scan = CommandScan::default();
    record_source(&mut scan, &path, fabricated, &src);

    let recorded = scan
        .files
        .iter()
        .find(|f| f.path == path)
        .expect("the file is recorded");
    assert_eq!(
        (recorded.mtime_secs, recorded.mtime_nanos),
        fabricated,
        "record_source stat'd the file instead of using the mtime it was given"
    );
}

/// The other half: the mtime `try_cached` hands out has to be the file's real
/// one, or every entry would be stamped with a value the next scan can never
/// match and the disk cache would never hit.
#[test]
fn try_cached_hands_back_the_real_mtime_when_a_read_is_needed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("MakeThing.php");
    std::fs::write(&path, command_class("MakeThing", "make:thing")).unwrap();

    let mut scan = CommandScan::default();
    let verdict = try_cached(&mut scan, None, &path);

    let CacheVerdict::NeedsSource { mtime } = verdict else {
        panic!("no cache was passed, so the file must need its source read");
    };
    assert_eq!(mtime, file_mtime(&path).expect("the fixture is stattable"));
}

/// An unstattable path is settled, not read. A verdict recorded without a
/// usable mtime could never be validated, so it would be trusted forever.
#[test]
fn try_cached_settles_a_path_it_cannot_stat() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("does/not/exist/Ghost.php");

    let mut scan = CommandScan::default();
    assert_eq!(try_cached(&mut scan, None, &missing), CacheVerdict::Settled);
    assert!(
        scan.files.is_empty(),
        "an unstattable path must leave no verdict behind"
    );
}
