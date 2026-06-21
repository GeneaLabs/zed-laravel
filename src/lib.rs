use std::fs;
use zed_extension_api::{self as zed, Result};

/// Extension version - used for versioned binary directory
const VERSION: &str = env!("CARGO_PKG_VERSION");

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

        // Also try generic name in PATH
        if let Some(path) = worktree.which("laravel-lsp") {
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
            .map_err(|e| format!("Failed to download Laravel LSP binary: {}", e))?;

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
            .map_err(|e| format!("Failed to make Laravel LSP binary executable: {}", e))?;

        Ok(binary_path)
    }

    /// Get platform-specific binary name
    fn get_platform_binary_name() -> String {
        let (os, arch) = zed::current_platform();
        Self::platform_binary_name(os, arch, Self::is_musl_libc())
    }

    /// Map a platform triple to the published release-asset binary name.
    ///
    /// `is_musl` only changes the Linux variants: musl-based distros (Alpine,
    /// etc.) need the `-musl` binary because the glibc-linked build fails to
    /// spawn against musl's loader.
    fn platform_binary_name(os: zed::Os, arch: zed::Architecture, is_musl: bool) -> String {
        match (os, arch) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                "laravel-lsp-windows-x64.exe".to_string()
            }
            (zed::Os::Windows, zed::Architecture::Aarch64) => {
                "laravel-lsp-windows-arm64.exe".to_string()
            }
            (zed::Os::Windows, _) => "laravel-lsp.exe".to_string(),
            (zed::Os::Mac, zed::Architecture::Aarch64) => "laravel-lsp-macos-arm64".to_string(),
            (zed::Os::Mac, zed::Architecture::X8664) => "laravel-lsp-macos-x64".to_string(),
            (zed::Os::Mac, _) => "laravel-lsp".to_string(),
            (zed::Os::Linux, zed::Architecture::X8664) if is_musl => {
                "laravel-lsp-linux-x64-musl".to_string()
            }
            (zed::Os::Linux, zed::Architecture::Aarch64) if is_musl => {
                "laravel-lsp-linux-arm64-musl".to_string()
            }
            (zed::Os::Linux, zed::Architecture::X8664) => "laravel-lsp-linux-x64".to_string(),
            (zed::Os::Linux, zed::Architecture::Aarch64) => "laravel-lsp-linux-arm64".to_string(),
            // Defensive fallback for any future Linux arch the SDK adds: still
            // honor musl so we never serve a glibc binary to a musl host.
            (zed::Os::Linux, _) if is_musl => "laravel-lsp-musl".to_string(),
            (zed::Os::Linux, _) => "laravel-lsp".to_string(),
        }
    }

    /// Detect a musl-based Linux host (Alpine and friends).
    ///
    /// Extensions run as WASM but `std::fs` reads resolve against the host the
    /// LSP will run on — including a remote/devcontainer — so we probe for
    /// musl's dynamic loaders and Alpine's release marker.
    fn is_musl_libc() -> bool {
        Self::detect_musl(|path| fs::metadata(path).is_ok())
    }

    /// Pure musl probe: returns true if any musl/Alpine marker path exists,
    /// per the injected existence check. Split out so it is unit-testable
    /// without touching the real filesystem.
    fn detect_musl(path_exists: impl Fn(&str) -> bool) -> bool {
        [
            "/lib/ld-musl-x86_64.so.1",
            "/lib/ld-musl-aarch64.so.1",
            "/etc/alpine-release",
        ]
        .iter()
        .any(|path| path_exists(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zed::{Architecture, Os};

    #[test]
    fn linux_gnu_names_unchanged() {
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::X8664, false),
            "laravel-lsp-linux-x64"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::Aarch64, false),
            "laravel-lsp-linux-arm64"
        );
    }

    #[test]
    fn linux_musl_names_get_suffix() {
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::X8664, true),
            "laravel-lsp-linux-x64-musl"
        );
        assert_eq!(
            LaravelExtension::platform_binary_name(Os::Linux, Architecture::Aarch64, true),
            "laravel-lsp-linux-arm64-musl"
        );
    }

    #[test]
    fn non_linux_ignores_musl_flag() {
        for is_musl in [true, false] {
            assert_eq!(
                LaravelExtension::platform_binary_name(Os::Mac, Architecture::Aarch64, is_musl),
                "laravel-lsp-macos-arm64"
            );
            assert_eq!(
                LaravelExtension::platform_binary_name(Os::Mac, Architecture::X8664, is_musl),
                "laravel-lsp-macos-x64"
            );
            assert_eq!(
                LaravelExtension::platform_binary_name(Os::Windows, Architecture::X8664, is_musl),
                "laravel-lsp-windows-x64.exe"
            );
            assert_eq!(
                LaravelExtension::platform_binary_name(Os::Windows, Architecture::Aarch64, is_musl),
                "laravel-lsp-windows-arm64.exe"
            );
        }
    }

    #[test]
    fn detect_musl_true_when_any_marker_present() {
        assert!(LaravelExtension::detect_musl(|p| p == "/etc/alpine-release"));
        assert!(LaravelExtension::detect_musl(
            |p| p == "/lib/ld-musl-x86_64.so.1"
        ));
        assert!(LaravelExtension::detect_musl(
            |p| p == "/lib/ld-musl-aarch64.so.1"
        ));
    }

    #[test]
    fn detect_musl_false_when_no_marker_present() {
        assert!(!LaravelExtension::detect_musl(|_| false));
    }
}

zed::register_extension!(LaravelExtension);
