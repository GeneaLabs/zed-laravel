//! Unit tests for scope-aware Blade variable rename and controller→view
//! binding rename. Each AC case from issue #55 has at least one test:
//! `@foreach` / `@forelse` / `@for` / `@php` scoping, the
//! `view(..., ['key' => …])` and `compact('key')` patterns, nested scope
//! conflicts, and the multi-controller cross-contamination guard.

use super::*;

// ── helpers ───────────────────────────────────────────────────────────────

#[test]
fn normalize_strips_sigil_and_whitespace() {
    assert_eq!(normalize_new_var_name("  $bar "), "bar");
    assert_eq!(normalize_new_var_name("bar"), "bar");
    assert_eq!(normalize_new_var_name("$user"), "user");
}

#[test]
fn identifier_validation() {
    assert!(is_valid_identifier("foo"));
    assert!(is_valid_identifier("_foo1"));
    assert!(!is_valid_identifier("1foo"));
    assert!(!is_valid_identifier("foo-bar"));
    assert!(!is_valid_identifier(""));
    assert!(!is_valid_identifier("foo bar"));
}

// ── variable_spans ──────────────────────────────────────────────────────────

#[test]
fn finds_variable_occurrences_excluding_property_access() {
    let src = "{{ $user }} and {{ $user->name }} but not $username";
    let spans = variable_spans(src, "user");
    // Two `$user` occurrences; `$user->name` matches the variable, `$username`
    // does not (word boundary).
    assert_eq!(spans.len(), 2);
    // First `$user`: `$` at col 3, name `user` at cols 4..8.
    assert_eq!(spans[0], VarSpan::new(0, 4, 8));
    // Second `$user` in `$user->name`: `$` at col 19, name at 20..24.
    assert_eq!(spans[1], VarSpan::new(0, 20, 24));
}

#[test]
fn variable_spans_skip_blade_comments() {
    let src = "{{ $foo }}\n{{-- $foo is hidden --}}\n{{ $foo }}";
    let spans = variable_spans(src, "foo");
    assert_eq!(spans.len(), 2, "the commented $foo must be ignored");
    assert_eq!(spans[0].line, 0);
    assert_eq!(spans[1].line, 2);
}

#[test]
fn variable_spans_skip_verbatim() {
    let src = "{{ $foo }}\n@verbatim\n{{ $foo }}\n@endverbatim\n{{ $foo }}";
    let spans = variable_spans(src, "foo");
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].line, 0);
    assert_eq!(spans[1].line, 4);
}

// ── in_scope_spans: @foreach ────────────────────────────────────────────────

#[test]
fn foreach_scopes_rename_to_the_loop_block() {
    let src = "\
{{ $item }}
@foreach ($items as $item)
    {{ $item->name }}
@endforeach
{{ $item }}";
    // Cursor inside the loop body (line 2) — rename only the in-loop `$item`s.
    let spans = in_scope_spans(src, "item", 2);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![1, 2], "loop directive + body, not lines 0 or 4");
}

#[test]
fn foreach_binding_line_is_in_scope() {
    let src = "\
@foreach ($items as $item)
    {{ $item }}
@endforeach";
    // Cursor on the `@foreach` binding line itself.
    let spans = in_scope_spans(src, "item", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 1]);
}

#[test]
fn file_scoped_variable_skips_loop_rebinding_same_name() {
    // `$item` at file level (line 0) and a loop that re-binds `$item`.
    let src = "\
{{ $item }}
@foreach ($items as $item)
    {{ $item }}
@endforeach
{{ $item }}";
    // Cursor on the file-level `$item` (line 0) — must NOT touch the loop's
    // shadowing `$item` (lines 1–2), only the file-level ones (lines 0, 4).
    let spans = in_scope_spans(src, "item", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 4]);
}

