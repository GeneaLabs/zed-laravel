use std::path::{Path, PathBuf};
use std::{env, fs};

/// This build script runs before compilation and downloads the tree-sitter-blade grammar
/// from GitHub, then compiles it using the C compiler.
///
/// Build scripts are special Rust programs that run at build time (not runtime).
/// They can access special environment variables set by Cargo.
fn main() {
    // Tell Cargo to re-run this build script if it changes
    println!("cargo:rerun-if-changed=build.rs");

    // Emit build-time metadata so the LSP startup banner can identify
    // which binary is actually running. Useful when "did Zed pick up
    // the new build?" comes up — the user matches the short hash to
    // a known commit.
    emit_build_metadata();

    // Bootstrap the `../test-project` fixture (gitignored `.env` + Composer
    // `vendor/`) so `cargo test` is self-contained — the integration tests
    // read real env files and vendor packages off disk. Guarded + best-effort,
    // so a plain build never breaks on it.
    bootstrap_test_fixture();

    // Get the output directory where Cargo puts build artifacts
    // OUT_DIR is set by Cargo and points to target/debug/build/<package>/out
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Path where we'll download and extract the grammar
    let grammar_dir = out_dir.join("tree-sitter-blade");

    // Only download if we haven't already
    if !grammar_dir.exists() {
        println!("cargo:warning=Downloading tree-sitter-blade grammar from GitHub...");
        download_and_extract_blade_grammar(&grammar_dir);
    } else {
        println!("cargo:warning=Using cached tree-sitter-blade grammar");
    }

    // Compile the Blade grammar's C code
    compile_blade_grammar(&grammar_dir);
}

