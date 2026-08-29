# 🌱 Environment files (`.env`)

[← Back to README](../README.md)

Open a `.env` in Zed and every single line lights up with a warning:

> `APP_NAME appears unused. Verify use (or export if used externally)`

Nothing is wrong with your file. The message is shellcheck's [SC2034](https://www.shellcheck.net/wiki/SC2034). Zed classifies `.env` files as the **Shell Script** language and runs its bundled bash language server on them, which calls shellcheck. SC2034 flags any variable that's assigned but never *referenced in the same file* — and a `.env` is nothing but assignments that Laravel reads at runtime through `config()` / `env()`, never inside the file itself. So shellcheck flags **every line**. It's shell-script linting applied to a data file.

## 🔧 How to silence SC2034

**You pick the approach — the extension does not do it for you.** Versions
0.7.2 and 0.7.3 injected `--exclude=SC2034` into the bash language server's
shellcheck arguments on your behalf. That was removed in 0.7.4: it
reconfigured a language server this extension neither provides nor owns,
which Zed's [extension publishing prerequisites](https://zed.dev/docs/extensions/publishing/prerequisites)
do not permit. If your `.env` files went quiet under 0.7.2 or 0.7.3 and are
noisy again, pick one of the four approaches below. Each is a one-time
setting; approach 2 (`.shellcheckrc`) is the one that works in any editor and
on every Zed version, so start there if you want the shortest route.

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

> ⚠️ **This detaches our `.env` features.** The reference-count lens and the inline-comment highlighting below attach to the **Shell Script** language — Zed's default classification for `.env` and every `.env.*` variant. Remap to Ini or Plain Text and you trade shellcheck noise for losing them. Approaches 1–3 silence SC2034 without giving anything up, so reach for this one only when you actually want a different grammar on `.env`.

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

## Why this extension attaches to `Shell Script`

`extension.toml` lists `Shell Script` under the `languages` key for the
`laravel-lsp` server. That key is an *attach* list — "run my server on files
Zed has already classified this way" — not an ownership claim:

- **This extension defines no languages and ships no grammars.** There is no
  `languages/` directory and no `[grammars]` section in its manifest. It
  cannot register, override, or take over a language, and it does not try to.
- **The `.env` features are gated on the filename, never on Zed's
  classification.** The server checks that a file is named `.env` or
  `.env.<something>` before it does anything env-specific. Inside such a
  buffer, hover and go-to-definition act on the key declaration under the
  cursor; find-references and rename still only ever run on `.php` and
  `.blade.php`. Every other file reaches those handlers through the
  `.php`/`.blade.php` test alone, so opening a real `.sh` script gets nothing
  from this extension — no hover, no go-to-definition, no diagnostics.
- **`Shell Script` is the only route to your `.env` files.** Zed ships no
  `env` language, and classifies every env file as Shell Script: bare `.env`
  via the bash grammar's `path_suffixes`, and the `.env.*` variants via Zed's
  own default settings. That one attach entry reaches `.env` and every
  variant alike, with no configuration from you.

## What the extension can't do for you

- **Silence shellcheck.** Writing configuration into the bash language server
  would mean reaching outside this extension's own environment. Zed's
  publishing prerequisites rule that out, so the approaches above are your
  levers, not ours.
- **Change how Zed classifies files.** The extension manifest has no
  `file_types` field, so the only lever an extension has is a language of its
  own with matching `path_suffixes` — and that lever reaches exactly half the
  problem. Bare `.env` is claimed by the bash grammar's `path_suffixes`, which
  an extension language could tie and win; the `.env.*` variants are claimed
  by Zed's *default settings*, a tier no extension can reach and only *your*
  `file_types` can override. Shipping a language would therefore fix `.env`
  and leave `.env.local` shell-linted — a split we'd rather not hand you.
  That's why approach 4 is one line in your config, and why this extension
  attaches to Shell Script instead of trying to replace it.
