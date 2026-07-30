<p align="center">
  <img src="docs/logo.svg" width="128" height="128" alt="Laravel (Community Edition) for Zed">
</p>

<h1 align="center">Laravel (Community Edition) for Zed</h1>

<p align="center">
<strong>Cmd+Click your way through Laravel projects</strong>
</p>

<p align="center">
<a href="https://github.com/mike-bronner/zed-laravel/actions/workflows/release.yml"><img src="https://github.com/mike-bronner/zed-laravel/actions/workflows/release.yml/badge.svg?event=release" alt="Release"></a>
<a href="https://github.com/mike-bronner/zed-laravel/releases"><img src="https://img.shields.io/github/v/release/mike-bronner/zed-laravel?label=version" alt="Latest Release"></a>
<img src="https://img.shields.io/github/downloads/mike-bronner/zed-laravel/total" alt="Downloads">
<img src="https://img.shields.io/github/stars/mike-bronner/zed-laravel?style=flat" alt="GitHub Stars">
</p>

<p align="center">
<img src="https://img.shields.io/badge/Laravel-FF2D20?logo=laravel&logoColor=white" alt="Laravel">
<img src="https://img.shields.io/badge/Zed-Extension-8B5CF6" alt="Zed Extension">
<img src="https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white" alt="Rust">
<a href="https://github.com/mike-bronner/zed-laravel/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
<sub>A community extension — not affiliated with Laravel LLC.<br>
Listed on the Zed marketplace as <strong>Laravel (Community Edition)</strong>; abbreviated to <strong>Laravel CE</strong> wherever Zed's UI is tight (status bar, language-server list, progress titles).</sub>
</p>

## ❤️ Why we built this

We love Laravel, and we love Zed. When we moved our Laravel work into Zed, the deep, framework-aware tooling we'd relied on elsewhere wasn't there yet — so we built it. This extension exists to give Laravel first-class support in Zed, because a framework this good deserves great tooling everywhere its developers work.

The intelligence lives in a standalone language server (LSP) — the same protocol your editor already speaks for other languages. Today it targets Zed; because it's LSP-based, the same engine could reach other LSP-capable editors (Neovim, Helix, Sublime Text, and more) down the road. That's a direction we'd love to grow toward, not something we ship yet.

### How it works — static analysis

Everything is parsed statically with tree-sitter: the extension reads your files, it never runs them. It only touches your database when *you* opt into schema-backed completion, and it keeps working even when your app won't boot — a half-applied migration, a missing `.env`, or a dirty branch won't stop it. Declared Eloquent magic is resolved statically through a project-wide semantic index — scopes, accessors, relationships, columns, and dynamic finders, in both property and call form, including builder chains. The honest trade-off: truly runtime-only behaviour (dynamic member *names* like `$model->$attribute`, runtime-registered routes) stays out of reach, and ambiguous sites are dropped rather than guessed.

**⚡ Indexing performance.** The extension indexes every PHP and Blade file in your project (including `vendor/`) at startup so find-references and goto-definition return instantly. A persistent on-disk cache makes subsequent project opens near-instant — only files whose `mtime` has changed since they were last indexed get re-parsed. External changes (a `git pull`, a `composer install`, a formatter running outside Zed) are picked up live via `workspace/didChangeWatchedFiles`. The status bar shows progress during the initial warmup.


### Laravel across editors

Laravel developers are spoiled for choice — every major editor has a strong way to work with the framework. Here's roughly where things stand and what each needs, so you can pick whatever fits how you work:

