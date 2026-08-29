use super::*;
use crate::livewire_config::LivewireConfig;
use crate::livewire_version::LivewireVersion;
use std::fs;
use tempfile::TempDir;

fn config_for(root: &Path) -> LivewireConfig {
    LivewireConfig::defaults(root)
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn resolves_v4_sfc_at_top_level() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_v4_sfc_in_nested_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/components/admin/{}user-list.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    let component =
        resolve_component("admin.user-list", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_v4_mfc_with_all_optional_children() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.php"), "<?php new class {}; ?>");
    write(&dir.join("counter.blade.php"), "<div></div>");
    write(&dir.join("counter.js"), "export {};");
    write(&dir.join("counter.css"), ".counter {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Mfc);
    // Directory first, then child files in MFC_CHILD_EXTENSIONS order.
    assert_eq!(component.paths[0], dir);
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.php"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.blade.php"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.js"));
    assert!(component
        .paths
        .iter()
        .any(|p| p.file_name().unwrap() == "counter.css"));
}

#[test]
fn v4_mfc_rejects_directory_without_required_class_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Directory exists with the view file but NO {leaf}.php — Livewire's
    // MultiFileParser throws on this. The resolver must not classify it as
    // MFC.
    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.blade.php"), "<div></div>");

    assert!(resolve_component("counter", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn resolves_volt_via_state_call() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/counter.blade.php");
    write(
        &path,
        "<?php\nuse function Livewire\\Volt\\state;\nstate(['count' => 0]);\n?>\n<div></div>",
    );

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_volt_via_class_extends() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/counter.blade.php");
    write(
        &path,
        "<?php\nuse Livewire\\Volt\\Component;\nnew class extends Component {};\n?>\n<div></div>",
    );

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
}

#[test]
fn resolves_anonymous_volt_under_mount_without_signature() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Bare .blade.php with no PHP front-matter at all — just template. Volt
    // auto-mounts every file under `view_path` (resources/views/livewire) as a
    // component, so an anonymous component with no class and no functional-API
    // signature must still resolve as Volt (issue #250).
    let path = root.join("resources/views/livewire/counter.blade.php");
    write(&path, "<div>no php here</div>");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_nested_anonymous_volt_without_signature() {
    // Issue #250: `<livewire:stats.team-stats />` backed by a signature-less
    // `resources/views/livewire/stats/team-stats.blade.php`.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/stats/team-stats.blade.php");
    write(&path, "<div>{{ $slot }}</div>");

    let component =
        resolve_component("stats.team-stats", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn resolves_anonymous_volt_on_livewire_v3() {
    // Volt ships on Livewire 3 too — the signature-less mount discovery must
    // not be gated behind the v4-only SFC/MFC check.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join("resources/views/livewire/counter.blade.php");
    write(&path, "<div>no php here</div>");

    let component = resolve_component("counter", &cfg, LivewireVersion::V3).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::Volt);
}

#[test]
fn signature_less_blade_under_components_dir_isnt_volt() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // A plain `.blade.php` under resources/views/components is an anonymous
    // *Blade* component, not Volt. Only the Volt mount root (view_path) gets
    // the signature-less treatment; elsewhere a Volt signature is required, so
    // this must NOT resolve as a Livewire component here.
    let path = root.join("resources/views/components/card.blade.php");
    write(&path, "<div>{{ $slot }}</div>");

    assert!(resolve_component("card", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn resolves_v3_class_based() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class_file = root.join("app/Livewire/Counter.php");
    write(&class_file, "<?php class Counter extends Component {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V3).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V3Class);
    assert_eq!(component.paths, vec![class_file]);
}

#[test]
fn resolves_v3_class_with_companion_view() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let class_file = root.join("app/Livewire/Admin/UserList.php");
    write(&class_file, "<?php class UserList extends Component {}");
    let view_file = root.join("resources/views/livewire/admin/user-list.blade.php");
    write(&view_file, "<div></div>");

    let component =
        resolve_component("admin.user-list", &cfg, LivewireVersion::V3).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V3Class);
    assert_eq!(component.paths.len(), 2);
    assert_eq!(component.paths[0], class_file);
    assert_eq!(component.paths[1], view_file);
}

#[test]
fn v3_project_skips_v4_formats() {
    // Even if v4-shaped files exist on disk, a v3 project only resolves
    // via class-based lookup. Documents the behavior so a v4 fixture
    // sneaking into a v3 project doesn't get a false-positive resolution.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let v4_sfc_path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&v4_sfc_path, "<?php new class extends Component {}; ?>");

    assert!(resolve_component("counter", &cfg, LivewireVersion::V3).is_none());
}

#[test]
fn resolves_namespaced_component() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let path = root.join(format!(
        "resources/views/pages/{}dashboard.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?>");

    let component =
        resolve_component("pages::dashboard", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
    assert_eq!(component.paths, vec![path]);
}

#[test]
fn namespaced_lookup_against_unknown_namespace_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // 'billing' namespace isn't in the defaults map, and we don't fall
    // through to component_locations for namespaced names.
    assert!(resolve_component("billing::invoice", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn sfc_preferred_over_v3_class_when_both_exist() {
    // V4-first projects sometimes have stale v3-style class files lying
    // around. The resolver picks the v4 SFC because that's what Livewire
    // discovery does at runtime.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    let sfc_path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&sfc_path, "<?php new class extends Component {}; ?>");
    let class_file = root.join("app/Livewire/Counter.php");
    write(&class_file, "<?php class Counter extends Component {}");

    let component = resolve_component("counter", &cfg, LivewireVersion::V4).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
}

#[test]
fn returns_none_for_missing_component() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(resolve_component("does.not.exist", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn empty_leaf_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Trailing dot → empty leaf segment. Defensive: don't crash, return None.
    assert!(resolve_component("admin.", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn empty_name_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    assert!(resolve_component("", &cfg, LivewireVersion::V4).is_none());
}

#[test]
fn unknown_version_tries_v4_first() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);

    // Unknown is conservative — tries all formats. A v4 SFC on disk should
    // still resolve as V4Sfc, not fall through to v3 class lookup.
    let path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?>");

    let component = resolve_component("counter", &cfg, LivewireVersion::Unknown).expect("resolves");
    assert_eq!(component.kind, LivewireComponentKind::V4Sfc);
}

// ---------- livewire_name_for_path (reverse, guess-verify) ----------

#[test]
fn reverse_v4_sfc_top_level() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join(format!(
        "resources/views/livewire/{}counter.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    assert_eq!(
        livewire_name_for_path(&path, &cfg, LivewireVersion::V4).as_deref(),
        Some("counter")
    );
}

#[test]
fn reverse_v4_sfc_nested() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join(format!(
        "resources/views/components/admin/{}user-list.blade.php",
        naming::LIVEWIRE_EMOJI
    ));
    write(&path, "<?php new class extends Component {}; ?><div></div>");

    assert_eq!(
        livewire_name_for_path(&path, &cfg, LivewireVersion::V4).as_deref(),
        Some("admin.user-list")
    );
}

#[test]
fn reverse_v4_mfc_class_and_blade_both_resolve() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let dir = root.join(format!(
        "resources/views/components/{}counter",
        naming::LIVEWIRE_EMOJI
    ));
    write(&dir.join("counter.php"), "<?php new class {}; ?>");
    write(&dir.join("counter.blade.php"), "<div></div>");

    assert_eq!(
        livewire_name_for_path(&dir.join("counter.php"), &cfg, LivewireVersion::V4).as_deref(),
        Some("counter")
    );
    assert_eq!(
        livewire_name_for_path(&dir.join("counter.blade.php"), &cfg, LivewireVersion::V4)
            .as_deref(),
        Some("counter")
    );
}

#[test]
fn reverse_v3_class_nested() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("app/Livewire/Admin/UserList.php");
    write(
        &path,
        "<?php namespace App\\Livewire\\Admin; class UserList {}",
    );

    assert_eq!(
        livewire_name_for_path(&path, &cfg, LivewireVersion::V4).as_deref(),
        Some("admin.user-list")
    );
}

#[test]
fn reverse_volt_sfc() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("resources/views/livewire/counter.blade.php");
    write(
        &path,
        "<?php use Livewire\\Volt\\Component; new class extends Component {}; ?><div></div>",
    );

    assert_eq!(
        livewire_name_for_path(&path, &cfg, LivewireVersion::V4).as_deref(),
        Some("counter")
    );
}

#[test]
fn reverse_none_for_non_livewire_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let cfg = config_for(root);
    let path = root.join("app/Models/User.php");
    write(&path, "<?php namespace App\\Models; class User {}");

    assert!(livewire_name_for_path(&path, &cfg, LivewireVersion::V4).is_none());
}

// ---- wire_attribute_target_at --------------------------------------------

#[test]
fn wire_click_method_call() {
    let line = r#"<button wire:click="enterEditMode">Edit</button>"#;
    let cursor = line.find("enterEditMode").unwrap() as u32 + 3;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("enterEditMode".to_string()))
    );
}

#[test]
fn wire_poll_with_modifier_and_method_call_with_args() {
    let line = r#"<div wire:poll.2000ms="checkPrefillStatus"></div>"#;
    let cursor = line.find("checkPrefillStatus").unwrap() as u32;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("checkPrefillStatus".to_string()))
    );
}