#[test]
fn foreach_with_method_call_iterable_still_scopes_to_the_loop() {
    // issue #55 regression: a parenthesized iterable (`->where(...)`) must not
    // defeat loop-scope detection. Before the paren-balancing fix the loop's
    // `$user` binding was lost, `loop_binding_ranges` returned empty, and the
    // file-scope arm admitted EVERY `$user` — clobbering the out-of-loop one.
    let src = "\
{{ $user }}
@foreach ($users->where('active', true) as $user)
    {{ $user->name }}
@endforeach
{{ $user }}";
    // Cursor inside the loop body (line 2) — rename only the in-loop `$user`s,
    // never the file-level ones on lines 0 and 4.
    let spans = in_scope_spans(src, "user", 2);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![1, 2],
        "loop directive + body only, not lines 0/4"
    );
}

#[test]
fn file_scoped_var_skips_loop_with_method_call_iterable() {
    // The mirror case: a file-level `$user` must not bleed into a loop that
    // re-binds `$user` via a parenthesized iterable.
    let src = "\
{{ $user }}
@foreach ($users->where('active', true) as $user)
    {{ $user }}
@endforeach
{{ $user }}";
    let spans = in_scope_spans(src, "user", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![0, 4],
        "file-level occurrences only, loop excluded"
    );
}

#[test]
fn foreach_with_multiline_directive_still_scopes_to_the_loop() {
    // issue #55 regression: a `@foreach` whose argument list wraps across
    // physical lines must still register as a loop block. Before the multi-line
    // paren-balancing fix, `matching_paren` gave up at the first line end, the
    // loop's `$user` binding was lost, `loop_binding_ranges` came back empty,
    // and the file-scope arm admitted EVERY `$user` — clobbering the file-level
    // occurrences on lines 0 and 5.
    let src = "\
{{ $user }}
@foreach ($users->where('active', true)
    as $user)
    {{ $user->name }}
@endforeach
{{ $user }}";
    // Cursor inside the loop body (line 3) — only the in-loop `$user`s.
    let spans = in_scope_spans(src, "user", 3);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![2, 3],
        "binding line + body only, never the file-level lines 0/5"
    );
}

#[test]
fn file_scoped_var_skips_multiline_loop_directive() {
    // The mirror case: a file-level `$user` must not bleed into a loop that
    // re-binds `$user` via a multi-line directive header.
    let src = "\
{{ $user }}
@foreach ($users->where('active', true)
    as $user)
    {{ $user }}
@endforeach
{{ $user }}";
    let spans = in_scope_spans(src, "user", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![0, 5],
        "file-level occurrences only, multi-line loop excluded"
    );
}

#[test]
fn foreach_destructuring_binding_scopes_to_the_loop() {
    // Array destructuring binds `$a` inside the loop; renaming it stays
    // loop-scoped and never touches the file-level `$a` on lines 0 and 4.
    let src = "\
{{ $a }}
@foreach ($pairs as [$a, $b])
    {{ $a }}: {{ $b }}
@endforeach
{{ $a }}";
    let spans = in_scope_spans(src, "a", 2);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![1, 2],
        "destructured binding + body, not file 0/4"
    );
}

#[test]
fn foreach_by_reference_binding_scopes_to_the_loop() {
    // By-reference binding `&$item` is recovered; rename stays loop-scoped.
    let src = "\
{{ $item }}
@foreach ($items as &$item)
    {{ $item }}
@endforeach
{{ $item }}";
    let spans = in_scope_spans(src, "item", 2);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![1, 2]);
}

#[test]
fn commented_out_loop_does_not_create_a_phantom_binding_block() {
    // A `@foreach` inside a `{{-- --}}` comment must not register as a binding
    // block. Loop detection runs over the masked copy (like variable_spans), so
    // the commented loop is invisible and a file-scoped rename of `$item`
    // touches the file-level occurrences only — the real loop (lines 2–4) is
    // excluded as a re-binding scope, the commented one contributes nothing.
    let src = "\
{{-- @foreach ($x as $item) --}}
{{ $item }}
@foreach ($real as $item)
    {{ $item }}
@endforeach
{{ $item }}";
    let spans = in_scope_spans(src, "item", 1);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![1, 5],
        "file-level only; no phantom block from the comment"
    );
}

