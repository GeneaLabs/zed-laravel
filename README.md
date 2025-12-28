<p align="center">
  <img src="https://raw.githubusercontent.com/laravel/art/master/logo-lockup/5%20SVG/2%20CMYK/1%20Full%20Color/laravel-logolockup-cmyk-red.svg" width="300" alt="Laravel">
</p>

<h1 align="center">Laravel for Zed</h1>

<p align="center">
  <strong>Cmd+Click your way through Laravel projects</strong><br>
  Views • Components • Livewire • Routes • Config • Translations • and more
</p>

<p align="center">
  <a href="#-features">Features</a> •
  <a href="#-diagnostics">Diagnostics</a> •
  <a href="#-quick-actions">Quick Actions</a> •
  <a href="#%EF%B8%8F-configuration">Configuration</a> •
  <a href="#-roadmap">Roadmap</a>
</p>

---

## What You Get

| Feature | What it does |
|---------|--------------|
| **Go-to-Definition** | Cmd+Click on `view('welcome')`, `<x-button>`, `config('app.name')`, etc. to jump to the source |
| **Diagnostics** | Real-time warnings when views, components, or translations don't exist |
| **Quick Actions** | One-click file creation for missing views, components, middleware, and more |

---

## Installation

<details>
<summary><strong>From Zed Extensions</strong> (Coming Soon)</summary>

Search for "Laravel" in Zed's extension panel.

</details>

<details>
<summary><strong>From Source</strong></summary>

```bash
git clone https://github.com/GeneaLabs/zed-laravel.git
cd zed-laravel
cargo build --release
```

Then in Zed: `Cmd+Shift+P` → "zed: install dev extension" → select the `zed-laravel` directory.

</details>

---

## ✨ Features

Cmd+Click (or Cmd+D) on any of these patterns to jump directly to the source file.

<details>
<summary><strong>Views</strong> — <code>view()</code>, <code>View::make()</code>, <code>Route::view()</code></summary>

```php
return view('users.profile', ['user' => $user]);
//           ^^^^^^^^^^^^^^ Cmd+Click → resources/views/users/profile.blade.php

View::make('dashboard');
Route::view('/welcome', 'welcome');
```

</details>

<details>
<summary><strong>Blade Components</strong> — <code>&lt;x-*&gt;</code> tags</summary>

```blade
<x-button type="submit">Save</x-button>
{{-- ^^^^^^ Cmd+Click → app/View/Components/Button.php --}}

<x-forms.input name="email" />
{{-- ^^^^^^^^^^^ Cmd+Click → app/View/Components/Forms/Input.php --}}
```

</details>

<details>
<summary><strong>Livewire</strong> — <code>&lt;livewire:*&gt;</code> and <code>@livewire()</code></summary>

```blade
<livewire:user-profile :user="$user" />
{{-- ^^^^^^^^^^^^ Cmd+Click → app/Livewire/UserProfile.php --}}

<livewire:admin.dashboard />
@livewire('counter')
```

</details>

<details>
<summary><strong>Blade Directives</strong> — <code>@extends</code>, <code>@include</code>, <code>@section</code></summary>

```blade
@extends('layouts.app')
{{-- ^^^^^^^^^^^ Cmd+Click → resources/views/layouts/app.blade.php --}}

@include('partials.header')
@section('content')
```

</details>

<details>
<summary><strong>Config</strong> — <code>config()</code></summary>

```php
$appName = config('app.name');
//                 ^^^^^^^^ Cmd+Click → config/app.php

$driver = config('database.default');
$mailHost = config('mail.mailers.smtp.host');
```

</details>

<details>
<summary><strong>Environment</strong> — <code>env()</code></summary>

```php
$name = env('APP_NAME', 'Laravel');
//          ^^^^^^^^ Cmd+Click → .env (jumps to the line)

$debug = env('APP_DEBUG', false);
```

</details>

<details>
<summary><strong>Routes</strong> — <code>route()</code>, <code>to_route()</code>, <code>URL::route()</code></summary>

```php
$url = route('users.show', $user);
//           ^^^^^^^^^^^ Cmd+Click → routes/web.php (at the definition)

return redirect()->route('dashboard');
return to_route('login');
```

</details>

<details>
<summary><strong>Translations</strong> — <code>__()</code>, <code>trans()</code>, <code>@lang</code></summary>

```php
$message = __('auth.failed');
//             ^^^^^^^^^^^ Cmd+Click → lang/en/auth.php

trans('messages.welcome');
Lang::get('validation.required');
```

```blade
{{ __('Welcome to our app') }}
@lang('messages.greeting')
```

