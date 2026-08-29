use super::*;

const REGISTRARS: &[&str] = &["loadLivewireComponentsFrom"];

fn registrars() -> Vec<String> {
    REGISTRARS.iter().map(|s| s.to_string()).collect()
}

fn module_layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let provider_path = root.join("app/Common/UI/app/Providers/AppServiceProvider.php");
    std::fs::create_dir_all(provider_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("app/Common/UI/app/Livewire")).unwrap();
    (tmp, root, provider_path)
}

#[test]
fn extracts_wrapper_registrar_call_deriving_namespace_from_file() {
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use App\Base\Providers\AbstractModuleServiceProvider;

class AppServiceProvider extends AbstractModuleServiceProvider
{
    public function boot(): void
    {
        $this->loadLivewireComponentsFrom(__DIR__.'/../Livewire', 'common-ui');
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    let reg = map.get("common-ui").expect("common-ui registered");
    assert_eq!(reg.class_namespace, "App\\Common\\UI\\Livewire");
    assert_eq!(
        reg.class_path,
        root.join("app/Common/UI/app/Livewire")
            .canonicalize()
            .unwrap()
    );
}

#[test]
fn extracts_direct_add_namespace_named_arguments_path_first() {
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace(
            classPath: __DIR__.'/../Livewire',
            namespace: 'common-ui',
            classNamespace: 'App\\Common\\UI\\Livewire',
        );
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    let reg = map.get("common-ui").expect("common-ui registered");
    assert_eq!(reg.class_namespace, "App\\Common\\UI\\Livewire");
    assert_eq!(
        reg.class_path,
        root.join("app/Common/UI/app/Livewire")
            .canonicalize()
            .unwrap()
    );
}

#[test]
fn extracts_direct_add_namespace_positional() {
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php
namespace App\Common\UI\Providers;
use Livewire\Livewire;
class AppServiceProvider {
    public function boot(): void {
        Livewire::addNamespace('common-ui', 'App\\Common\\UI\\Livewire', __DIR__.'/../Livewire');
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    assert_eq!(
        map.get("common-ui").unwrap().class_namespace,
        "App\\Common\\UI\\Livewire"
    );
}

#[test]
fn skips_calls_with_variable_arguments() {
    let (_tmp, root, provider_path) = module_layout();
    // The abstract base class's own forwarding call — every argument is a
    // variable or expression, statically unresolvable, must not register.
    let source = r#"<?php
namespace App\Base\Providers;
use Livewire\Livewire;
abstract class AbstractModuleServiceProvider {
    protected function loadLivewireComponentsFrom(string $path, string $prefix = ''): void {
        Livewire::addNamespace(
            namespace: $prefix,
            classNamespace: Str::beforeLast(static::class, '\\Providers\\').'\\Livewire',
            classPath: $path,
        );
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    assert!(map.is_empty());
}

#[test]
fn ignores_unlisted_wrapper_methods() {
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php
namespace App\Common\UI\Providers;
class AppServiceProvider {
    public function boot(): void {
        $this->someOtherLoader(__DIR__.'/../Livewire', 'common-ui');
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    assert!(map.is_empty());
}

#[test]
fn module_root_namespace_variants() {
    assert_eq!(
        module_root_namespace("App\\Common\\UI\\Providers"),
        "App\\Common\\UI"
    );
    assert_eq!(
        module_root_namespace("App\\Common\\UI\\Providers\\Nested"),
        "App\\Common\\UI"
    );
    assert_eq!(
        module_root_namespace("App\\NoProviders"),
        "App\\NoProviders"
    );
}

#[test]
fn extracts_direct_add_namespace_named_arguments_namespace_first() {
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace(
            namespace: 'common-ui',
            classPath: __DIR__.'/../Livewire',
            classNamespace: 'App\\Common\\UI\\Livewire',
        );
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    let reg = map.get("common-ui").expect("common-ui registered");
    assert_eq!(reg.class_namespace, "App\\Common\\UI\\Livewire");
}

#[test]
fn unknown_named_argument_skips_the_argument_not_the_call() {
    // `lazy: true` (or any parameter a future Livewire adds) must not
    // silently disable the whole namespace registration.
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace(
            namespace: 'common-ui',
            classNamespace: 'App\\Common\\UI\\Livewire',
            classPath: __DIR__.'/../Livewire',
            lazy: true,
        );
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    assert!(
        map.contains_key("common-ui"),
        "one unknown flag must not drop the registration: {map:?}"
    );
}

#[test]
fn within_a_file_the_last_registration_wins() {
    // PHP executes both statements; Livewire's registry keeps the LATER
    // one — the extractor must agree.
    let (_tmp, root, provider_path) = module_layout();
    std::fs::create_dir_all(root.join("app/Common/UI/app/Alt")).unwrap();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace('common-ui', 'App\\Common\\UI\\Livewire', __DIR__.'/../Livewire');
        Livewire::addNamespace('common-ui', 'App\\Common\\UI\\Alt', __DIR__.'/../Alt');
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    let reg = map.get("common-ui").expect("registered");
    assert_eq!(
        reg.class_namespace, "App\\Common\\UI\\Alt",
        "the later statement overwrites the earlier one"
    );
}

#[test]
fn a_skipped_positional_still_consumes_its_slot() {
    // An unusable leading positional (`$variable`) must not shift the later
    // literals one parameter to the left — the namespace would silently
    // take the class-namespace's value.
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

use Livewire\Livewire;

class AppServiceProvider
{
    public function boot(): void
    {
        Livewire::addNamespace($dynamicPrefix, 'App\\Common\\UI\\Livewire', __DIR__.'/../Livewire');
    }
}
"#;
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &registrars());
    assert!(
        map.is_empty(),
        "the literals stay in their own slots, so nothing resolves: {map:?}"
    );
}

#[test]
fn a_custom_registrar_wrapper_name_is_honored() {
    // Proves `modules.livewireRegistrars` is consulted rather than the
    // extractor recognizing a hardcoded default: this name ships nowhere.
    let (_tmp, root, provider_path) = module_layout();
    let source = r#"<?php

namespace App\Common\UI\Providers;

class AppServiceProvider
{
    public function boot(): void
    {
        $this->registerModuleLivewire(__DIR__.'/../Livewire', 'common-ui');
    }
}
"#;
    let configured = vec!["registerModuleLivewire".to_string()];
    let map = extract_livewire_namespaces(source, &provider_path, &root, None, &configured);
    assert!(
        map.contains_key("common-ui"),
        "the configured wrapper name is recognized: {map:?}"
    );

    let defaults = registrars();
    assert!(
        !extract_livewire_namespaces(source, &provider_path, &root, None, &defaults)
            .contains_key("common-ui"),
        "negative control: an unconfigured wrapper name registers nothing"
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_class_path_yields_no_registration() {
    // #354 item 1 regression: gating with `path_within_root_lexical` admitted
    // any in-root path it could not canonicalize, so a DANGLING under-root
    // symlink minted a registration whose real target is unprovable. The class
    // path is walked and read downstream (the component-completion walk,
    // `try_namespaced_class`), so a target created later could resolve outside
    // the module — issues #134/#155. It must fail closed.
    let (_tmp, root, provider_path) = module_layout();
    let module_dir = root.join("app/Common/UI");
    let dangling = module_dir.join("app/Dangling");
    std::os::unix::fs::symlink(module_dir.join("NEVER_CREATED"), &dangling).unwrap();

    assert!(
        dangling.canonicalize().is_err(),
        "precondition: the link dangles, so it cannot be canonicalized"
    );

    let source = r#"<?php

namespace App\Common\UI\Providers;

class AppServiceProvider
{
    public function boot(): void
    {
        $this->loadLivewireComponentsFrom(__DIR__.'/../Dangling', 'common-ui');
    }
}
"#;
    let map = extract_livewire_namespaces(
        source,
        &provider_path,
        &root,
        Some(&module_dir),
        &registrars(),
    );

    assert!(
        !map.contains_key("common-ui"),
        "a dangling under-root symlink is unverifiable and must yield no registration"
    );
}

#[test]
fn a_genuinely_absent_class_path_yields_no_registration() {
    // The guard is fail-closed, not merely dangling-aware: a class path with
    // nothing on disk proves nothing about where it will later resolve, so it
    // is refused too. This is where `path_within_root_registration` parts
    // company with `path_within_root_emit_safe`, which admits an absent path
    // because a *create target* legitimately does not exist yet.
    let (_tmp, root, provider_path) = module_layout();
    let module_dir = root.join("app/Common/UI");

    let source = r#"<?php

namespace App\Common\UI\Providers;

class AppServiceProvider
{
    public function boot(): void
    {
        $this->loadLivewireComponentsFrom(__DIR__.'/../NotCreatedYet', 'common-ui');
    }
}
"#;
    let map = extract_livewire_namespaces(
        source,
        &provider_path,
        &root,
        Some(&module_dir),
        &registrars(),
    );

    assert!(
        !map.contains_key("common-ui"),
        "an absent class path yields no registration — the gate fails closed"
    );
}
