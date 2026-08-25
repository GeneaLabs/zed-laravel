//! Absolute-path fixtures that hold on every platform.
//!
//! A literal like `"/tmp/routes/web.php"` is absolute on Unix but merely
//! *rooted* on Windows — it carries no drive prefix, so `Path::is_absolute`
//! is false and `Url::from_file_path` refuses it. Any code that turns a path
//! into a URI then returns `None`, and the test sees an empty result rather
//! than the failure's real cause (issue #292).
//!
//! Fixtures that need a genuinely absolute path build it here instead of
//! spelling one inline.

use std::path::PathBuf;

/// An absolute path for the current platform, from a Unix-style spelling.
///
/// `abs("/tmp/routes/web.php")` yields `/tmp/routes/web.php` on Unix and
/// `C:\tmp\routes\web.php` on Windows.
pub fn abs(spelling: &str) -> PathBuf {
    let relative = spelling.trim_start_matches('/');
    if cfg!(windows) {
        PathBuf::from(format!(r"C:\{}", relative.replace('/', r"\")))
    } else {
        PathBuf::from(format!("/{relative}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_is_absolute_on_this_platform() {
        // The whole point: `is_absolute` must hold wherever the suite runs,
        // which a bare "/tmp/..." literal does not satisfy on Windows.
        assert!(abs("/tmp/routes/web.php").is_absolute());
        assert!(abs("tmp/routes/web.php").is_absolute());
    }

    #[test]
    fn abs_round_trips_through_a_file_url() {
        // The exact conversion that silently returned None on Windows.
        let url = tower_lsp::lsp_types::Url::from_file_path(abs("/tmp/routes/web.php"))
            .expect("fixture path must convert to a file URL");
        assert_eq!(url.scheme(), "file");
    }
}