#[test]
fn opaque_loop_refuses_rename_rather_than_clobbering() {
    // A PHP block comment inside the header desyncs the paren scan, so the
    // binding can't be resolved — an opaque loop. A rename whose cursor sits
    // inside it is refused (no spans) rather than falling through to the
    // file-scope arm and clobbering every `$user`. `cursor_in_unresolved_loop`
    // gates prepare_rename identically.
    let src = "\
{{ $user }}
@foreach ($users /* :) */ as $user)
    {{ $user }}
@endforeach
{{ $user }}";
    assert!(
        cursor_in_unresolved_loop(src, 2),
        "cursor is inside the opaque loop"
    );
    assert!(
        in_scope_spans(src, "user", 2).is_empty(),
        "rename refused inside an opaque loop"
    );
}

#[test]
fn broken_loop_header_refuses_rename_rather_than_clobbering() {
    // A loop header whose parens never close is broken Blade that forms no
    // block at all — so the opaque-loop backstop alone wouldn't catch it. The
    // scope below it is unreliable, so a rename there is refused (cursor inside
    // the unresolved region) rather than clobbering every `$user` file-wide.
    let src = "\
{{ $user }}
@foreach ($users as $user
    {{ $user->name }}
@endforeach
{{ $user }}";
    assert!(
        cursor_in_unresolved_loop(src, 2),
        "cursor is below a broken loop header"
    );
    assert!(
        in_scope_spans(src, "user", 2).is_empty(),
        "rename refused below a broken header"
    );
}

#[test]
fn blade_comment_inside_a_multiline_header_still_scopes() {
    // A `{{-- --}}` comment inside a wrapped header is masked before loop
    // detection, so the binding is recovered and the rename stays loop-scoped
    // (rewritten correctly, NOT refused and NOT clobbered). Before masking, the
    // `)` inside the comment desynced the paren scan and clobbered the file.
    let src = "\
{{ $user }}
@foreach ($users
    {{-- pick the active ones :) --}}
    as $user)
    {{ $user->name }}
@endforeach
{{ $user }}";
    let spans = in_scope_spans(src, "user", 4);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![3, 4],
        "binding line + body, comment masked away"
    );
}

#[test]
fn file_scoped_rename_skips_an_opaque_loop() {
    // A file-scoped rename (cursor outside any loop) must not clobber INTO an
    // opaque loop whose scope is unknown — those occurrences are left alone.
    let src = "\
{{ $user }}
@foreach ($users /* :) */ as $user)
    {{ $user }}
@endforeach
{{ $user }}";
    let spans = in_scope_spans(src, "user", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(
        lines,
        vec![0, 4],
        "file-level occurrences only, opaque loop skipped"
    );
}

#[test]
fn loop_magic_variable_is_not_renameable() {
    // `$loop` is Blade-injected, never in a header, and renaming it would
    // clobber across unrelated loops — refused at the prepare gate.
    let src = "\
@foreach ($users as $user)
    {{ $loop->index }}
@endforeach
@foreach ($posts as $post)
    {{ $loop->index }}
@endforeach";
    assert!(
        !is_template_variable(src, "loop"),
        "$loop is reserved and not renameable"
    );
}

// ── in_scope_spans: @forelse ────────────────────────────────────────────────

#[test]
fn forelse_scopes_rename() {
    let src = "\
@forelse ($users as $user)
    {{ $user->email }}
@empty
    none
@endforelse
{{ $user }}";
    let spans = in_scope_spans(src, "user", 1);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Body inside forelse, not the trailing line-5 `$user`.
    assert_eq!(lines, vec![0, 1]);
}

// ── in_scope_spans: @for ────────────────────────────────────────────────────

#[test]
fn for_loop_scopes_rename() {
    let src = "\
@for ($i = 0; $i < 3; $i++)
    {{ $i }}
@endfor
{{ $i }}";
    let spans = in_scope_spans(src, "i", 1);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // The three `$i` in the header (init / condition / step) plus the body
    // `$i` on line 1 — and NOT the out-of-loop `$i` on line 3.
    assert_eq!(lines, vec![0, 0, 0, 1]);
}

// ── in_scope_spans: @php block (file-scoped) ────────────────────────────────

#[test]
fn php_block_variable_is_file_scoped() {
    let src = "\
@php $total = 0; @endphp
{{ $total }}
@foreach ($rows as $row)
    {{ $total }}
@endforeach";
    // `$total` is not loop-introduced, so it is file-scoped: every occurrence
    // renames, including the one inside the loop (the loop doesn't re-bind it).
    let spans = in_scope_spans(src, "total", 0);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 1, 3]);
}

