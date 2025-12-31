<p align="center">
  <img src="docs/logo.svg" width="128" height="128" alt="Laravel for Zed">
</p>

<h1 align="center">Laravel for Zed</h1>

<p align="center">
<strong>Cmd+Click your way through Laravel projects</strong>
</p>

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/actions/workflows/release.yml"><img src="https://github.com/GeneaLabs/zed-laravel/actions/workflows/release.yml/badge.svg" alt="Build Status"></a>
<a href="https://github.com/GeneaLabs/zed-laravel/releases"><img src="https://img.shields.io/github/v/release/GeneaLabs/zed-laravel?label=version" alt="Latest Release"></a>
<img src="https://img.shields.io/github/downloads/GeneaLabs/zed-laravel/total" alt="Downloads">
<img src="https://img.shields.io/github/stars/GeneaLabs/zed-laravel?style=flat" alt="GitHub Stars">
</p>

<p align="center">
<img src="https://img.shields.io/badge/Laravel-FF2D20?logo=laravel&logoColor=white" alt="Laravel">
<img src="https://img.shields.io/badge/Zed-Extension-8B5CF6" alt="Zed Extension">
<img src="https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white" alt="Rust">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
<sub>A community extension — not affiliated with Laravel LLC</sub>
</p>

---

## Install

Search **"Laravel"** in Zed Extensions and click Install.

**From source:** Clone the repo, run `cargo build --release` in `laravel-lsp/`, then use "zed: install dev extension".

## Configuration

The extension works out of the box with zero configuration. It automatically discovers your Laravel project structure, including view paths, component namespaces, route files, and service providers.

**Optional settings** can be added to your Zed `settings.json`:

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
| `debounceMs` | `200` | Delay before diagnostics update after typing. Lower values (50-100ms) give faster feedback on fast machines. Higher values (300-500ms) reduce CPU usage on slower machines or large projects. |

**Database autocomplete** (`exists:`, `unique:` rules) requires a working database connection. Configure in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

## Features

### Go-to-Definition

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

**Supported patterns:**
`view()` `View::make()` `@extends` `@include` `@component` `<x-*>` `<livewire:*>` `@livewire()` `route()` `to_route()` `config()` `Config::get()` `env()` `__()` `trans()` `@lang` `->middleware()` `app()` `resolve()` `asset()` `@vite` `app_path()` `base_path()` `storage_path()` `resource_path()` `public_path()`

### Autocomplete

Get intelligent suggestions as you type. The extension provides context-aware completions for validation rules, database schemas, config keys, routes, translations, and environment variables. Completions include helpful metadata like resolved values and source file locations.

```php
$request->validate([
    'email' => 'required|email|exists:',
    //                               ^ database tables appear here

    'email' => 'required|email|exists:users,',
    //                                     ^ column names appear here

    'name' => 'required|',
    //                  ^ 90+ validation rules appear here
]);

$name = config('app.');
//                  ^ config keys with resolved values

$url = route('users.');
//                  ^ named routes from routes/*.php

$message = __('auth.');
//                  ^ translation keys with values
```

### Diagnostics

See problems in real-time as you type. The extension validates your Laravel code against your actual project structure, highlighting missing views, undefined components, invalid validation rules, and other issues before you run your application.

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ⚠️ View not found: resources/views/users/dashboard.blade.php

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ⚠️ Middleware not found

$request->validate([
    'email' => 'required|emal|unique:users',
    //                   ^^^^ ❌ Unknown validation rule: 'emal'
]);
```

```blade
<x-dashboard-widget />
{{-- ^^^^^^^^^^^^^^^^ ⚠️ Component not found --}}

<livewire:admin-panel />
{{--       ^^^^^^^^^^^ ⚠️ Livewire component not found --}}
```

### Quick Actions

Fix problems with a single click. When you see a warning, press `Cmd+.` to open quick actions. The extension offers to create missing files with the correct Laravel structure—views, components, middleware, translations, and more.

```php
return view('users.dashboard');
//          ^^^^^^^^^^^^^^^^^ ⚠️ View not found
//                            ⚡ Create view: users.dashboard

Route::middleware('admin-only')->group(...);
//                ^^^^^^^^^^^^ ⚠️ Middleware not found
//                             ⚡ Create middleware: admin-only
```

```blade
<x-dashboard-widget />
{{-- ⚠️ Component not found
     ⚡ Create component (anonymous)
     ⚡ Create component with class --}}

<livewire:admin-panel />
{{-- ⚠️ Livewire component not found
     ⚡ Create Livewire component --}}
```

**Available quick actions:**
- Create missing views
- Create Blade components (anonymous or with class)
- Create Livewire components
- Create middleware
- Add translations to existing files
- Add environment variables to `.env`

## Planned Features

- Component name autocomplete (`<x-▌`)
- Eloquent model field and relationship autocomplete
- Hover documentation with resolved values
- Inertia.js support (`Inertia::render('Page')`)
- Folio page routing
- Volt component support

## Contributing

```bash
cd laravel-lsp && cargo build --release && cargo test
```

Reload in Zed: `Cmd+Shift+P` → "zed: reload extensions"

---

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE">MIT</a> · <a href="https://github.com/GeneaLabs">GeneaLabs</a>
</p>
