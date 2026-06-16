use super::*;
use std::collections::HashSet;
use std::path::PathBuf;

/// Byte offset of the `nth` (0-based) occurrence of `needle`, nudged one byte
/// in so the cursor lands *inside* the `$name` token (on the identifier).
fn cursor_byte(source: &str, needle: &str, nth: usize) -> usize {
    let pos = source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found"))
        .0;
    pos + 1
}

/// Absolute byte offset of a 0-based `(line, column)` position. Uses
/// `split_inclusive` so newlines are counted exactly.
fn abs_byte(source: &str, line: u32, col: u32) -> usize {
    let mut offset = 0usize;
    for (i, l) in source.split_inclusive('\n').enumerate() {
        if i as u32 == line {
            return offset + col as usize;
        }
        offset += l.len();
    }
    offset + col as usize
}

/// The source text an edit target rewrites — used to assert every edit lands
/// on a `$name` token, never a stray slice.
fn target_text(source: &str, t: &EditTarget) -> String {
    let line = source.split_inclusive('\n').nth(t.line as usize).unwrap();
    line[t.start_column as usize..t.end_column as usize].to_string()
}

/// The set of absolute byte offsets the targets rewrite.
fn edited_offsets(source: &str, targets: &[EditTarget]) -> HashSet<usize> {
    targets
        .iter()
        .map(|t| abs_byte(source, t.line, t.start_column))
        .collect()
}

fn rename(source: &str, needle: &str, nth: usize, new_name: &str) -> Vec<EditTarget> {
    variable_rename_targets(
        source,
        &PathBuf::from("test.php"),
        cursor_byte(source, needle, nth),
        new_name,
    )
    .expect("rename should not error")
}

// ── Simple function-local rename ──────────────────────────────────────────

#[test]
fn renames_every_in_scope_occurrence() {
    let src = "\
<?php
function greet($user) {
    $user = trim($user);
    return \"Hello \" . $user;
}
";
    let targets = rename(src, "$user", 0, "$account");
    // param + assignment LHS + trim() arg + concatenation = 4 sites.
    assert_eq!(targets.len(), 4, "all four in-scope occurrences");
    for t in &targets {
        assert_eq!(target_text(src, t), "$user");
        assert_eq!(t.new_text, "$account");
    }
}

#[test]
fn rename_accepts_new_name_with_or_without_dollar() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let with = rename(src, "$x", 0, "$y");
    let without = rename(src, "$x", 0, "y");
    assert_eq!(with.len(), 2);
    assert_eq!(with, without, "leading $ is optional in the new name");
    assert!(with.iter().all(|t| t.new_text == "$y"));
}

// ── Nested closure isolation ──────────────────────────────────────────────

#[test]
fn nested_closure_without_use_is_isolated() {
    let src = "\
<?php
function outer() {
    $user = 1;
    $fn = function () {
        $user = 2;
        return $user;
    };
    return $user + $fn();
}
";
    // Renaming the OUTER $user touches only the two outer sites.
    let outer = rename(src, "$user", 0, "$person");
    assert_eq!(outer.len(), 2, "outer scope only");

    // The closure's two $user occurrences (the 2nd and 3rd in the file) must
    // be untouched — only the outer #0 and #3 sites get rewritten.
    let closure_user_1 = abs_byte_of_match(src, "$user", 1); // `$user = 2`
    let closure_user_2 = abs_byte_of_match(src, "$user", 2); // `return $user`
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&closure_user_1));
    assert!(!edited.contains(&closure_user_2));
}

#[test]
fn nested_closure_variable_renames_only_itself() {
    let src = "\
<?php
function outer() {
    $user = 1;
    $fn = function () {
        $user = 2;
        return $user;
    };
    return $user + $fn();
}
";
    // Cursor on the closure's own $user (3rd occurrence, 0-based index 2).
    let inner = rename(src, "$user", 2, "$local");
    assert_eq!(inner.len(), 2, "closure scope only");
    // Both edits sit inside the closure (lines 4 and 5).
    assert!(inner.iter().all(|t| t.line == 4 || t.line == 5));
}

