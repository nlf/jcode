//! Splitting an authored patch into per-file sections.
//!
//! Ported from the header-and-path layer of oh-my-pi's `input.ts`. The splitter
//! is purely lexical: it does not know whether a path exists, which is the
//! patcher's job. That separation is what lets this be tested without a
//! filesystem.
//!
//! # Why the leniency
//!
//! omp's recovery paths are not politeness, they are load-bearing. Their
//! comments cite shapes observed in real benchmark traces: models reflexively
//! prepend `Update File:` to a path, or duplicate the header sigil as `***`,
//! because those are apply-patch conventions they were trained on. Rejecting
//! those outright converts a recoverable near-miss into a failed turn, and the
//! whole point of hashline is that near-misses should land.

use crate::format::FILE_HASH_LENGTH;

/// One file's section of a patch: a header plus the raw body beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSection {
    /// Path as authored, normalized but not resolved.
    pub path: String,
    /// The 4-hex content tag, when the header carried one.
    pub file_hash: Option<String>,
    /// Raw body text, unparsed.
    pub body: String,
    /// True when same-path sections that were *not* adjacent were merged.
    ///
    /// Merging moves later ops up to the first occurrence, which silently
    /// reorders any clipboard register sequence, so callers reject clipboard
    /// ops in such sections rather than applying them out of order.
    pub interleaved: bool,
}