</details>

<details>
<summary><strong>Middleware</strong> — Route middleware aliases</summary>

```php
Route::middleware('auth')->group(function () {
//                 ^^^^ Cmd+Click → app/Http/Middleware/Authenticate.php
});

Route::middleware(['auth', 'verified'])->get('/dashboard', ...);
```

</details>

<details>
<summary><strong>Service Container</strong> — <code>app()</code>, <code>resolve()</code></summary>

```php
$cache = app('cache');
//           ^^^^^^^ Cmd+Click → finds where 'cache' is bound

$payment = app(PaymentGateway::class);
$service = resolve(UserService::class);
```

</details>

<details>
<summary><strong>Assets</strong> — <code>asset()</code>, <code>mix()</code>, <code>@vite</code></summary>

```php
$css = asset('css/app.css');
//           ^^^^^^^^^^^^ Cmd+Click → public/css/app.css

$js = mix('js/app.js');
```

```blade
@vite(['resources/css/app.css', 'resources/js/app.js'])
{{--   ^^^^^^^^^^^^^^^^^^^^^^^ Each path is clickable --}}
```

</details>

<details>
<summary><strong>Path Helpers</strong> — <code>app_path()</code>, <code>base_path()</code>, etc.</summary>

```php
$public = public_path('assets/logo.png');
$storage = storage_path('logs/laravel.log');
$app = app_path('Models/User.php');
$base = base_path('routes/api.php');
$database = database_path('seeders/UserSeeder.php');
$resource = resource_path('views/welcome.blade.php');
$config = config_path('app.php');
$lang = lang_path('en/messages.php');
```

</details>

---

## 🔍 Diagnostics

Real-time validation as you type. Missing files show inline warnings so you catch issues before running your app.

<details>
<summary><strong>Missing Views</strong></summary>

```php
return view('users.missing');
//          ^^^^^^^^^^^^^^^ ⚠️ View file not found: 'users.missing'
//                             Expected at: resources/views/users/missing.blade.php
```

```blade
@extends('layouts.missing')  {{-- ⚠️ Layout not found --}}
@include('partials.undefined')  {{-- ⚠️ Partial not found --}}
```

</details>

<details>
<summary><strong>Missing Components</strong></summary>

```blade
<x-undefined-component />
{{-- ⚠️ Blade component not found: 'undefined-component'
        Expected at: resources/views/components/undefined-component.blade.php --}}

<livewire:missing-component />
{{-- ⚠️ Livewire component not found: 'missing-component'
        Expected at: app/Livewire/MissingComponent.php --}}
```

</details>

<details>
<summary><strong>Undefined Environment Variables</strong></summary>

```php
$key = env('UNDEFINED_VAR');
//         ^^^^^^^^^^^^^ ⚠️ No fallback provided - will return null if not set

$key = env('UNDEFINED_VAR', 'default');  // ✅ Has fallback, safe
```

</details>

<details>
<summary><strong>Invalid Middleware</strong></summary>

```php
Route::middleware('undefined-middleware')->group(...);
//                ^^^^^^^^^^^^^^^^^^^^^^ ⚠️ Middleware 'undefined-middleware' not found
//                                          Expected at: app/Http/Middleware/UndefinedMiddleware.php
```

</details>

<details>
<summary><strong>Missing Translations</strong></summary>

```php
$msg = __('undefined.key');
//         ^^^^^^^^^^^^^ ⚠️ Translation not found

@lang('undefined.message')  {{-- ⚠️ Translation not found --}}
```

</details>

<details>
<summary><strong>Undefined Bindings</strong></summary>

```php
$service = app('undefined-service');
//             ^^^^^^^^^^^^^^^^^^^ ⚠️ Container binding 'undefined-service' not found
//                                    Define in a service provider's register() method
```

</details>

<details>
<summary><strong>Missing Assets</strong></summary>

```php
$css = asset('css/missing.css');  // ⚠️ Asset file not found

@vite(['resources/css/missing.css'])  {{-- ⚠️ Vite asset not found --}}
```

</details>

---

## ⚡ Quick Actions

See a warning? Press `Cmd+.` or click the lightning icon to instantly create the missing file.

<details>
<summary><strong>Create Views</strong></summary>

```php
return view('users.profile');
//          ^^^^^^^^^^^^^^^ ⚠️ View file not found
//                          ⚡ "Create view: users.profile"
//                          → Creates resources/views/users/profile.blade.php
```

</details>

<details>
<summary><strong>Create Blade Components</strong></summary>

