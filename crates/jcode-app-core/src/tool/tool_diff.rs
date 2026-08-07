//! The one diff renderer for tool output.
//!
//! `edit`, `patch`, and `apply_patch` each grew their own
//! near-identical `generate_diff`, and they drifted: two stripped every line's
//! leading whitespace with `.trim()`, so the model was shown indented code
//! rendered flush left and lost the structure the indentation carried. The
//! other two kept indentation but numbered lines differently.
//!
//! The dedent rule here is the one already proven in the TUI's `ui_diff.rs`:
//! remove only the indentation *common* to every non-blank line, so a hunk
//! indented twenty columns still renders flush left (which is what the original
//! per-line trim was reaching for) while nesting inside the hunk survives.
//!
//! Tabs are measured at `TAB_WIDTH` and normalized to spaces on output, so a
//! tab-indented file and a space-indented one align the same way.

use similar::{ChangeTag, TextDiff};

/// Columns a tab advances to. Matches `ui_diff.rs` so the tool output and the
/// TUI rendering of the same hunk agree.
const TAB_WIDTH: usize = 4;

/// Default cap on rendered diff lines. `edit` historically had no cap; the
/// others used 30.
pub(crate) const DEFAULT_MAX_DIFF_LINES: usize = 30;

/// How a truncated diff announces itself.
const TRUNCATION_MARKER: &str = "... (diff truncated)";

/// One rendered change, before the common indent is known.
struct DiffRow {
    line_number: usize,
    marker: char,
    content: String,
}

/// Visual width of a line's leading whitespace, with tabs expanded.
fn indent_width(content: &str) -> usize {
    let mut width = 0usize;
    for ch in content.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += TAB_WIDTH - (width % TAB_WIDTH),
            _ => break,
        }
    }
    width
}

/// Re-indent a line to `width` columns fewer than it had, normalizing leading
/// tabs to spaces so indentation is predictable regardless of the source file.
fn strip_indent(content: &str, width: usize) -> String {
    let original = indent_width(content);
    let rest = content.trim_start_matches([' ', '\t']);
    let remaining = original.saturating_sub(width);
    let mut out = " ".repeat(remaining);
    out.push_str(rest);
    out
}

/// Render a line-numbered diff of `old` into `new`.
///
/// Numbering starts at `start_line`, which is the 1-based line the compared
/// region begins at in the file; pass 1 when comparing whole files.
///
/// Only changed lines are shown. Blank-only changes are skipped, matching what
/// every caller did before. Indentation common to all shown lines is removed;
/// anything deeper is kept.
pub(crate) fn render_diff(old: &str, new: &str, start_line: usize, max_lines: usize) -> String {
    let diff = TextDiff::from_lines(old, new);

    let mut rows: Vec<DiffRow> = Vec::new();
    let mut old_line = start_line;
    let mut new_line = start_line;
    let mut truncated = false;

    for change in diff.iter_all_changes() {
        // Only the trailing newline goes: leading whitespace is the file's
        // indentation and is the whole point of this module.
        let content = change.value().trim_end_matches(['\n', '\r']);
        let (marker, line_number) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                ('-', num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                ('+', num)
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };
        if content.trim().is_empty() {
            continue;
        }
        if rows.len() >= max_lines {
            truncated = true;
            break;
        }
        rows.push(DiffRow {
            line_number,
            marker,
            content: content.to_string(),
        });
    }

    if rows.is_empty() {
        return String::new();
    }

    // Common-prefix dedent, computed over the whole set: a per-line decision
    // cannot know the minimum, which is why the original per-line trim could
    // only ever remove everything.
    let common = rows
        .iter()
        .map(|row| indent_width(&row.content))
        .min()
        .unwrap_or(0);

    // Right-align the numbers so content does not shift a column when a hunk
    // crosses 9->10 or 99->100.
    let widest = rows
        .iter()
        .map(|row| row.line_number.to_string().len())
        .max()
        .unwrap_or(1);

    let mut output = String::new();
    for row in &rows {
        let content = if common == 0 {
            row.content.clone()
        } else {
            strip_indent(&row.content, common)
        };
        output.push_str(&format!(
            "{:>widest$}{} {}\n",
            row.line_number, row.marker, content
        ));
    }
    if truncated {
        output.push_str(TRUNCATION_MARKER);
        output.push('\n');
    }

    output.trim_end().to_string()
}

#[cfg(test)]
#[path = "tool_diff_tests.rs"]
mod tool_diff_tests;