#[test]
fn wire_submit_prevent_method_call_with_arguments() {
    let line = r#"<form wire:submit.prevent="save('draft')">"#;
    let cursor = line.find("save(").unwrap() as u32 + 1;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("save".to_string()))
    );
}

#[test]
fn wire_model_targets_the_first_dot_segment_as_a_property() {
    let line = r#"<input wire:model="contractData.title">"#;
    let cursor = line.find("contractData").unwrap() as u32 + 2;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Property("contractData".to_string()))
    );
}

#[test]
fn wire_model_live_modifier_still_targets_the_property() {
    let line = r#"<input wire:model.live.debounce.500ms="filters.search">"#;
    let cursor = line.find("filters").unwrap() as u32 + 1;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Property("filters".to_string()))
    );
}

#[test]
fn wire_click_js_expression_has_no_target() {
    let line = r#"<button wire:click="$wire.count++">+</button>"#;
    let cursor = line.find("count").unwrap() as u32;
    assert!(wire_attribute_target_at(line, cursor).is_none());
}

#[test]
fn cursor_outside_the_quoted_value_has_no_target() {
    let line = r#"<button wire:click="enterEditMode">Edit</button>"#;
    let cursor = line.find("wire:click").unwrap() as u32;
    assert!(wire_attribute_target_at(line, cursor).is_none());
}

