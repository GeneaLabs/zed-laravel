# 🩺 Troubleshooting

[← Back to README](../README.md)

**The extension installed but nothing happens — no features, no "Laravel CE" entry in the language-server list.** Work through these in order; each is a real cause we've seen.

## 1. Is your Zed new enough?

The extension is built against `zed_extension_api 0.7.0`, which requires **Zed 0.205 or newer**. Older versions silently refuse to load it — the extension appears installed but never activates, and the log stays quiet. Check `Zed → About Zed` (or `zed --version`) and update if you're behind.

## 2. Does Zed recognize the file as PHP / Blade?

The language server only attaches to files Zed has classified as **PHP**, **Blade**, **XML**, or `.env` (Shell Script). Open a `.php` file and check the **bottom-right status bar**:

- Says **"PHP"** → good, the language is registered.
- Says **"Plain Text"** → install the official [**PHP**](https://github.com/zed-extensions/php) extension (and [**Laravel Blade**](https://github.com/bajrangCoder/zed-laravel-blade) for `.blade.php`). Without a language registered, no language server — ours included — can attach.

## 3. Did you override `language_servers` for PHP or Blade?

If features work in `.env` files but not in `.php` / `.blade.php`, you've almost certainly set an explicit `language_servers` list that omits `laravel-lsp`. See [the settings block](configuration.md#all-settings) for the fix — include `"laravel-lsp"` in the list.

## 4. Check the language-server log

The running servers show under the **lightning-bolt icon** in the status bar. For the full log: `Cmd+Shift+P → "open language server logs"` and look for **Laravel CE**. If it's missing entirely, the server never started (revisit steps 1–3). If it's present but erroring, the log will say why — please [open an issue](https://github.com/mike-bronner/zed-laravel/issues) with that output.

## 5. Manual binary fallback

The extension downloads its server binary from GitHub releases on first use. If that download is blocked (proxy, firewall, offline), drop the binary on your `PATH` instead — Zed will find it there. Grab the archive for your platform from the [latest release](https://github.com/mike-bronner/zed-laravel/releases/latest), then (macOS x86_64 shown):

```bash
tar -xzf ~/Downloads/laravel-lsp-macos-x64.tar.gz
mkdir -p ~/.local/bin && mv laravel-lsp-macos-x64 ~/.local/bin/laravel-lsp
chmod +x ~/.local/bin/laravel-lsp
xattr -d com.apple.quarantine ~/.local/bin/laravel-lsp 2>/dev/null
# ensure ~/.local/bin is on your PATH, then fully restart Zed
```

---

See also: [⚙️ Configuration](configuration.md) · [🌱 Environment files](environment.md) · [🔧 Tuning Intelephense](tuning-intelephense.md)
