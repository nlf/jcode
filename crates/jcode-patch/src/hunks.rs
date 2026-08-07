//! Parsing the diff body inside an update hunk.
//!
//! Ported from oh-my-pi's `src/edit/diff.ts` (`parseDiffHunks`),
//! behaviour-first.
//!
//! The body is unified-diff-shaped but looser than a real unified diff, because
//! it is written by a model rather than produced by `git diff`. The `@@` header
//! may carry a context string, line numbers, both, or nothing at all, and a
//! bare body with no header is accepted.

use crate::envelope::ParseError;

/// A line-elision marker. Models paste both spellings.
const ELISION: [&str; 2] = ["...", "…"];
/// Terminates a chunk at the end of the file.
const EOF_MARKER: &str = "*** End of File";

/// One change within a file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffHunk {
    /// Text from the `@@` header locating where to apply this.
    pub change_context: Option<String>,
    /// 1-based line hint from the header, when it carried one.
    pub old_start_line: Option<usize>,
    /// Lines the patch expects to find, in order.
    pub old_lines: Vec<String>,
    /// Lines to put in their place.
    pub new_lines: Vec<String>,
    /// Whether any context (unchanged) lines were given.
    ///
    /// A hunk with no context is a blind replacement, and callers treat it more
    /// carefully than one anchored by surrounding lines.
    pub has_context_lines: bool,
    /// Whether the chunk ran to the end of the file.
    pub is_end_of_file: bool,
}

/// Whether a line carries diff content rather than metadata.
///
/// `--- ` and `+++ ` are excluded despite starting with a diff marker: they are
/// the file headers `git diff` emits, and treating them as content makes the
/// patch look for a line reading `-- a/x.rs` in the file. Matching omp's
/// `isDiffContentLine` (`diff.ts:438`), which carves out the same two prefixes.
///
/// A removal of a line that genuinely begins with `-- ` is unreachable through
/// this path, which is the tradeoff omp took and is worth the metadata being
/// handled correctly.
fn is_diff_content(line: &str) -> bool {
    match line.chars().next() {
        Some(' ') => true,
        Some('+') => !line.starts_with("+++ "),
        Some('-') => !line.starts_with("--- "),
        _ => false,
    }
}

/// Metadata a model copies in from a real unified diff, which is not content.
fn is_unified_metadata(trimmed: &str) -> bool {
    trimmed.starts_with("diff --git ")
        || trimmed.starts_with("index ")
        || trimmed.starts_with("--- ")
        || trimmed.starts_with("+++ ")
        || trimmed == "---"
        || trimmed == "+++"
        || trimmed.starts_with("new file mode ")
        || trimmed.starts_with("deleted file mode ")
        || trimmed.starts_with("similarity index ")
        || trimmed.starts_with("rename from ")
        || trimmed.starts_with("rename to ")
}

/// Read `@@ -a,b +c,d @@ context` into its line numbers and context.
fn parse_unified_header(trimmed: &str) -> Option<(usize, String)> {
    let rest = trimmed.strip_prefix("@@")?;
    let (ranges, tail) = rest.split_once("@@")?;
    let ranges = ranges.trim();
    let old = ranges.split_whitespace().find(|part| part.starts_with('-'))?;
    let number: String = old
        .trim_start_matches('-')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let start = number.parse().ok()?;
    Some((start, tail.trim().to_string()))
}

/// Whether the header names a bare line number, as `@@ 42`.
fn parse_line_hint(text: &str) -> Option<usize> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    trimmed.parse().ok()
}

/// Parse the body of one update hunk into its changes.
pub fn parse_diff_hunks(diff: &str) -> Result<Vec<DiffHunk>, ParseError> {
    // A single-file body carrying several file markers means the caller pasted
    // a multi-file patch into one hunk. Applying the first and dropping the
    // rest would silently lose changes.
    let file_markers = diff
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("*** Add File:")
                || trimmed.starts_with("*** Delete File:")
                || trimmed.starts_with("*** Update File:")
        })
        .count();
    if file_markers > 1 {
        return Err(ParseError {
            message: format!(
                "Diff contains {file_markers} file markers. Single-file patches \
                 cannot contain multi-file markers."
            ),
            line: None,
        });
    }

    let lines: Vec<&str> = diff.split('\n').collect();
    let mut hunks = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        // Metadata only counts as metadata when it is not diff content: a
        // context line reading `--- separator` is content, not a file header.
        if !is_diff_content(line) && is_unified_metadata(trimmed) {
            index += 1;
            continue;
        }

        // A trailing header with nothing under it is not a hunk.
        if trimmed.starts_with("@@")
            && lines[index + 1..].iter().all(|next| next.trim().is_empty())
        {
            break;
        }

        let (hunk, consumed) = parse_one_hunk(&lines[index..], index + 1)?;
        hunks.push(hunk);
        index += consumed;
    }

    Ok(hunks)
}