#[test]
fn no_wire_attribute_on_line_has_no_target() {
    let line = r#"<button class="btn">Edit</button>"#;
    assert!(wire_attribute_target_at(line, 10).is_none());
}

#[test]
fn wire_completion_context_empty_value_is_method_kind() {
    let line = r#"<button wire:click="">"#;
    let col = line.find('"').unwrap() as u32 + 1;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Method, String::new()))
    );
}

#[test]
fn wire_completion_context_partial_value_carries_prefix() {
    let line = r#"<div wire:poll.2000ms="check">"#;
    let col = (line.find("check").unwrap() + "check".len()) as u32;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Method, "check".to_string()))
    );
}

#[test]
fn wire_completion_context_model_is_property_kind() {
    let line = r#"<input wire:model="contract">"#;
    let col = (line.find("contract").unwrap() + 3) as u32;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Property, "con".to_string()))
    );
}

#[test]
fn wire_completion_context_unclosed_quote_still_matches() {
    let line = r#"<button wire:click="sa"#;
    let col = line.len() as u32;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Method, "sa".to_string()))
    );
}

#[test]
fn wire_completion_context_rejects_expression_values() {
    let line = r#"<button wire:click="$wire.count++">"#;
    let col = (line.find("count").unwrap() + 2) as u32;
    assert_eq!(wire_attribute_completion_context(line, col), None);
}

#[test]
fn wire_completion_context_non_member_attribute_is_none() {
    let line = r#"<div wire:key="row-1">"#;
    let col = (line.find("row").unwrap() + 1) as u32;
    assert_eq!(wire_attribute_completion_context(line, col), None);
}

#[test]
fn wire_target_show_and_text_bind_properties() {
    let line = r#"<span wire:text="prefillStatus">"#;
    let col = (line.find("prefill").unwrap() + 2) as u32;
    assert_eq!(
        wire_attribute_target_at(line, col),
        Some(WireTarget::Property("prefillStatus".to_string()))
    );
}

