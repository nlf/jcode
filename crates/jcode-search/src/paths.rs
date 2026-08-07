//! Parsing the path arguments `grep` and `glob` accept.
//!
//! Ported from oh-my-pi's `src/tools/path-utils.ts`, behaviour-first: their
//! tests are the specification.
//!
//! A search path is not just a path. It can carry a line selector
//! (`src/foo.ts:50-100`), several ranges (`:5-16,960-973`), or a count form
//! (`:50+10`). Several paths arrive as one semicolon-delimited string. Getting
//! this wrong is quiet rather than loud: a literal file named `notes:1-2`
//! silently becomes `notes` plus a selector, and the search runs against the
//! wrong file without an error.

/// An inclusive 1-indexed line range. `end` of `None` means "to end of file".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub end: Option<usize>,
}

/// Why a selector was refused.
///
/// Separate from a plain `None`: "this is not a selector" and "this is a
/// selector with impossible bounds" want different answers. The first leaves
/// the text as part of the path, the second is a mistake worth reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    /// Line 0, when lines are 1-indexed.
    ZeroLine,
    /// `N-M` where M < N.
    Backwards { start: usize, end: usize },
    /// `N+K` where K < 1.
    EmptyCount { start: usize, count: usize },
}

impl SelectorError {
    pub fn message(&self) -> String {
        match self {
            Self::ZeroLine => "Line selector 0 is invalid; lines are 1-indexed. Use :1.".to_string(),
            Self::Backwards { start, end } => {
                format!("Invalid range {start}-{end}: end must be >= start.")
            }
            Self::EmptyCount { start, count } => {
                format!("Invalid range {start}+{count}: count must be >= 1.")
            }
        }
    }
}

/// Parse one chunk: `N`, `N-M`, `N-`, `N+K`, `N..M`, `N..`, each optionally
/// `L`-prefixed (`L50`).
///
/// Returns `Ok(None)` when the text is not a selector at all, which is what
/// keeps a path containing a colon from being misread.
pub fn parse_line_range_chunk(text: &str) -> Result<Option<LineRange>, SelectorError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    // Optional leading `L`, as models paste `L50` from line-number UIs.
    let rest = strip_line_prefix(text);
    let (start_digits, rest) = take_digits(rest);
    if start_digits.is_empty() {
        return Ok(None);
    }
    let Ok(start) = start_digits.parse::<usize>() else {
        return Ok(None);
    };

    // `..` is a forgiving alias for `-`, since models paste Rust range syntax.
    let (separator, rest) = if let Some(rest) = rest.strip_prefix("..") {
        ("-", rest)
    } else if let Some(rest) = rest.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = rest.strip_prefix('+') {
        ("+", rest)
    } else if rest.is_empty() {
        // A bare number: one line onward, matching `N-`.
        if start < 1 {
            return Err(SelectorError::ZeroLine);
        }
        return Ok(Some(LineRange { start, end: None }));
    } else {
        return Ok(None);
    };

    let rest = strip_line_prefix(rest);
    let (end_digits, tail) = take_digits(rest);
    if !tail.is_empty() {
        return Ok(None);
    }

    // Bounds are checked only once the shape is known to be a selector, so
    // ordinary text is never rejected for having a zero in it.
    if start < 1 {
        return Err(SelectorError::ZeroLine);
    }

    if end_digits.is_empty() {
        return match separator {
            // `301-` is "from 301 onward", the same as a bare `301`.
            "-" => Ok(Some(LineRange { start, end: None })),
            // `50+` has no count, so it is not a selector.
            _ => Ok(None),
        };
    }

    let Ok(value) = end_digits.parse::<usize>() else {
        return Ok(None);
    };

    match separator {
        "+" => {
            if value < 1 {
                return Err(SelectorError::EmptyCount {
                    start,
                    count: value,
                });
            }
            Ok(Some(LineRange {
                start,
                end: Some(start + value - 1),
            }))
        }
        _ => {
            if value < start {
                return Err(SelectorError::Backwards { start, end: value });
            }
            Ok(Some(LineRange {
                start,
                end: Some(value),
            }))
        }
    }
}

fn strip_line_prefix(text: &str) -> &str {
    text.strip_prefix('L')
        .or_else(|| text.strip_prefix('l'))
        .unwrap_or(text)
}