// ── in_scope_spans: nested scope conflict ───────────────────────────────────

#[test]
fn nested_loops_rebinding_same_name_do_not_cross_contaminate() {
    let src = "\
@foreach ($outer as $item)
    {{ $item }}
    @foreach ($inner as $item)
        {{ $item }}
    @endforeach
    {{ $item }}
@endforeach";
    // Cursor in the OUTER loop body (line 1). The inner loop (lines 2–4)
    // re-binds `$item`, so its occurrences (lines 2, 3) are excluded; the
    // outer `$item`s (lines 0, 1, 5) rename.
    let outer = in_scope_spans(src, "item", 1);
    let outer_lines: Vec<u32> = outer.iter().map(|s| s.line).collect();
    assert_eq!(outer_lines, vec![0, 1, 5]);

    // Cursor in the INNER loop body (line 3) — only the inner `$item`s
    // (lines 2, 3) rename.
    let inner = in_scope_spans(src, "item", 3);
    let inner_lines: Vec<u32> = inner.iter().map(|s| s.line).collect();
    assert_eq!(inner_lines, vec![2, 3]);
}

#[test]
fn file_scope_spans_exclude_loop_rebinding() {
    // The controller→view path renames a file-scoped `$user`, but a loop that
    // re-binds `$user` is a separate scope and must be left alone.
    let src = "\
{{ $user->name }}
@foreach ($admins as $user)
    {{ $user }}
@endforeach
{{ $user->email }}";
    let spans = file_scope_spans(src, "user");
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![0, 4], "loop-rebound $user (lines 1–2) excluded");
}

// ── view_binding_key_at: array key ──────────────────────────────────────────

const ARRAY_CONTROLLER: &str = "\
<?php
class UserController
{
    public function show()
    {
        return view('users.profile', ['name' => $user->name]);
    }
}";

#[test]
fn array_key_binding_detected_under_cursor() {
    // Line 5, the `name` key sits inside the quotes after `['`.
    // `        return view('users.profile', ['name' => ...`
    // Find the column of `name` inside `['name'`.
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let key_quote = line.find("'name'").unwrap();
    let cursor = (key_quote + 2) as u32; // somewhere inside `name`

    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, cursor).expect("key under cursor");
    assert_eq!(binding.view_name, "users.profile");
    assert_eq!(binding.key, "name");
    assert_eq!(binding.form, BindingForm::ArrayKey);
    // Span covers `name` (4 chars) inside the quotes.
    assert_eq!(binding.key_span.line, 5);
    assert_eq!(
        binding.key_span.end_col - binding.key_span.start_col,
        4,
        "span covers the 4-char key name only"
    );
}

#[test]
fn cursor_on_value_expression_is_not_a_binding_key() {
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let value = line.find("$user").unwrap();
    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, (value + 1) as u32);
    assert!(binding.is_none(), "value side must not classify as a key");
}

#[test]
fn cursor_on_view_name_is_not_a_binding_key() {
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let view = line.find("users.profile").unwrap();
    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, (view + 1) as u32);
    assert!(binding.is_none(), "view name is not a data-binding key");
}

