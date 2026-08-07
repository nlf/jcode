//! Fuzzy matching for patch application.
//!
//! Ported from oh-my-pi's `src/edit/modes/replace.ts`, behaviour-first.
//!
//! A patch names lines it expects to find. Requiring those to match byte for
//! byte fails on a re-indented block or a changed line ending, which is a
//! rejection the caller cannot act on because the code is *there*. Matching too
//! loosely applies the edit to the wrong place, which is worse. This module is
//! where that tradeoff lives.

/// Similarity below which a candidate is not a match.
///
/// omp's value. High enough that a genuinely different block is refused, low
/// enough to absorb whitespace and small formatting drift.
pub const DEFAULT_FUZZY_THRESHOLD: f64 = 0.95;

/// Levenshtein edit distance.
///
/// Two rolling rows rather than a full matrix: patch bodies can be long, and
/// the full matrix is O(n*m) memory for no benefit when only the last row is
/// ever read.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b.len()]
}

/// Similarity from 0 to 1.
pub fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein_distance(a, b) as f64 / max_len as f64)
}

/// Leading whitespace of a line.
pub fn leading_whitespace(line: &str) -> &str {
    let end = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    &line[..end]
}

/// Width of a line's leading whitespace, counting a tab as one.
pub fn count_leading_whitespace(line: &str) -> usize {
    leading_whitespace(line).chars().count()
}

/// Collapse a line for comparison: trimmed, with interior runs of whitespace
/// reduced to one space.
///
/// This is what lets a re-indented or re-wrapped line still match. Comparing
/// raw text instead would reject a patch whose only difference is that the file
/// was reformatted since it was written.
pub fn normalize_for_fuzzy(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for c in line.trim().chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// Where a sequence was found, and how well it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceMatch {
    /// 0-based index of the first matching line.
    pub start: usize,
    /// 1.0 for an exact match.
    pub score: f64,
}

/// Find `needle` in `haystack`, exactly.
///
/// Tried before any fuzzy search: an exact match is unambiguous, and preferring
/// it means a file containing both an exact and an approximate occurrence
/// patches the one the caller actually wrote.
pub fn find_exact_sequence(
    haystack: &[String],
    needle: &[String],
    from: usize,
) -> Option<SequenceMatch> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    for start in from..=(haystack.len() - needle.len()) {
        if haystack[start..start + needle.len()] == *needle {
            return Some(SequenceMatch { start, score: 1.0 });
        }
    }
    None
}

/// Find `needle` in `haystack`, ignoring indentation and interior whitespace.
///
/// Every line must clear the threshold. Scoring the block as a whole would let
/// one badly wrong line hide inside a run of good ones, which is how a patch
/// lands in the wrong place.
pub fn find_fuzzy_sequence(
    haystack: &[String],
    needle: &[String],
    from: usize,
    threshold: f64,
) -> Option<SequenceMatch> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }

    let normalized_needle: Vec<String> = needle.iter().map(|line| normalize_for_fuzzy(line)).collect();
    let mut best: Option<SequenceMatch> = None;

    for start in from..=(haystack.len() - needle.len()) {
        let mut total = 0.0;
        let mut all_pass = true;
        for (offset, wanted) in normalized_needle.iter().enumerate() {
            let found = normalize_for_fuzzy(&haystack[start + offset]);
            let score = similarity(wanted, &found);
            if score < threshold {
                all_pass = false;
                break;
            }
            total += score;
        }
        if !all_pass {
            continue;
        }
        let score = total / needle.len() as f64;
        // Strictly better, so the earliest of several equal matches wins.
        // Ties going to the later one would move an edit further from where the
        // caller was looking for no reason.
        if best.as_ref().is_none_or(|current| score > current.score) {
            best = Some(SequenceMatch { start, score });
        }
    }

    best
}

/// Find a sequence, exactly if possible and fuzzily otherwise.
pub fn seek_sequence(
    haystack: &[String],
    needle: &[String],
    from: usize,
    threshold: f64,
) -> Option<SequenceMatch> {
    find_exact_sequence(haystack, needle, from)
        .or_else(|| find_fuzzy_sequence(haystack, needle, from, threshold))
}

/// The closest match to `needle` even if it is below the threshold.
///
/// For error messages only. A rejection that says "closest match at line 40,
/// 82% similar" tells the caller their patch is stale; a bare "not found" sends
/// them re-reading the whole file.
pub fn find_closest_sequence(haystack: &[String], needle: &[String]) -> Option<SequenceMatch> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let normalized_needle: Vec<String> = needle.iter().map(|line| normalize_for_fuzzy(line)).collect();
    let mut best: Option<SequenceMatch> = None;

    for start in 0..=(haystack.len() - needle.len()) {
        let total: f64 = normalized_needle
            .iter()
            .enumerate()
            .map(|(offset, wanted)| {
                similarity(wanted, &normalize_for_fuzzy(&haystack[start + offset]))
            })
            .sum();
        let score = total / needle.len() as f64;
        if best.as_ref().is_none_or(|current| score > current.score) {
            best = Some(SequenceMatch { start, score });
        }
    }

    best
}

/// Find the line a `@@ context` header names.
///
/// The header is a hint about where in the file to look, not part of the
/// change, so it is matched loosely and its absence is not fatal.
pub fn find_context_line(haystack: &[String], context: &str, from: usize) -> Option<usize> {
    let wanted = normalize_for_fuzzy(context);
    if wanted.is_empty() {
        return None;
    }
    haystack
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, line)| normalize_for_fuzzy(line) == wanted)
        .map(|(index, _)| index)
}

/// Re-indent `replacement` to sit where `matched` was.
///
/// A patch written against a differently indented copy carries the *relative*
/// shape of the block correctly but the wrong absolute indentation. Applying it
/// verbatim produces code that is right but misaligned, which then shows up as
/// a spurious diff on every later edit.
pub fn adjust_indentation(matched: &[String], replacement: &[String]) -> Vec<String> {
    let matched_indent = matched
        .iter()
        .find(|line| !line.trim().is_empty())
        .map(|line| leading_whitespace(line))
        .unwrap_or("");
    let replacement_indent = replacement
        .iter()
        .find(|line| !line.trim().is_empty())
        .map(|line| leading_whitespace(line))
        .unwrap_or("");

    if matched_indent == replacement_indent {
        return replacement.to_vec();
    }

    replacement
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                // A blank line has no indentation to preserve, and giving it
                // some would add trailing whitespace.
                return line.clone();
            }
            match line.strip_prefix(replacement_indent) {
                Some(rest) => format!("{matched_indent}{rest}"),
                // Less indented than the block's own base: leave it alone
                // rather than guess, since the shape is already unusual.
                None => line.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod fuzzy_tests;
