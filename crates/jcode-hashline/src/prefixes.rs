//! Recovering raw text from payloads authored against `read`/`search` output.
//!
//! Ported from oh-my-pi's `prefixes.ts`. This runs **before** the tokenizer,
//! and their note on why is the whole justification: hashline mode is the
//! common case for echoed file content, so an erroneously echoed prefix
//! otherwise "turns every content line into a (malformed) op".
//!
//! The failure it prevents is concrete. A model reading `12:    let x = 1;`
//! and pasting it back as a body row would write the literal text `12:` into
//! the file. Worse, a line like `5:PUT 3.=3:` could be read as an operation.
//!
//! Two modes, because the right amount of leniency differs by caller:
//!
//! - [`strip_new_line_prefixes`] — opportunistic. Strips when the input clearly
//!   carries hashline or diff prefixes, leaves it alone otherwise.
//! - [`strip_hashline_prefixes`] — strict. Strips only when *every* content
//!   line is hashline-prefixed.
//!
//! The asymmetry is deliberate: guessing wrong in the lenient direction
//! corrupts content, so the strict variant exists for callers that cannot
//! tolerate that.

/// A hashline line-number prefix: `12:`, `>>12:`, `+ 12:`, `* 12:`, `- 12:`.
///
/// The `>>`/`>>>` forms appear in search output that marks matched lines, and
/// the `[+*-]` forms in diff-style echoes.
fn hashline_prefix_len(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;

    let skip_spaces = |i: &mut usize| {
        while *i < bytes.len() && (bytes[*i] == b' ' || bytes[*i] == b'\t') {
            *i += 1;
        }
    };

    skip_spaces(&mut i);
    // Optional `>>>` or `>>` marker.
    if line[i..].starts_with(">>>") {
        i += 3;
    } else if line[i..].starts_with(">>") {
        i += 2;
    }
    skip_spaces(&mut i);
    // Optional single `+`, `*` or `-` sigil.
    if i < bytes.len() && matches!(bytes[i], b'+' | b'*' | b'-') {
        i += 1;
        skip_spaces(&mut i);
    }
    // Then one or more digits, then a colon.
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start || i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    Some(i + 1)
}

fn has_hashline_prefix(line: &str) -> bool {
    hashline_prefix_len(line).is_some()
}

/// A `+`-sigil hashline prefix specifically: `+12:`, `>> + 12:`.
fn has_plus_hashline_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if line[i..].starts_with(">>>") {
        i += 3;
    } else if line[i..].starts_with(">>") {
        i += 2;
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'+' {
        return false;
    }
    has_hashline_prefix(line)
}

/// A diff-style leading `+`, but not `++` (which is an escaped literal `+`).
fn has_diff_plus(line: &str) -> bool {
    line.starts_with('+') && !line.starts_with("++")
}

/// A section header line: `[path#1A2B]`.
fn is_section_header(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let Some((path, tag)) = inner.rsplit_once('#') else {
        return false;
    };
    !path.is_empty()
        && !path.contains(['\r', '\n'])
        && tag.len() == crate::format::FILE_HASH_LENGTH
        && tag.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether a row is display-only metadata `read` emits, never source.
///
/// Elision markers matter especially: a `…` row stands for content the model
/// never saw, so treating it as a body row would write the marker into the file
/// and silently delete whatever it elided.
pub fn is_read_metadata_line(line: &str) -> bool {
    let trimmed = line.trim();

    // `[…120ln elided; re-read needed ranges with foo.rs:5-16]`
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = trimmed.trim_start_matches('[').trim_end_matches(']');
        if inner.contains("ln elided;") && inner.contains("re-read needed ranges with") {
            return true;
        }
        // `[Showing lines 1-50 of 900 ... Use :50-]`
        if inner.starts_with("Showing lines ") && inner.contains("Use :") {
            return true;
        }
        // `[850 more lines in file ... Use :51-]`
        if inner.contains("more line") && inner.contains("Use :") {
            return true;
        }
    }

    // A single ellipsis row.
    if trimmed == "…" || trimmed == "..." {
        return true;
    }

    // A collapsed range summary: `12-40:  … body elided …`
    if let Some((range, rest)) = trimmed.split_once(':')
        && (rest.contains('…') || rest.contains("..."))
        && let Some((start, end)) = range.split_once('-')
    {
        let start = start.trim();
        let end = end.trim();
        if !start.is_empty()
            && !end.is_empty()
            && start.chars().all(|c| c.is_ascii_digit())
            && end.chars().all(|c| c.is_ascii_digit())
            && !start.starts_with('0')
            && !end.starts_with('0')
        {
            return true;
        }
    }

    false
}

