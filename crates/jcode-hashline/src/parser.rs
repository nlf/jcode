//! Parsing hunk headers and body rows into edit operations.
//!
//! Ported from the op layer of oh-my-pi's `tokenizer.ts` and `parser.ts`,
//! against the grammar in their `grammar.lark`. Clipboard registers (`@name`)
//! are deliberately out of scope for v1: they are only useful once moves are
//! common.
//!
//! # The separator leniency is the point
//!
//! Canonical range syntax is `PUT 5.=9:`, but the scanner accepts `-`, `.`,
//! `..`, `=`, `…`, mixed runs, and bare whitespace. That is not sloppiness:
//! `5-9` is what a model writes when it is thinking about line ranges rather
//! than about this format, and rejecting it costs a turn to teach syntax the
//! model will forget again. The canonical form exists so *we* can render one
//! spelling; the parser exists so every spelling lands.

use crate::prefixes::{is_read_metadata_line, strip_one_leading_hashline_prefix};

/// Where an edit is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// An inclusive range of original lines, 1-indexed.
    Range { start: usize, end: usize },
    /// The syntactic block beginning on a line, resolved against the file.
    ///
    /// Deferred rather than resolved here, because the parser has neither the
    /// file text nor its language: `PUT 5*:` is only meaningful once someone
    /// can look at line 5. [`crate::blocks::resolve`] turns it into a
    /// [`Anchor::Range`], and every later stage sees only concrete ranges.
    Block(usize),
    /// The gap before a line.
    Before(usize),
    /// The gap after a line.
    After(usize),
    /// The gap before the first line.
    Bof,
    /// The gap after the last line.
    Eof,
}

/// One parsed operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Replace a range, or insert at a gap, with `body`.
    Put { anchor: Anchor, body: Vec<String> },
    /// Delete a range.
    Cut { start: usize, end: usize },
    /// Delete the whole file.
    Rem,
    /// Move the file to `dest`.
    Mv { dest: String },
}

/// A parsed patch body plus anything worth telling the caller about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedOps {
    pub ops: Vec<Op>,
    /// Recoveries applied. These are surfaced so a model can learn the
    /// canonical form, without the edit having failed to teach it.
    pub warnings: Vec<String>,
}

/// Ranges above this are refused rather than expanded, so a mistyped line
/// number cannot allocate unboundedly.
const MAX_RANGE_SPAN: usize = 100_000;

/// Characters accepted between two line numbers.
///
/// `…` is here because models paste it from elided read output.
fn is_range_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '.' | '=' | '…')
}

/// Scan a 1-indexed line number. Leading zeros are rejected: `01` is more
/// likely a typo or a quoted string than a line reference.
fn scan_line_number(text: &str) -> Option<(usize, &str)> {
    let trimmed = text.trim_start();
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.starts_with('0') {
        return None;
    }
    let value = digits.parse::<usize>().ok()?;
    Some((value, &trimmed[digits.len()..]))
}

/// Scan `N`, `N<sep>M`, or a bare `N` when `allow_single` is set.
fn scan_range(text: &str, allow_single: bool) -> Option<(usize, usize, &str)> {
    let (start, rest) = scan_line_number(text)?;

    let separator_len = rest.chars().take_while(|c| is_range_separator(*c)).count();
    if separator_len == 0 {
        return allow_single.then_some((start, start, rest));
    }
    let after_separator = &rest[rest
        .char_indices()
        .nth(separator_len)
        .map(|(i, _)| i)
        .unwrap_or(rest.len())..];

    match scan_line_number(after_separator) {
        Some((end, tail)) => Some((start, end, tail)),
        // Trailing separator with no second number: `CUT 5 ` is still a
        // single-line cut, and the trailing space is not a syntax error.
        None if allow_single => Some((start, start, after_separator)),
        None => None,
    }
}

/// Outcome of reading a line as a hunk header.
enum HeaderScan {
    /// Not a header at all; treat as body or payload.
    NotAHeader,
    /// A header this version implements.
    Op(Op, bool),
    /// A header the format defines but this version does not implement.
    ///
    /// Distinguished from `NotAHeader` because the messages differ in kind: an
    /// unsupported feature should not be reported as unrecognized syntax, or a
    /// model retries the same op instead of choosing another.
    Unsupported(String),
}