#[test]
fn closure_use_clause_cascades() {
    let src = "\
<?php
function make() {
    $count = 0;
    $inc = function () use ($count) {
        return $count + 1;
    };
    return $inc();
}
";
    // Renaming $count must rewrite the assignment, the `use (...)` capture,
    // and the body reference together — otherwise the closure breaks.
    let targets = rename(src, "$count", 0, "$total");
    assert_eq!(targets.len(), 3, "assignment + use-clause + body");
    assert!(targets.iter().all(|t| t.new_text == "$total"));
}

#[test]
fn closure_use_by_reference_cascades() {
    let src = "\
<?php
function make() {
    $count = 0;
    $inc = function () use (&$count) {
        $count++;
    };
    $inc();
    return $count;
}
";
    // The by-reference capture `use (&$count)` binds to the OUTER $count, so a
    // rename must cascade through the assignment, the `use (&...)` capture, the
    // closure body, and the final return — leaving the closure intact. Missing
    // the capture (the `by_ref` wrapper hides the `variable_name`) would sever
    // it and silently corrupt valid code.
    let targets = rename(src, "$count", 0, "$total");
    assert_eq!(
        targets.len(),
        4,
        "assignment + use(&...) capture + body + return"
    );
    assert!(targets.iter().all(|t| t.new_text == "$total"));
    // Every edit lands on the `$count` token only — the `&` reference marker is
    // preserved (`use (&$total)`, not `&$total` mangled).
    for t in &targets {
        assert_eq!(target_text(src, t), "$count");
    }
    // The capture site and the body site must both be among the rewritten
    // offsets — the cascade reaches into the closure, not just the outer scope.
    let edited = edited_offsets(src, &targets);
    let capture_site = abs_byte_of_match(src, "$count", 1); // `use (&$count)`
    let body_site = abs_byte_of_match(src, "$count", 2); // `$count++`
    assert!(
        edited.contains(&capture_site),
        "use(&...) capture rewritten"
    );
    assert!(edited.contains(&body_site), "closure body rewritten");
}

#[test]
fn dynamic_property_access_renames_the_variable_not_the_property() {
    let src = "\
<?php
function f($obj, $key) {
    $val = $obj->$key;
    return $this->{$key} . $val;
}
";
    // `$obj->$key` and `$this->{$key}` use `$key` as a *real local variable*
    // (the dynamic member name), so renaming $key must rewrite all three of its
    // occurrences while leaving the property mechanism (`$obj`, `$this`) and the
    // unrelated `$val` untouched.
    let targets = rename(src, "$key", 0, "$prop");
    assert_eq!(targets.len(), 3, "param + $obj->$key + $this->{{$key}}");
    for t in &targets {
        assert_eq!(target_text(src, t), "$key");
        assert_eq!(t.new_text, "$prop");
    }
    let edited = edited_offsets(src, &targets);
    // The objects and the unrelated local stay put.
    assert!(!edited.contains(&abs_byte_of_match(src, "$obj", 0)));
    assert!(!edited.contains(&abs_byte_of_match(src, "$this", 0)));
    assert!(!edited.contains(&abs_byte_of_match(src, "$val", 0)));
}

// ── Arrow-function captures + shadowing ───────────────────────────────────

#[test]
fn arrow_function_captures_outer_variable() {
    let src = "\
<?php
function calc() {
    $base = 10;
    $add = fn ($x) => $x + $base;
    return $add(5) + $base;
}
";
    // $base is auto-captured by the arrow function — renaming it reaches
    // inside the arrow body.
    let targets = rename(src, "$base", 0, "$origin");
    assert_eq!(targets.len(), 3, "assignment + arrow body + return");
    assert!(targets.iter().all(|t| t.new_text == "$origin"));
}