fn take_digits(text: &str) -> (&str, &str) {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

/// Parse a comma-separated list of ranges, sorted and merged.
///
/// Merging matters downstream: a consumer streams each range in one forward
/// pass, so overlapping ranges would read the same lines twice and report
/// duplicate matches.
pub fn parse_line_ranges(text: &str) -> Result<Option<Vec<LineRange>>, SelectorError> {
    let mut parsed = Vec::new();
    for chunk in text.split(',') {
        match parse_line_range_chunk(chunk)? {
            Some(range) => parsed.push(range),
            // One bad chunk disqualifies the whole selector rather than
            // silently searching a subset of what was asked for.
            None => return Ok(None),
        }
    }
    if parsed.is_empty() {
        return Ok(None);
    }

    parsed.sort_by_key(|range| range.start);

    let mut merged: Vec<LineRange> = vec![parsed[0]];
    for current in parsed.into_iter().skip(1) {
        let last = merged
            .last_mut()
            .expect("merged is seeded with the first range");
        match last.end {
            // Open-ended already runs to EOF, so anything later is inside it.
            None => continue,
            Some(last_end) => {
                // `<= last_end + 1` merges adjacent ranges too: 1-5 and 6-9
                // describe one span, and keeping them apart would re-read the
                // boundary.
                if current.start <= last_end + 1 {
                    match current.end {
                        None => last.end = None,
                        Some(end) if end > last_end => last.end = Some(end),
                        Some(_) => {}
                    }
                    continue;
                }
            }
        }
        merged.push(current);
    }

    Ok(Some(merged))
}

/// Whether a 1-indexed line falls in any range.
pub fn is_line_in_ranges(line: usize, ranges: &[LineRange]) -> bool {
    ranges.iter().any(|range| {
        line >= range.start && range.end.map(|end| line <= end).unwrap_or(true)
    })
}

/// A path split from its trailing selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPath {
    pub path: String,
    pub selector: Option<String>,
}

/// Split a trailing `:selector` off a path.
///
/// Only a selector-shaped suffix is split, so `C:\src` and a file literally
/// named `notes:txt` survive. Compound forms (`path:1-50:raw`, `path:raw:1-50`)
/// are recognised in either order.
pub fn split_path_and_selector(raw: &str) -> SplitPath {
    let Some(colon) = raw.rfind(':') else {
        return SplitPath {
            path: raw.to_string(),
            selector: None,
        };
    };
    // `colon == 0` is a leading colon, which leaves no path at all.
    if colon == 0 {
        return SplitPath {
            path: raw.to_string(),
            selector: None,
        };
    }

    let candidate = &raw[colon + 1..];
    if !is_selector_chunk(candidate) {
        return SplitPath {
            path: raw.to_string(),
            selector: None,
        };
    }

    let mut base = &raw[..colon];
    let mut selector = candidate.to_string();

    // A compound selector is one range plus one `raw`, in either order.
    if let Some(inner_colon) = base.rfind(':')
        && inner_colon > 0
    {
        let inner = &base[inner_colon + 1..];
        let inner_raw = is_display_mode(inner);
        let outer_raw = is_display_mode(candidate);
        let inner_range = is_range_list(inner);
        let outer_range = is_range_list(candidate);
        if (inner_raw && outer_range) || (inner_range && outer_raw) {
            selector = format!("{inner}:{candidate}");
            base = &base[..inner_colon];
        }
    }

    SplitPath {
        path: base.to_string(),
        selector: Some(selector),
    }
}

/// The line ranges a selector names, ignoring display modes.
///
/// A selector of only `raw` or `conflicts` yields `None`, meaning the whole
/// resource. Search has no use for display modes, so it accepts and ignores
/// them rather than refusing a selector the read tool would have taken.
pub fn selector_line_ranges(selector: Option<&str>) -> Result<Option<Vec<LineRange>>, SelectorError> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    for chunk in selector.split(':') {
        if is_display_mode(chunk) {
            continue;
        }
        if let Some(ranges) = parse_line_ranges(chunk)? {
            return Ok(Some(ranges));
        }
    }
    Ok(None)
}

fn is_display_mode(text: &str) -> bool {
    text.eq_ignore_ascii_case("raw") || text.eq_ignore_ascii_case("conflicts")
}

fn is_range_list(text: &str) -> bool {
    matches!(parse_line_ranges(text), Ok(Some(_)))
        // A range with impossible bounds is still selector-shaped. Treating it
        // as part of the path would hide the mistake instead of reporting it.
        || parse_line_ranges(text).is_err()
}

fn is_selector_chunk(text: &str) -> bool {
    is_display_mode(text) || is_range_list(text)
}

/// Characters that make a path a glob rather than a literal.
const GLOB_CHARS: [char; 5] = ['*', '?', '[', ']', '{'];

/// Whether a path should be treated as a pattern.
pub fn has_glob_chars(path: &str) -> bool {
    path.chars().any(|c| GLOB_CHARS.contains(&c))
}

/// Split a semicolon-delimited path list.
///
/// Empty entries are dropped rather than becoming a search of the working
/// directory: a trailing `;` is a typo, and treating it as "also search
/// everything" turns a scoped search into a whole-repo one.
pub fn split_path_list(input: &str) -> Vec<String> {
    input
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod paths_tests;
