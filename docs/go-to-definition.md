# 🔗 Go-to-Definition

[← Back to README](../README.md)

Navigate your Laravel codebase by Cmd+Clicking (or `Cmd+D`) on any recognized pattern. The extension understands Laravel's conventions and jumps directly to the source file, whether it's a view, component, route, config key, or translation.

```php
class UserController extends Controller
{
    public function show(User $user)
    {
        return view('users.profile', compact('user'));
        //          ^^^^^^^^^^^^^^^ → resources/views/users/profile.blade.php
    }
}
```

```blade
@extends('layouts.app')
{{--      ^^^^^^^^^^^ → resources/views/layouts/app.blade.php --}}

<x-button type="submit">Save</x-button>
{{-- ^^^^ → resources/views/components/button.blade.php --}}

<livewire:user-settings :user="$user" />
{{--       ^^^^^^^^^^^^^ → app/Livewire/UserSettings.php --}}
```

```php
$url = route('users.show', $user);
//           ^^^^^^^^^^^^ → routes/web.php

$name = config('app.name');
//             ^^^^^^^^^^ → config/app.php

$message = __('auth.failed');
//            ^^^^^^^^^^^^ → lang/en/auth.php
```

Cmd+Click also works on **query-chain literals** — columns jump to the migration line that defines them, relations to the relation method on the model, and `DB::table()` names to the create-table migration:

```php
User::where('email', $value)->with('posts');
//          ^^^^^ → database/migrations/..._create_users_table.php  ($table->string('email'))
//                                ^^^^^ → app/Models/User.php  (public function posts())

DB::table('users')->get();
//        ^^^^^ → database/migrations/..._create_users_table.php  (Schema::create('users'))
```

**Eloquent magic members** resolve through the semantic index — the usage jumps to the declaration that actually backs it, even when the names don't match textually:

```php
$user->posts;
//     ^^^^^ → app/Models/User.php  (public function posts(): HasMany)

User::active()->get();
//    ^^^^^^ → app/Models/User.php  (public function scopeActive(...))

$user->full_name;
//     ^^^^^^^^^ → app/Models/User.php  (public function getFullNameAttribute())

$user->email;
//     ^^^^^ → database/migrations/..._create_users_table.php  ($table->string('email'))

User::whereEmail($value);
//   ^^^^^^^^^^ dynamic finder → the email column's migration line

User::factory()->suspended()->create();
//    ^^^^^^^ → database/factories/UserFactory.php  (class UserFactory)
//               ^^^^^^^^^ → database/factories/UserFactory.php  (public function suspended())

$user->pivot;
//     ^^^^^ → app/Models/Pivots/Membership.php  (when the model declares $pivotClass)
```

`Model::factory()` resolves the model to its factory class — a `newFactory()` override when the model declares one, else Laravel's `Database\Factories\…Factory` convention — and chained calls the factory actually declares (custom states, `state`, …) jump to their declaration in the factory file. `->pivot` jumps to a custom pivot class only when the model declares `protected $pivotClass = …::class;` (the framework default `Pivot` is left alone).

Resolution is inheritance- and trait-aware — a member declared in a trait or a parent model jumps to the file that declares it — and chain-aware: `User::query()->active()`, `self::` / `static::` calls, and `$query->active()` inside scope bodies all resolve. Plain properties and plain method calls are left to your PHP language server (no duplicate results), and factory states sharing a scope's name (`User::factory()->active()`) are correctly NOT treated as scopes. Dynamic finders classify against the model's source-visible column surface (`$casts`, `$fillable`, timestamps) — a `$guarded = []` model that declares neither won't resolve its finders. Not resolved (conservatively dropped rather than guessed): `parent::` receivers, `(new User)->active()`, and relation-hopped chains (`$user->posts()->active()` — that's Post's scope).

**Artisan command strings** jump to the `Command` class declaring the matching `protected $signature` — across all four invocation patterns, with app-defined commands taking priority over same-named package/framework commands:

```php
Artisan::call('emails:send');
//             ^^^^^^^^^^^ → app/Console/Commands/SendEmails.php  (protected $signature)

$schedule->command('emails:send --queue')->daily();
//                  ^^^^^^^^^^^ → same — options/arguments after the name are ignored
```

`@use('App\Support\Reader\VerseMarkerResolver')` in a Blade template jumps to the class file — the one Blade directive whose argument is a class rather than a view:

```blade
@use('App\Support\Reader\VerseMarkerResolver')
```

The import also binds the short name for the rest of the template, so `VerseMarkerResolver::class` in a `@php` block resolves to the same class. A group import works member by member — the cursor picks out which of `@use('App\Models\{Flight, Airport}')` you meant. `function` / `const` imports name no class, so no target is offered.

A PHP `use App\Models\Flight;` statement jumps to the same place — the import sites are symmetric across the two file types.