#[test]
fn multiple_controllers_different_keys_do_not_cross_contaminate() {
    // The SAME view is rendered from two controllers under different key names.
    // Each binding rename is driven end-to-end through `binding_rename_spans`
    // (cursor → binding → cross-file spans). A rename initiated from one
    // controller rewrites only that key's in-view usages; the other
    // controller's key is a different identifier and is never touched (AC #6:
    // no cross-contamination across key names).
    let view = "\
{{ $name }}
{{ $other }}
{{ $name->email }}";

    let ctrl_a = "<?php\nreturn view('shared', ['name' => $u->name]);";
    let ctrl_b = "<?php\nreturn view('shared', ['other' => $u->other]);";

    let a_line = ctrl_a.lines().nth(1).unwrap();
    let a_cursor = (a_line.find("'name'").unwrap() + 2) as u32;
    let binding_a = view_binding_key_at(ctrl_a, 1, a_cursor).expect("name key under cursor");
    let spans_a = binding_rename_spans(&binding_a, ctrl_a, Some(view));
    assert_eq!(
        spans_a.view.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![0, 2],
        "controller A's rename moves only the in-view $name usages"
    );

    let b_line = ctrl_b.lines().nth(1).unwrap();
    let b_cursor = (b_line.find("'other'").unwrap() + 2) as u32;
    let binding_b = view_binding_key_at(ctrl_b, 1, b_cursor).expect("other key under cursor");
    let spans_b = binding_rename_spans(&binding_b, ctrl_b, Some(view));
    assert_eq!(
        spans_b.view.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![1],
        "controller B's rename moves only the in-view $other usage"
    );

    // The two renames' in-view edit sets are disjoint — no shared span.
    assert!(spans_a.view.iter().all(|s| !spans_b.view.contains(s)));
}

// ── binding_rename_spans: cross-file orchestration (AC #6) ──────────────────

#[test]
fn binding_rename_spans_cover_compact_controller_and_view() {
    // The full cross-file pipeline for `compact('name')`: the controller gets
    // the local `$name` AND the compact key; the resolved view gets the
    // in-view `$name` usages, with an unrelated `$other` left alone.
    let view = "\
{{ $name }}
{{ $other }}
{{ $name->email }}";
    let line = COMPACT_CONTROLLER.lines().nth(6).unwrap();
    let cursor = (line.find("'name'").unwrap() + 2) as u32;
    let binding = view_binding_key_at(COMPACT_CONTROLLER, 6, cursor).expect("compact key");

    let spans = binding_rename_spans(&binding, COMPACT_CONTROLLER, Some(view));
    // Controller: the enclosing-function local `$name` (line 5) and the compact
    // key string (line 6), sorted by position.
    assert_eq!(
        spans.controller.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![5, 6]
    );
    // View: `$name` on lines 0 and 2 — `$other` (line 1) is untouched.
    assert_eq!(
        spans.view.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![0, 2],
        "only $name usages move; $other is left alone"
    );
}