/// Parse one hunk header, returning the anchor and whether a body follows.
fn parse_header(line: &str) -> HeaderScan {
    let trimmed = line.trim();

    if trimmed == "REM" {
        return HeaderScan::Op(Op::Rem, false);
    }
    if let Some(dest) = trimmed.strip_prefix("MV ") {
        let dest = dest.trim().trim_matches(['"', '\'']);
        if dest.is_empty() {
            return HeaderScan::NotAHeader;
        }
        return HeaderScan::Op(
            Op::Mv {
                dest: dest.to_string(),
            },
            false,
        );
    }

    if let Some(rest) = trimmed.strip_prefix("CUT ") {
        let rest = rest.trim();
        if let Some(reason) = unsupported_feature(rest) {
            return HeaderScan::Unsupported(reason);
        }
        if let Some(line) = scan_block_anchor(rest) {
            return HeaderScan::Op(
                Op::Put {
                    anchor: Anchor::Block(line),
                    body: Vec::new(),
                },
                false,
            );
        }
        let Some((start, end, tail)) = scan_range(rest, true) else {
            return HeaderScan::NotAHeader;
        };
        if !tail.trim().is_empty() {
            return HeaderScan::NotAHeader;
        }
        return HeaderScan::Op(Op::Cut { start, end }, false);
    }

    let Some(rest) = trimmed.strip_prefix("PUT ") else {
        return HeaderScan::NotAHeader;
    };
    let rest = rest.trim();
    if let Some(reason) = unsupported_feature(rest) {
        return HeaderScan::Unsupported(reason);
    }

    let (locator, expects_body) = match rest.strip_suffix(':') {
        Some(locator) => (locator.trim(), true),
        None => (rest, false),
    };

    // A block anchor, resolved later against the file. Checked before the gap
    // locators so `>N*` is not mistaken for a plain `>N` with trailing junk.
    if let Some(line) = scan_block_anchor(locator) {
        return HeaderScan::Op(
            Op::Put {
                anchor: Anchor::Block(line),
                body: Vec::new(),
            },
            expects_body,
        );
    }

    // Gap locators.
    if let Some(target) = locator.strip_prefix('<') {
        let Some((line, tail)) = scan_line_number(target) else {
            return HeaderScan::NotAHeader;
        };
        if !tail.trim().is_empty() {
            return HeaderScan::NotAHeader;
        }
        let anchor = if line == 1 {
            Anchor::Bof
        } else {
            Anchor::Before(line)
        };
        return HeaderScan::Op(
            Op::Put {
                anchor,
                body: Vec::new(),
            },
            expects_body,
        );
    }
    if let Some(target) = locator.strip_prefix('>') {
        let target = target.trim();
        if target == "$" {
            return HeaderScan::Op(
                Op::Put {
                    anchor: Anchor::Eof,
                    body: Vec::new(),
                },
                expects_body,
            );
        }
        let Some((line, tail)) = scan_line_number(target) else {
            return HeaderScan::NotAHeader;
        };
        if !tail.trim().is_empty() {
            return HeaderScan::NotAHeader;
        }
        return HeaderScan::Op(
            Op::Put {
                anchor: Anchor::After(line),
                body: Vec::new(),
            },
            expects_body,
        );
    }

    // A range replacement.
    let Some((start, end, tail)) = scan_range(locator, true) else {
        return HeaderScan::NotAHeader;
    };
    if !tail.trim().is_empty() {
        return HeaderScan::NotAHeader;
    }
    HeaderScan::Op(
        Op::Put {
            anchor: Anchor::Range { start, end },
            body: Vec::new(),
        },
        expects_body,
    )
}

/// Scan a block anchor: a line number followed by `*`, optionally after `>`.
///
/// `PUT 5*:` replaces the block beginning on line 5; `CUT 5*` deletes it. The
/// `>` form (`PUT >5*:`, insert after the block) is deliberately not accepted
/// here and falls through to [`unsupported_feature`], because landing an
/// insertion after a block needs the depth correction that is not built yet,
/// and accepting it would put the body at the block's last line with no regard
/// for the scope it claimed.
fn scan_block_anchor(locator: &str) -> Option<usize> {
    let locator = locator.trim();
    let (line, tail) = scan_line_number(locator)?;
    (tail.trim() == "*").then_some(line)
}