#[test]
fn arrow_function_parameter_shadows_outer() {
    let src = "\
<?php
function f() {
    $x = 1;
    $g = fn ($x) => $x * 2;
    return $g($x);
}
";
    // Renaming the OUTER $x leaves the arrow's parameter + body untouched.
    let outer = rename(src, "$x", 0, "$seed");
    assert_eq!(outer.len(), 2, "outer scope only (assignment + call arg)");
    let arrow_param = abs_byte_of_match(src, "$x", 1); // `fn ($x)`
    let arrow_body = abs_byte_of_match(src, "$x", 2); // `=> $x * 2`
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&arrow_param));
    assert!(!edited.contains(&arrow_body));

    // Renaming the arrow's own $x touches only the arrow's two occurrences.
    let inner = rename(src, "$x", 1, "$n");
    assert_eq!(inner.len(), 2, "arrow scope only");
    assert!(inner.iter().all(|t| t.line == 3));
}

// ── Property exclusion ────────────────────────────────────────────────────

#[test]
fn properties_are_not_caught_by_variable_rename() {
    let src = "\
<?php
class Account {
    public static $user = 'static';
    public function show($user) {
        $this->user = $user;
        return self::$user . $user;
    }
}
";
    // Renaming the local $user (the method parameter): param + RHS of the
    // assignment + the concatenation = 3 sites. The static property
    // `self::$user` and the object property `$this->user` stay put.
    let targets = rename(src, "$user", 1, "$person");
    assert_eq!(targets.len(), 3, "only the three local-variable sites");
    for t in &targets {
        assert_eq!(target_text(src, t), "$user");
    }

    let edited = edited_offsets(src, &targets);
    // `self::$user` — the `$user` token starts right after `self::`.
    let static_prop = src.match_indices("self::$user").next().unwrap().0 + "self::".len();
    assert!(
        !edited.contains(&static_prop),
        "static property self::$user must be excluded"
    );
    // The static property declaration on line 2 must also be untouched.
    let decl_prop = abs_byte_of_match(src, "$user", 0);
    assert!(!edited.contains(&decl_prop));
}

#[test]
fn this_is_not_renameable() {
    let src = "<?php\nclass C {\n    public function m() { return $this->x; }\n}\n";
    let byte = cursor_byte(src, "$this", 0);
    assert!(variable_at_cursor(src, byte).is_none());
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$that")
            .unwrap()
            .is_empty()
    );
}

// ── Global declarations (alias refusal) ───────────────────────────────────

#[test]
fn global_declared_variable_is_not_renameable() {
    let src = "\
<?php
$count = 10;
function bump() {
    global $count;
    $count = 20;
    return $count;
}
";
    // `global $count;` makes the in-function `$count` an alias of the top-level
    // global. A scope-local rename would rewrite only the three in-function
    // sites and leave the file-level `$count = 10` behind — `global $new;` would
    // then bind to a non-existent global. Refuse outright, from the `global`
    // token AND from any body occurrence (the cursor can land on either).
    for nth in [1usize, 2, 3] {
        let byte = cursor_byte(src, "$count", nth);
        assert!(
            variable_at_cursor(src, byte).is_none(),
            "prepare must refuse at occurrence {nth}"
        );
        assert!(
            variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$total")
                .unwrap()
                .is_empty(),
            "rename must be a no-op at occurrence {nth}"
        );
    }
}

