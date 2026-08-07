//! Parsing the Codex `apply_patch` envelope.
//!
//! Ported from oh-my-pi's `src/edit/apply-patch/parser.ts`, behaviour-first.
//!
//! ```text
//! *** Begin Patch
//! *** Add File: <path>
//! +<line>
//! *** Delete File: <path>
//! *** Update File: <path>
//! *** Move to: <newpath>
//! @@ <optional context>
//! -old
//! +new
//!  context
//! *** End of File
//! *** End Patch
//! ```

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";

/// What a hunk does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Create,
    Delete,
    Update,
}

/// One file's worth of a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub path: String,
    pub op: Operation,
    /// Destination when the hunk also moves the file.
    pub rename: Option<String>,
    /// Body: whole contents for a create, a unified diff for an update.
    pub diff: String,
}

/// Why an envelope could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// 1-based, counting `*** Begin Patch` as line 1. `None` when the problem
    /// is the envelope itself rather than a line inside it.
    pub line: Option<usize>,
}

impl ParseError {
    pub fn message(&self) -> String {
        match self.line {
            Some(line) => format!("{} (line {line})", self.message),
            None => self.message.clone(),
        }
    }
}

/// Parse an envelope into per-file hunks.
pub fn parse(patch_text: &str) -> Result<Vec<Hunk>, ParseError> {
    parse_with_options(patch_text, false)
}

/// Best-effort parse for rendering a patch that is still streaming in.
///
/// Tolerates missing envelope markers and an incomplete trailing hunk. Never
/// use it to apply edits: it will happily return half of what was intended.
pub fn parse_streaming(patch_text: &str) -> Vec<Hunk> {
    parse_with_options(patch_text, true).unwrap_or_default()
}

fn parse_with_options(patch_text: &str, streaming: bool) -> Result<Vec<Hunk>, ParseError> {
    let mut lines: Vec<&str> = patch_text.trim().split('\n').collect();

    // A heredoc wrapper is stripped, because models emit the shell form they
    // would have typed. Rejecting it teaches nothing the caller can act on.
    if lines.len() >= 2 {
        let first = lines[0];
        let last = lines[lines.len() - 1].trim();
        let openers = ["<<EOF", "<<'EOF'", "<<\"EOF\""];
        if openers.contains(&first) && last == "EOF" {
            lines = lines[1..lines.len() - 1].to_vec();
        }
    }

    if lines.is_empty() || lines[0].trim() != BEGIN_PATCH {
        if streaming {
            return Ok(Vec::new());
        }
        return Err(ParseError {
            message: format!("The first line of the patch must be '{BEGIN_PATCH}'"),
            line: None,
        });
    }

    let has_end = lines[lines.len() - 1].trim() == END_PATCH;
    if !has_end && !streaming {
        return Err(ParseError {
            message: format!("The last line of the patch must be '{END_PATCH}'"),
            line: None,
        });
    }

    let body: Vec<&str> = if has_end {
        lines[1..lines.len() - 1].to_vec()
    } else {
        lines[1..].to_vec()
    };

    let mut hunks = Vec::new();
    let mut index = 0usize;
    // 1-based, and `*** Begin Patch` was line 1.
    let mut line_number = 2usize;

    while index < body.len() {
        // Blank separators between hunks are ignored.
        if body[index].trim().is_empty() {
            index += 1;
            line_number += 1;
            continue;
        }

        let first = body[index].trim();

        if let Some(path) = first.strip_prefix(ADD_FILE) {
            let mut contents = String::new();
            let mut consumed = 1;
            for line in &body[index + 1..] {
                match line.strip_prefix('+') {
                    Some(rest) => {
                        contents.push_str(rest);
                        contents.push('\n');
                        consumed += 1;
                    }
                    None => break,
                }
            }
            hunks.push(Hunk {
                path: path.to_string(),
                op: Operation::Create,
                rename: None,
                diff: contents,
            });
            index += consumed;
            line_number += consumed;
            continue;
        }

        if let Some(path) = first.strip_prefix(DELETE_FILE) {
            hunks.push(Hunk {
                path: path.to_string(),
                op: Operation::Delete,
                rename: None,
                diff: String::new(),
            });
            index += 1;
            line_number += 1;
            continue;
        }

        if let Some(path) = first.strip_prefix(UPDATE_FILE) {
            let path = path.to_string();
            index += 1;
            line_number += 1;

            let mut rename = None;
            if let Some(line) = body.get(index)
                && let Some(destination) = line.strip_prefix(MOVE_TO)
            {
                rename = Some(destination.to_string());
                index += 1;
                line_number += 1;
            }

            // The body runs to the next file marker. `*** End of File` stays
            // inside it: it terminates a chunk, not the file's hunk, and the
            // diff parser handles it.
            let mut diff_lines = Vec::new();
            while let Some(line) = body.get(index) {
                if line.starts_with("*** Add File:")
                    || line.starts_with("*** Delete File:")
                    || line.starts_with("*** Update File:")
                {
                    break;
                }
                diff_lines.push(*line);
                index += 1;
                line_number += 1;
            }

            if diff_lines.is_empty() {
                if streaming {
                    hunks.push(Hunk {
                        path,
                        op: Operation::Update,
                        rename,
                        diff: String::new(),
                    });
                    continue;
                }
                return Err(ParseError {
                    message: format!("Update file hunk for path '{path}' is empty"),
                    line: Some(line_number),
                });
            }

            hunks.push(Hunk {
                path,
                op: Operation::Update,
                rename,
                diff: diff_lines.join("\n"),
            });
            continue;
        }

        if streaming {
            break;
        }
        return Err(ParseError {
            message: format!(
                "'{first}' is not a valid hunk header. Valid hunk headers: \
                 '*** Add File: {{path}}', '*** Delete File: {{path}}', \
                 '*** Update File: {{path}}'"
            ),
            line: Some(line_number),
        });
    }

    Ok(hunks)
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod envelope_tests;
