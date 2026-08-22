# 🔍 Find References

[← Back to README](../README.md)

Right-click any recognised pattern and choose **"Find All References"** (or `Shift+F12`) to surface every call site across the project — including inside installed Composer packages.

```php
$url = route('users.index', $user);
//           ^^^^^^^^^^^^^ Find references here →
//
//   📂 routes/web.php:42         ->name('users.index')
//   📂 app/Http/Controllers/UserController.php:67   redirect()->route('users.index')
//   📂 resources/views/nav.blade.php:12   <a href="{{ route('users.index') }}">
//   📂 vendor/some/package/src/Helpers.php:8   route('users.index')
```

**Pattern kinds covered:**

| Kind | Example | Where references come from |
|---|---|---|
| Views | `view('users.profile')` | every `view()` / `View::make()` / `@extends` / `@include` call site |
| Routes | `route('home')` | every `route()` call + the `->name(...)` declaration |
| Configs | `config('app.name')` | every `config()` / `Config::get()` call + the array-key in the source config file |
| Translations | `__('messages.key')` | every `__()` / `trans()` / `@lang` call + the array-key in every locale's lang file |
| Env vars | `env('APP_NAME')` | every `env()` call site |
| Blade components | `<x-button>` | every `<x-button>` opening/closing tag, including tags built as PHP string literals |
| Livewire | `<livewire:counter>` | tags AND `@livewire('counter')` directives, including tags built as PHP string literals |
| Middleware | `'auth'` | every `->middleware()` registration |
| Bindings | `app('cache')` | every `app()` / `resolve()` resolution |
| Classes | `use App\Models\Flight;` | every PHP `use` statement and Blade `@use` importing that class |
| Magic members | `$user->posts`, `->active()` | every property-form and call-form usage (PHP + Blade) of a relationship / scope / accessor / column / dynamic finder |

**Eloquent magic is first-class.** Find references on a usage site — `$user->posts`, a `->active()` or `User::active()` scope call, `$model->full_name`, `$user->email`, a `User::whereEmail()` dynamic finder — and every indexed usage of that member surfaces, in PHP and Blade alike. Property reads and method calls of the same member key together (`$user->posts` and `$user->posts()` are one reference set), and resolution is chain-aware: `User::query()->active()`, `User::where(…)->active()`, `self::` / `static::` calls, and `$query->active()` inside scope bodies all count as usages. The usage is resolved through the semantic index to its declaring class (inheritance- and trait-aware), so the results are keyed by what the member *is*, not what the string looks like — a factory state sharing a scope's name (`User::factory()->active()`) is correctly **not** a reference. These are exactly the sites a generic PHP language server can't see through `__get` / `__call`. For the same counts anchored on the *declaration* side (the `posts()` method itself), see [Code Lens](code-lens.md) — each lens is click-to-open into this reference list.

**Config and translation keys are chain-aware.** Find references on an intermediate key — the `redshift_sync` array in `config/reporting.php` — and every call that reaches *through* it (`config('reporting.redshift_sync.enabled')`, `config('reporting.redshift_sync.export_connection')`) is returned, not just verbatim fetches of the parent. Simple dynamic keys participate too: `config("{$config}.export_connection")` resolves to the real key when `$config` is a single same-scope string-literal assignment; anything less certain (properties, parameters, reassignments) is skipped rather than guessed.

**Blade markup built in PHP counts.** Laravel code often assembles a component tag as a string and renders it later — a job rewriting text into `"<x-reader.cross-reference :id=\"{$id}\" />"`, a mailer building `'<x-mail::button>'`. Those tags are found inside PHP string literals (single-quoted, double-quoted, heredoc, nowdoc) and count as references, so a component used only this way is no longer reported as unreferenced. A tag whose name is interpolated (`"<x-alert-{$type} />"`) names no single component and is skipped rather than guessed at, and a tag merely mentioned in a comment or docblock is not a literal and never counts.

**Class references are import sites, in PHP and Blade alike.** A PHP `use App\Models\Flight;` and a Blade `@use('App\Models\Flight')` both index as references to the same class, so find-references from either one reaches the other. Grouped imports contribute one reference per member on both sides — `use App\Models\{Flight, Airport};` and `@use('App\Models\{Flight, Airport}')` alike — each positioned on its own name. A Volt single-file component's `<?php … ?>` front matter counts too: those are ordinary PHP `use` statements. `function` / `const` imports bind no class and are skipped.

**Import sites, not every usage.** This index holds the places a class is *imported* — not `::class`, `new`, type hints, or docblock references. Those are a general PHP language server's job, and the extension's policy is to suppress duplicates at the source rather than half-cover them. Rename is the exception: F2 on any import site runs the project-wide class rename, which *does* rewrite every reference shape and moves the declaring file. See [Rename](rename.md).

**Parser-classified guarantee:** a coincidental string `'home'` sitting in an unrelated PHP literal is never returned. Only positions the parser has classified as the matching pattern kind appear in results. The LSP `includeDeclaration` flag is honoured — declaration sites (route names, config-key array entries, translation-key array entries) are included or excluded as the client asks.