#[test]
fn top_level_variable_aliased_by_global_is_not_renameable() {
    let src = "\
<?php
$count = 10;
function bump() {
    global $count;
    $count++;
}
";
    // The sibling of the in-function case: renaming the file-level `$count`
    // (program scope) would leave the function's `global $count;` aliasing a
    // global that no longer exists. Any `global $count;` in the file aliases the
    // one true top-level `$count`, so the program-scope rename is refused too.
    let byte = cursor_byte(src, "$count", 0); // `$count = 10` at top level
    assert!(variable_at_cursor(src, byte).is_none());
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$total")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unrelated_nested_global_does_not_block_a_real_local() {
    let src = "\
<?php
function outer() {
    $x = 1;
    $c = function () {
        global $x;
        $x = 2;
    };
    return $x;
}
";
    // `outer`'s `$x` (`$x = 1` and `return $x`) is a genuine function-local,
    // distinct from the closure's `global $x` (which binds to the global). The
    // refusal must be resolution-aware: renaming the outer local stays allowed
    // and touches only its two sites — the closure's `global $x` / `$x = 2` are
    // a different binding and stay put.
    let outer = rename(src, "$x", 0, "$seed");
    assert_eq!(outer.len(), 2, "only the two outer-local sites");
    assert!(outer.iter().all(|t| t.new_text == "$seed"));
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&abs_byte_of_match(src, "$x", 1))); // `global $x`
    assert!(!edited.contains(&abs_byte_of_match(src, "$x", 2))); // `$x = 2`

    // The closure's `$x` is global-aliased there, so renaming *it* is refused.
    let closure_byte = cursor_byte(src, "$x", 2); // `$x = 2` inside the closure
    assert!(variable_at_cursor(src, closure_byte).is_none());
}

#[test]
fn plain_top_level_variable_without_global_is_renameable() {
    let src = "\
<?php
$user = 'a';
echo $user;
";
    // Guard against over-refusal: a top-level script variable with no `global`
    // alias anywhere is a normal, safe rename — the global guard must not reach
    // for it.
    let targets = rename(src, "$user", 0, "$account");
    assert_eq!(targets.len(), 2, "assignment + echo");
    assert!(targets.iter().all(|t| t.new_text == "$account"));
}

// ── compact() / extract() string references (refusal) ─────────────────────

#[test]
fn compact_referenced_variable_is_not_renameable() {
    let src = "\
<?php
function profile() {
    $user = currentUser();
    $role = $user->role;
    return view('profile', compact('user', 'role'));
}
";
    // `compact('user', 'role')` references the locals `$user` and `$role` *by
    // string*. A scope-local rename would rewrite the `$user` tokens and leave
    // `compact('user')` pointing at a variable that no longer exists — broken
    // PHP, silently. Refuse from the assignment AND the `$user->role` site (the
    // cursor can land on either).
    for nth in [0usize, 1] {
        let byte = cursor_byte(src, "$user", nth);
        assert!(
            variable_at_cursor(src, byte).is_none(),
            "prepare must refuse at occurrence {nth}"
        );
        assert!(
            variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$account")
                .unwrap()
                .is_empty(),
            "rename must be a no-op at occurrence {nth}"
        );
    }
    // The other compacted local, `$role`, is equally unsafe and equally refused.
    let role_byte = cursor_byte(src, "$role", 0);
    assert!(variable_at_cursor(src, role_byte).is_none());
}

