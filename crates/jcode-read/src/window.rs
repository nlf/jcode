//! Selecting which lines of a file to show.
//!
//! Ported from oh-my-pi's `src/tools/read.ts`, behaviour-first.
//!
//! Reading is not "return the file". It is choosing a window, and the choices
//! that matter are: how much is too much, what happens at the edges of an
//! explicit range, and how a truncated result explains itself well enough that
//! the caller can ask for the rest.

use jcode_search::LineRange;

/// Lines returned when the caller names no range.
pub const DEFAULT_MAX_LINES: usize = 3000;

/// Bytes returned before truncation, whatever the line count.
///
/// A file of 200 very long lines is as expensive as one of 3000 short ones, so
/// both caps apply and whichever binds first wins.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// Context lines added *above* an explicitly requested range.
///
/// omp's value, and their comment explains the asymmetry with data: anchor
/// failures cluster on edits whose anchors land just outside the last read
/// window, and one line above catches the common accidental single-line read
/// where the anchor is immediately overhead.
pub const RANGE_LEADING_CONTEXT_LINES: usize = 1;

/// Context lines added *below* an explicitly requested range.
///
/// Three rather than one: their telemetry showed follow-up reads are mostly
/// disjoint hops rather than adjacent extensions, so symmetric padding rarely
/// pays for itself, but a narrow range often needs the next few lines to
/// disambiguate an anchor.
pub const RANGE_TRAILING_CONTEXT_LINES: usize = 3;

/// A window of a file, resolved against its actual length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// 1-based, inclusive.
    pub start: usize,
    /// 1-based, inclusive. Never past the end of the file.
    pub end: usize,
}

impl Window {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start) + 1
    }

    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

/// What the caller asked for, before it meets the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// Ranges from a selector, if any. Empty means "from the top".
    pub ranges: Vec<LineRange>,
    /// Explicit line cap, overriding the default.
    pub limit: Option<usize>,
}

/// Expand an explicitly requested range with context.
///
/// Context is added only on the sides the caller actually constrained. An
/// open-ended read from the top has no "above" to expand into, and padding it
/// would silently shift the line the caller asked to start at.
pub fn expand_with_context(
    start: usize,
    end: usize,
    total_lines: usize,
    expand_start: bool,
    expand_end: bool,
) -> Window {
    let start = if expand_start {
        start.saturating_sub(RANGE_LEADING_CONTEXT_LINES).max(1)
    } else {
        start.max(1)
    };
    let end = if expand_end {
        (end + RANGE_TRAILING_CONTEXT_LINES).min(total_lines)
    } else {
        end.min(total_lines)
    };
    Window { start, end }
}

/// Resolve a request against a file of `total_lines`.
///
/// Returns the windows to show, in order, merged where they overlap after
/// context expansion: two ranges three lines apart become one window rather
/// than two with a misleading elision between them.
pub fn resolve(request: &Request, total_lines: usize) -> Vec<Window> {
    if total_lines == 0 {
        return Vec::new();
    }

    if request.ranges.is_empty() {
        // No range named: from the top, capped. No leading context, because
        // there is nothing above line 1 and the caller constrained nothing.
        let limit = request.limit.unwrap_or(DEFAULT_MAX_LINES);
        return vec![Window {
            start: 1,
            end: limit.min(total_lines).max(1),
        }];
    }

    let mut windows: Vec<Window> = Vec::new();
    for range in &request.ranges {
        if range.start > total_lines {
            // A range wholly past the end of the file describes nothing.
            // Skipping it rather than clamping avoids showing the last few
            // lines and implying they are what was asked for.
            continue;
        }
        // An open-ended range (`50-`) already runs to EOF, so trailing context
        // is arithmetically a no-op there: end is total_lines, and +3 clamps
        // straight back. Passing `true` unconditionally keeps the call honest
        // about intent - both sides of an explicit range get context - rather
        // than implying a decision the arithmetic makes for us.
        let end = range.end.unwrap_or(total_lines).min(total_lines);
        windows.push(expand_with_context(range.start, end, total_lines, true, true));
    }

    merge_windows(windows)
}

/// Merge overlapping or adjacent windows.
///
/// Adjacent counts as overlapping: two windows with no gap between them are one
/// span, and rendering an elision marker between consecutive lines would claim
/// content was omitted when none was.
fn merge_windows(mut windows: Vec<Window>) -> Vec<Window> {
    if windows.len() < 2 {
        return windows;
    }
    windows.sort_by_key(|window| window.start);

    let mut merged: Vec<Window> = vec![windows[0].clone()];
    for window in windows.into_iter().skip(1) {
        let last = merged.last_mut().expect("seeded above");
        if window.start <= last.end + 1 {
            last.end = last.end.max(window.end);
        } else {
            merged.push(window);
        }
    }
    merged
}

/// What was shown, and what was not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub windows: Vec<Window>,
    pub total_lines: usize,
    /// Lines actually displayed, for the seen-line record hashline keeps.
    pub shown_lines: Vec<usize>,
    /// True when a cap cut the output short.
    pub truncated: bool,
}

impl Outcome {
    /// The continuation hint, or empty when everything was shown.
    ///
    /// Takes the path because the hint is only useful if it is a call the model
    /// can copy: `Read src/lib.rs:120-` rather than a bare line number it has
    /// to assemble. An unconditional hint would teach it to paginate reads that
    /// are already complete, so an untruncated outcome returns nothing.
    pub fn continuation(&self, path: &str) -> String {
        if !self.truncated {
            return String::new();
        }
        let last = self.windows.last().map(|window| window.end).unwrap_or(0);
        let remaining = self.total_lines.saturating_sub(last);
        format!("... {remaining} more lines. Read {path}:{}- to continue.", last + 1)
    }
}

/// Build the outcome for a set of windows.
pub fn outcome(windows: Vec<Window>, total_lines: usize) -> Outcome {
    let shown_lines: Vec<usize> = windows
        .iter()
        .flat_map(|window| window.start..=window.end)
        .collect();
    let last = windows.last().map(|window| window.end).unwrap_or(0);
    Outcome {
        truncated: last < total_lines,
        windows,
        total_lines,
        shown_lines,
    }
}

/// Merging, exposed for tests that need to construct windows directly.
///
/// Real selectors rarely pad into exactly-adjacent windows, so the adjacency
/// rule is otherwise only reachable through contrived ranges.
#[cfg(test)]
fn merge_for_test(windows: Vec<Window>) -> Vec<Window> {
    merge_windows(windows)
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod window_tests;