#[test]
fn wire_completion_context_accepts_dotted_binding_paths() {
    let line = r#"<input wire:model="formData.">"#;
    let col = (line.find("formData.").unwrap() + "formData.".len()) as u32;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Property, "formData.".to_string()))
    );

    let line = r#"<input wire:model="formData.ti">"#;
    let col = (line.find("ti\"").unwrap() + 2) as u32;
    assert_eq!(
        wire_attribute_completion_context(line, col),
        Some((WireValueKind::Property, "formData.ti".to_string()))
    );
}

#[test]
fn wire_completion_context_still_rejects_dotted_action_values() {
    let line = r#"<button wire:click="save.now">"#;
    let col = (line.find("save").unwrap() + 6) as u32;
    assert_eq!(wire_attribute_completion_context(line, col), None);
}

#[test]
fn property_path_prefix_rejects_malformed_paths() {
    assert!(is_property_path_prefix("a.b.c"));
    assert!(is_property_path_prefix("a."));
    assert!(is_property_path_prefix(""));
    assert!(!is_property_path_prefix(".a"));
    assert!(!is_property_path_prefix("a..b"));
    assert!(!is_property_path_prefix("a.b c"));
}

#[test]
fn wire_keydown_enter_is_an_action_binding() {
    // Any DOM-event directive is an action binding, not just click/submit/
    // poll — and the method name need not echo the event name.
    let line = r#"<input wire:keydown.enter="performSearch">"#;
    let cursor = line.find("performSearch").unwrap() as u32 + 2;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("performSearch".to_string()))
    );
}

#[test]
fn wire_submit_with_mismatched_method_name_navigates() {
    let line = r#"<form wire:submit="handleSubmit">"#;
    let cursor = line.find("handleSubmit").unwrap() as u32;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("handleSubmit".to_string()))
    );
}

#[test]
fn wire_change_and_blur_are_action_bindings() {
    for line in [
        r#"<select wire:change="applyFilter">"#,
        r#"<input wire:blur="validateField">"#,
    ] {
        let value_start = line.find('"').unwrap() as u32 + 1;
        assert!(
            matches!(
                wire_attribute_target_at(line, value_start),
                Some(WireTarget::Method(_))
            ),
            "line: {line}"
        );
    }
}

#[test]
fn wire_confirm_value_is_a_message_not_a_member() {
    // Deviation from the issue's example list, deliberately: `wire:confirm`'s
    // value is the confirmation MESSAGE shown to the user, not a method name.
    let line = r#"<button wire:confirm="Delete" wire:click="delete">x</button>"#;
    let cursor = line.find("Delete").unwrap() as u32;
    assert!(wire_attribute_target_at(line, cursor).is_none());
}

#[test]
fn non_wire_prefixed_js_expressions_have_no_target() {
    // The `$wire.`-prefixed case is covered above; these two prove the
    // exclusion is the value GRAMMAR, not a `$wire` special case.
    for line in [
        r#"<button wire:click="count++">+</button>"#,
        r#"<button wire:click="open = true">go</button>"#,
    ] {
        let value_start = line.find('"').unwrap() as u32 + 1;
        assert!(
            wire_attribute_target_at(line, value_start).is_none(),
            "line: {line}"
        );
    }
}

#[test]
fn wire_target_works_with_single_quoted_values() {
    let line = "<button wire:click='enterEditMode'>Edit</button>";
    let cursor = line.find("enterEditMode").unwrap() as u32 + 1;
    assert_eq!(
        wire_attribute_target_at(line, cursor),
        Some(WireTarget::Method("enterEditMode".to_string()))
    );
}

#[test]
fn wire_completion_context_single_quotes_and_unclosed_mid_line() {
    // Single-quote style.
    let line = "<input wire:model='contract";
    let cursor = line.len() as u32;
    assert_eq!(
        wire_attribute_completion_context(line, cursor),
        Some((WireValueKind::Property, "contract".to_string()))
    );
    // Unclosed double quote while the document continues past this line —
    // the value ends at end-of-line for completion purposes.
    let line = r#"    <button wire:click="ent"#;
    let cursor = line.len() as u32;
    assert_eq!(
        wire_attribute_completion_context(line, cursor),
        Some((WireValueKind::Method, "ent".to_string()))
    );
}

