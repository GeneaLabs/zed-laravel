# ⚙️ Configuration

[← Back to README](../README.md)

The extension works out of the box with **zero configuration** — it auto-discovers your view paths, component namespaces, route files, and service providers. Everything below is optional.

> ⚠️ **One conditional requirement.** If you set an explicit `language_servers` list for PHP or Blade (common when pinning a PHP LSP), that list *replaces* Zed's defaults — you **must** include `"laravel-lsp"` or the extension won't attach. Symptom: features work in `.env` files but do nothing in `.php` / `.blade.php`. The block below shows the correct form.

## All settings

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

## Database connection

**Database autocomplete** (`exists:` / `unique:` rules, Eloquent properties) and query-chain diagnostics only work with a live database connection. Configure it in your `.env`:

```env
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=secret
```

Supports MySQL, PostgreSQL, SQLite, and SQL Server.

## Outline panel

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

Zed defaults to tree-sitter outlines, which don't call any LSP — the `document_symbols: "on"` toggles in the [settings block above](#all-settings) opt you into LSP outlines (`PHP` unlocks our route outline *and* your PHP LSP's class outline; `Blade` unlocks our Blade outline). This opt-in is a Zed quirk ([`zed#48780`](https://github.com/zed-industries/zed/pull/48780)) — clients that always request `textDocument/documentSymbol`, like Helix or Neovim, don't need it.

> **Quirks worth knowing** — Zed colors outline labels by word-matching them against the source buffer's tree-sitter highlights, which produces slightly inconsistent colors on multi-segment URLs (e.g., `/cra-details` may color `cra` and `details` differently if they match different tokens elsewhere in the file). Route names appear in the LSP `detail` field, which Zed's outline panel doesn't currently render (VSCode and Sublime/LSP do). Both are tracked upstream: [zed#57576](https://github.com/zed-industries/zed/issues/57576).

---

See also: [🗺️ Outline Panel](outline.md) · [🌱 Environment files](environment.md) · [🔧 Tuning Intelephense](tuning-intelephense.md) · [🩺 Troubleshooting](troubleshooting.md)