| Editor | Laravel-aware tooling | Cost |
|---|---|---|
| **PHPStorm** | Laravel support built in, powered by the [Laravel Idea](https://laravel-idea.com/) plugin | Paid IDE (free for non-commercial use) |
| **VS Code** | [Official Laravel extension](https://github.com/laravel/vs-code-extension), maintained by the Laravel team | Free |
| **Zed** | This extension, in addition to companion extensions:  [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade), [PHP](https://github.com/zed-extensions/php) (Intelephense), [phpcs](https://github.com/mike-bronner/zed-phpcs-lsp), and [phpmd](https://github.com/mike-bronner/zed-phpmd-lsp) | Free |

<sub>A high-level snapshot as of 2026-05-30 — not a feature-by-feature scorecard. Every option here is capable and actively developed. (As of 2025, the Laravel Idea plugin is bundled free with PhpStorm.) Corrections welcome via PR.</sub>

### Community Edition vs. the official Laravel extension

Laravel now ships an official Zed extension of its own, powered by the `laravel/lsp` language server ([zed-industries/extensions#6996](https://github.com/zed-industries/extensions/pull/6996)). Both extensions are listed separately and can be installed side by side. Neither is a strict superset of the other — they're built on genuinely different architectures, and which one fits you depends on how you work.

**The core difference is how project data gets gathered.** The official server invokes `artisan tinker --execute` per data category to collect routes, config, translations, env, middleware, auth policies, the Mix manifest, and models — which boots your full Laravel kernel, service providers included, on every project (re)index. This extension never runs your app: everything comes from static tree-sitter parsing.

Neither approach wins outright. Booting the app surfaces genuinely runtime-only information that static analysis can't see. Static analysis keeps working precisely when the app *won't* boot — a half-applied migration, a missing `.env`, an unregistered provider, a database the editor can't reach — costs less per refresh, and never executes your application code (including whatever a provider's `boot()`/`register()` does). It also works in any PHP or Blade file in any repo, including packages and libraries with no bootable app at the root. Dirty branches and WIP migrations favour static analysis; genuinely dynamic runtime state favours booting the app.

**Architecture**

| | Official (`laravel/lsp`) | Community Edition (this extension) |
|---|---|---|
| Implementation | PHP (Composer package) | Rust + tree-sitter (compiled to Wasm) |
| Runtime model | Boots your app via detected PHP (Herd / Valet / Sail / Lando / DDEV) to introspect routes, config, and more | Pure static analysis — never executes app code |
| Works on a broken or dirty app | No — requires a bootable app | Yes — parses files even with a broken migration, a missing `.env`, etc. |
| Editor reach | Sublime Text, VS Code, Cursor, Neovim, OpenCode, Zed | Zed only |
| Indexing | Runs PHP scripts as needed | Persistent on-disk cache, incremental (mtime-based), indexes `vendor/`, live file watcher |

**LSP capabilities advertised**

| Capability | Official | Community Edition |
|---|---|---|
| Completion | ✅ | ✅ |
| Hover | ✅ | ✅ |
| Definition | ✅ | ✅ |
| Document links | ✅ | ✅ |
| Code actions / quick fixes | ✅ (narrower scope, see below) | ✅ |
| Rename | ❌ | ✅ |
| Find references | ❌ | ✅ (project + vendor-wide) |
| Code lens | ❌ | ✅ (opt-in reference counts + unused-symbol warning) |
| Document symbols / outline | ❌ | ✅ (route + Blade structure) |

**Per-feature depth**

| Feature area | Official | Community Edition |
|---|---|---|
| Routes | Completion, hover, diagnostics, links | Same + rename + find-references |
| Views / Blade | Completion, hover, diagnostics, links, "create missing view" quick fix | Same + rename + find-references + directive autocomplete, bracket expansion, closing-tag nav, outline |
| Translations | Key / locale / param completion, hover | Same + rename + find-references |
| Config | Completion, hover, diagnostics, links | Same + rename + find-references |
| Env vars | Completion, hover, diagnostics, links, Vite quick fix | Same + rename + find-references |
| Middleware | Completion, hover, diagnostics, links | Same + rename + find-references |
| Container bindings | Completion, hover, diagnostics, links | Same (`app()` / `resolve()`) |
| Assets | Completion, diagnostics, links | `asset()` / `vite()` links only |
| Mix (webpack) | Full manifest-aware feature: completion, hover, diagnostics, links | Not implemented — `mix()` is recognised only as a generic legacy asset helper |
| Inertia | Page / prop completion, links, diagnostics, "create page" quick fix | Same + framework-aware (Vue / React / Svelte) page scaffolding |
| Livewire | Completion, hover, links | Same + rename + find-references |
| Auth & policies | Full feature: `Gate::` / `Auth::` / `Route::can`, `@can` / `@cannot` / `@canany`, `#[Authorize]` — completion, hover with policy links, diagnostics (unknown ability + model mismatch) | Not implemented — only `@can`→`@endcan` bracket closing is recognised |
| Storage disks | Completion, diagnostics, links for `Storage::disk/fake/persistentFake/forgetDisk` and `#[Storage]` | Not implemented |
| Validation rules | Completion only, parsed dynamically from the framework | Same (completion only), also parsed dynamically, and param-type aware (field-ref / DB / mimes / timezone / …) |
| Controller actions | Completion, diagnostics, links | Same |
| Eloquent | Completion only (relations, fillable attrs, query attrs, relation methods) | Completion + hover (cast-aware types, scopes, accessors) + diagnostics validated against your live DB schema + rename + find-references |
| Rename (all of the above) | Not implemented | Routes, configs, translations, env vars, views, components, Livewire, middleware, bindings, PHP classes, magic members, local vars, DB columns (migration included), scope-aware Blade vars |
| Quick-action scaffolding | Create missing view, create Inertia page, env / Vite quick fix | Create view, component (+ backing class), Livewire, middleware, translation, config, `.env`, feature class, framework-aware Inertia page |
| Pest | Auto-generates / updates test helper docblocks | Not implemented |

<sub>A snapshot as of 2026-07-30, compiled from the official server's `Initialize.php` capabilities block and this repo's `main.rs` — not a scorecard, and not a claim that either project stands still. Both are actively developed and this table will drift. Corrections welcome via PR.</sub>

## ✨ Features

Each feature has a focused reference under [`docs/`](docs/) — click through to dive in.

| Feature | What it does |
|---|---|
| [🔗 Go-to-Definition](docs/go-to-definition.md) | Jump to views, components, routes, config, translations, env, assets, middleware, bindings, Artisan commands — plus query-chain columns / relations / tables and Eloquent magic members |
| [ℹ️ Hover](docs/hover.md) | Intelephense-style summary cards for every recognised pattern, including semantic cards for Eloquent magic (scopes, accessors, relationships, cast-aware column types) |
| [🔍 Find References](docs/find-references.md) | Every call site across the project, vendor packages included — including the magic-member usages Intelephense can't see |
| [✏️ Rename](docs/rename.md) | Atomic rename of routes, configs, translations, env vars, views, components, Livewire, middleware, bindings, PHP classes (models, controllers, jobs, services, form requests), magic members, function-local PHP variables, database columns (migration included) — and scope-aware Blade template variables, including the controller→view `view('x', ['key' => …])` / `compact('key')` binding linkage |
| [🔢 Code Lens](docs/code-lens.md) | Opt-in reference counts above magic members, routes, config / translation / env keys, and Blade templates — plus an unused-symbol warning |
| [💡 Autocomplete](docs/autocomplete.md) | Cast types, model properties, query chains, builder methods, Blade / loop / slot variables, Pennant flags |
| [❌ Diagnostics](docs/diagnostics.md) | Missing views / components / features, invalid rules, query-chain typos against your real schema |
| [⚡ Quick Actions](docs/quick-actions.md) | One-click create missing views, components, middleware, features, and migrations |
| [🎨 Blade Editing](docs/blade-editing.md) | Directive autocomplete, smart bracket expansion, closing-tag navigation |
| [🗺️ Outline Panel](docs/outline.md) | Laravel-aware route + Blade structure in Zed's outline and breadcrumbs |

## 📦 Install

Search **"Laravel"** in Zed Extensions and install **Laravel (Community Edition)**.

### 🤝 Recommended companions

Also install the following extensions for a more complete experience:
- [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) extension (`bajrangCoder/zed-laravel-blade`) for Blade-related fetures, syntax highlighting, etc.
- [**PHP**](https://github.com/zed-extensions/php) (Intelephense) for php intellisense functionality
- [**phpcs**](https://github.com/mike-bronner/zed-phpcs-lsp) PHP CodeSniffer linting
- [**phpmd**](https://github.com/mike-bronner/zed-phpmd-lsp) PHP Mess Detector linting

### From source

Clone the repo, run `cargo build --release` in `laravel-lsp/`, then use "zed: install dev extension".

## ⚙️ Configuration

The extension works out of the box with **zero configuration** — it auto-discovers your view paths, component namespaces, route files, and service providers. Everything below is optional.

> ⚠️ **One conditional requirement.** If you set an explicit `language_servers` list for PHP or Blade (common when pinning a PHP LSP), that list *replaces* Zed's defaults — you **must** include `"laravel-lsp"` or the extension won't attach. Symptom: features work in `.env` files but do nothing in `.php` / `.blade.php`. The block below shows the correct form.

### 🎛️ All settings

Everything goes in your Zed `settings.json`. Zed settings are JSONC, so the inline comments below are valid — copy what you need. Every value is shown at its **default**; delete a line to keep that default.

```jsonc
{
  "lsp": {
    // ── This extension's own settings ──────────────────────────────────
    "laravel-lsp": {
      "settings": {
        // Delay (ms) before autocomplete refreshes after a keystroke.
        // Lower 50–100 = snappier; higher 300–500 = less CPU.   Default: 200
        "autoCompleteDebounce": 200,

        "blade": {
          // Space between a directive and its parentheses.
          // false → @if($x)    true → @if ($x)                  Default: false
          "directiveSpacing": false
        },

        "codeLens": {
          // Reference-count lenses + unused-symbol diagnostic (opt-in while
          // the feature matures). Guide: docs/code-lens.md.     Default: false
          "enabled": false
        },

        "diagnostics": {
          // Severity for query-chain diagnostics (unknown column / relation /
          // table in Eloquent & DB::table() chains). Silent without a live DB
          // connection. One of: "error" | "warning" | "info" | "off".
          "severity": "warning"   // Default: "warning"
        }
      }
    },

    // ── Optional third-party language-server tweaks ────────────────────
    // Silence shellcheck SC2034 "APP_NAME appears unused" on .env lines
    // (Zed lints .env as Shell Script). The bashIde wrapper is required.
    // Trade-offs & alternatives: docs/environment.md.
    "bash-language-server": {
      "settings": {
        "bashIde": { "shellcheckArguments": ["--exclude=SC2034"] }
      }
    },

    // Trim Intelephense goto-definition noise. This extension resolves facades,
    // macros & mixins to their concrete implementation, so the generated IDE-helper
    // stubs (and the PhpStorm meta file) only add duplicate facade/Eloquent hits to
    // the goto multibuffer. Excluding them lets our resolution stand alone.
    // Trade-off: Intelephense then loses completion for *package-added* facade
    // methods (Scout, Telescope, Spatie…); core facade completion is unaffected
    // (it reads framework docblocks, not the helper). Full rationale, the cache
    // caveat, and a per-project `.intelephense.json` variant: docs/tuning-intelephense.md.
    // Restart after editing (Cmd+Shift+P → "lsp: restart").
    "intelephense": {
      "settings": {
        "files": {
          "exclude": [
            "**/stubs/**",            // scaffold templates (Jetstream, Filament…), never run
            "**/_ide_helper*.php",    // barryvdh/laravel-ide-helper facade + model stubs
            "**/.phpstorm.meta.php"   // PhpStorm container-binding type hints
          ]
        }
      }
    }
  },

  // ── Zed per-language toggles that unlock our features ───────────────
  "languages": {
    "PHP": {
      // An explicit list REPLACES Zed's defaults, so "laravel-lsp" must appear
      // or this extension won't attach; "..." re-expands the remaining defaults.
      // Prefix a server with "!" to DISABLE it — pick ONE PHP LSP (Intelephense
      // here) and turn the rest off, because running several PHP servers at once
      // produces duplicate completions, hovers, and diagnostics.
      "language_servers": ["laravel-lsp", "intelephense", "!phpactor", "!phptools", "..."],
      // LSP outlines: our route-file outline + your PHP LSP's class outline.
      // "on" | "off"                                            Default: "off"
      "document_symbols": "on"
    },
    "Blade": {
      // Same rules as PHP. "!phpactor" stops the PHP-oriented Phpactor server
      // from also attaching to .blade.php files and double-reporting.
      "language_servers": ["laravel-lsp", "!phpactor", "..."],
      // Our Blade outline (@extends / @section / <x-*> / <livewire:*> …).
      "document_symbols": "on",
      // Highlight custom inline @directive() macros tree-sitter can't see
      // (e.g. a @money($x) Blade::directive). Requires Zed semantic-token
      // support; "combined" overlays them on the Blade extension's colors.
      "semantic_tokens": "combined"
    }
  }
}
```

### 🗄️ Database connection

**Database autocomplete** (`exists:` / `unique:` rules, Eloquent properties) and query-chain diagnostics only work with a live database connection. Configure it in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

### 🗺️ Outline panel

The extension populates Zed's outline panel and breadcrumbs with Laravel-specific structure that no PHP language server understands:

- **Route files** — every `Route::get/post/...` call labelled `METHOD URI [name=...]`, with nested `Route::group(...)` calls becoming hierarchical containers labelled `group [prefix=..., name=...]`. Prefix and name chains propagate to children. Covers all Route methods including `resource`, `apiResource`, `singleton`, `livewire`, `view`, `redirect`, `fallback`, etc.
- **Blade templates** — `@extends`, `@section`, `@push`, `@yield`, `@stack`, `@include*`, `@props`, plus the modern tag syntax: `<x-component>`, `<livewire:counter>`, `<flux:icon>`, `<x-slot:name>`. Paired tags nest their children; self-closing tags appear as leaves.

PHP class outlines (controllers, models, Livewire components, jobs, services) come from whatever PHP language server you have installed — those servers have real semantic understanding of PHP that a tree-sitter walker can't match. The official [**PHP**](https://github.com/zed-extensions/php) Zed extension registers Intelephense, Phpactor, and PhpTools; install it and pick whichever LSP you prefer.

**Requirements**

| Outline | Requires |
|---|---|
| Route files | This extension, plus `document_symbols: on` for `PHP` (route files use the `PHP` language). |
| Blade templates | This extension, the [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade) extension (for the `Blade` language definition), plus `document_symbols: on` for `Blade`. |
| PHP class files | A PHP language server (the [PHP](https://github.com/zed-extensions/php) extension provides Intelephense / Phpactor / PhpTools), plus `document_symbols: on` for `PHP`. |

Zed defaults to tree-sitter outlines, which don't call any LSP — the `document_symbols: "on"` toggles in the [settings block above](#-all-settings) opt you into LSP outlines (`PHP` unlocks our route outline *and* your PHP LSP's class outline; `Blade` unlocks our Blade outline). This opt-in is a Zed quirk ([`zed#48780`](https://github.com/zed-industries/zed/pull/48780)) — clients that always request `textDocument/documentSymbol`, like Helix or Neovim, don't need it.

> **Quirks worth knowing** — Zed colors outline labels by word-matching them against the source buffer's tree-sitter highlights, which produces slightly inconsistent colors on multi-segment URLs (e.g., `/cra-details` may color `cra` and `details` differently if they match different tokens elsewhere in the file). Route names appear in the LSP `detail` field, which Zed's outline panel doesn't currently render (VSCode and Sublime/LSP do). Both are tracked upstream: [zed#57576](https://github.com/zed-industries/zed/issues/57576).

## 🩺 Troubleshooting

**The extension installed but nothing happens — no features, no "Laravel CE" entry in the language-server list.** Work through these in order; each is a real cause we've seen.

### 1. Is your Zed new enough?

The extension is built against `zed_extension_api 0.7.0`, which requires **Zed 0.205 or newer**. Older versions silently refuse to load it — the extension appears installed but never activates, and the log stays quiet. Check `Zed → About Zed` (or `zed --version`) and update if you're behind.

### 2. Does Zed recognize the file as PHP / Blade?

The language server only attaches to files Zed has classified as **PHP**, **Blade**, **XML**, or `.env` (Shell Script). Open a `.php` file and check the **bottom-right status bar**:

- Says **"PHP"** → good, the language is registered.
- Says **"Plain Text"** → install the official [**PHP**](https://github.com/zed-extensions/php) extension (and [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) for `.blade.php`). Without a language registered, no language server — ours included — can attach.

### 3. Did you override `language_servers` for PHP or Blade?

If features work in `.env` files but not in `.php` / `.blade.php`, you've almost certainly set an explicit `language_servers` list that omits `laravel-lsp`. See [the settings block](#-all-settings) for the fix — include `"laravel-lsp"` in the list.

### 4. Check the language-server log

The running servers show under the **lightning-bolt icon** in the status bar. For the full log: `Cmd+Shift+P → "open language server logs"` and look for **Laravel CE**. If it's missing entirely, the server never started (revisit steps 1–3). If it's present but erroring, the log will say why — please [open an issue](https://github.com/mike-bronner/zed-laravel/issues) with that output.

### 5. Manual binary fallback

The extension downloads its server binary from GitHub releases on first use. If that download is blocked (proxy, firewall, offline), drop the binary on your `PATH` instead — Zed will find it there. Grab the archive for your platform from the [latest release](https://github.com/mike-bronner/zed-laravel/releases/latest), then (macOS x86_64 shown):

```bash
tar -xzf ~/Downloads/laravel-lsp-macos-x64.tar.gz
mkdir -p ~/.local/bin && mv laravel-lsp-macos-x64 ~/.local/bin/laravel-lsp
chmod +x ~/.local/bin/laravel-lsp
xattr -d com.apple.quarantine ~/.local/bin/laravel-lsp 2>/dev/null
# ensure ~/.local/bin is on your PATH, then fully restart Zed
```

## 🚧 Planned Features

**Rename — remaining work** (the class-backed kinds, the FQCN class-rename engine across all common PHP class kinds, magic members, database columns, function-local PHP variables, and scope-aware Blade template variables shipped):

- 🔧 **Class-property rename** — `$this->foo`, `self::$bar`, and dynamic property access have different reference shapes than local variables, so they get their own scope-aware pass.

**Framework integrations:**

- 🎨 **Inertia.js support** — go-to-definition and autocomplete for `Inertia::render('Page')` calls
- 📁 **Folio page routing** — surface Folio's filesystem-routed pages in goto-definition / completion / find-references

## 🤝 Contributing

Contributions are welcome! See **[CONTRIBUTING.md](CONTRIBUTING.md)** for the project layout, local development setup, building the LSP, running tests, and code style.

---

<p align="center">
<a href="https://github.com/mike-bronner/zed-laravel/blob/main/LICENSE">MIT</a> · <a href="https://github.com/mike-bronner">mike-bronner</a>
</p>
