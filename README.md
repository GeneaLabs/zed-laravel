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

## Contents

- [❤️ Why we built this](#️-why-we-built-this)
- [✨ Features](#-features)
- [📦 Install](#-install)
- [⚙️ Configuration](#️-configuration)
- [🩺 Troubleshooting](#-troubleshooting)
- [🚧 Planned Features](#-planned-features)
- [🤝 Contributing](#-contributing)

📚 [Full documentation index](docs/README.md)

## ❤️ Why we built this

We love Laravel, and we love Zed. When we moved our Laravel work into Zed, the deep, framework-aware tooling we'd relied on elsewhere wasn't there yet — so we built it. This extension exists to give Laravel first-class support in Zed, because a framework this good deserves great tooling everywhere its developers work.

The intelligence lives in a standalone language server (LSP) — the same protocol your editor already speaks for other languages. Today it targets Zed; because it's LSP-based, the same engine could reach other LSP-capable editors (Neovim, Helix, Sublime Text, and more) down the road. That's a direction we'd love to grow toward, not something we ship yet.

### How it works — static analysis

Everything is parsed statically with tree-sitter: the extension reads your files, it never runs them. It only touches your database when *you* opt into schema-backed completion, and it keeps working even when your app won't boot — a half-applied migration, a missing `.env`, or a dirty branch won't stop it. Declared Eloquent magic is resolved statically through a project-wide semantic index — scopes, accessors, relationships, columns, and dynamic finders, in both property and call form, including builder chains. The honest trade-off: truly runtime-only behaviour (dynamic member *names* like `$model->$attribute`, runtime-registered routes) stays out of reach, and ambiguous sites are dropped rather than guessed.

**⚡ Indexing performance.** The extension indexes every PHP and Blade file in your project (including `vendor/`) at startup so find-references and goto-definition return instantly. A persistent on-disk cache makes subsequent project opens near-instant — only files whose `mtime` has changed since they were last indexed get re-parsed. External changes (a `git pull`, a `composer install`, a formatter running outside Zed) are picked up live via `workspace/didChangeWatchedFiles`. The status bar shows progress during the initial warmup.


### How this compares

Laravel developers are spoiled for choice — every major editor has strong framework-aware tooling, and Laravel now ships an **official Zed extension** of its own. Neither it nor this one is a strict superset of the other: the official server boots your app via `artisan tinker` to gather project data, while this one never runs your code. Dirty branches and WIP migrations favour static analysis; genuinely runtime-only state favours booting the app.

⚖️ **[Full comparison →](docs/comparison.md)** — Laravel tooling across editors, plus architecture, LSP capabilities, and per-feature depth against the official extension.

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

The extension works out of the box with **zero configuration** — it auto-discovers your view paths, component namespaces, route files, and service providers. Everything else is optional.

> ⚠️ **The one thing that trips people up.** If you set an explicit `language_servers` list for PHP or Blade (common when pinning a PHP LSP), that list *replaces* Zed's defaults — you **must** include `"laravel-lsp"` or the extension won't attach. Symptom: features work in `.env` files but do nothing in `.php` / `.blade.php`.

⚙️ **[Full settings reference →](docs/configuration.md)** — every option at its default (autocomplete debounce, Blade directive spacing, code lens, diagnostic severity), the third-party LSP tweaks, database-connection setup, and the outline-panel toggles.

## 🩺 Troubleshooting

**Installed, but nothing happens?** The usual causes, in order: Zed older than **0.205** (the minimum this extension builds against), the file isn't classified as PHP / Blade, or a `language_servers` override that omits `laravel-lsp`.

🩺 **[Troubleshooting guide →](docs/troubleshooting.md)** — the full five-step decision tree, how to read the language-server log, and the manual binary fallback for blocked downloads.

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
