//! Text shape: line endings and byte-order marks.
//!
//! Ported from oh-my-pi's `packages/hashline/src/normalize.ts`.
//!
//! A patch is written and matched in LF, but the file on disk may be CRLF and
//! may start with a BOM. Both have to survive the round trip: rewriting a CRLF
//! file as LF turns one edit into a whole-file diff, and dropping a BOM can
//! change how another tool reads the file.

/// The line ending a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// The first line ending style in `content`, defaulting to LF.
///
/// Decided by which comes first rather than by counting: a file whose first
/// line ends CRLF is a CRLF file, even if it has stray LF lines later, and
/// rewriting it wholesale to the majority style would touch every line.
pub fn detect_line_ending(content: &str) -> LineEnding {
    match content.find('\n') {
        None => LineEnding::Lf,
        Some(lf) => match content.find("\r\n") {
            Some(crlf) if crlf < lf => LineEnding::Crlf,
            _ => LineEnding::Lf,
        },
    }
}

/// Convert every line ending to LF.
///
/// Handles a lone CR as well as CRLF, since old Mac-style files and some
/// generated content still use it.
pub fn normalize_to_lf(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            // CRLF collapses to LF; a lone CR becomes one too.
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Re-encode LF text with the requested ending.
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_string(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

/// A UTF-8 BOM, if the text started with one.
pub const BOM: char = '\u{feff}';

/// Split a leading BOM off the text.
///
/// Returned separately rather than stripped, because it has to be put back:
/// some Windows tooling treats its absence as a different file encoding.
pub fn strip_bom(content: &str) -> (bool, &str) {
    match content.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, content),
    }
}

/// Whether the text ends with a newline.
///
/// Tracked because a patch that adds or removes the final newline is a real
/// change, and one that does neither must not introduce one: a file with no
/// trailing newline is a deliberate state in some formats.
pub fn has_trailing_newline(text: &str) -> bool {
    text.ends_with('\n')
}

/// The shape of a file, so it can be restored after editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextShape {
    pub line_ending: LineEnding,
    pub bom: bool,
    pub trailing_newline: bool,
}

impl TextShape {
    /// Record the shape of `content` and return it normalized to LF with no
    /// BOM, which is the form patches are matched against.
    pub fn capture(content: &str) -> (Self, String) {
        let (bom, without_bom) = strip_bom(content);
        let shape = Self {
            line_ending: detect_line_ending(without_bom),
            bom,
            trailing_newline: has_trailing_newline(without_bom),
        };
        (shape, normalize_to_lf(without_bom))
    }

    /// Put `content` back into this shape.
    pub fn restore(&self, content: &str) -> String {
        let mut text = content.to_string();

        // Only the file's original trailing-newline state is restored. A patch
        // that deliberately changed it has already produced text with the new
        // state, and forcing the old one back would undo the edit.
        if self.trailing_newline && !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        } else if !self.trailing_newline {
            while text.ends_with('\n') {
                text.pop();
            }
        }

        let mut text = restore_line_endings(&text, self.line_ending);
        if self.bom {
            text.insert(0, BOM);
        }
        text
    }
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod shape_tests;