fn parse_one_hunk(lines: &[&str], line_number: usize) -> Result<(DiffHunk, usize), ParseError> {
    if lines.is_empty() {
        return Err(ParseError {
            message: "Diff does not contain any lines".to_string(),
            line: Some(line_number),
        });
    }

    let mut contexts: Vec<String> = Vec::new();
    let mut old_start_line = None;
    let mut start = 0usize;

    let header = lines[0].trim_end();
    if let Some(after_marker) = header.strip_prefix("@@") {
        start = 1;
        if let Some((old, context)) = parse_unified_header(header) {
            if old < 1 {
                return Err(ParseError {
                    message: "Line numbers in @@ header must be >= 1".to_string(),
                    line: Some(line_number),
                });
            }
            old_start_line = Some(old);
            if !context.is_empty() {
                contexts.push(context);
            }
        } else {
            let value = after_marker.trim().trim_end_matches('@').trim();
            if let Some(hint) = parse_line_hint(value) {
                if hint < 1 {
                    return Err(ParseError {
                        message: "Line hint must be >= 1".to_string(),
                        line: Some(line_number),
                    });
                }
                old_start_line = Some(hint);
            } else if !value.is_empty() {
                contexts.push(value.to_string());
            }
        }
    }

    // Several stacked `@@` lines each add context, which is how a model names
    // a nested location: an outer function and an inner branch.
    while start < lines.len() && lines[start].starts_with("@@") {
        let nested = lines[start].trim_end();
        let value = nested[2..].trim().trim_end_matches('@').trim();
        if !value.is_empty() {
            contexts.push(value.to_string());
        }
        start += 1;
    }

    if start >= lines.len() {
        return Err(ParseError {
            message: "Hunk does not contain any lines".to_string(),
            line: Some(line_number + 1),
        });
    }

    let mut hunk = DiffHunk {
        change_context: (!contexts.is_empty()).then(|| contexts.join("\n")),
        old_start_line,
        ..DiffHunk::default()
    };

    let mut consumed = start;
    let mut parsed = 0usize;

    for (offset, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim();
        let next = lines.get(offset + 1);

        // A blank line before the next header ends this hunk rather than
        // becoming a trailing empty context line.
        if line.is_empty()
            && parsed > 0
            && next.is_some_and(|next| next.trim_start().starts_with("@@"))
        {
            break;
        }

        if !is_diff_content(line) && line.trim_end() == EOF_MARKER {
            if parsed == 0 {
                return Err(ParseError {
                    message: "Hunk does not contain any lines".to_string(),
                    line: Some(line_number + 1),
                });
            }
            hunk.is_end_of_file = true;
            consumed = offset + 1;
            break;
        }

        // An elision marks omitted context. It is not content, but it does
        // mean the hunk is anchored rather than blind.
        if ELISION.contains(&trimmed) {
            hunk.has_context_lines = true;
            consumed = offset + 1;
            parsed += 1;
            continue;
        }

        match line.chars().next() {
            None => {
                hunk.has_context_lines = true;
                hunk.old_lines.push(String::new());
                hunk.new_lines.push(String::new());
            }
            Some(' ') => {
                hunk.has_context_lines = true;
                hunk.old_lines.push(line[1..].to_string());
                hunk.new_lines.push(line[1..].to_string());
            }
            Some('+') => hunk.new_lines.push(line[1..].to_string()),
            Some('-') => hunk.old_lines.push(line[1..].to_string()),
            Some(_) if line.starts_with("@@") => break,
            Some(_) => {
                // An unprefixed line is context. Models omit the leading space
                // constantly, and refusing would reject most real patches.
                hunk.has_context_lines = true;
                hunk.old_lines.push(line.to_string());
                hunk.new_lines.push(line.to_string());
            }
        }

        consumed = offset + 1;
        parsed += 1;
    }

    Ok((hunk, consumed))
}

#[cfg(test)]
#[path = "hunks_tests.rs"]
mod hunks_tests;
