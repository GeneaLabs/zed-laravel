# 🩺 Troubleshooting

[← Back to README](../README.md)

Three failure modes, all common:

- **Nothing happens** — no features, no "Laravel CE" entry in the language-server list. Work through steps 1–3 in order; each is a real cause we've seen.
- **Everything happens twice** — duplicate hovers, doubled completions, two diagnostics on one line. Jump to [step 4](#4-seeing-everything-twice).
- **One feature is missing while the rest work** — most often `F2` rename on a class name. Jump to [step 5](#5-rename-does-nothing-on-a-class-name).

## 1. Is your Zed new enough?

The extension is built against `zed_extension_api 0.7.0`, which requires **Zed 0.205 or newer**. Older versions silently refuse to load it — the extension appears installed but never activates, and the log stays quiet. Check `Zed → About Zed` (or `zed --version`) and update if you're behind.

## 2. Does Zed recognize the file as PHP / Blade?

The language server only attaches to files Zed has classified as **PHP**, **Blade**, **XML**, or **Shell Script** — the last being Zed's default classification for `.env` and every `.env.*` variant. If you've remapped `.env` to another language via `file_types`, the server no longer attaches there; see [Environment files](environment.md). Open a `.php` file and check the **bottom-right status bar**:

- Says **"PHP"** → good, the language is registered.
- Says **"Plain Text"** → install the official [**PHP**](https://github.com/zed-extensions/php) extension (and [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) for `.blade.php`). Without a language registered, no language server — ours included — can attach.

## 3. Did you override `language_servers` for PHP or Blade?

If features work in `.env` files but not in `.php` / `.blade.php`, you've almost certainly set an explicit `language_servers` list that omits `laravel-lsp`. See [the settings block](configuration.md#all-settings) for the fix — include `"laravel-lsp"` in the list.

## 4. Seeing everything twice?

The opposite symptom: everything works, but **twice**. Two hover cards stacked on one `view()` call, every completion listed in duplicate, the same diagnostic reported on a line twice over. Nothing is broken — you simply have more than one language server attached to the file, and Zed merges what all of them return.

The usual culprit is a second **PHP** language server. The official [**PHP**](https://github.com/zed-extensions/php) extension ships four, and enables the ones you haven't disabled:

| Server | Disable with |
|---|---|
| Intelephense | `"!intelephense"` |
| Phpactor | `"!phpactor"` |
| PHPantom | `"!phpantom"` |
| PhpTools | `"!phptools"` |

Pick **one**, `"!"`-prefix the other three, and keep `"laravel-lsp"` in the list — this extension is Laravel-pattern-aware and deliberately stays out of the way of whichever PHP LSP you keep (see [🔧 Tuning Intelephense](tuning-intelephense.md) for how that division of labour is drawn). The [settings block](configuration.md#all-settings) shows the finished form for both PHP and Blade.

> 📌 **`"!phpantom"` is the one people miss.** It was added to the PHP extension after Phpactor and PhpTools, so a `language_servers` list copied from older docs — including earlier versions of *these* docs — disables three of the four and leaves PHPantom running alongside your chosen LSP and `laravel-lsp`. If your duplicates appeared out of nowhere after a PHP-extension update, this is why. PHPantom also has a second, sharper interaction with this extension that isn't about duplication at all — see [step 5](#5-rename-does-nothing-on-a-class-name).

Blade files follow the same rule, and need their own list — the [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) extension attaches Intelephense, Phpactor, and PhpTools to `.blade.php` as well, so the same `"!"` entries belong in your `Blade` block. (It doesn't currently wire PHPantom to Blade, so `"!phpantom"` there is future-proofing rather than a live fix — harmless either way, and it saves you a repeat visit when that list grows.)

## 5. Rename does nothing on a class name?

A different shape of problem: everything else works, but `F2` on a PHP **class name** either does nothing, or refuses to move the class to another namespace. This is not duplication — it's the opposite. Only **one** server gets to answer.

**Why it happens.** Hover and goto-definition are *merged* across every attached language server, which is why extra servers produce duplicates. Rename is not: a rename has to produce one coherent set of edits, so Zed picks a single server and asks only that one. This extension advertises a rename provider and claims the position for any class name that resolves to a project (non-vendor) file — which means it answers, and the PHP language server sitting next to it is never consulted. When we decline, Zed does **not** fall through to the other server; the request is simply dropped.

That matters most alongside [**PHPantom**](https://github.com/PHPantom-dev/phpantom_lsp), whose class rename does strictly more than ours: it can move a class between namespaces, so renaming `Foo` to `Bar\Foo` relocates the file to match. Ours is **same-namespace only** — a namespaced new name is refused with *"moving it to another namespace isn't supported"*. So with both attached you get the smaller operation and no way to reach the larger one.

**The fix: disable PHPantom.** Add `"!phpantom"` to your PHP `language_servers` list — see the [settings block](configuration.md#all-settings) for the finished form:

```jsonc
"language_servers": ["laravel-lsp", "intelephense", "!phpactor", "!phpantom", "!phptools", "..."]
```

Rename then behaves as documented in the [rename docs](rename.md) — views, routes, config keys, components, Livewire, translations, and magic Eloquent members all rename correctly, and class renames work within their namespace.

**What you give up:** moving a class to a different namespace by renaming it. Ours stays same-namespace; for a move, relocate the file and update the namespace by hand.

This is the current workaround, not the intended end state — [#282](https://github.com/mike-bronner/zed-laravel/issues/282) tracks the conflict and [#277](https://github.com/mike-bronner/zed-laravel/issues/277) the broader question of how the two servers should divide the work. The goal is for both to run together with rename routed per symbol kind; until that lands, `"!"` is the only lever Zed gives you.

> 📌 **Intelephense users see a milder version of this.** It has no move-on-rename to lose, so the overlap is confined to Eloquent relationship methods — see the [rename docs](rename.md) and [#74](https://github.com/mike-bronner/zed-laravel/issues/74).

## 6. Check the language-server log

The running servers show under the **lightning-bolt icon** in the status bar. For the full log: `Cmd+Shift+P → "open language server logs"` and look for **Laravel CE**. If it's missing entirely, the server never started (revisit steps 1–3). If it's present but erroring, the log will say why — please [open an issue](https://github.com/mike-bronner/zed-laravel/issues) with that output.

## 7. Manual binary fallback

The extension downloads its server binary from GitHub releases on first use. If that download is blocked (proxy, firewall, offline), drop the binary on your `PATH` instead — Zed will find it there. Grab the archive for your platform from the [latest release](https://github.com/mike-bronner/zed-laravel/releases/latest), then (macOS x86_64 shown):

```bash
tar -xzf ~/Downloads/laravel-ce-lsp-macos-x64.tar.gz
mkdir -p ~/.local/bin && mv laravel-ce-lsp-macos-x64 ~/.local/bin/laravel-ce-lsp
chmod +x ~/.local/bin/laravel-ce-lsp
xattr -d com.apple.quarantine ~/.local/bin/laravel-ce-lsp 2>/dev/null
# ensure ~/.local/bin is on your PATH, then fully restart Zed
```

## 8. Did the extension pick the wrong project root?

Everything path-shaped hangs off one decision: which directory the extension considers your project root. Get it wrong and the symptoms are baffling rather than obvious — `.env` values resolve to nothing, route and config lookups come back empty, go-to-definition misses files that plainly exist, and the file watchers sit on a subtree instead of your project.

**How the root is chosen.** Inside your open workspace, the extension walks *down* from the workspace folder toward the file you opened and takes the **outermost** directory that looks like a Laravel project — one holding a `composer.json` plus any of:

- `artisan`
- `app/` **and** `resources/`
- `src/` **and** `vendor/` (a package checkout)

Outermost-wins is deliberate. In a modular monolith — `app/{Parent}/{Module}/` layouts where per-module `composer.json` files are merged into the workspace manifest via composer-merge-plugin — *every module* matches the same markers as the workspace itself. Picking the nearest match instead would hand the entire server to whichever module you happened to open first.

For a file **outside** your workspace (a vendor file, a globally installed package), there is no folder to walk down from, so the extension walks up from the file and takes the nearest enclosing project.

**Confirm which root it picked.** `Cmd+Shift+P → "open language server logs"`, choose **Laravel CE**, and search for `Found Laravel`. The line names both the directory and the rule that matched it (a package checkout logs *package root* rather than *project root*).

**The usual cause of a wrong answer: a stray `vendor/`.** A leftover or half-installed `vendor/` directory inside a subdirectory used to be enough to convince older versions that the subdirectory was a project in its own right. The check now requires `vendor/autoload.php` to exist as a real file, so an empty `vendor/` no longer counts. If you are on an older build and seeing this, delete the stray directory and restart Zed.

**Opening a module directly is supported.** If you genuinely want to work on one module in isolation, open *that folder* as your Zed workspace — it then becomes the outermost match and the root, exactly as you'd expect.

> 📌 **A sub-app with hoisted dependencies resolves to the workspace root.** A directory holding `composer.json` + `app/` + `resources/` but no `artisan` of its own — dependencies hoisted up to the workspace `vendor/` — is treated as part of the workspace rather than as its own project. Real Laravel applications ship `artisan`, so this is rare; if you hit it, open the sub-app as its own workspace.

---

See also: [⚙️ Configuration](configuration.md) · [🌱 Environment files](environment.md) · [🔧 Tuning Intelephense](tuning-intelephense.md)