#[test]
fn compact_referenced_top_level_variable_is_not_renameable() {
    let src = "\
<?php
$user = currentUser();
$data = compact('user');
";
    // The program-scope sibling: at the top level `compact('user')` names the
    // file-level `$user`, so renaming it would strand the string the same way.
    let byte = cursor_byte(src, "$user", 0);
    assert!(variable_at_cursor(src, byte).is_none());
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$account")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn extract_string_referenced_variable_is_not_renameable() {
    // `extract` is guarded symmetrically with `compact`: a string-literal
    // argument naming the variable makes a scope-local rename a partial edit.
    // (The common dynamic `extract($data)` form carries no literal name and is
    // the deferred cross-scope case — not detectable here, by design.)
    let src = "\
<?php
function load() {
    $user = null;
    extract('user');
    return $user;
}
";
    let byte = cursor_byte(src, "$user", 0);
    assert!(variable_at_cursor(src, byte).is_none());
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$account")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unrelated_nested_compact_does_not_block_a_real_local() {
    let src = "\
<?php
function outer() {
    $user = currentUser();
    $cb = function () {
        $user = guest();
        return compact('user');
    };
    return [$user, $cb];
}
";
    // `outer`'s `$user` (`$user = currentUser()` and `[$user, ...]`) is a genuine
    // function-local. The closure's `compact('user')` names the closure's *own*
    // `$user` (a distinct binding — no `use`), so it must NOT block renaming the
    // outer local. The refusal is resolution-aware, exactly like the
    // nested-`global` guard.
    let outer = rename(src, "$user", 0, "$account");
    assert_eq!(outer.len(), 2, "only the two outer-local sites");
    assert!(outer.iter().all(|t| t.new_text == "$account"));
    let edited = edited_offsets(src, &outer);
    assert!(!edited.contains(&abs_byte_of_match(src, "$user", 1))); // `$user = guest()`

    // The closure's `$user` *is* named by its own `compact('user')`, so renaming
    // it is refused.
    let closure_byte = cursor_byte(src, "$user", 1); // `$user = guest()` in closure
    assert!(variable_at_cursor(src, closure_byte).is_none());
}

#[test]
fn compact_free_function_is_still_renameable() {
    let src = "\
<?php
function show() {
    $user = currentUser();
    return $user->name;
}
";
    // Guard against over-refusal: a function with no `compact`/`extract` naming
    // the variable renames normally.
    let targets = rename(src, "$user", 0, "$account");
    assert_eq!(targets.len(), 2, "assignment + $user->name");
    assert!(targets.iter().all(|t| t.new_text == "$account"));
}

// ── Sibling-function isolation ────────────────────────────────────────────

#[test]
fn sibling_functions_isolate_their_own_variables() {
    let src = "\
<?php
function first($user) {
    return strtoupper($user);
}
function second($user) {
    return strtolower($user);
}
";
    // Two top-level functions each own a `$user`. The `function_definition` hard
    // boundary means renaming `first`'s `$user` touches only `first` — `second`'s
    // identically-named param + body are a separate binding and stay put.
    let targets = rename(src, "$user", 0, "$name");
    assert_eq!(targets.len(), 2, "only first()'s param + body");
    assert!(targets.iter().all(|t| t.line == 1 || t.line == 2));
    let edited = edited_offsets(src, &targets);
    assert!(!edited.contains(&abs_byte_of_match(src, "$user", 2))); // second's param
    assert!(!edited.contains(&abs_byte_of_match(src, "$user", 3))); // second's body
}

// ── Validation + prepare-rename range ─────────────────────────────────────

#[test]
fn invalid_new_name_is_an_error() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let byte = cursor_byte(src, "$x", 0);
    assert!(variable_rename_targets(src, &PathBuf::from("t.php"), byte, "1bad").is_err());
    assert!(variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$ ").is_err());
}

#[test]
fn renaming_to_same_name_is_a_noop() {
    let src = "<?php\nfunction f($x) { return $x; }\n";
    let byte = cursor_byte(src, "$x", 0);
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$x")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn variable_at_cursor_spans_the_whole_token() {
    let src = "<?php\nfunction f($user) { return $user; }\n";
    let byte = cursor_byte(src, "$user", 0);
    let (line, start, end) = variable_at_cursor(src, byte).expect("renameable");
    assert_eq!(line, 1);
    let line_text = src.split_inclusive('\n').nth(1).unwrap();
    assert_eq!(&line_text[start as usize..end as usize], "$user");
}

#[test]
fn variable_at_cursor_none_off_a_variable() {
    let src = "<?php\nfunction greet() { return 1; }\n";
    // Cursor on the function name, not a variable.
    let byte = src.match_indices("greet").next().unwrap().0 + 1;
    assert!(variable_at_cursor(src, byte).is_none());
}