/// Strip every stacked hashline prefix from one line.
///
/// Repeated because a line can carry more than one layer when output has been
/// echoed through more than one tool.
fn strip_leading_hashline_prefixes(line: &str) -> String {
    let mut result = line;
    while let Some(len) = hashline_prefix_len(result) {
        result = &result[len..];
    }
    result.to_string()
}

/// Strip at most one leading prefix, without looping.
///
/// For input carrying at most one snapshot prefix, such as a bare body row
/// pasted from `read`. Recursive stripping would corrupt content whose own
/// text begins with `digits:` — a Python dict literal, a timestamp, a YAML key.
pub fn strip_one_leading_hashline_prefix(line: &str) -> String {
    match hashline_prefix_len(line) {
        Some(len) => line[len..].to_string(),
        None => line.to_string(),
    }
}

#[derive(Debug, Default)]
struct LinePrefixStats {
    non_empty: usize,
    header_count: usize,
    hash_prefix_count: usize,
    diff_plus_hash_prefix_count: usize,
    diff_plus_count: usize,
}

fn collect_line_prefix_stats(lines: &[String]) -> LinePrefixStats {
    let mut stats = LinePrefixStats::default();
    for line in lines {
        if line.is_empty() || is_read_metadata_line(line) {
            continue;
        }
        stats.non_empty += 1;
        if is_section_header(line) {
            stats.header_count += 1;
            continue;
        }
        if has_hashline_prefix(line) {
            stats.hash_prefix_count += 1;
        }
        if has_plus_hashline_prefix(line) {
            stats.diff_plus_hash_prefix_count += 1;
        }
        if has_diff_plus(line) {
            stats.diff_plus_count += 1;
        }
    }
    stats
}

/// Strip whichever prefix scheme the lines appear to carry, or leave them
/// untouched when none is recognized.
///
/// Hashline prefixes require **every** content line to have one, because a
/// partial match is more likely to be real content than an echo. Diff `+`
/// needs only half, since a `+`-prefixed block commonly interleaves with blank
/// lines.
pub fn strip_new_line_prefixes(lines: &[String]) -> Vec<String> {
    let stats = collect_line_prefix_stats(lines);
    if stats.non_empty == 0 {
        return lines.to_vec();
    }

    let content_line_count = stats.non_empty - stats.header_count;
    let strip_hash = content_line_count > 0 && stats.hash_prefix_count == content_line_count;
    let strip_plus = !strip_hash
        && stats.diff_plus_hash_prefix_count == 0
        && stats.diff_plus_count > 0
        && stats.diff_plus_count * 2 >= stats.non_empty;

    if !strip_hash && !strip_plus && stats.diff_plus_hash_prefix_count == 0 {
        return lines.to_vec();
    }

    lines
        .iter()
        .filter(|line| !is_read_metadata_line(line) && !(strip_hash && is_section_header(line)))
        .map(|line| {
            if strip_hash {
                strip_leading_hashline_prefixes(line)
            } else if strip_plus {
                line.strip_prefix('+').unwrap_or(line).to_string()
            } else if stats.diff_plus_hash_prefix_count > 0 && has_plus_hashline_prefix(line) {
                strip_one_leading_hashline_prefix(line)
            } else {
                line.clone()
            }
        })
        .collect()
}

/// Strict variant: strip only when every content line is hashline-prefixed.
pub fn strip_hashline_prefixes(lines: &[String]) -> Vec<String> {
    let stats = collect_line_prefix_stats(lines);
    if stats.non_empty == 0 {
        return lines.to_vec();
    }
    let content_line_count = stats.non_empty - stats.header_count;
    if content_line_count == 0 || stats.hash_prefix_count != content_line_count {
        return lines.to_vec();
    }
    lines
        .iter()
        .filter(|line| !is_read_metadata_line(line) && !is_section_header(line))
        .map(|line| strip_leading_hashline_prefixes(line))
        .collect()
}

/// Normalize a payload into prefix-stripped lines.
///
/// A trailing newline is dropped rather than yielding a phantom empty row, and
/// carriage returns are removed so CRLF input behaves like LF.
pub fn parse_payload_text(text: &str) -> Vec<String> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<String> = trimmed.replace('\r', "").split('\n').map(str::to_string).collect();
    strip_new_line_prefixes(&lines)
}

#[cfg(test)]
#[path = "prefixes_tests.rs"]
mod prefixes_tests;
