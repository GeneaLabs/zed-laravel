<h1 align="center">Laravel for Zed</h1>

<p align="center">
<strong>Cmd+Click your way through Laravel projects</strong>
</p>

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
<img src="https://img.shields.io/badge/Zed-Extension-8B5CF6" alt="Zed Extension">
<img src="https://img.shields.io/badge/community-project-orange" alt="Community Project">
</p>

<p align="center">
<sub>A community extension — not affiliated with Laravel LLC</sub>
</p>

---

**🔗 Go-to-Definition** — Cmd+Click on `view()`, `route()`, `<x-component>` to jump to source
**💡 Smart Autocomplete** — Validation rules, database tables, config keys, routes, translations
**⚡ Quick Actions** — Create missing views, components, middleware with one click

<!-- TODO: Add demo GIF here
<p align="center">
<img src="docs/demo.gif" alt="Laravel for Zed in action" width="600">
</p>
-->

## Install

Search **"Laravel"** in Zed Extensions and click Install.

<details>
<summary>Build from source</summary>

```bash
git clone https://github.com/GeneaLabs/zed-laravel.git
cd zed-laravel/laravel-lsp && cargo build --release
```
Then in Zed: `Cmd+Shift+P` → "zed: install dev extension" → select the `zed-laravel` folder.

</details>

## 🔗 Go-to-Definition

Cmd+Click (or `Cmd+D`) on any pattern to jump to its source:

| Pattern | Jumps to |
|---------|----------|
| `view('users.profile')` | `resources/views/users/profile.blade.php` |
| `<x-button>` | `resources/views/components/button.blade.php` |
| `<livewire:counter>` | `app/Livewire/Counter.php` |
| `@extends('layouts.app')` | `resources/views/layouts/app.blade.php` |
| `route('users.show')` | Route definition in `routes/*.php` |
| `config('app.name')` | `config/app.php` |
| `env('APP_KEY')` | `.env` at the matching line |
| `__('auth.failed')` | `lang/en/auth.php` |
| `->middleware('auth')` | Middleware class |
| `app('cache')` | Service container binding |
| `asset('css/app.css')` | `public/css/app.css` |
| `@vite('resources/js/app.js')` | Vite entry point |

<details>
<summary>More patterns</summary>

**Views**: `view()`, `View::make()`, `Route::view()`
**Blade**: `@include`, `@extends`, `@component`, `@each`
**Routes**: `route()`, `to_route()`, `Route::has()`, `URL::route()`
**Config**: `config()`, `Config::get()`, `Config::string()`, `Config::boolean()`
**Translations**: `__()`, `trans()`, `trans_choice()`, `Lang::get()`, `@lang()`
**Paths**: `app_path()`, `base_path()`, `resource_path()`, `storage_path()`, `public_path()`

</details>

## 💡 Autocomplete

Start typing to get intelligent suggestions:

| Context | Suggestions |
|---------|-------------|
| `exists:▌` | Database tables and connections |
| `exists:users,▌` | Column names for `users` table |
| `'required\|▌'` | All 90+ Laravel validation rules |
| `config('▌` | Config keys with resolved values |
| `route('▌` | Named routes with file locations |
| `__('▌` | Translation keys with values |
| `env('▌` | Environment variables from `.env` |

<details>
<summary>Database autocomplete setup</summary>

Database table/column autocomplete requires a working database connection. Configure in `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=your_database
DB_USERNAME=root
DB_PASSWORD=
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

</details>

## ⚡ Diagnostics & Quick Actions

Real-time warnings as you type. Press `Cmd+.` on any warning to fix it:

| Problem | Fix |
|---------|-----|
| Missing view: `view('missing')` | ⚡ Create view |
| Missing component: `<x-missing>` | ⚡ Create component (with or without class) |
| Missing Livewire: `<livewire:missing>` | ⚡ Create Livewire component |
| Missing middleware: `->middleware('custom')` | ⚡ Create middleware |
| Missing translation: `__('missing.key')` | ⚡ Add to translation file |
| Invalid validation rule: `'required\|typo'` | Highlights the invalid rule |
| Missing env variable: `env('MISSING')` | ⚡ Add to .env |
| `env()` outside config files | Warning (breaks config caching) |

## Configuration

**Zero config required.** The extension auto-discovers your Laravel project structure.

<details>
<summary>Optional settings</summary>

Adjust diagnostic timing in Zed's `settings.json`:

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

| Value | Use case |
|-------|----------|
| 50-100ms | Fast machine, instant feedback |
| 200ms | Default — balanced |
| 300-500ms | Slower machine, reduce CPU |

</details>

## Roadmap

<details>
<summary>See what's coming</summary>

**Planned:**
- [ ] Component name autocomplete: `<x-▌`
- [ ] Eloquent field and relationship autocomplete
- [ ] Hover documentation with resolved values
- [ ] Inertia.js: `Inertia::render('Page')`
- [ ] Folio and Volt support

**Done:**
- [x] Go-to-definition for views, components, routes, config, translations
- [x] Validation rule autocomplete (90+ rules)
- [x] Database table/column autocomplete
- [x] Real-time diagnostics
- [x] Quick actions for file creation

</details>

## Contributing

<details>
<summary>Development setup</summary>

```bash
cd laravel-lsp && cargo build --release
cargo test
```

Reload in Zed: `Cmd+Shift+P` → "zed: reload extensions"

**Structure:**
```
zed-laravel/
├── src/                    # Zed extension (Rust → WASM)
├── laravel-lsp/            # Language server
│   └── src/
│       ├── main.rs         # LSP handlers
│       ├── queries.rs      # Pattern extraction
│       └── database.rs     # DB connectivity
└── extension.toml
```

</details>

**Want to help?** New Laravel patterns, autocomplete improvements, and performance optimizations welcome.

---

<p align="center">
<a href="https://github.com/GeneaLabs/zed-laravel/blob/main/LICENSE">MIT License</a> · Made by <a href="https://github.com/GeneaLabs">GeneaLabs</a>
</p>