/// Absolute byte offset of the `nth` raw match of `needle` (no cursor nudge).
fn abs_byte_of_match(source: &str, needle: &str, nth: usize) -> usize {
    source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found"))
        .0
}

// ── #96: full boundary of string-keyed & cross-scope reference shapes ──────
//
// One fixture per reference shape enumerated in issue #96, each tagged with its
// classification:
//   • REWRITE — the reference is a real `variable_name`; the rename rewrites it.
//   • REFUSE  — the rename is rejected rather than emit a partial (corrupting) edit.
//   • DEFER   — undetectable in source (no literal name); documented fail-open.
// See the module-level "Engine default: fail-open with a complete denylist".

/// Shape: `${'x'}` (dynamic variable over a string literal). CLASSIFICATION:
/// REFUSE. `${'x'}` evaluates to `$x`, but the name sits in a `string` node, not
/// a `variable_name`, so a scope-local rename can't rewrite it. Before #96 this
/// silently corrupted (renamed `$x`, stranded `${'x'}`); it is now guarded by
/// `scope_references_dynamic_string_var`.
#[test]
fn dynamic_string_variable_reference_is_refused() {
    let src = "\
<?php
function f() {
    $x = 1;
    return ${'x'};
}
";
    let byte = cursor_byte(src, "$x", 0);
    assert!(
        variable_at_cursor(src, byte).is_none(),
        "prepare must refuse: ${{'x'}} reference can't be rewritten"
    );
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$y")
            .unwrap()
            .is_empty(),
        "rename must be a no-op, not a partial edit"
    );
}

/// Over-refusal guard for the `${'x'}` shape: a *function-local* `$x` whose only
/// `${'x'}` reference lives in a *nested closure* binds to that closure, not the
/// outer function. `scope_references_dynamic_string_var` is resolution-aware
/// (it fires only when the `${'x'}` resolves to *this* scope), so an unrelated
/// dynamic-string access in an inner scope must NOT block renaming the local.
/// This is the FALSE arm of `dynamic_string_variable_reference_is_refused` — the
/// companion to `function_local_is_renamable_despite_unrelated_globals_access`.
#[test]
fn function_local_is_renamable_despite_dynamic_string_in_inner_scope() {
    let src = "\
<?php
function f() {
    $x = 1;
    $g = function () { return ${'x'}; };
    return $x;
}
";
    // `f`'s `$x` (assignment + return) renames; the `${'x'}` inside the closure
    // resolves to the closure scope, a separate binding, so it is neither a
    // blocker nor rewritten.
    let targets = rename(src, "$x", 0, "$y");
    assert_eq!(targets.len(), 2, "only the two function-local sites");
    assert!(targets.iter().all(|t| t.new_text == "$y"));
    let edited = edited_offsets(src, &targets);
    assert!(
        !edited.contains(&abs_byte_of_match(src, "${'x'}", 0)),
        "the ${{'x'}} in the inner closure stays put"
    );
}

/// Shape: `$GLOBALS['x']` (superglobal array access). CLASSIFICATION: REFUSE.
/// `$GLOBALS['x']` aliases the program-scope global `$x` through a string key the
/// rename can't rewrite. Before #96 the `scope_aliases_global` guard only saw
/// `global $x;` declarations, so this silently corrupted; it is now guarded by
/// `scope_references_globals_array`.
#[test]
fn globals_array_referenced_variable_is_refused() {
    let src = "\
<?php
$x = 1;
echo $GLOBALS['x'];
";
    let byte = cursor_byte(src, "$x", 0); // top-level `$x = 1`
    assert!(
        variable_at_cursor(src, byte).is_none(),
        "prepare must refuse: $GLOBALS['x'] aliases the program global"
    );
    assert!(
        variable_rename_targets(src, &PathBuf::from("t.php"), byte, "$y")
            .unwrap()
            .is_empty(),
        "rename must be a no-op, not a partial edit"
    );
}

