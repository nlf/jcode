//! Format primitives: the content tag, its normalization, and line rendering.
//!
//! The tag is the single load-bearing compatibility point with omp. Everything
//! else in the format is ours to shape; the tag has to agree byte for byte, or
//! a patch authored against one implementation is silently rejected by the
//! other.

use xxhash_rust::xxh32::xxh32;

/// Number of hex characters in a content tag.
pub const FILE_HASH_LENGTH: usize = 4;

/// Separator between a path and its tag in a section header.
const FILE_HASH_SEP: char = '#';
/// Separator between a line number and its content in numbered output.
const LINE_BODY_SEP: char = ':';

/// Trim trailing spaces, tabs and carriage returns from every line.
///
/// This is what makes a tag survive CRLF line endings and display-trimmed
/// output: a file read back through a renderer that stripped trailing
/// whitespace still hashes to the tag the model was given. Leading whitespace
/// is content and is never touched.
fn normalize_for_hash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t', '\r']));
    }
    out
}

/// Compute the content tag for a whole file: the low 16 bits of XXH32 with
/// seed 0 over the normalized text, as four uppercase hex digits.
///
/// Sixteen bits collide by design. The tag is a fast index, never an identity:
/// a snapshot store must key on full text as well, or two colliding states fuse
/// and one's provenance is attributed to the other (omp issue #4075).
pub fn compute_file_hash(text: &str) -> String {
    let normalized = normalize_for_hash(text);
    let low16 = xxh32(normalized.as_bytes(), 0) & 0xffff;
    format!("{low16:0FILE_HASH_LENGTH$X}", FILE_HASH_LENGTH = FILE_HASH_LENGTH)
}

/// Render a section header: `[path#TAG]`.
pub fn format_hashline_header(path: &str, tag: &str) -> String {
    format!("[{path}{FILE_HASH_SEP}{tag}]")
}

/// Render one numbered line: `12:content`.
pub fn format_numbered_line(line_number: usize, line: &str) -> String {
    format!("{line_number}{LINE_BODY_SEP}{line}")
}

/// Render text as numbered lines starting at `start_line`.
pub fn format_numbered_lines(text: &str, start_line: usize) -> String {
    text.split('\n')
        .enumerate()
        .map(|(i, line)| format_numbered_line(start_line + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod format_tests;
