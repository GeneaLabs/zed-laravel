# ⚖️ Comparison

[← Back to README](../README.md)

Which Laravel tooling is right for you — across editors, and between this extension and the official Laravel one.

## Laravel across editors

Laravel developers are spoiled for choice — every major editor has a strong way to work with the framework. Here's roughly where things stand and what each needs, so you can pick whatever fits how you work:

| Editor | Laravel-aware tooling | Cost |
|---|---|---|
| **PHPStorm** | Laravel support built in, powered by the [Laravel Idea](https://laravel-idea.com/) plugin | Paid IDE (free for non-commercial use) |
| **VS Code** | [Official Laravel extension](https://github.com/laravel/vs-code-extension), maintained by the Laravel team | Free |
| **Zed** | This extension, in addition to companion extensions:  [Laravel Blade](https://github.com/bajrangCoder/zed-laravel-blade), [PHP](https://github.com/zed-extensions/php) (Intelephense), [phpcs](https://github.com/mike-bronner/zed-phpcs-lsp), and [phpmd](https://github.com/mike-bronner/zed-phpmd-lsp) | Free |

<sub>A high-level snapshot as of 2026-05-30 — not a feature-by-feature scorecard. Every option here is capable and actively developed. (As of 2025, the Laravel Idea plugin is bundled free with PhpStorm.) Corrections welcome via PR.</sub>

## Community Edition vs. the official Laravel extension

Laravel now ships an official Zed extension of its own, powered by the `laravel/lsp` language server ([zed-industries/extensions#6996](https://github.com/zed-industries/extensions/pull/6996)). Both are listed separately in Zed's extension registry. Neither is a strict superset of the other — they're built on genuinely different architectures, and which one fits you depends on how you work.

> ⚠️ **You probably shouldn't run both at the same time.** Each registers a language server for PHP and Blade, and both answer `hover`, `completion`, `documentLink`, and diagnostics for the same patterns. Zed merges the results of every attached server, so running the pair tends to double what you see: two hover cards on one `view()` call, duplicate completion entries, two diagnostics for a single missing view. Nothing breaks — it's noise, not conflict, the same kind you'd get from running Intelephense, Phpactor, and PhpTools together. If you want both installed so you can switch between them, disable one per-language with a `"!"` prefix in [`language_servers`](configuration.md).

**Which one to pick**

Both are good tools. Choose on architecture, not on today's feature lists — those move every release on both sides. The architectural split is the part that stays true.

| Choose Community Edition — reads your code, never runs it | Choose the official extension — asks your running app directly |
|---|---|
| You spend time on branches where the app isn't always runnable: a half-applied migration, a missing `.env`, an unregistered provider, a database you're not connected to. Parsing carries on regardless. | Your app boots cleanly on demand, and you'd like Laravel itself to be the source of truth |
| You open packages, libraries, and shared component sets — repos with no application at the root | You work on full applications, where running the app is already part of the loop |
| You like knowing your editor only ever reads: no provider `boot()`, no container, nothing executed on your behalf | You're glad to have the editor run `artisan tinker` for you — it's what you'd type by hand to answer the same question |
| You want what's **written** — the code on disk right now, including edits you haven't run yet | You want what's **resolved** — runtime-registered routes, computed config, and container state only a booted kernel can report |
| You want re-indexing to stay cheap and incremental: an mtime-based cache, only changed files re-parsed | You want each index gathered fresh from the framework, so what you see matches what your app would do right now |
| You want tooling that works the moment you clone a repo, with no PHP runtime to locate first | Your PHP environment is set up and humming (Herd / Valet / Sail / Lando / DDEV), and you want tooling that runs on the very same runtime your app does |

The rest of this page is the detail behind that split — including a feature-by-feature snapshot, which is exactly the part that will drift as both projects ship.

**The core difference is how project data gets gathered.** The official server invokes `artisan tinker --execute` per data category to collect routes, config, translations, env, middleware, auth policies, the Mix manifest, and models — which boots your full Laravel kernel, service providers included, on every project (re)index. This extension never runs your app: everything comes from static tree-sitter parsing.

Neither approach wins outright — the table above is the short version. The boundary worth spelling out is where static analysis stops: declared Eloquent magic (scopes, accessors, relationships, columns, dynamic finders) *is* resolved here, through a project-wide semantic index, but truly runtime-only behaviour — dynamic member *names* like `$model->$attribute`, routes registered at runtime, config computed during boot — stays out of reach, and ambiguous sites are dropped rather than guessed. Booting the app is how you see that last category. Not booting it is how you keep working when the app can't.

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

---

See also: [📚 Documentation index](README.md) · [⚙️ Configuration](configuration.md)