// ---- template-local bindings shadow class properties ----------------------

#[test]
fn foreach_loop_variable_is_local_inside_the_loop_only() {
    let content = "\
<div>
@foreach ($users as $user)
    <span>{{ $user->name }}</span>
@endforeach
<span>{{ $user }}</span>
</div>";
    assert!(is_template_local_binding(content, 2, "user"));
    assert!(
        is_template_local_binding(content, 1, "user"),
        "binding and use on the @foreach line itself count as in scope"
    );
    assert!(
        !is_template_local_binding(content, 4, "user"),
        "after @endforeach the loop variable is out of scope"
    );
}

#[test]
fn foreach_key_value_binds_both_names_and_loop() {
    let content = "\
@foreach ($rows as $index => $row)
    {{ $loop->iteration }} {{ $index }} {{ $row }}
@endforeach";
    assert!(is_template_local_binding(content, 1, "row"));
    assert!(is_template_local_binding(content, 1, "index"));
    assert!(is_template_local_binding(content, 1, "loop"));
    assert!(!is_template_local_binding(content, 1, "rows"));
}

#[test]
fn php_block_assignment_persists_after_endphp() {
    let content = "\
@php
    $total = 0;
@endphp
<span>{{ $total }}</span>";
    assert!(
        is_template_local_binding(content, 3, "total"),
        "PHP locals persist in the compiled template scope"
    );
    assert!(!is_template_local_binding(content, 3, "other"));
}

#[test]
fn inline_php_and_for_and_props_bind_locals() {
    let content = "\
@props(['variant', 'size' => 'md'])
@php($discount = 5)
@for ($i = 0; $i < 3; $i++)
    {{ $i }} {{ $variant }} {{ $discount }}
@endfor
{{ $i }}";
    assert!(is_template_local_binding(content, 3, "variant"));
    assert!(is_template_local_binding(content, 3, "size"));
    assert!(is_template_local_binding(content, 3, "discount"));
    assert!(is_template_local_binding(content, 3, "i"));
    assert!(
        !is_template_local_binding(content, 5, "i"),
        "@endfor closes the loop-variable scope"
    );
}

#[test]
fn class_property_reference_is_not_shadowed() {
    let content = "\
<div>
    <span>{{ $prefillStatus }}</span>
</div>";
    assert!(!is_template_local_binding(content, 1, "prefillStatus"));
}

#[test]
fn unclosed_quote_with_cursor_elsewhere_on_the_line_does_not_panic() {
    // Regression: with an unclosed value, the scan's resume point stepped
    // one byte past the end of the line — any cursor NOT inside that value
    // (which returns early) then sliced out of bounds and killed the serve
    // loop. Cursor in `class=""`, half-typed wire:click to its right.
    let line = r#"<div class="" wire:click="save"#;
    assert!(wire_attribute_completion_context(line, 5).is_none());
    assert!(wire_attribute_target_at(line, 5).is_none());
}

#[test]
fn non_ascii_value_with_mid_codepoint_cursor_does_not_panic() {
    // Regression: `position.character` is a UTF-16 code-unit offset, not a
    // byte index — a caret after `prü` arrives as column 22, which lands
    // INSIDE the two-byte `ü` when used as a byte offset and panicked on
    // the slice. The guard (same as every other cursor site) bails out
    // instead of unwinding the server.
    let line = r#"<input wire:model="prüfung">"#;
    assert!(wire_attribute_completion_context(line, 22).is_none());
    assert!(wire_attribute_target_at(line, 22).is_none());
}

#[test]
fn cursor_past_end_of_line_does_not_panic() {
    let line = r#"<input wire:model="title">"#;
    let past = line.len() as u32 + 5;
    assert!(wire_attribute_completion_context(line, past).is_none());
    assert!(wire_attribute_target_at(line, past).is_none());
}

