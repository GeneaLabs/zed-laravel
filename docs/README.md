# 📚 Documentation

[← Back to README](../README.md)

Every guide for **Laravel (Community Edition) for Zed**, grouped by what you're trying to do.

## ✨ Features

| Guide | What it covers |
|---|---|
| [🔗 Go-to-Definition](go-to-definition.md) | Jumping to views, components, routes, config, translations, env, assets, middleware, bindings, Artisan commands, query-chain columns / relations / tables, and Eloquent magic members |
| [ℹ️ Hover](hover.md) | Summary cards for every recognised pattern, including semantic cards for Eloquent magic |
| [🔍 Find References](find-references.md) | Locating every call site across the project and vendor packages |
| [✏️ Rename](rename.md) | Atomic rename across routes, configs, translations, env vars, views, components, Livewire, middleware, bindings, PHP classes, magic members, local variables, DB columns, and Blade template variables |
| [🔢 Code Lens](code-lens.md) | Opt-in reference counts and the unused-symbol warning |
| [💡 Autocomplete](autocomplete.md) | Cast types, model properties, query chains, builder methods, Blade / loop / slot variables, Pennant flags |
| [❌ Diagnostics](diagnostics.md) | Missing views / components / features, invalid rules, and query-chain typos checked against your real schema |
| [⚡ Quick Actions](quick-actions.md) | One-click creation of missing views, components, middleware, features, and migrations |
| [🎨 Blade Editing](blade-editing.md) | Directive autocomplete, smart bracket expansion, closing-tag navigation |
| [🗺️ Outline Panel](outline.md) | Laravel-aware route and Blade structure in Zed's outline and breadcrumbs |

## ⚙️ Configuration & tuning

| Guide | What it covers |
|---|---|
| [⚙️ Configuration](configuration.md) | The full settings reference (every option at its default), database connection setup, and the outline-panel requirements |
| [🌱 Environment files](environment.md) | Handling `.env` files in Zed — silencing shellcheck SC2034, or reclassifying `.env` away from Shell Script |
| [🔧 Tuning Intelephense](tuning-intelephense.md) | Trimming duplicate goto-definition hits from IDE-helper stubs, the trade-offs, and a per-project variant |

## ⚖️ Choosing this extension

| Guide | What it covers |
|---|---|
| [⚖️ Comparison](comparison.md) | Which to pick — Community Edition vs. the official Laravel extension (and why you probably want just one running), plus Laravel tooling across editors, architecture, LSP capabilities, and per-feature depth |

## 🩺 Help

| Guide | What it covers |
|---|---|
| [🩺 Troubleshooting](troubleshooting.md) | The extension installed but nothing happens — the decision tree, log inspection, and the manual binary fallback |

## 🔬 Design notes

Internal discovery and rationale documents. Not user guides — they record why the implementation looks the way it does.

| Document | What it covers |
|---|---|
| [Unifying the route caches](route-cache-unification.md) | Benchmark-backed discovery for [#48](https://github.com/mike-bronner/zed-laravel/issues/48): whether the byte-scan route index and the tree-sitter route walkers should collapse onto one model |