/// Strip one layer of matching surrounding quotes.
fn unquote_path(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() < 2 {
        return text;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first == b'"' || first == b'\'') && first == last {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

/// Strip apply-patch-style noise models prepend to a path.
///
/// Observed in omp's traces: `Update File:foo.ts`, `Update:foo.ts`,
/// `UpdateFile:foo.ts`, `Update/File:foo.ts`, `Update-file:foo.ts`,
/// `Update(File):foo.ts`, `Add File:foo.ts`, `Delete File:foo.ts`,
/// `Move to:foo.ts`, `***foo.ts`, `***Update File:foo.ts`.
///
/// The pattern is a leading `***` sigil and/or an
/// `(update|add|delete|move)`-keyword block terminated by a colon.
fn strip_apply_patch_noise(text: &str) -> &str {
    let mut rest = text.trim_start();
    rest = rest.trim_start_matches('*').trim_start();

    const KEYWORDS: [&str; 4] = ["update", "add", "delete", "move"];
    let lowered = rest.to_ascii_lowercase();
    for keyword in KEYWORDS {
        if !lowered.starts_with(keyword) {
            continue;
        }
        // A keyword only counts when a colon terminates its block, otherwise
        // `update_config.rs` would lose its leading word.
        let after_keyword = &rest[keyword.len()..];
        let Some(colon) = after_keyword.find(':') else {
            continue;
        };
        // Between the keyword and the colon only separators and an optional
        // `file`/`to` noise word may appear.
        let between = after_keyword[..colon].to_ascii_lowercase();
        let cleaned: String = between
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if cleaned.is_empty() || cleaned == "file" || cleaned == "to" {
            rest = after_keyword[colon + 1..].trim_start();
            rest = rest.trim_start_matches('*').trim_start();
            break;
        }
    }
    rest
}

/// Normalize an authored path: unquote, strip noise, and make an absolute path
/// inside `cwd` relative to it.
///
/// Paths outside `cwd` stay absolute, because rewriting them relative would
/// produce a `../..` chain that reads as an escape attempt rather than a
/// location.
pub fn normalize_path(raw: &str, cwd: Option<&str>) -> String {
    let unquoted = unquote_path(raw.trim());
    let cleaned = strip_apply_patch_noise(unquoted);

    let Some(cwd) = cwd else {
        return cleaned.to_string();
    };
    if !std::path::Path::new(cleaned).is_absolute() {
        return cleaned.to_string();
    }

    let cwd_path = std::path::Path::new(cwd);
    match std::path::Path::new(cleaned).strip_prefix(cwd_path) {
        Ok(relative) => {
            let text = relative.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
            if text.is_empty() { ".".to_string() } else { text }
        }
        Err(_) => cleaned.to_string(),
    }
}

/// Whether `tag` is a well-formed content tag.
fn is_valid_tag(tag: &str) -> bool {
    tag.len() == FILE_HASH_LENGTH && tag.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parse a `[PATH]` or `[PATH#TAG]` header line.
///
/// Returns `Ok(None)` for a line that is not a header at all. Returns `Err` for
/// a line that looks like a header but is malformed, so a bad path surfaces
/// immediately rather than being silently reclassified as body content — the
/// failure mode where a header becomes a literal line in the file.
pub fn parse_header_line(line: &str, cwd: Option<&str>) -> Result<Option<RawSection>, String> {
    let trimmed = line.trim_end();
    if !trimmed.starts_with('[') {
        return Ok(None);
    }
    let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return Err(format!(
            "Input header must be [PATH] or [PATH#TAG] with a {FILE_HASH_LENGTH}-hex \
             content-hash tag; got {trimmed:?}."
        ));
    };

    let (raw_path, file_hash) = match inner.rsplit_once('#') {
        Some((path_part, tag)) if is_valid_tag(tag) => {
            (path_part, Some(tag.to_ascii_uppercase()))
        }
        // A `#` that is not a valid tag is part of the path: a fragment, or a
        // filename that genuinely contains one.
        _ => (inner, None),
    };

    let path = normalize_path(raw_path, cwd);
    if path.is_empty() {
        return Err("Input header \"[]\" is empty; provide a file path.".to_string());
    }

    Ok(Some(RawSection {
        path,
        file_hash,
        body: String::new(),
        interleaved: false,
    }))
}

/// Split an authored patch into sections.
///
/// Text before the first header belongs to a headerless leading section, which
/// callers treat as a single-file patch whose path came from elsewhere.
pub fn split_sections(input: &str, cwd: Option<&str>) -> Result<Vec<RawSection>, String> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut sections: Vec<RawSection> = Vec::new();
    let mut pending: Option<RawSection> = None;
    let mut body: Vec<&str> = Vec::new();

    for line in input.split('\n') {
        match parse_header_line(line, cwd)? {
            Some(header) => {
                if let Some(mut section) = pending.take() {
                    section.body = body.join("\n");
                    sections.push(section);
                }
                body.clear();
                pending = Some(header);
            }
            None => body.push(line),
        }
    }

    if let Some(mut section) = pending.take() {
        section.body = body.join("\n");
        sections.push(section);
    } else if !body.is_empty() && body.iter().any(|line| !line.trim().is_empty()) {
        // Headerless input: one anonymous section carrying everything.
        sections.push(RawSection {
            path: String::new(),
            file_hash: None,
            body: body.join("\n"),
            interleaved: false,
        });
    }

    Ok(merge_same_path_sections(sections))
}

/// Coalesce sections targeting one path, flagging non-adjacent merges.
///
/// Adjacent duplicates are a model repeating a header, which is harmless.
/// Non-adjacent ones mean another file's section sat between them, so merging
/// reorders operations relative to how they were authored; the flag lets the
/// caller refuse order-sensitive ops rather than applying them wrongly.
fn merge_same_path_sections(sections: Vec<RawSection>) -> Vec<RawSection> {
    let mut merged: Vec<RawSection> = Vec::new();

    for section in sections {
        let existing = merged
            .iter()
            .position(|candidate| candidate.path == section.path && !section.path.is_empty());

        match existing {
            Some(index) => {
                let was_adjacent = index + 1 == merged.len();
                let target = &mut merged[index];
                if !target.body.is_empty() && !section.body.is_empty() {
                    target.body.push('\n');
                }
                target.body.push_str(&section.body);
                if !was_adjacent {
                    target.interleaved = true;
                }
                // A later header carrying a tag wins: it is the fresher
                // observation of the file.
                if section.file_hash.is_some() {
                    target.file_hash = section.file_hash;
                }
            }
            None => merged.push(section),
        }
    }

    merged
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;