/// Over-refusal guard for the `$GLOBALS` shape: a *function-local* `$x` is a
/// different binding from `$GLOBALS['x']` (which always names the program
/// global), so an unrelated superglobal access must NOT block renaming the local.
#[test]
fn function_local_is_renamable_despite_unrelated_globals_access() {
    let src = "\
<?php
function f() {
    $x = 1;
    $GLOBALS['x'] = 2;
    return $x;
}
";
    // The local `$x` (assignment + return) renames; `$GLOBALS['x']` names the
    // *global* `$x`, a separate binding, and is neither collected nor a blocker.
    let targets = rename(src, "$x", 0, "$y");
    assert_eq!(targets.len(), 2, "only the two function-local sites");
    assert!(targets.iter().all(|t| t.new_text == "$y"));
    let edited = edited_offsets(src, &targets);
    assert!(
        !edited.contains(&abs_byte_of_match(src, "$GLOBALS", 0)),
        "the $GLOBALS superglobal token stays put"
    );
}

/// Shape: `extract($runtimeArray)` (runtime keys, no string literal).
/// CLASSIFICATION: DEFER. The variable names come from a runtime array with no
/// literal in the source, so the reference is undetectable in one file. The
/// rename proceeds — it strands no *source* reference of its own — and the
/// deferral is recorded as a KNOWN LIMITATION in `php_variable_rename.rs`.
#[test]
fn extract_runtime_array_does_not_refuse_rename() {
    let src = "\
<?php
function f($data) {
    $x = 1;
    extract($data);
    return $x;
}
";
    // Contrast with `extract('x')` (string literal → REFUSE): the dynamic form
    // carries no detectable name, so the rename is allowed to proceed.
    let targets = rename(src, "$x", 0, "$y");
    assert_eq!(
        targets.len(),
        2,
        "assignment + return — rename proceeds (deferred)"
    );
    assert!(targets.iter().all(|t| t.new_text == "$y"));
}

/// Shape: `get_defined_vars()`. CLASSIFICATION: DEFER. The call returns an array
/// keyed by every in-scope variable's runtime name; there is no source reference
/// to `$x` to detect or rewrite, so the rename proceeds. Recorded as a KNOWN
/// LIMITATION in `php_variable_rename.rs`.
#[test]
fn get_defined_vars_does_not_refuse_rename() {
    let src = "\
<?php
function f() {
    $x = 1;
    return get_defined_vars();
}
";
    let targets = rename(src, "$x", 0, "$y");
    assert_eq!(
        targets.len(),
        1,
        "the single assignment — rename proceeds (deferred)"
    );
    assert!(targets.iter().all(|t| t.new_text == "$y"));
}

/// Shape: `$$name` (variable-variables). CLASSIFICATION: REWRITE. The inner
/// `$name` is a real `variable_name` (wrapped in a `dynamic_variable_name`), so
/// renaming `$name` rewrites every occurrence — the binding, the `$$name` use,
/// and the return — leaving the outer `$$` wrapper syntactically valid (`$$key`).
/// The value `$$name` dereferences at runtime is a separate, truly-dynamic
/// concern, out of scope here. (Also proves the dynamic-string guard does not
/// over-refuse `$$name`: its inner child is a `variable_name`, not a string.)
#[test]
fn variable_variables_inner_name_renames_safely() {
    let src = "\
<?php
function f() {
    $name = 'a';
    $$name = 1;
    return $name;
}
";
    let targets = rename(src, "$name", 0, "$key");
    assert_eq!(targets.len(), 3, "binding + inner $name of $$name + return");
    for t in &targets {
        assert_eq!(target_text(src, t), "$name");
        assert_eq!(t.new_text, "$key");
    }
    // The `$$name` use is rewritten on its inner `$name` (one byte past the outer
    // `$`), so `$$name` becomes `$$key` with the leading `$` wrapper untouched.
    let edited = edited_offsets(src, &targets);
    let inner_of_dollar_dollar = src.match_indices("$$name").next().unwrap().0 + 1;
    assert!(
        edited.contains(&inner_of_dollar_dollar),
        "inner $name of $$name is rewritten"
    );
}

