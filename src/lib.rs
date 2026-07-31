use std::fs;
use zed_extension_api::{self as zed, Result};

/// Extension version - used for versioned binary directory
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Unsuffixed binary names to look for on the user's `PATH`, in preference
/// order, when the automatic download is blocked and they've placed the
/// server there by hand (see `docs/troubleshooting.md`).
///
/// The pre-rebrand `laravel-lsp` stays as a fallback: the published
/// troubleshooting doc told users to install under that name for every
/// release before the "Laravel CE" rename, and silently failing to find a
/// binary they already installed would look exactly like the download
/// problem they worked around in the first place.
const PATH_BINARY_NAMES: [&str; 2] = ["laravel-ce-lsp", "laravel-lsp"];

/// The main struct for our Laravel extension
struct LaravelExtension {
    /// Cached path to the language server binary
    cached_binary_path: Option<String>,
}

impl zed::Extension for LaravelExtension {
    fn new() -> Self {
        LaravelExtension {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary_path = self.language_server_binary_path(worktree)?;

        Ok(zed::Command {
            command: binary_path,
            args: vec![],
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        // Forward `lsp.laravel-lsp.initialization_options` to the server's
        // `initialize` params. Without this Zed sends `None`, and any settings
        // a user places under `initialization_options` are silently dropped.
        Ok(
            zed::settings::LspSettings::for_worktree("laravel-lsp", worktree)
                .ok()
                .and_then(|s| s.initialization_options),
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        // Forward `lsp.laravel-lsp.settings` so the server's
        // `workspace/configuration` pull (and `didChangeConfiguration`) actually
        // carries the user's settings. Zed does NOT do this automatically for
        // extension-provided servers — without this hook it answers the pull
        // with `{}`, so every setting (codeLens.enabled, blade.directiveSpacing,
        // diagnostics.severity, …) stays at its default.
        Ok(
            zed::settings::LspSettings::for_worktree("laravel-lsp", worktree)
                .ok()
                .and_then(|s| s.settings),
        )
    }
}

impl LaravelExtension {
    /// Get or download the language server binary
    ///
    /// Search order:
    /// 1. Check cached path (verify still exists)
    /// 2. Check versioned extension directory (laravel-lsp-{VERSION}/)
    /// 3. Try system PATH via worktree.which()
    /// 4. Download from GitHub releases
    fn language_server_binary_path(&mut self, worktree: &zed::Worktree) -> Result<String> {
        // Step 1: Check cached path
        if let Some(cached_path) = &self.cached_binary_path {
            if fs::metadata(cached_path).is_ok() {
                return Ok(cached_path.clone());
            }
        }

        let binary_name = Self::get_platform_binary_name();
        let version_dir = format!("laravel-lsp-{}", VERSION);
        let binary_path = format!("{}/{}", version_dir, binary_name);

        // Step 2: Check versioned extension directory
        if fs::metadata(&binary_path).is_ok() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // Step 3: Try system PATH
        if let Some(path) = worktree.which(&binary_name) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Also try the unsuffixed names in PATH, current name first.
        if let Some(path) = Self::which_generic(|name| worktree.which(name)) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Step 4: Download from GitHub releases
        let downloaded_path = self.download_binary(&binary_name, &version_dir)?;
        self.cached_binary_path = Some(downloaded_path.clone());
        Ok(downloaded_path)
    }

    /// Download the binary from GitHub releases
    fn download_binary(&self, binary_name: &str, version_dir: &str) -> Result<String> {
        let binary_path = format!("{}/{}", version_dir, binary_name);

        // Check if already downloaded
        if fs::metadata(&binary_path).is_ok() {
            return Ok(binary_path);
        }

        let (os, _arch) = zed::current_platform();
        let archive_ext = match os {
            zed::Os::Windows => "zip",
            _ => "tar.gz",
        };
        let archive_name = format!("{}.{}", binary_name, archive_ext);

        let release_url = format!(
            "https://github.com/mike-bronner/zed-laravel/releases/download/{}/{}",
            VERSION, archive_name
        );

        let file_type = match os {
            zed::Os::Windows => zed::DownloadedFileType::Zip,
            _ => zed::DownloadedFileType::GzipTar,
        };

        // Download and extract
        zed::download_file(&release_url, version_dir, file_type)
            .map_err(|e| format!("Failed to download Laravel CE LSP binary: {}", e))?;

        // Verify extraction succeeded
        if fs::metadata(&binary_path).is_err() {
            return Err(format!(
                "Binary not found after extraction. Expected at: {}",
                binary_path
            ));
        }

        // Make the binary executable via the Zed host (extensions run as WASM,
        // so std::os::unix::fs is unavailable here).
        zed::make_file_executable(&binary_path)
            .map_err(|e| format!("Failed to make Laravel CE LSP binary executable: {}", e))?;

        Ok(binary_path)
    }

    /// Get platform-specific binary name
    fn get_platform_binary_name() -> String {
        let (os, arch) = zed::current_platform();
        Self::platform_binary_name(os, arch)
    }

    /// Map a platform triple to the published release-asset binary name.
    ///
    /// The Linux assets are statically-linked musl builds, so a single
    /// binary per arch runs on glibc distros, musl distros (Alpine), and
    /// loader-less setups (NixOS) alike — no libc detection needed.
    fn platform_binary_name(os: zed::Os, arch: zed::Architecture) -> String {
        match (os, arch) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                "laravel-ce-lsp-windows-x64.exe".to_string()
            }
            (zed::Os::Windows, zed::Architecture::Aarch64) => {
                "laravel-ce-lsp-windows-arm64.exe".to_string()
            }
            (zed::Os::Windows, _) => "laravel-ce-lsp.exe".to_string(),
            (zed::Os::Mac, zed::Architecture::Aarch64) => "laravel-ce-lsp-macos-arm64".to_string(),
            (zed::Os::Mac, zed::Architecture::X8664) => "laravel-ce-lsp-macos-x64".to_string(),
            (zed::Os::Mac, _) => "laravel-ce-lsp".to_string(),
            (zed::Os::Linux, zed::Architecture::X8664) => "laravel-ce-lsp-linux-x64".to_string(),
            (zed::Os::Linux, zed::Architecture::Aarch64) => {
                "laravel-ce-lsp-linux-arm64".to_string()
            }
            (zed::Os::Linux, _) => "laravel-ce-lsp".to_string(),
        }
    }

    /// Resolve the server binary from the user's `PATH` by its unsuffixed
    /// name, trying each of [`PATH_BINARY_NAMES`] in order.
    ///
    /// Takes the lookup as a closure rather than a `&Worktree` so the
    /// preference order is testable — `Worktree` is a host-provided handle
    /// that can't be constructed outside a running Zed.
    fn which_generic(mut which: impl FnMut(&str) -> Option<String>) -> Option<String> {
        PATH_BINARY_NAMES.iter().find_map(|name| which(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zed::{Architecture, Os};

    #[test]
    fn linux_names() {
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::X8664),
            "laravel-ce-lsp-linux-x64"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::Aarch64),
            "laravel-ce-lsp-linux-arm64"
        );
    }

    #[test]
    fn mac_and_windows_names() {
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Mac, Architecture::Aarch64),
            "laravel-ce-lsp-macos-arm64"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Mac, Architecture::X8664),
            "laravel-ce-lsp-macos-x64"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Windows, Architecture::X8664),
            "laravel-ce-lsp-windows-x64.exe"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Windows, Architecture::Aarch64),
            "laravel-ce-lsp-windows-arm64.exe"
        );
    }

    /// A `worktree.which()` stand-in that only knows about `present`.
    fn path_with(present: &'static [&'static str]) -> impl FnMut(&str) -> Option<String> {
        move |name| present.contains(&name).then(|| format!("/usr/bin/{name}"))
    }

    #[test]
    fn path_lookup_finds_the_current_name() {
        assert_eq!(
            LaravelExtension::which_generic(path_with(&["laravel-ce-lsp"])),
            Some("/usr/bin/laravel-ce-lsp".to_string())
        );
    }

    #[test]
    fn path_lookup_falls_back_to_the_pre_rebrand_name() {
        assert_eq!(
            LaravelExtension::which_generic(path_with(&["laravel-lsp"])),
            Some("/usr/bin/laravel-lsp".to_string()),
            "a binary installed under the old name must keep working"
        );
    }

    #[test]
    fn path_lookup_prefers_the_current_name_over_the_legacy_one() {
        assert_eq!(
            LaravelExtension::which_generic(path_with(&["laravel-lsp", "laravel-ce-lsp"])),
            Some("/usr/bin/laravel-ce-lsp".to_string()),
            "with both on PATH the rebranded binary wins"
        );
    }

    #[test]
    fn path_lookup_reports_nothing_when_no_binary_is_installed() {
        assert_eq!(LaravelExtension::which_generic(path_with(&[])), None);
    }

    #[test]
    fn path_lookup_stops_at_the_first_hit() {
        let mut probed = Vec::new();
        LaravelExtension::which_generic(|name| {
            probed.push(name.to_string());
            (name == "laravel-ce-lsp").then(|| name.to_string())
        });
        assert_eq!(
            probed,
            ["laravel-ce-lsp"],
            "the legacy name must not be probed once the current one resolves"
        );
    }
}

zed::register_extension!(LaravelExtension);