/// Recognize a locator that uses a defined-but-unimplemented feature.
///
/// Reported by name so a model learns the feature is absent rather than that
/// its syntax was wrong. The latter invites an identical retry.
fn unsupported_feature(locator: &str) -> Option<String> {
    if locator.starts_with('>') && locator.contains('*') {
        return Some(
            "Inserting after a block (`>N*`) is not supported yet; use `PUT N*:` to \
             replace the block, or `PUT >M:` with the block's last line."
                .to_string(),
        );
    }
    if locator.contains('@') {
        return Some(
            "Clipboard registers (`@name`) are not supported yet; delete with `CUT` \
             and re-insert the content with `PUT`."
                .to_string(),
        );
    }
    None
}

/// Parse a section body into operations.
///
/// Body rows are `+TEXT`, where `+` alone is a blank line. A bare row that
/// follows a `:` header is auto-prefixed rather than rejected: a model omitting
/// the sigil is the single most common near-miss, and the row's position makes
/// its intent unambiguous.
pub fn parse_ops(body: &str) -> Result<ParsedOps, String> {
    let mut parsed = ParsedOps::default();
    let mut pending: Option<(Op, Vec<String>)> = None;

    let flush = |parsed: &mut ParsedOps, pending: Option<(Op, Vec<String>)>| {
        let Some((op, body)) = pending else {
            return;
        };
        match op {
            Op::Put { anchor, .. } => {
                if body.is_empty() {
                    // An empty replacement body means deletion, which is what
                    // the model meant even though `CUT` says it better.
                    if let Anchor::Range { start, end } = anchor {
                        parsed.ops.push(Op::Cut { start, end });
                        parsed.warnings.push(format!(
                            "Read an empty `PUT` body as a deletion of lines {start}-{end}; \
                             `CUT {start}.={end}` states that directly."
                        ));
                        return;
                    }
                }
                parsed.ops.push(Op::Put { anchor, body });
            }
            other => parsed.ops.push(other),
        }
    };

    for raw in body.split('\n') {
        if is_read_metadata_line(raw) {
            continue;
        }

        match parse_header(raw) {
            HeaderScan::Unsupported(reason) => return Err(reason),
            HeaderScan::Op(op, expects_body) => {
                flush(&mut parsed, pending.take());
                if expects_body {
                    pending = Some((op, Vec::new()));
                } else {
                    parsed.ops.push(op);
                }
                continue;
            }
            HeaderScan::NotAHeader => {}
        }

        match pending.as_mut() {
            Some((_, rows)) => {
                if let Some(text) = raw.strip_prefix('+') {
                    rows.push(text.to_string());
                } else if raw.trim().is_empty() {
                    // A blank line between hunks is formatting, not content.
                    continue;
                } else {
                    // Auto-prefix a bare row, stripping any read-output line
                    // number it carried, so `3:replaced` becomes `replaced`
                    // rather than writing the prefix into the file.
                    let recovered = strip_one_leading_hashline_prefix(raw);
                    parsed.warnings.push(format!(
                        "Auto-prefixed bare body row {recovered:?}; body rows start with `+`."
                    ));
                    rows.push(recovered);
                }
            }
            None => {
                if raw.trim().is_empty() {
                    continue;
                }
                return Err(format!(
                    "Payload line has no preceding hunk header: {raw:?}. Start a hunk \
                     with `PUT`, `CUT`, `REM` or `MV`."
                ));
            }
        }
    }

    flush(&mut parsed, pending.take());

    for op in &parsed.ops {
        let (start, end) = match op {
            Op::Cut { start, end } => (*start, *end),
            Op::Put {
                anchor: Anchor::Range { start, end },
                ..
            } => (*start, *end),
            _ => continue,
        };
        if end < start {
            return Err(format!("Range {start}.={end} ends before it starts."));
        }
        let span = end - start + 1;
        if span > MAX_RANGE_SPAN {
            return Err(format!(
                "Range spans {span} lines; the maximum is {MAX_RANGE_SPAN}."
            ));
        }
    }

    Ok(parsed)
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod parser_tests;