#[test]
fn namespaced_component_resolution_with_negative_controls() {
    // The AC trio for `<livewire:ns::component>`: a component reachable
    // ONLY through a registered namespace resolves; the same lookup with
    // the namespace registration removed fails (negative control); a
    // genuinely missing component under the valid namespace still fails
    // (true negative).
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Outside every default component-discovery root:
    let ns_dir = root.join("app/Common/Ui/app/Livewire");
    std::fs::create_dir_all(&ns_dir).unwrap();
    write(
        &ns_dir.join("Badge.php"),
        "<?php namespace App\\Common\\Ui\\Livewire; use Livewire\\Component; class Badge extends Component {}",
    );

    let mut with_ns = config_for(&root);
    with_ns.class_namespaces.insert(
        "common-ui".to_string(),
        crate::livewire_namespaces::LivewireClassNamespace {
            class_path: ns_dir.clone(),
            class_namespace: "App\\Common\\Ui\\Livewire".to_string(),
        },
    );

    let resolved = resolve_component("common-ui::badge", &with_ns, LivewireVersion::V4)
        .expect("namespace-only component resolves");
    assert!(resolved.paths.contains(&ns_dir.join("Badge.php")));

    let without_ns = config_for(&root);
    assert!(
        resolve_component("common-ui::badge", &without_ns, LivewireVersion::V4).is_none(),
        "negative control: registration removed, resolution fails again"
    );

    assert!(
        resolve_component("common-ui::missing", &with_ns, LivewireVersion::V4).is_none(),
        "true negative: a missing component under the valid namespace stays missing"
    );
}

// ---- @props / @aware bind KEYS, never default values (issue #339, item 8) --

#[test]
fn props_binds_the_key_and_not_its_default_value() {
    let content = "@props(['color' => 'blue'])\n<div>{{ $color }} {{ $blue }}</div>\n";
    assert!(
        is_template_local_binding(content, 1, "color"),
        "the declared prop is locally bound"
    );
    assert!(
        !is_template_local_binding(content, 1, "blue"),
        "a default VALUE is not a prop name"
    );
}

#[test]
fn props_ignores_unrelated_quoted_text_later_on_the_same_line() {
    let content = "@props(['color' => 'blue']) <div title=\"literal\">\n{{ $literal }}\n";
    assert!(is_template_local_binding(content, 1, "color"));
    assert!(
        !is_template_local_binding(content, 1, "blue"),
        "the default value is still excluded"
    );
    assert!(
        !is_template_local_binding(content, 1, "literal"),
        "scanning stops at the directive's closing paren"
    );
}

#[test]
fn props_parses_the_multiline_array_form() {
    let content = "@props([\n    'color' => 'blue',\n    'size',\n])\n<div>{{ $color }}</div>\n";
    assert!(
        is_template_local_binding(content, 4, "color"),
        "a key on a continuation line still binds"
    );
    assert!(
        is_template_local_binding(content, 4, "size"),
        "a bare entry declares a prop with no default"
    );
    assert!(
        !is_template_local_binding(content, 4, "blue"),
        "its default value does not"
    );
}

#[test]
fn aware_binds_keys_the_same_way_as_props() {
    let content = "@aware(['variant' => 'primary'])\n<div>{{ $variant }}</div>\n";
    assert!(is_template_local_binding(content, 1, "variant"));
    assert!(!is_template_local_binding(content, 1, "primary"));
}

#[test]
fn props_with_a_nested_default_array_keeps_the_nesting_out_of_the_names() {
    let content = "@props(['options' => ['a' => 'b'], 'label'])\n<div></div>\n";
    assert!(is_template_local_binding(content, 1, "options"));
    assert!(is_template_local_binding(content, 1, "label"));
    assert!(
        !is_template_local_binding(content, 1, "a"),
        "a nested array's own keys are not props"
    );
    assert!(!is_template_local_binding(content, 1, "b"));
}

// ---- wire:target navigates (issue #339, item 4) ---------------------------

#[test]
fn wire_target_resolves_the_segment_under_the_cursor() {
    let line = r#"<div wire:target="save, delete">"#;
    let save_col = line.find("save").unwrap() as u32;
    let delete_col = line.find("delete").unwrap() as u32;
    assert_eq!(
        wire_attribute_target_at(line, save_col),
        Some(WireTarget::Member("save".to_string())),
        "cursor on the first entry"
    );
    assert_eq!(
        wire_attribute_target_at(line, delete_col),
        Some(WireTarget::Member("delete".to_string())),
        "cursor on the second entry"
    );
}