/// Shape: string interpolation + heredoc (`"$x"`, `"{$x}"`, `<<<EOT $x EOT`).
/// CLASSIFICATION: REWRITE. Interpolated variables parse as `variable_name` nodes
/// inside the `encapsed_string` / `heredoc_body`, so the rename rewrites them; the
/// heredoc delimiter (`EOT`) is a separate token, left untouched. A nowdoc
/// (`<<<'EOT'`) does NOT interpolate — its `$x` is literal text (`nowdoc_string`)
/// and must NOT be rewritten.
#[test]
fn interpolation_and_heredoc_are_rewritten_nowdoc_is_not() {
    let src = "\
<?php
function f() {
    $x = 1;
    $a = \"Hello $x\";
    $b = \"V {$x}\";
    $h = <<<EOT
val $x
EOT;
    $n = <<<'EOT'
raw $x
EOT;
    return $a . $b . $h . $n;
}
";
    let targets = rename(src, "$x", 0, "$y");
    // assignment + "Hello $x" + "{$x}" + heredoc $x = 4 sites. The nowdoc `$x` is
    // literal text and is NOT among them.
    assert_eq!(targets.len(), 4, "assignment + 2 interpolations + heredoc");
    for t in &targets {
        assert_eq!(target_text(src, t), "$x");
        assert_eq!(t.new_text, "$y");
    }
    // The nowdoc occurrence (5th `$x`, 0-based index 4) stays literal.
    let edited = edited_offsets(src, &targets);
    assert!(
        !edited.contains(&abs_byte_of_match(src, "$x", 4)),
        "nowdoc $x is literal, not rewritten"
    );
}

/// Shape: by-reference positions (`&$x`). CLASSIFICATION: REWRITE. Whether `$x`
/// is bound by a by-reference parameter (`function f(&$x)`) or passed by reference
/// at a call site, the `$x` is a plain `variable_name` and the `&` is a separate
/// `reference_modifier` token. The rename rewrites the `$x` occurrences and leaves
/// every `&` intact.
#[test]
fn by_reference_positions_rename_and_keep_the_ampersand() {
    // (a) by-reference PARAMETER — the idiomatic modern form.
    let param_src = "\
<?php
function bump(&$counter) {
    $counter = $counter + 1;
    return $counter;
}
";
    let targets = rename(param_src, "$counter", 0, "$total");
    assert_eq!(targets.len(), 4, "by-ref param + three body uses");
    for t in &targets {
        assert_eq!(target_text(param_src, t), "$counter");
        assert_eq!(t.new_text, "$total");
    }
    assert!(
        !edited_offsets(param_src, &targets)
            .contains(&param_src.match_indices('&').next().unwrap().0),
        "the & reference marker is a separate token, never edited"
    );

    // (b) call-site `f(&$x)` (legacy call-time pass-by-reference). tree-sitter
    // still parses it as `argument` → `reference_modifier` + `variable_name`, so
    // the `$x` is collected and the `&` left intact, identical to (a).
    let call_src = "\
<?php
function f() {
    $x = 1;
    process(&$x);
    return $x;
}
";
    let call_targets = rename(call_src, "$x", 0, "$y");
    assert_eq!(call_targets.len(), 3, "assignment + &-arg + return");
    for t in &call_targets {
        assert_eq!(target_text(call_src, t), "$x");
    }
    assert!(
        !edited_offsets(call_src, &call_targets)
            .contains(&call_src.match_indices('&').next().unwrap().0),
        "the & at the call site is untouched"
    );
}
