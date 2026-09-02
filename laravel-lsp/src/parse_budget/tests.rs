//! Tests for the shared parse budget (issue #371).

use super::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn json_php_is_recognised_by_its_full_suffix() {
    assert!(is_json_php(Path::new(
        "/p/vendor/aws/data/endpoints.json.php"
    )));
    assert!(
        !is_json_php(Path::new("/p/app/Models/User.php")),
        "an ordinary PHP file is not a data blob"
    );
    assert!(
        !is_json_php(Path::new("/p/data/endpoints.json")),
        "a bare .json file is not PHP and never reaches the PHP parser"
    );
    assert!(
        !is_json_php(Path::new("/p/app/json.php")),
        "the suffix is `.json.php`, not the filename `json.php` — this file is \
         ordinary PHP source"
    );
}

#[test]
fn json_php_ignores_directory_names() {
    // A filename-suffix test must not be satisfied by a DIRECTORY called
    // `x.json.php`, which would exclude every real source file beneath it.
    assert!(
        !is_json_php(Path::new("/p/weird.json.php/Model.php")),
        "only the file's own name decides"
    );
}

#[test]
fn the_size_cap_binds_at_the_documented_boundary() {
    let p = Path::new("/p/app/Big.php");
    assert_eq!(
        skip_reason(p, MAX_PARSED_FILE_SIZE_BYTES),
        None,
        "a file exactly at the cap is still parsed — the rule is `>`, not `>=`"
    );
    assert_eq!(
        skip_reason(p, MAX_PARSED_FILE_SIZE_BYTES + 1),
        Some(SkipReason::TooLarge(MAX_PARSED_FILE_SIZE_BYTES + 1)),
        "one byte over is excluded"
    );
    assert_eq!(skip_reason(p, 0), None, "an empty file is parsed");
}

#[test]
fn the_cap_is_the_documented_256_kb() {
    // The value is load-bearing: it was tuned from 4 MB down to this, and the
    // module docs quote the resulting numbers. A silent change would make that
    // prose false.
    assert_eq!(MAX_PARSED_FILE_SIZE_BYTES, 256 * 1024);
}

#[test]
fn a_small_json_php_is_still_excluded() {
    // The name test must run first. A tiny `.json.php` is excluded for what it
    // is, not for how big it is — tree-sitter's trouble is the nesting, and a
    // size-only rule would let small ones through.
    assert_eq!(
        skip_reason(Path::new("/p/data/tiny.json.php"), 10),
        Some(SkipReason::JsonPhp),
        "a 10-byte .json.php is still a data blob"
    );
}

#[test]
fn on_disk_sizing_matches_the_in_memory_rule() {
    let tmp = TempDir::new().unwrap();
    let small = tmp.path().join("Small.php");
    fs::write(&small, "<?php\n").unwrap();
    let big = tmp.path().join("Big.php");
    fs::write(&big, vec![b'x'; (MAX_PARSED_FILE_SIZE_BYTES + 1) as usize]).unwrap();
    let data: PathBuf = tmp.path().join("blob.json.php");
    fs::write(&data, "<?php return [];").unwrap();

    assert_eq!(skip_reason_on_disk(&small), None);
    assert_eq!(
        skip_reason_on_disk(&big),
        Some(SkipReason::TooLarge(MAX_PARSED_FILE_SIZE_BYTES + 1))
    );
    assert_eq!(skip_reason_on_disk(&data), Some(SkipReason::JsonPhp));
}

#[test]
fn an_unstattable_path_reports_no_exclusion() {
    // Fail-open is correct HERE and only here: the caller owns existence
    // handling, and reporting "excluded" for a transient stat failure would let
    // a live file be silently withdrawn from an index it belongs in.
    let tmp = TempDir::new().unwrap();
    assert_eq!(skip_reason_on_disk(&tmp.path().join("gone.php")), None);
}

#[test]
fn an_unstattable_json_php_is_still_excluded() {
    // The name rule needs no stat, so it must survive one failing. Otherwise a
    // deleted-then-recreated data blob could slip past the exclusion on a race.
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        skip_reason_on_disk(&tmp.path().join("gone.json.php")),
        Some(SkipReason::JsonPhp)
    );
}

#[test]
fn describe_reports_the_measured_size() {
    // The log line is the only place an operator learns WHY a file was skipped.
    assert!(SkipReason::TooLarge(300_000).describe().contains("300000"));
    assert!(SkipReason::TooLarge(300_000)
        .describe()
        .contains(&MAX_PARSED_FILE_SIZE_BYTES.to_string()));
    assert!(SkipReason::JsonPhp.describe().contains("JSON"));
}
