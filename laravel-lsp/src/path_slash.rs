//! Rewrite a native filesystem path's separators to `/` — the single source of
//! truth for "match a forward-slash directory marker against a real path".
//!
//! Several call sites locate a directory inside a path by searching for a
//! literal forward-slash marker (`"/components/"`,
//! `"resources/views/components/"`) or split one on `'/'`. The paths they
//! search are *native* — they come from [`url::Url::to_file_path`], which
//! yields `C:\Users\…\resources\views\components\button.blade.php` on Windows
//! — so every one of those markers silently fails to match there, and the
//! feature behind it goes quietly dead rather than erroring (issue #292).
//!
//! Mixing [`std::path::MAIN_SEPARATOR`] into an otherwise forward-slash marker
//! does not fix it: `format!("{}resources/views/components/", MAIN_SEPARATOR)`
//! builds `\resources/views/components/` on Windows, which matches neither a
//! native path nor a slash-normalized one.
//!
//! Normalizing the *path* once, then keeping the ordinary `/` marker, is the
//! rule used instead. Matching goes through [`std::path::is_separator`] — `/`
//! on Unix, `/` or `\` on Windows — so a Unix filename containing a literal
//! backslash (a legal character there, and never a separator) is left alone,
//! while every Windows separator is rewritten. No `cfg` needed, the same
//! approach [`crate::path_join`] already takes.

use std::borrow::Cow;

/// Rewrite `path`'s native directory separators to `/`.
///
/// Borrows unchanged when there is nothing to rewrite, which on Unix is always
/// — [`std::path::is_separator`] accepts only `/` there, so this is a no-op on
/// the platform whose markers already work.
///
/// The result is for **matching and splitting only**. Do not hand it back to
/// the filesystem: on Windows a verbatim path (`\\?\C:\…`, what `canonicalize`
/// returns) is passed to the OS unnormalized, and a `/` inside one does not
/// resolve.
pub fn to_slash(path: &str) -> Cow<'_, str> {
    if path.contains(|c| std::path::is_separator(c) && c != '/') {
        Cow::Owned(
            path.chars()
                .map(|c| if std::path::is_separator(c) { '/' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_slash_path_is_borrowed_unchanged() {
        let out = to_slash("/app/resources/views/components/button.blade.php");
        assert!(matches!(out, Cow::Borrowed(_)), "no rewrite should borrow");
        assert_eq!(out, "/app/resources/views/components/button.blade.php");
    }

    #[test]
    fn marker_matches_after_normalizing() {
        // The whole point: this assertion is false against the raw input on
        // Windows and true after normalizing, on both platforms.
        let native = if cfg!(windows) {
            r"C:\app\resources\views\components\button.blade.php"
        } else {
            "/app/resources/views/components/button.blade.php"
        };
        assert!(to_slash(native).contains("/components/"));
    }

    #[test]
    fn empty_path_is_unchanged() {
        assert_eq!(to_slash(""), "");
    }

    #[cfg(windows)]
    #[test]
    fn backslashes_become_slashes_on_windows() {
        assert_eq!(
            to_slash(r"C:\app\resources\views\components\button.blade.php"),
            "C:/app/resources/views/components/button.blade.php"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_prefix_is_normalized_too() {
        // `canonicalize` returns verbatim paths on Windows, so this shape is
        // reachable from ordinary code here.
        assert_eq!(
            to_slash(r"\\?\C:\app\resources\views\components\card.blade.php"),
            "//?/C:/app/resources/views/components/card.blade.php"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn backslash_is_an_ordinary_filename_character_on_unix() {
        // `\` is legal in a Unix filename and is never a separator there, so
        // rewriting it would corrupt the path.
        let weird = r"/app/we\ird/components/button.blade.php";
        let out = to_slash(weird);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, weird);
    }
}
