use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ComponentUsageIndex;
use crate::salsa_impl::{ComponentReferenceData, LivewireReferenceData, ParsedPatternsData};

/// Patterns for a Blade file rendering `components` as `<x-…>` tags and
/// `livewire` as `<livewire:…>` tags. Positions are irrelevant here — the
/// index keys on names only — so they are all zero.
fn patterns(components: &[&str], livewire: &[&str]) -> ParsedPatternsData {
    let mut data = ParsedPatternsData::default();
    data.components = components
        .iter()
        .map(|name| {
            Arc::new(ComponentReferenceData {
                name: (*name).to_string(),
                tag_name: format!("x-{name}"),
                line: 0,
                column: 0,
                end_column: 0,
            })
        })
        .collect();
    data.livewire_refs = livewire
        .iter()
        .map(|name| {
            Arc::new(LivewireReferenceData {
                name: (*name).to_string(),
                line: 0,
                column: 0,
                end_column: 0,
            })
        })
        .collect();
    data
}

fn blade(name: &str) -> PathBuf {
    PathBuf::from(format!("/app/resources/views/{name}.blade.php"))
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| (*n).to_string()).collect()
}

#[test]
fn finds_the_files_rendering_a_component_tag() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&["save-button"], &[]));
    index.insert_file(&blade("settings"), &patterns(&["other"], &[]));

    assert_eq!(
        index.find(&names(&["save-button"]), &[]),
        vec![blade("dashboard")]
    );
}

/// `<livewire:counter />` is the other half of the usage graph — the AC names
/// both syntaxes, and a lookup that only consults `by_component` would answer
/// this one with silence.
#[test]
fn finds_the_files_rendering_a_livewire_tag() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&[], &["counter"]));

    assert_eq!(
        index.find(&[], &names(&["counter"])),
        vec![blade("dashboard")]
    );
    // …and the same name looked up on the component surface finds nothing,
    // so the two maps are genuinely separate rather than one merged bucket.
    assert!(index.find(&names(&["counter"]), &[]).is_empty());
}

#[test]
fn results_are_sorted_so_the_first_match_is_stable() {
    let mut index = ComponentUsageIndex::default();
    // Inserted in reverse lexicographic order — a map-iteration answer would
    // hand these back in whatever order the hasher chose.
    for view in ["zeta", "middle", "alpha"] {
        index.insert_file(&blade(view), &patterns(&["icon"], &[]));
    }

    assert_eq!(
        index.find(&names(&["icon"]), &[]),
        vec![blade("alpha"), blade("middle"), blade("zeta")]
    );
}

/// Invariant 2: re-folding a file replaces its entries rather than adding to
/// them. Without the `remove_file` at the top of `insert_file`, `dashboard`
/// would still answer for `save-button` after the tag was deleted from it.
#[test]
fn reindexing_withdraws_the_tags_a_file_no_longer_renders() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(
        &blade("dashboard"),
        &patterns(&["save-button"], &["counter"]),
    );
    index.insert_file(&blade("dashboard"), &patterns(&["icon"], &[]));

    assert!(index.find(&names(&["save-button"]), &[]).is_empty());
    assert!(index.find(&[], &names(&["counter"])).is_empty());
    assert_eq!(index.find(&names(&["icon"]), &[]), vec![blade("dashboard")]);
    assert_eq!(index.indexed_file_count(), 1);
}

#[test]
fn removing_a_file_withdraws_only_its_own_entries() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&["icon"], &[]));
    index.insert_file(&blade("settings"), &patterns(&["icon"], &[]));

    index.remove_file(&blade("dashboard"));

    assert_eq!(index.find(&names(&["icon"]), &[]), vec![blade("settings")]);
    assert_eq!(index.indexed_file_count(), 1);
}

#[test]
fn removing_an_unknown_file_is_a_no_op() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&["icon"], &[]));

    index.remove_file(&blade("never-indexed"));

    assert_eq!(index.find(&names(&["icon"]), &[]), vec![blade("dashboard")]);
    assert_eq!(index.indexed_file_count(), 1);
}

/// A file that renders nothing is still recorded, so a later `remove_file`
/// has an entry to withdraw and the drain doesn't re-queue it forever.
#[test]
fn a_file_rendering_nothing_is_still_recorded() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("plain"), &patterns(&[], &[]));

    assert_eq!(index.indexed_file_count(), 1);
}

#[test]
fn unknown_names_find_nothing() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&["icon"], &[]));

    assert!(index
        .find(&names(&["absent"]), &names(&["absent"]))
        .is_empty());
}

#[test]
fn only_blade_paths_are_queued() {
    let mut index = ComponentUsageIndex::default();
    index.mark_dirty(&blade("dashboard"));
    index.mark_dirty(Path::new("/app/app/Livewire/Counter.php"));

    assert_eq!(index.take_pending(), vec![blade("dashboard")]);
}

#[test]
fn draining_the_queue_empties_it() {
    let mut index = ComponentUsageIndex::default();
    index.mark_dirty(&blade("dashboard"));

    assert_eq!(index.take_pending().len(), 1);
    assert!(index.take_pending().is_empty());
}

#[test]
fn clear_drops_entries_and_the_queue() {
    let mut index = ComponentUsageIndex::default();
    index.insert_file(&blade("dashboard"), &patterns(&["icon"], &[]));
    index.mark_dirty(&blade("settings"));

    index.clear();

    assert!(index.find(&names(&["icon"]), &[]).is_empty());
    assert!(index.take_pending().is_empty());
    assert_eq!(index.indexed_file_count(), 0);
}
