//! Tests for the reindex command surface: the declared capability, the
//! global code action (file-type gate + `only`-filter semantics), and the
//! concurrency guard. Each test targets a seam a regression would actually
//! break: dropping the command from the capability, losing the action from
//! the `cmd-.` menu, mis-matching hierarchical kinds, or letting two
//! indexing passes run concurrently.

use super::*;

/// Unwrap the single expected reindex action or panic with context.
fn only_action(actions: Vec<CodeActionOrCommand>) -> CodeAction {
    assert_eq!(actions.len(), 1, "expected exactly one global action");
    match actions.into_iter().next().unwrap() {
        CodeActionOrCommand::CodeAction(action) => action,
        CodeActionOrCommand::Command(_) => panic!("expected a CodeAction, got a bare Command"),
    }
}

/// The other tests here compare against the constant, so they'd stay green
/// through any relabelling. This one pins the literal: the "Laravel CE:"
/// prefix is what distinguishes our entry in a code-actions menu that may
/// also carry Laravel's official extension's actions.
#[test]
fn reindex_action_label_carries_the_short_brand_prefix() {
    assert_eq!(REINDEX_ACTION_TITLE, "Laravel CE: Reindex project");
}

#[test]
fn capability_declares_the_reindex_command() {
    let options = execute_command_options();
    assert_eq!(
        options.commands,
        vec![REINDEX_COMMAND.to_string()],
        "server capability must declare exactly the reindex command"
    );
}

#[test]
fn php_file_gets_the_reindex_action() {
    let action = only_action(global_code_actions("/app/Models/User.php", None));
    assert_eq!(action.title, REINDEX_ACTION_TITLE);
    assert_eq!(action.kind, Some(REINDEX_ACTION_KIND));
    let command = action.command.expect("action must carry the command");
    assert_eq!(command.command, REINDEX_COMMAND);
    assert!(
        action.edit.is_none(),
        "the action's effect is the command round-trip, never an edit"
    );
}

#[test]
fn blade_file_gets_the_reindex_action() {
    let action = only_action(global_code_actions(
        "/resources/views/welcome.blade.php",
        None,
    ));
    assert_eq!(action.title, REINDEX_ACTION_TITLE);
}

#[test]
fn non_php_files_get_nothing() {
    for path in [
        "/project/.env",
        "/project/phpunit.xml",
        "/resources/js/app.js",
    ] {
        assert!(
            global_code_actions(path, None).is_empty(),
            "{path} must not get the reindex action"
        );
    }
}

#[test]
fn only_filter_none_admits_the_action() {
    // Zed's cmd-. menu sends no `only` filter — this is the case that must
    // never break, or the action disappears from the menu entirely.
    assert!(!global_code_actions("/app/User.php", None).is_empty());
}

#[test]
fn only_filter_source_prefix_admits_the_action() {
    // Hierarchical matching: the parent kind `source` admits our sub-kind.
    for kinds in [
        vec![CodeActionKind::SOURCE],
        vec![REINDEX_ACTION_KIND],
        vec![CodeActionKind::EMPTY], // empty kind matches everything
        vec![CodeActionKind::QUICKFIX, CodeActionKind::SOURCE], // any match wins
    ] {
        assert!(
            !global_code_actions("/app/User.php", Some(&kinds)).is_empty(),
            "only={kinds:?} must admit the reindex action"
        );
    }
}

#[test]
fn only_filter_without_source_hides_the_action() {
    for kinds in [
        vec![CodeActionKind::QUICKFIX],
        vec![CodeActionKind::SOURCE_FIX_ALL], // sibling sub-kind, not a parent
        vec![CodeActionKind::new("sourcery")], // prefix of the string, not of the segments
    ] {
        assert!(
            global_code_actions("/app/User.php", Some(&kinds)).is_empty(),
            "only={kinds:?} must hide the reindex action"
        );
    }
}

#[test]
fn flight_guard_admits_one_pass_at_a_time() {
    let flag = Arc::new(AtomicBool::new(false));

    let first = IndexingFlightGuard::try_acquire(flag.clone());
    assert!(first.is_some(), "first acquire must succeed");
    assert!(flag.load(Ordering::SeqCst), "flag is raised while held");

    // A concurrent trigger while the first pass runs must no-op.
    assert!(
        IndexingFlightGuard::try_acquire(flag.clone()).is_none(),
        "second acquire while the first is held must fail"
    );

    drop(first);
    assert!(!flag.load(Ordering::SeqCst), "drop must release the flag");
    assert!(
        IndexingFlightGuard::try_acquire(flag).is_some(),
        "after release the slot is free again"
    );
}

#[test]
fn flight_guard_releases_on_panic() {
    // The Drop-based release is what keeps a crashed warming task from
    // bricking reindex for the rest of the session — prove it survives
    // an unwind, not just a clean drop.
    let flag = Arc::new(AtomicBool::new(false));
    let flag_for_panic = flag.clone();
    let result = std::panic::catch_unwind(move || {
        let _guard = IndexingFlightGuard::try_acquire(flag_for_panic).unwrap();
        panic!("warming task died");
    });
    assert!(result.is_err(), "the closure must actually panic");
    assert!(
        IndexingFlightGuard::try_acquire(flag).is_some(),
        "the flag must be released by the unwinding drop"
    );
}
