//! Length-split rendering for config and translation completion items.
//!
//! A completion item shows its value in two places that want two different
//! lengths:
//!
//! - `CompletionItem.detail` — a **single line** rendered inline beside the
//!   label in the completion list. Long values blow the line out, or the
//!   editor clips them at a width this server can't see.
//! - `CompletionItem.documentation` — the **markdown panel** beside the
//!   list. It has room for a full sentence, and clipping there throws away
//!   space that was already paid for.
//!
//! Both used to read one already-truncated string, so a single limit had to
//! serve both surfaces and served neither (issue #326). Laravel's own
//! validation and auth messages run 80–150 characters, which is exactly the
//! band where the two surfaces disagree: too long for the line, comfortably
//! inside the panel.
//!
//! The fix is to keep the full value on the completion struct and truncate
//! at each render site instead — [`COMPLETION_DETAIL_LIMIT`] for the line,
//! [`COMPLETION_DOC_LIMIT`] for the panel. Every truncation still routes
//! through [`crate::display_truncate::truncate_for_display`], so the
//! char-boundary safety won in #319 holds at both new cut points.

use crate::completion_format::{CodeBlock, CompletionDoc};
use crate::display_truncate::truncate_for_display;

/// Char budget for the inline `detail` line beside a completion label.
///
/// Deliberately short: `detail` competes for horizontal space with the label
/// and the editor's own chrome, and a value that overflows is either wrapped
/// or silently clipped by the client.
pub const COMPLETION_DETAIL_LIMIT: usize = 50;

/// Char budget for the markdown `documentation` panel.
///
/// Four times [`COMPLETION_DETAIL_LIMIT`], which clears the 80–150 char band
/// Laravel's stock validation and auth messages occupy, so those render in
/// full. Still bounded — the value is attacker-adjacent only in the sense
/// that it comes off disk, but an unbounded panel is a hostile popup for
/// anyone with a long string in a catalogue.
pub const COMPLETION_DOC_LIMIT: usize = 200;

/// The whole point of issue #326 is that these two are *different*. A future
/// tidy-up that collapses them back to one value reproduces the bug, so make
/// it a build failure rather than something a reviewer has to notice.
const _: () = assert!(COMPLETION_DETAIL_LIMIT < COMPLETION_DOC_LIMIT);

/// The inline `detail` line: the value, then its source file in parens.
///
/// Shared by the config and translation branches — the two rendered
/// identically before this split and still do, so one function keeps them
/// from drifting apart when the limit is tuned.
///
/// An empty value degrades to the source alone rather than rendering a
/// leading space and an orphan paren.
pub fn completion_detail(value: &str, source: &str) -> String {
    if value.is_empty() {
        format!("({})", source)
    } else {
        format!(
            "{} ({})",
            truncate_for_display(value, COMPLETION_DETAIL_LIMIT),
            source
        )
    }
}

/// The `documentation` panel for a config key: the key, the value as a PHP
/// `return` statement, and the file it came from.
///
/// Truncates `value` independently of [`completion_detail`] — it is handed
/// the same raw value, never that function's already-clipped output.
pub fn config_documentation(key: &str, value: &str, source: &str) -> CompletionDoc {
    let mut doc = CompletionDoc::new().header(key);
    if !value.is_empty() {
        doc = doc.code(CodeBlock::new(
            "php",
            format!(
                "return {};",
                truncate_for_display(value, COMPLETION_DOC_LIMIT)
            ),
        ));
    }
    doc.section(format!("Source: {}", source))
}

/// The `documentation` panel for a translation key: the key, the translated
/// string as the summary, and the catalogue it came from.
///
/// Truncates `value` independently of [`completion_detail`], for the same
/// reason. A key with no extractable value falls back to a generic summary
/// rather than an empty panel.
pub fn translation_documentation(key: &str, value: &str, source: &str) -> CompletionDoc {
    let summary = if value.is_empty() {
        "Translation key.".to_string()
    } else {
        truncate_for_display(value, COMPLETION_DOC_LIMIT)
    };
    CompletionDoc::new()
        .header(key)
        .summary(summary)
        .section(format!("Source: {}", source))
}

#[cfg(test)]
mod tests;
