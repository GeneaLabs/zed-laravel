# 🌱 Environment files (`.env`)

[← Back to README](../README.md)

Open a `.env` in Zed and every single line lights up with a warning:

> `APP_NAME appears unused. Verify use (or export if used externally)`

Nothing is wrong with your file. The message is shellcheck's [SC2034](https://www.shellcheck.net/wiki/SC2034). Zed classifies `.env` files as the **Shell Script** language and runs its bundled bash language server on them, which calls shellcheck. SC2034 flags any variable that's assigned but never *referenced in the same file* — and a `.env` is nothing but assignments that Laravel reads at runtime through `config()` / `env()`, never inside the file itself. So shellcheck flags **every line**. It's shell-script linting applied to a data file.

## ✅ Fixed automatically in Laravel projects (v0.7.2+)

This extension quiets the noise for you. In any worktree whose `composer.json` depends on the `laravel/*` or `illuminate/*` vendor namespaces (full apps **and** packages — detection doesn't rely on `artisan` existing), the extension injects `--exclude=SC2034` into the bash language server's shellcheck arguments.

- **Your own arguments are preserved.** Anything you've set under `lsp.bash-language-server.settings.bashIde.shellcheckArguments` is kept, with the exclusion appended — and if your arguments already mention `SC2034` (excluded *or* deliberately kept), they pass through untouched.
- **Scope:** shellcheck configuration is per-server, not per-file, so SC2034 is muted for every shell file in that worktree — real `.sh` scripts included. Every *other* shellcheck rule still applies to them. Non-Laravel worktrees are never touched.
- **Works on every Zed version**, including releases where `lsp.bash-language-server.settings` itself is silently dropped (see the warning below) — the injection travels through Zed's extension hook for configuring *other* language servers, which is delivered independently.

Opt out per-project or globally:

```json
{
  "lsp": {
    "laravel-lsp": {
      "settings": {
        "shellcheck": { "suppressUnusedVarWarnings": false }
      }
    }
  }
}
```

The approaches below remain useful for non-Laravel worktrees, opted-out setups, and editors other than Zed.

## 🔧 Manual approaches

### 1. Silence the rule via Zed settings

> ⚠️ **Broken on Zed 1.8.2 and older.** Zed's built-in bash adapter never forwarded `lsp.bash-language-server.settings` to the server — a correctly-written block below is silently ignored (the server's config parser falls back to defaults without an error). Fixed upstream in [zed#57487](https://github.com/zed-industries/zed/pull/57487); on affected versions use the `.shellcheckrc` route instead.

```json
{
  "lsp": {
    "bash-language-server": {
      "settings": {
        "bashIde": {
          "shellcheckArguments": ["--exclude=SC2034"]
        }
      }
    }
  }
}
```

> The `bashIde` wrapper is required — the bash server only reads config nested under that key; without it the setting is silently ignored. (`shellcheckArguments` takes an array; the server adds `--shell` / `--format` on its own.)

This applies to every shell file Zed lints, in every project.

### 2. `.shellcheckrc` (editor-agnostic, works on every Zed version)

Drop a `.shellcheckrc` in your project root (or `~/.shellcheckrc` for all projects) containing:

```
disable=SC2034
```

shellcheck reads this file directly — the server pipes your buffer to shellcheck with the project root as working directory, and shellcheck walks up from there (falling back to `~/.shellcheckrc`). Works in any editor. Scope is the whole tree, so real `.sh` scripts there also lose SC2034.

For a single file, a comment at the top works too:

```bash
# shellcheck disable=SC2034
```

### 3. Disable the shell language server — syntax highlighting stays

Highlighting comes from Zed's built-in tree-sitter bash grammar, not from the language server — so you can switch the server off entirely and `.env` (and `.sh`) files keep their colors:

```json
{
  "languages": {
    "Shell Script": {
      "language_servers": ["!bash-language-server", "..."]
    }
  }
}
```

No shellcheck, no bash completions/hover, no shfmt formatting — for **all** Shell Script files, not just `.env`. Highlighting, brackets, and indentation are untouched. Use `.zed/settings.json` to scope it to one project.

### 4. Reclassify `.env` away from Shell Script

Map `.env` files to a non-shell language in `settings.json`. A `.env` is `KEY=value` with `#` comments — structurally INI — so the **Ini** language highlights it cleanly and never invokes shellcheck:

```json
{
  "file_types": {
    "Ini": [".env", ".env.*"]
  }
}
```

Install the Ini extension (`zed: extensions`, search "INI") if you don't already have it. To avoid any extra install, map to the built-in **Plain Text** instead — the warnings disappear, but `.env` renders without highlighting.

> ⚠️ **This detaches our `.env` features.** The reference-count lens, hover, and go-to attach to the **Shell Script** language — Zed's default classification for `.env` and every `.env.*` variant. Remap to Ini or Plain Text and you trade shellcheck noise for losing them. Since v0.7.2 silences that noise automatically, approach 4 is now rarely the right trade.

### A note on dedicated dotenv extensions

[`zarifpour/zed-env`](https://github.com/zarifpour/zed-env) ships a purpose-built dotenv grammar and looks like the obvious answer. It isn't — for two reasons worth knowing before you install it:

- **It does not claim your `.env` files.** Its `path_suffixes` list a bare `"env"`, which loses the length tie to Shell Script's `".env"`; the `.env.*` variants are claimed by Zed's own default settings at a tier the extension can't reach. Installing it changes nothing about `.env` unless you *also* write a `file_types` line — and that line works with or without the extension. This is the extension's own [issue #5](https://github.com/zarifpour/zed-env/issues/5), still open.
- **It reclassifies files you didn't ask it to.** Those same `path_suffixes` include bare `"conf"`, `"example"`, `"local"`, and `"test"`, which match *any* otherwise-unclaimed file ending in them. In a stock Laravel app that captures `laravel/sail`'s `supervisord.conf` files, and it will keep reaching across every project you open.

## 💬 Inline comments

Inline comments are standard dotenv syntax — `vlucas/phpdotenv` (what Laravel runs), `motdotla/dotenv`, `bkeepers/dotenv`, and `python-dotenv` all support them, and all agree that inside an **unquoted** value a `#` starts a comment with no preceding space required:

```dotenv
APP_NAME=Laravel        # this is a comment
SECRET="has#a#hash"     # quoted, so the hashes are part of the value
DB_PASSWORD=p@ss#word   # the value here is p@ss — NOT p@ss#word
```

**To keep a literal `#` in a value, quote it.** That is the documented remedy in every implementation, not a workaround.

Because Zed highlights `.env` with the bash grammar, and bash treats a mid-word `#` as ordinary text, the third line above renders as though `#word` were part of the value. The value colouring is misleading; Laravel still reads `p@ss`. Comments written the idiomatic way — with a space before the `#` — highlight correctly.

The extension corrects this via LSP semantic tokens, which Zed leaves **off** by default. To turn it on:

```json
{
  "languages": {
    "Shell Script": {
      "semantic_tokens": "combined"
    }
  }
}
```

`"combined"` overlays our tokens on the bash grammar's colours rather than replacing them. Without it the highlighting is cosmetic-only and everything above still parses the same way at runtime.

## Per-project

Any of the `settings.json` blocks above also work in `.zed/settings.json` at the project root, scoping the change to one project instead of all of Zed. (The `.shellcheckrc` approach is already project-scoped by nature.)

## What the extension can — and can't — do for you

- **Can (and does):** configure the bash language server. Zed lets one extension contribute *additional workspace configuration* to another server, merged into that server's own config — that's how the automatic fix above is delivered, and why it works even on Zed versions that drop your own `bash-language-server` settings.
- **Can't:** change how Zed classifies files. The extension manifest has no `file_types` field, so the only lever an extension has is a language of its own with matching `path_suffixes` — and that lever reaches exactly half the problem. Bare `.env` is claimed by the bash grammar's `path_suffixes`, which an extension language could tie and win; the `.env.*` variants are claimed by Zed's *default settings*, a tier no extension can reach and only *your* `file_types` can override. Shipping a language would therefore fix `.env` and leave `.env.local` shell-linted — a split we'd rather not hand you. That's why approach 4 is one line in your config, and why this extension attaches to Shell Script instead of trying to replace it.