#[test]
fn binding_rename_spans_array_key_leaves_controller_value_untouched() {
    // The `['name' => $user->name]` form: only the key string moves in the
    // controller (the value `$user->name` is independent of the key), plus the
    // in-view usages.
    let view = "{{ $name }}\n{{ $name }}";
    let line = ARRAY_CONTROLLER.lines().nth(5).unwrap();
    let cursor = (line.find("'name'").unwrap() + 2) as u32;
    let binding = view_binding_key_at(ARRAY_CONTROLLER, 5, cursor).expect("array key");

    let spans = binding_rename_spans(&binding, ARRAY_CONTROLLER, Some(view));
    assert_eq!(
        spans.controller.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![5],
        "only the key string moves — no compact local for the array form"
    );
    assert_eq!(
        spans.view.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn binding_rename_spans_without_resolved_view_only_touch_controller() {
    // When the view can't be located/read, only the controller-side spans are
    // produced (no panic, no view edits).
    let line = COMPACT_CONTROLLER.lines().nth(6).unwrap();
    let cursor = (line.find("'name'").unwrap() + 2) as u32;
    let binding = view_binding_key_at(COMPACT_CONTROLLER, 6, cursor).unwrap();

    let spans = binding_rename_spans(&binding, COMPACT_CONTROLLER, None);
    assert_eq!(
        spans.controller.iter().map(|s| s.line).collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert!(spans.view.is_empty());
}

// ── view_binding_key_at: compact ────────────────────────────────────────────

const COMPACT_CONTROLLER: &str = "\
<?php
class UserController
{
    public function show()
    {
        $name = $user->name;
        return view('users.profile', compact('name'));
    }
}";

#[test]
fn compact_key_binding_detected_under_cursor() {
    let line = COMPACT_CONTROLLER.lines().nth(6).unwrap();
    let key_quote = line.find("'name'").unwrap();
    let cursor = (key_quote + 2) as u32;

    let binding = view_binding_key_at(COMPACT_CONTROLLER, 6, cursor).expect("compact key");
    assert_eq!(binding.view_name, "users.profile");
    assert_eq!(binding.key, "name");
    assert_eq!(binding.form, BindingForm::Compact);
}

#[test]
fn compact_renames_enclosing_function_local() {
    // The `compact('name')` case must also rename the controller-local `$name`
    // within the enclosing method so the code stays valid.
    let anchor = VarSpan::new(6, 0, 0); // anchor on the `view(...)` line
    let spans = enclosing_function_local_spans(COMPACT_CONTROLLER, "name", anchor);
    // `$name` appears once as the assignment target on line 5.
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![5]);
}

#[test]
fn enclosing_local_scope_does_not_leak_across_methods() {
    let src = "\
<?php
class C
{
    public function a()
    {
        $name = 1;
    }
    public function b()
    {
        $name = 2;
        return view('v', compact('name'));
    }
}";
    // Anchor in method b (line 10). Only b's `$name` (line 9) should be found,
    // not a()'s `$name` (line 5).
    let anchor = VarSpan::new(10, 0, 0);
    let spans = enclosing_function_local_spans(src, "name", anchor);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, vec![9]);
}

#[test]
fn compact_local_skips_non_capturing_closure() {
    // A plain `function () { … }` does NOT capture the outer `$name` (PHP
    // closures capture nothing without a `use` clause), so renaming the
    // `compact('name')` local must leave the closure's own `$name` untouched.
    // Regression for the issue #55 correctness fix.
    let src = "\
<?php
class C
{
    public function show()
    {
        $name = 'Alice';
        $fn = function () { $name = 'inner'; };
        return view('v', compact('name'));
    }
}";
    // Anchor on the `view(...)` line (line 7).
    let anchor = VarSpan::new(7, 0, 0);
    let spans = enclosing_function_local_spans(src, "name", anchor);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Only the outer `$name` on line 5 — NOT the closure body's `$name` (line 6).
    assert_eq!(
        lines,
        vec![5],
        "the independent closure variable must be left alone"
    );
}

#[test]
fn compact_local_descends_into_use_capturing_closure() {
    // A closure that captures `$name` via `use ($name)` binds its body `$name`
    // to the outer variable's name, so the rename MUST descend — both the
    // `use` capture and the body usage move with the outer local, or the
    // closure would reference a renamed-away variable.
    let src = "\
<?php
class C
{
    public function show()
    {
        $name = 'Alice';
        $fn = function () use ($name) { return strtoupper($name); };
        return view('v', compact('name'));
    }
}";
    let anchor = VarSpan::new(7, 0, 0);
    let spans = enclosing_function_local_spans(src, "name", anchor);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Line 5 (outer local) plus line 6 twice: the `use ($name)` capture and the
    // body `$name`.
    assert_eq!(lines, vec![5, 6, 6]);
}

#[test]
fn compact_inside_route_closure_does_not_leak_to_sibling_closure() {
    // The Laravel route-closure pattern: `compact('name')` sits INSIDE a
    // closure, so the closure itself must be elected as the rename scope. A
    // sibling closure's unrelated `$name` must stay untouched. Regression for
    // the round-2 fix: `enclosing_function_local_spans` previously matched only
    // the legacy `anonymous_function_creation_expression` node kind (which never
    // exists in tree-sitter-php 0.24), so the real `anonymous_function` closure
    // was never elected and scope fell back to the whole file.
    let src = "\
<?php

Route::get('/a', function () use ($name) {
    return view('users.index', compact('name'));
});

Route::get('/b', function () use ($name) {
    return response($name);
});";
    // Anchor on the `view(...)` line inside the `/a` closure (line 3).
    let anchor = VarSpan::new(3, 0, 0);
    let spans = enclosing_function_local_spans(src, "name", anchor);
    let lines: Vec<u32> = spans.iter().map(|s| s.line).collect();
    // Only the `/a` closure's `use ($name)` capture on line 2 — NOT the sibling
    // `/b` closure's `$name` on lines 6 and 7.
    assert_eq!(
        lines,
        vec![2],
        "rename must stay inside the enclosing closure, not leak file-wide"
    );
}

// ── is_template_variable: prepare_rename admissibility gate (AC #5) ─────────

#[test]
fn template_variable_recognized_when_used_in_markup() {
    // A controller-passed / echoed variable surfaces in markup — renameable.
    assert!(is_template_variable("{{ $user->name }}", "user"));
    // A loop variable surfaces in its header — renameable.
    assert!(is_template_variable(
        "@foreach ($items as $row)\n    {{ $row }}\n@endforeach",
        "row"
    ));
    // A `@php`-assigned variable that is then echoed surfaces in markup.
    assert!(is_template_variable(
        "@php $total = 0; @endphp\n{{ $total }}",
        "total"
    ));
}

#[test]
fn out_of_context_variable_is_not_a_template_variable() {
    // Undefined `$ghost`: appears nowhere → not renameable (AC #5).
    assert!(!is_template_variable("{{ $user }}", "ghost"));

    // A PHP-block / function-local confined to `@php … @endphp`, never echoed,
    // is not a Blade template variable — rename must reject it.
    let src = "\
@php
    $helper = function () {
        $local = 5;
        return $local;
    };
@endphp
{{ $user }}";
    assert!(!is_template_variable(src, "local"));
}

#[test]
fn inline_php_directive_does_not_mask_rest_of_file() {
    // The inline `@php(expr)` form has no `@endphp`; masking must not blank the
    // rest of the file, which would wrongly reject every later variable.
    let src = "@php($total = 1)\n{{ $total }}";
    assert!(is_template_variable(src, "total"));
}

#[test]
fn php_word_prefix_does_not_anchor_a_mask_block() {
    // `@phpdoc` shares the `@php` prefix but is NOT a `@php` block opener. The
    // bare-substring match used to anchor a spurious mask region here, swallowing
    // the real `{{ $total }}` markup that follows and wrongly rejecting the
    // rename. The genuine `@php … @endphp` block later must still mask only itself.
    let src = "@phpdoc note\n{{ $total }}\n@php $x = 1; @endphp";
    assert!(is_template_variable(src, "total"));
}

// ── view_binding_key_at: ->with chains ──────────────────────────────────────

#[test]
fn with_array_chain_binding_detected() {
    let src = "\
<?php
return view('dash')->with(['count' => 3]);";
    let line = src.lines().nth(1).unwrap();
    let key_quote = line.find("'count'").unwrap();
    let binding = view_binding_key_at(src, 1, (key_quote + 2) as u32);
    // ->with chains aren't a direct view() data arg; documented as not yet
    // routed through the key classifier. Assert current behavior so a future
    // expansion updates the test deliberately.
    assert!(binding.is_none());
}
