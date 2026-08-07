//! Applying parsed hunks to file content.
//!
//! Ported from oh-my-pi's `src/edit/modes/patch.ts`, behaviour-first.
//!
//! Everything here is pure: content in, content out. The tool layer does the
//! I/O, which is what lets the whole module be tested in milliseconds.

use crate::fuzzy::{
    adjust_indentation, find_closest_sequence, find_context_line, find_fuzzy_sequence,
    DEFAULT_FUZZY_THRESHOLD,
};
use crate::hunks::DiffHunk;

/// Why a hunk could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The lines the patch expects are not in the file.
    NotFound {
        expected: Vec<String>,
        /// The nearest thing found, so the caller can see how stale they are.
        closest: Option<(usize, String)>,
    },
    /// The lines appear more than once, so the target is unclear.
    Ambiguous {
        expected: Vec<String>,
        occurrences: usize,
    },
    /// The `@@` context could not be located.
    ContextNotFound { context: String },
    /// A line hint points past the end of the file.
    HintOutOfRange { hint: usize, lines: usize },
    /// The patch applied but changed nothing.
    NoOp,
}

impl ApplyError {
    /// The message the model reads, written so it can act on it.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { expected, closest } => {
                let mut message = format!(
                    "Failed to find the expected lines:\n{}",
                    expected.join("\n")
                );
                if let Some((line, text)) = closest {
                    message.push_str(&format!(
                        "\n\nClosest match is at line {line}:\n{text}\n\n\
                         The file has changed since this patch was written. \
                         Re-read it and rewrite the patch against what is there now."
                    ));
                }
                message
            }
            Self::Ambiguous {
                expected,
                occurrences,
            } => format!(
                "The expected lines appear {occurrences} times, so the target is \
                 ambiguous:\n{}\n\nAdd surrounding context lines, or an @@ header \
                 naming the enclosing function, to say which one you mean.",
                expected.join("\n")
            ),
            Self::ContextNotFound { context } => format!(
                "Could not find the @@ context '{context}' in the file. \
                 Re-read the file and use a line that is actually in it."
            ),
            Self::HintOutOfRange { hint, lines } => format!(
                "Line hint {hint} is past the end of the file, which has {lines} lines."
            ),
            Self::NoOp => {
                "The patch applied cleanly but changed nothing. Check that the \
                 replacement text differs from what is already there."
                    .to_string()
            }
        }
    }
}

/// Locate `needle` in `lines`, refusing when it appears more than once.
///
/// Ambiguity is an error rather than a first-match: omp's adversarial tests pin
/// this, and the reason is that picking one silently edits code the caller
/// never looked at. A patch that cannot say which occurrence it means is a
/// patch that has not said enough.
fn locate_unique(
    lines: &[String],
    needle: &[String],
    from: usize,
) -> Result<usize, ApplyError> {
    // Exact matches decide on their own: if the caller's text is in the file
    // verbatim, an approximate match elsewhere is not a competing candidate.
    let exact: Vec<usize> = (from..=lines.len().saturating_sub(needle.len()))
        .filter(|start| lines[*start..*start + needle.len()] == *needle)
        .collect();
    match exact.len() {
        1 => return Ok(exact[0]),
        n if n > 1 => {
            return Err(ApplyError::Ambiguous {
                expected: needle.to_vec(),
                occurrences: n,
            });
        }
        _ => {}
    }

    let Some(first) = find_fuzzy_sequence(lines, needle, from, DEFAULT_FUZZY_THRESHOLD) else {
        return Err(ApplyError::NotFound {
            expected: needle.to_vec(),
            closest: find_closest_sequence(lines, needle)
                .map(|found| (found.start + 1, lines[found.start].clone())),
        });
    };

    // A second fuzzy match means the same ambiguity, just approximate.
    if find_fuzzy_sequence(lines, needle, first.start + 1, DEFAULT_FUZZY_THRESHOLD).is_some() {
        return Err(ApplyError::Ambiguous {
            expected: needle.to_vec(),
            occurrences: 2,
        });
    }

    Ok(first.start)
}

/// Where a hunk should be applied.
fn locate_hunk(lines: &[String], hunk: &DiffHunk) -> Result<usize, ApplyError> {
    // A line hint narrows the search but never decides it: models miscount
    // lines constantly, and trusting the number over the content is how a patch
    // lands in the wrong place.
    let mut from = 0usize;

    if let Some(context) = &hunk.change_context {
        // Several stacked contexts nest: each is searched after the last.
        for part in context.split('\n') {
            let Some(found) = find_context_line(lines, part, from) else {
                return Err(ApplyError::ContextNotFound {
                    context: part.to_string(),
                });
            };
            from = found;
        }
    }

    // A pure insertion with only a line number has nothing to match against, so
    // the hint is all there is and must be in range.
    if let Some(hint) = hunk.old_start_line
        && hunk.change_context.is_none()
        && !hunk.has_context_lines
        && hunk.old_lines.is_empty()
    {
        if hint > lines.len() + 1 {
            return Err(ApplyError::HintOutOfRange {
                hint,
                lines: lines.len(),
            });
        }
        return Ok(hint - 1);
    }

    if hunk.old_lines.is_empty() {
        // An insertion anchored by context goes just after it.
        return Ok(if hunk.change_context.is_some() {
            from + 1
        } else {
            from
        });
    }

    locate_unique(lines, &hunk.old_lines, from)
}

/// Apply one hunk to `lines`, returning the new lines.
pub fn apply_hunk(lines: &[String], hunk: &DiffHunk) -> Result<Vec<String>, ApplyError> {
    let at = locate_hunk(lines, hunk)?;

    let mut out = lines[..at.min(lines.len())].to_vec();

    if hunk.old_lines.is_empty() {
        out.extend(hunk.new_lines.iter().cloned());
        out.extend_from_slice(&lines[at.min(lines.len())..]);
        return Ok(out);
    }

    let end = (at + hunk.old_lines.len()).min(lines.len());
    // Re-indent the replacement to sit where the matched block was, so a patch
    // written against a differently indented copy does not leave misaligned
    // code behind.
    let matched = &lines[at..end];
    out.extend(adjust_indentation(matched, &hunk.new_lines));
    out.extend_from_slice(&lines[end..]);

    Ok(out)
}

/// Apply every hunk to `content`, in order.
///
/// Hunks are located independently rather than sequentially, so a patch whose
/// hunks are out of order still applies. omp pins this as "applies hunks
/// regardless of order".
pub fn apply_hunks(content: &str, hunks: &[DiffHunk]) -> Result<String, ApplyError> {
    let (shape, normalized) = crate::shape::TextShape::capture(content);
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    // `split` on a trailing newline leaves an empty final element that is not a
    // line of the file. Dropping it keeps line numbers honest, and the shape
    // restores the newline afterwards.
    if shape.trailing_newline {
        lines.pop();
    }

    for hunk in hunks {
        lines = apply_hunk(&lines, hunk)?;
    }

    let mut joined = lines.join("\n");
    if shape.trailing_newline {
        joined.push('\n');
    }

    let result = shape.restore(&joined);
    if result == content {
        return Err(ApplyError::NoOp);
    }
    Ok(result)
}

/// The content a create hunk produces.
///
/// Normalised the same way an edit is, so a file created by a patch has the
/// same shape as one created by any other tool.
pub fn create_content(body: &str) -> String {
    let normalized = crate::shape::normalize_to_lf(body);
    if normalized.is_empty() || normalized.ends_with('\n') {
        normalized
    } else {
        format!("{normalized}\n")
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod apply_tests;
