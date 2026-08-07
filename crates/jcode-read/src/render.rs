//! Rendering a read window for the model.
//!
//! Ported from oh-my-pi's `src/tools/read.ts`, behaviour-first.
//!
//! Line numbering has two shapes, and which one is used matters: `N:content` is
//! hashline's editable form, so a read can be patched without a second read;
//! `N|content` is display-only. Numbers are never padded, because padding
//! shifts the content a hashline anchor refers to.

use crate::window::Window;

/// Marks lines omitted between two windows.
pub const ELISION: &str = "…";

/// Openers whose matching closer can absorb an elided body.
const BRACE_PAIRS: [(char, char); 3] = [('{', '}'), ('(', ')'), ('[', ']')];

/// How lines are labelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    /// `N:content`. The form `edit` accepts, so a read can be patched directly.
    Hashline,
    /// `N|content`. Display only.
    Plain,
    /// No numbers at all.
    None,
}

/// Render one line.
pub fn format_line(number: usize, text: &str, numbering: Numbering) -> String {
    match numbering {
        Numbering::Hashline => format!("{number}:{text}"),
        Numbering::Plain => format!("{number}|{text}"),
        Numbering::None => text.to_string(),
    }
}

/// Whether an elided range between these two lines collapses to one line.
///
/// True when the head opens a brace and the tail is its matching closer,
/// optionally followed by terminating punctuation (`};`, `})`, `]);`). The
/// point is that `fn foo() {` … `}` tells the reader everything the elision
/// would have, in one line instead of three.
pub fn can_merge_brace_pair(head: &str, tail: &str) -> bool {
    let head = head.trim_end();
    let tail = tail.trim();
    let Some(opener) = head.chars().last() else {
        return false;
    };
    let Some((_, closer)) = BRACE_PAIRS.iter().find(|(open, _)| *open == opener) else {
        return false;
    };
    let Some(rest) = tail.strip_prefix(*closer) else {
        return false;
    };
    // Only closing punctuation may follow, so `} else {` is not a merge: the
    // body between them is doing something the reader needs to see.
    rest.chars().all(|c| matches!(c, ';' | ',' | ')' | ']' | '}'))
}

/// Render a brace pair whose body was elided, as one line.
pub fn format_merged_brace(
    start: usize,
    end: usize,
    head: &str,
    tail: &str,
    numbering: Numbering,
) -> String {
    let merged = format!("{} {ELISION} {}", head.trim_end(), tail.trim());
    match numbering {
        // The line number is a RANGE, so an anchor built from it covers the
        // whole collapsed region rather than pointing at the head alone.
        Numbering::Hashline => format!("{start}-{end}:{merged}"),
        Numbering::Plain => format!("{start}-{end}|{merged}"),
        Numbering::None => merged,
    }
}

/// The anchor a hashline header should carry for `display_path`.
///
/// A relative path collapses to its file name: `edit`'s tag recovery rebinds a
/// bare `[name#tag]` onto the in-tree file it uniquely names, so the short form
/// is enough and costs fewer tokens.
///
/// An absolute path must stay resolvable. Recovery refuses to redirect a write
/// outside the workspace, so a bare basename there would resolve against the
/// working directory, miss, and fail the edit with "File not found".
pub fn header_anchor(display_path: &str) -> String {
    let is_absolute = display_path.starts_with('/') || display_path.starts_with('~');
    if is_absolute {
        display_path.to_string()
    } else {
        display_path
            .rsplit('/')
            .next()
            .unwrap_or(display_path)
            .to_string()
    }
}

/// Render the windows of a file.
///
/// `lines` is the whole file, 0-indexed. Windows are 1-based and inclusive.
pub fn render(lines: &[String], windows: &[Window], numbering: Numbering) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut previous_end: Option<usize> = None;

    for window in windows {
        let mut first_line = window.start;

        if let Some(gap_start) = previous_end {
            let head = lines.get(gap_start.saturating_sub(1));
            let tail = lines.get(window.start.saturating_sub(1));
            if let (Some(head), Some(tail)) = (head, tail)
                && can_merge_brace_pair(head, tail)
            {
                // The previous window's last line opens a brace that this
                // window's first line closes, so the elided body between them
                // adds nothing: `fn foo() { … }` says it all. Replace the
                // already-emitted head with the merged form, and skip the tail
                // since it is now part of that line.
                out.pop();
                out.push(format_merged_brace(
                    gap_start,
                    window.start,
                    head,
                    tail,
                    numbering,
                ));
                first_line = window.start + 1;
            } else {
                out.push(ELISION.to_string());
            }
        }

        for number in first_line..=window.end {
            if let Some(text) = lines.get(number - 1) {
                out.push(format_line(number, text, numbering));
            }
        }
        previous_end = Some(window.end);
    }

    out.join("\n")
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
