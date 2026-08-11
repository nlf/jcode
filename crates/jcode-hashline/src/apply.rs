//! Applying parsed operations to text.
//!
//! Ported from the core of oh-my-pi's `apply.ts`, **excluding its repair
//! layer**. That omission is deliberate and large: measured by function
//! boundaries, roughly 1,166 of their 1,425 lines are forgiveness (boundary
//! echo, delimiter balance, dropped closers, indentation, landing shift), and
//! only ~220 are the splice. This is the ~220.
//!
//! # Line numbers name ORIGINAL lines
//!
//! Every anchor refers to the file as the model read it. Applying edits
//! top-to-bottom would shift every later anchor by the size of each earlier
//! edit, so operations are collected and applied in one pass over the original
//! text. This is the property that makes a multi-hunk patch authorable at all:
//! the model reads once and writes every anchor against that one snapshot.
//!
//! # The phantom trailing line
//!
//! Splitting `"a\nb\n"` on `\n` yields `["a", "b", ""]`. That final empty
//! element is not content, it is the trailing newline. It is addressable for
//! insertion (appending past the end), but deleting it only strips the
//! newline, so a `CUT` that lands there is ignored and a range ending there is
//! treated as ending at the last real line. omp pins all three cases; without
//! them a model that counts lines from a read consistently deletes one line too
//! many at the end of a file.

use crate::parser::{Anchor, Op};

/// The result of applying operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    pub text: String,
    /// 1-indexed first changed line, or `None` when nothing changed.
    pub first_changed_line: Option<usize>,
    /// A `MV` destination, when the patch moved the file.
    pub move_dest: Option<String>,
    /// True when the patch deleted the file.
    pub removed: bool,
}

/// Split text into lines, preserving the phantom trailing element.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

/// Index of the phantom trailing line, if the text is newline-terminated.
///
/// Returns `0` (not a valid 1-indexed line) when there is none, so callers can
/// compare without an `Option`.
fn phantom_line(lines: &[&str]) -> usize {
    if lines.len() > 1 && lines[lines.len() - 1].is_empty() {
        lines.len()
    } else {
        0
    }
}

/// Apply `ops` to `text`.
///
/// Anchors index the original text, so ordering among operations does not
/// affect where any of them land.
pub fn apply_ops(text: &str, ops: &[Op]) -> Result<ApplyResult, String> {
    let lines = split_lines(text);
    let phantom = phantom_line(&lines);
    let line_count = lines.len();

    let mut move_dest = None;
    let mut removed = false;

    // Per original line: the replacement rows, and whether the line survives.
    let mut deleted = vec![false; line_count + 1];
    let mut insert_before: Vec<Vec<String>> = vec![Vec::new(); line_count + 2];
    let mut insert_after: Vec<Vec<String>> = vec![Vec::new(); line_count + 2];

    for op in ops {
        match op {
            Op::Rem => removed = true,
            Op::Mv { dest } => move_dest = Some(dest.clone()),
            Op::Cut { start, end } => {
                let (start, end) = clamp_range(*start, *end, line_count, phantom)?;
                if start == 0 {
                    continue;
                }
                // Indexed by 1-based line number: `deleted` is sized
                // line_count + 1 so line N sits at index N. An iterator with
                // skip/take would hide that correspondence.
                #[allow(clippy::needless_range_loop)]
                for line in start..=end {
                    deleted[line] = true;
                }
            }
            Op::Put { anchor, body } => match anchor {
                Anchor::Range { start, end } => {
                    let (start, end) = clamp_range(*start, *end, line_count, phantom)?;
                    if start == 0 {
                        continue;
                    }
                    // See the note in `Op::Cut`: indices are 1-based line
                    // numbers, not offsets.
                    #[allow(clippy::needless_range_loop)]
                    for line in start..=end {
                        deleted[line] = true;
                    }
                    insert_before[start].extend(body.iter().cloned());
                }
                Anchor::Before(line) => {
                    validate_anchor(*line, line_count)?;
                    insert_before[*line].extend(body.iter().cloned());
                }
                Anchor::After(line) => {
                    validate_anchor(*line, line_count)?;
                    insert_after[*line].extend(body.iter().cloned());
                }
                Anchor::Bof => insert_before[1].extend(body.iter().cloned()),
                // Unreachable by contract: `prepare` resolves every block
                // anchor into a range before anything else runs. Reported
                // rather than ignored, because a block that reached the applier
                // means the resolution step was skipped, and silently dropping
                // the op would lose an edit the model believes it made.
                Anchor::Block(line) => {
                    return Err(format!(
                        "Internal error: the block anchor on line {line} was never \
                         resolved to a line range."
                    ));
                }
                Anchor::Eof => {
                    // Append *before* the phantom, so the file keeps exactly
                    // one trailing newline instead of gaining a blank line
                    // before the appended content.
                    if phantom > 0 {
                        insert_before[phantom].extend(body.iter().cloned());
                    } else {
                        insert_after[line_count].extend(body.iter().cloned());
                    }
                }
            },
        }
    }

    if removed {
        return Ok(ApplyResult {
            text: String::new(),
            first_changed_line: Some(1),
            move_dest,
            removed: true,
        });
    }

    // An empty file splits to `[""]`: a single element that looks like a
    // phantom but is the entire file. Content written into it replaces that
    // element rather than landing after it, or every new file starts with a
    // blank line.
    let empty_file = line_count == 1 && lines[0].is_empty();

    let mut out: Vec<String> = Vec::with_capacity(line_count);
    let mut first_changed_line = None;
    let changed_at = |line: usize, first: &mut Option<usize>| {
        if first.is_none() {
            *first = Some(line);
        }
    };

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;

        for row in &insert_before[number] {
            changed_at(out.len() + 1, &mut first_changed_line);
            out.push(row.clone());
        }

        let drop_line = deleted[number] || (empty_file && !out.is_empty());
        if drop_line {
            changed_at(out.len() + 1, &mut first_changed_line);
        } else {
            out.push((*line).to_string());
        }

        for row in &insert_after[number] {
            changed_at(out.len() + 1, &mut first_changed_line);
            out.push(row.clone());
        }
    }

    let text = out.join("\n");
    Ok(ApplyResult {
        text,
        first_changed_line,
        move_dest,
        removed: false,
    })
}

/// Clamp a range against the file, handling the phantom trailing line.
///
/// Returns `(0, 0)` for a range that targets only the phantom, which the
/// caller skips: deleting the trailing newline is not what the model meant.
fn clamp_range(
    start: usize,
    end: usize,
    line_count: usize,
    phantom: usize,
) -> Result<(usize, usize), String> {
    if start > line_count || end > line_count {
        return Err(format!(
            "Line {} does not exist (file has {} lines).",
            start.max(end),
            real_line_count(line_count, phantom)
        ));
    }
    if phantom > 0 && start >= phantom {
        // The range starts at or past the phantom: nothing real to delete.
        return Ok((0, 0));
    }
    // A range ending at the phantom ends at the last real line instead.
    let end = if phantom > 0 && end >= phantom {
        phantom - 1
    } else {
        end
    };
    Ok((start, end))
}

/// Anchors may target the phantom line, because appending past the end is a
/// legitimate insertion point.
fn validate_anchor(line: usize, line_count: usize) -> Result<(), String> {
    if line == 0 || line > line_count {
        return Err(format!(
            "Line {line} does not exist (file has {line_count} lines)."
        ));
    }
    Ok(())
}

fn real_line_count(line_count: usize, phantom: usize) -> usize {
    if phantom > 0 {
        line_count - 1
    } else {
        line_count
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod apply_tests;