#[test]
fn wire_target_single_value_is_still_a_member() {
    let line = r#"<div wire:target="save">"#;
    let col = line.find("save").unwrap() as u32;
    assert_eq!(
        wire_attribute_target_at(line, col),
        Some(WireTarget::Member("save".to_string()))
    );
}

#[test]
fn wire_target_completion_prefix_is_the_current_entry() {
    let line = r#"<div wire:target="save, del">"#;
    let cursor = (line.find("del").unwrap() + 3) as u32;
    assert_eq!(
        wire_attribute_completion_context(line, cursor),
        Some((WireValueKind::Member, "del".to_string())),
        "a list completes per entry, not across the whole value"
    );
}

#[test]
fn attributes_that_name_no_member_still_resolve_to_nothing() {
    let line = r#"<div wire:key="row-{{ $id }}" wire:ignore>"#;
    let col = line.find("row-").unwrap() as u32;
    assert_eq!(wire_attribute_target_at(line, col), None);
}

#[test]
fn an_absolute_component_name_cannot_escape_the_class_path() {
    // Regression: `bare` is discovered data — whatever follows `::` in a
    // `<livewire:…>` tag or an `@livewire('…')` literal. Because
    // `Path::join` replaces the base on an absolute right-hand side, an
    // absolute name resolved to a file completely outside the registered
    // class directory and was handed back as a goto-definition target.
    // A dot-free temp prefix matters: `TempDir::new()` names its directory
    // `.tmpXXXX`, and splitting the name on `.` would mangle the probe path
    // before it could escape — making this test pass for the wrong reason.
    let tmp = tempfile::Builder::new().prefix("zz").tempdir().unwrap();
    let root = tmp.path();
    let mut cfg = config_for(root);

    // A decoy outside every configured location.
    let outside = tmp.path().join("outside");
    let decoy = outside.join("Secret.php");
    write(&decoy, "<?php // not yours");

    cfg.class_namespaces.insert(
        "ui".to_string(),
        crate::livewire_namespaces::LivewireClassNamespace {
            class_namespace: "App\\UiKit\\Livewire".to_string(),
            class_path: root.join("app/UiKit/Livewire"),
        },
    );

    let absolute = outside.join("Secret");
    let absolute = absolute.to_string_lossy();

    assert!(
        decoy.is_file(),
        "precondition: the decoy exists, so only the guard can refuse it"
    );
    assert!(
        !absolute.contains('.'),
        "precondition: the probe name must be dot-free, or `split_dotted` mangles \
         it and this test passes without exercising the guard at all"
    );
    assert!(
        resolve_component(&format!("ui::{absolute}"), &cfg, LivewireVersion::V3).is_none(),
        "a namespaced absolute name must not resolve outside the class path"
    );
    assert!(
        resolve_component(&absolute, &cfg, LivewireVersion::V3).is_none(),
        "an un-namespaced absolute name must not resolve outside the class path"
    );
}

#[test]
fn absolute_parent_segments_cannot_escape_via_the_v4_branch() {
    // The V4 SFC/MFC/Volt branch runs BEFORE the class branch and builds its
    // search directory with `parents_to_path`, which uses `PathBuf::push` —
    // and an ABSOLUTE segment replaces the whole path. So gating only the
    // class branch left the first branch wide open: the parent segments of a
    // dotted name could name any directory on disk.
    let proj = tempfile::Builder::new().prefix("zzproj").tempdir().unwrap();
    let ext = tempfile::Builder::new().prefix("zzext").tempdir().unwrap();
    let root = proj.path();
    fs::create_dir_all(root.join("resources/views/livewire")).unwrap();

    // A real V4 SFC sitting entirely outside the project root.
    let outside = ext.path().join("resources/views/livewire");
    let decoy = outside.join(format!("{}secret.blade.php", naming::LIVEWIRE_EMOJI));
    write(
        &decoy,
        "<?php new class extends Component {}; ?><div></div>",
    );

    let cfg = config_for(root);
    let name = format!("{}.secret", outside.display());

    assert!(
        decoy.is_file(),
        "precondition: the decoy resolves if the guard is absent"
    );
    assert!(
        !name.contains(char::is_whitespace),
        "precondition: the probe name is a single component name"
    );
    assert!(
        resolve_component(&name, &cfg, LivewireVersion::V4).is_none(),
        "absolute parent segments must not re-root the V4 component search"
    );
}