**Third-party Blade directives** resolve too. A package that registers its own view-rendering directive through `Blade::directive()` is on no built-in list, so its first quoted argument is treated as a view name and offered as a target when — and only when — that view file actually exists inside the project root:

```blade
@renderPartial('dashboard.summary')
{{--             ^^^^^^^^^^^^^^^^^ → resources/views/dashboard/summary.blade.php --}}
```

Directives whose argument is a *label* rather than a view — `@section`, `@hasSection`, `@yield`, `@stack`, `@push`, and the standard control-flow set — are excluded, so `@section('content')` never jumps to `resources/views/content.blade.php` just because that file happens to exist.

This applies to go-to-definition only. The "View file not found" diagnostic still validates just `@extends` and `@include`, so a directive resolved this way can never produce a false squiggle.

Two escape hatches cover what the heuristic can't infer — a directive that takes a view but is on the excluded list, or one whose view name is the *second* argument (after a condition, like `@includeWhen`):

```jsonc
"blade": {
  "viewDirectives": {
    "firstArg": ["renderPartial"],
    "secondArg": ["renderWhen"]
  }
}
```

Names with dedicated handling (`@component`, `@livewire`, `@feature`, `@includeFirst`, `@extends`, `@include`, `@includeIf`, `@each`, `@includeWhen`, `@includeUnless`) ignore both lists — their own resolution always wins. See [Configuration](configuration.md).

**Component members in Blade** — inside a template backed by a component class (a Livewire component in any format, a class-based Volt component, or a Filament `$view`-property page), `$this->member`, bare `$variable` references, and `wire:` attribute values all jump to the member's declaration in the backing class. For a Livewire v4 single-file component or a class-based Volt component the class lives in the template's own front matter, so the jump lands inside the `.blade.php` itself:

```blade
<button wire:click="enterEditMode">Edit</button>
{{--                ^^^^^^^^^^^^^ → app/Livewire/ContractPage.php  (public function enterEditMode()) --}}

<input wire:model.live="contractData.title">
{{--                    ^^^^^^^^^^^^ → app/Livewire/ContractPage.php  (public ContractData $contractData) --}}

<input wire:keydown.enter="performSearch">
{{--                       ^^^^^^^^^^^^^ any DOM-event directive works, not just click/submit/poll --}}

{{ $this->getCalculatedEndDateForDisplay() }}
{{--      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ → the method declaration in the backing class --}}

{{ $prefillStatus }}
{{-- ^^^^^^^^^^^^ → public string $prefillStatus in the backing class --}}
```

A `wire:` value that isn't a plain member reference (`$wire.count++`, `count++`, `open = true`) is left alone entirely, so nothing conflicts with Alpine. A bare `$variable` bound locally in the template first — an enclosing `@foreach`/`@for` loop variable, a `@php` assignment, a component `@props` entry, or Blade's own `$loop` — is NOT treated as a class member: local scope wins and no navigation is offered. Members declared in the component's own class file resolve (front matter included), as do those of a trait it `use`s in that same file. Traits declared in another file, and parent-class members, are a known limitation — they do not resolve.

## Env keys, in reverse

Go-to-definition on a key declaration inside a `.env*` buffer jumps *forwards*,
to the `env('KEY')` call sites that consume it — the mirror of `env('APP_NAME')`
in PHP jumping back to the declaring `.env` line. More than one consumer returns
all of them and your editor offers a picker; a key nothing consumes has nowhere
to jump, so nothing happens (its hover card still works, and still says
`0 references`).

```
APP_NAME=Acme
^^^^^^^^ go-to-definition → config/app.php  ('name' => env('APP_NAME', 'Laravel'))
```

Explicit *Find All References* stays a `.php`/`.blade.php` action. Inside a
`.env*` buffer the reference [code lens](code-lens.md) above each key is the
way to see every consumer at once — the same entry point config and translation
keys use.

**Supported patterns:**
`view()` `View::make()` `@extends` `@include` `@includeIf` `@includeWhen` `@includeUnless` `@includeFirst` `@each` `@component` custom `Blade::directive()` view directives `@use` `<x-*>` `</x-*>` `<livewire:*>` `</livewire:*>` `@livewire()` `route()` `to_route()` `signed_route()` `URL::signedRoute()` `config()` `Config::get()` `Config::getMany()` `config()->string()` `env()` `Env::get()` `__()` `trans()` `@lang` `->middleware()` `app()` `resolve()` `App::bound()` `App::isShared()` `asset()` `@vite` `app_path()` `base_path()` `storage_path()` `resource_path()` `public_path()` `Feature::active()` `Feature::inactive()` `Feature::value()` `@feature` `Artisan::call()` `Artisan::queue()` `->command()` `->artisan()` · query-chain columns / relations / tables · magic members (relationships, scopes, accessors, columns, dynamic finders) · `wire:*` attribute values · `$this->member` / bare `$variable` in component-backed Blade