/// Capture the current git short-hash + dirty-state at build time and
/// expose them through `env!()` for inclusion in the startup banner.
fn emit_build_metadata() {
    let git_hash = std::process::Command::new("git")
        .args(["rev-parse", "--short=5", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false);

    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=LARAVEL_LSP_GIT_HASH={git_hash}{suffix}");

    // Best-effort re-run triggers. Cargo can't depend on "is the
    // working tree dirty," but `.git/HEAD` and `.git/index` cover
    // commits and `git add`. Out-of-band edits won't bust the cache
    // — full clean builds will pick them up.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}

/// Prepare the integration-test fixture under `../test-project` so that
/// `cargo test` works without any manual setup. Mirrors the CI steps
/// "Bootstrap test fixture .env" and "Install test-project Composer
/// dependencies" (see `.github/workflows/ci.yml`):
///
///   1. Copy `.env.example` → `.env` (gitignored) for the env-parsing tests.
///   2. Run `composer update` to populate `vendor/` (also gitignored) so the
///      route-discovery tests can read real packages like Fortify.
///
/// Design notes (the same shape as the grammar download above):
///   - **Debug-only.** Skipped for release builds — `build.sh` ships the LSP
///     via `cargo build --release`, which has no business touching a test
///     fixture. `cargo test` uses the debug profile, so tests still get it.
///   - **Existence-guarded.** Each step is a no-op once done, so this stays
///     cheap on the many rebuilds triggered by `.git/HEAD`/`.git/index`.
///   - **Best-effort.** A missing or failing `composer` only emits a
///     `cargo:warning`; a plain `cargo build` for someone who never runs the
///     integration tests must still succeed. The affected tests will fail
///     with a clear message if the fixture ends up incomplete.
fn bootstrap_test_fixture() {
    // PROFILE is "debug" or "release" for build scripts. Only bootstrap for
    // debug builds (which is what `cargo test` produces).
    if env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // The crate lives at `<root>/laravel-lsp`; the fixture is `<root>/test-project`.
    let Some(fixture) = manifest_dir.parent().map(|p| p.join("test-project")) else {
        return;
    };
    if !fixture.is_dir() {
        // No fixture in this checkout (e.g. a packaged crate) — nothing to do.
        return;
    }

    // Re-run when the fixture's dependency manifest changes (e.g. a package
    // is added), so `vendor/` can be refreshed on the next build.
    println!(
        "cargo:rerun-if-changed={}",
        fixture.join("composer.json").display()
    );

    // 1. `.env` from `.env.example`.
    let env_file = fixture.join(".env");
    let env_example = fixture.join(".env.example");
    if !env_file.exists() && env_example.exists() {
        if let Err(e) = fs::copy(&env_example, &env_file) {
            println!("cargo:warning=Could not create test-project/.env: {e}");
        }
    }

    // 2. Composer dependencies. `vendor/` and `composer.lock` are both
    //    gitignored, so we use `composer update` (not `install`) to resolve a
    //    fresh lock on the fly — identical flags to CI. `--no-dev` skips the
    //    Pest/PHPUnit dev deps; `--no-scripts` skips the fixture's post-update
    //    artisan hooks (which need dev-only packages and would exit non-zero).
    if !fixture.join("vendor").is_dir() {
        println!("cargo:warning=Installing test-project Composer dependencies (first run)...");
        match std::process::Command::new("composer")
            .current_dir(&fixture)
            .args([
                "update",
                "--no-interaction",
                "--prefer-dist",
                "--no-progress",
                "--no-dev",
                "--no-scripts",
            ])
            .status()
        {
            Ok(s) if s.success() => {
                println!("cargo:warning=test-project Composer dependencies ready");
            }
            Ok(s) => {
                println!(
                    "cargo:warning=composer exited with {s}; route-discovery integration tests may fail"
                );
            }
            Err(e) => {
                println!(
                    "cargo:warning=composer not found ({e}); install Composer to run the integration tests"
                );
            }
        }
    }
}

/// Downloads the tree-sitter-blade grammar from GitHub and extracts it
fn download_and_extract_blade_grammar(dest: &PathBuf) {
    // GitHub URL for the latest release tarball
    // Using the 'main' branch - in production, you'd pin a specific version/tag
    let url = "https://github.com/EmranMR/tree-sitter-blade/archive/refs/heads/main.tar.gz";

    println!("cargo:warning=Downloading from: {}", url);

    // Download the tarball using ureq (a simple HTTP client)
    // This is synchronous - it blocks until download completes
    let response = ureq::get(url)
        .call()
        .expect("Failed to download tree-sitter-blade grammar");

    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    std::io::copy(&mut reader, &mut bytes).expect("Failed to read download response");

    // The downloaded file is a .tar.gz (gzipped tarball)
    // We need to:
    // 1. Decompress with gzip (flate2)
    // 2. Extract tar archive (tar crate)

    // Step 1: Decompress gzip
    use flate2::read::GzDecoder;
    let decompressed = GzDecoder::new(&bytes[..]);

    // Step 2: Extract tar archive
    let mut archive = tar::Archive::new(decompressed);

    // Create the destination directory
    fs::create_dir_all(dest.parent().unwrap()).expect("Failed to create grammar directory");

    // Extract to a temporary location (archive root is "tree-sitter-blade-main")
    let temp_dir = dest.parent().unwrap().join("tree-sitter-blade-temp");
    archive
        .unpack(&temp_dir)
        .expect("Failed to extract tar archive");

    // Move the extracted folder to our desired location
    // The archive extracts to "tree-sitter-blade-main/"
    let extracted = temp_dir.join("tree-sitter-blade-main");
    fs::rename(extracted, dest).expect("Failed to move extracted grammar");

    // Clean up temp directory
    fs::remove_dir_all(temp_dir).ok();

    println!("cargo:warning=Successfully extracted tree-sitter-blade grammar");
}

/// Compiles the Blade grammar's C code using the cc crate
fn compile_blade_grammar(grammar_dir: &Path) {
    // Tree-sitter grammars are written in C and consist of:
    // - parser.c: The main parser logic (generated from grammar.js)
    // - scanner.c: Custom lexer for language-specific tokens (optional, hand-written)

    let src_dir = grammar_dir.join("src");

    println!(
        "cargo:warning=Compiling Blade grammar C code from {:?}",
        src_dir
    );

    // The cc crate is a build-time dependency that wraps the C compiler
    // It automatically detects your system's C compiler (gcc, clang, msvc)
    cc::Build::new()
        // Include the tree-sitter header files
        .include(&src_dir)
        // Compile the main parser
        .file(src_dir.join("parser.c"))
        // Compile the custom scanner (if it exists)
        .file(src_dir.join("scanner.c"))
        // Output as a static library named "tree-sitter-blade"
        // This creates libtree-sitter-blade.a (Unix) or tree-sitter-blade.lib (Windows)
        .compile("tree-sitter-blade");

    println!("cargo:warning=Successfully compiled Blade grammar");
}