```blade
<x-button>Click me</x-button>
{{-- ⚠️ Component not found
     ⚡ "Create component: button"
        → resources/views/components/button.blade.php
     ⚡ "Create component with class: button"
        → resources/views/components/button.blade.php
        → app/View/Components/Button.php
--}}
```

</details>

<details>
<summary><strong>Create Livewire Components</strong></summary>

```blade
<livewire:counter />
{{-- ⚠️ Livewire component not found
     ⚡ "Create Livewire: counter"
        → app/Livewire/Counter.php
        → resources/views/livewire/counter.blade.php
--}}
```

</details>

<details>
<summary><strong>Create Middleware</strong></summary>

```php
Route::middleware('custom')->group(...);
//                ^^^^^^^^ ⚠️ Middleware not found
//                         ⚡ "Create middleware: custom"
//                         → app/Http/Middleware/Custom.php
```

</details>

<details>
<summary><strong>Create Translations</strong></summary>

```php
__('messages.welcome');
// ⚠️ Translation not found
// ⚡ "Create translation: messages.welcome" → lang/en/messages.php
// ⚡ "Add translation: messages.welcome" (if file exists)
```

</details>

<details>
<summary><strong>Create Config</strong></summary>

```php
config('custom.setting');
// ⚠️ Config not found
// ⚡ "Create config: custom.setting" → config/custom.php
```

</details>

<details>
<summary><strong>Create Environment Variables</strong></summary>

```php
env('CUSTOM_KEY');
// ⚠️ Environment variable not found
// ⚡ "Add env var: CUSTOM_KEY" (if .env exists)
// ⚡ "Create .env with CUSTOM_KEY" (if .env missing)
// ⚡ "Copy .env.example to .env" (if .env.example exists)
```

</details>

---

## ⚙️ Configuration

**Zero config required.** The extension automatically discovers your Laravel project structure:

- View paths from `config/view.php`
- Component namespaces from `composer.json`
- Middleware aliases from `bootstrap/app.php` or `app/Http/Kernel.php`
- Service bindings from your providers

<details>
<summary><strong>Optional: Tune Performance</strong></summary>

Add to your Zed `settings.json` if you want to adjust the diagnostic update timing:

```json
{
  "lsp": {
    "laravel-lsp": {
      "settings": {
        "laravel": {
          "debounceMs": 200
        }
      }
    }
  }
}
```

| Setting | Default | Description |
|---------|---------|-------------|
| `debounceMs` | `200` | How long to wait after you stop typing before updating diagnostics |

**When to adjust:**

| Value | When to use |
|-------|-------------|
| **50-100ms** | Fast machine, want instant feedback |
| **200ms** *(default)* | Good balance — skips brief pauses mid-thought, feels instant when you stop to read |
| **300-500ms** | Slower machine or large project, reduce CPU during quick pauses |

</details>

---

## 🚀 Roadmap

<details>
<summary><strong>Auto-Completion</strong> (Planned)</summary>

- [ ] Route names: `route('█')`
- [ ] Config keys: `config('█')`
- [ ] Translation keys: `__('█')`
- [ ] Component names: `<x-█`
- [ ] Validation rules
- [ ] Eloquent fields and relationships

</details>

<details>
<summary><strong>Hover Information</strong> (Planned)</summary>

- [ ] Show actual `.env` values
- [ ] Show resolved config values
- [ ] Links to Laravel docs

</details>

<details>
<summary><strong>More Framework Support</strong> (Planned)</summary>

- [ ] Inertia.js: `Inertia::render('Page')`
- [ ] Folio page routing
- [ ] Volt components

</details>

---

## Requirements

- [Zed Editor](https://zed.dev)
- A Laravel project (auto-detected via `composer.json`)

---

## Contributing

<details>
<summary><strong>Development Setup</strong></summary>

```bash
# Build the LSP server
cd laravel-lsp && cargo build --release

# Run tests
cargo test

# Reload in Zed
Cmd+Shift+P → "zed: reload extensions"
```

**Project Structure:**

```
zed-laravel/
├── src/                    # Zed extension (Rust → WASM)
├── laravel-lsp/            # Language server (Rust)
│   ├── src/
│   │   ├── main.rs         # LSP handlers
│   │   ├── queries.rs      # Pattern extraction
│   │   ├── parser.rs       # Tree-sitter parsing
│   │   └── config.rs       # Project discovery
│   └── queries/            # Tree-sitter query files
└── extension.toml          # Extension manifest
```

</details>

**Areas of interest:**

- New Laravel patterns (Inertia, Folio, Volt)
- Auto-completion
- More diagnostics
- Performance improvements

---

## License

MIT
