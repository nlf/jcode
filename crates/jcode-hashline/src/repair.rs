//! Repairing the near-misses a model makes at the edges of a replacement.
//!
//! Ported from the repair half of oh-my-pi's `apply.ts`, against their
//! `boundary-repair.test.ts`. This is the layer the port deliberately left out
//! at v1, and it is most of what makes hashline forgiving rather than merely
//! precise.
//!
//! # The mistake being repaired
//!
//! `PUT 4=6:` says "replace lines 4 to 6 with this body". A model that has just
//! read those lines frequently sends back the surrounding lines too, because
//! restating context is what every other diff format asks for. Applied
//! literally, the result duplicates the neighbours: the line above the range
//! now appears twice, and so does the line below. The edit looks like it worked
//! and the file is quietly wrong.
//!
//! # Evidence, never plausibility
//!
//! Every repair here fires only on positive evidence of a specific mistake, and
//! that is what makes the whole approach safe rather than reckless. A repair
//! needs an exact line-for-line match against text already in the file, plus a
//! delimiter count proving the removal restores the balance the range had. When
//! two readings are equally consistent, nothing fires.
//!
//! That design has a consequence worth stating, because it is the reason the
//! crude delimiter counting below is acceptable. The counter does not
//! understand regex literals, or `${}` inside a template, or JSX. It will
//! sometimes get a count wrong. But a wrong count can only fail an equality
//! check, and failing an equality check only ever *suppresses* a repair. There
//! is no path from a miscount to a repair that would not otherwise have fired.
//! Preserve that property in anything added here: the moment a rule fires
//! because a balance "looks fine" rather than because it matches exactly, the
//! naivety turns into a corruption vector.
//!
//! # What is not here
//!
//! omp also spares dropped structural closers, which needs a syntax probe to
//! decide whether a lone `}` is code or prose. Their entire safety argument for
//! it is that tree-sitter vetoes the repair: the authored result must fail to
//! parse and the repaired one must parse before anything is rewritten. Twenty
//! one of their pinned behaviours are about that veto. Without it the rule
//! resurrects braces inside Markdown and strings, so it is left out rather than
//! approximated. The detection could be ported ahead of the probe to warn
//! without rewriting, which is what omp does when spares are disabled.

use crate::parser::{Anchor, Op};

/// Counts of the three bracket kinds, positive for unclosed openers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Balance {
    paren: i32,
    bracket: i32,
    brace: i32,
}

impl Balance {
    fn is_zero(self) -> bool {
        self == Self::default()
    }

    fn minus(self, other: Self) -> Self {
        Self {
            paren: self.paren - other.paren,
            bracket: self.bracket - other.bracket,
            brace: self.brace - other.brace,
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            paren: self.paren + other.paren,
            bracket: self.bracket + other.bracket,
            brace: self.brace + other.brace,
        }
    }
}

/// Count unclosed brackets across `lines`, ignoring comments and string bodies.
///
/// Deliberately crude, and see the module note on why that is safe. Two pieces
/// of state survive a line ending, because they genuinely span lines in the
/// languages this sees: a block comment, and a backtick string. Single and
/// double quotes are reset at the newline instead, since an unterminated one is
/// far more likely to be an apostrophe in prose than a real string.
pub fn compute_balance<S: AsRef<str>>(lines: &[S]) -> Balance {
    let mut balance = Balance::default();
    let mut in_block_comment = false;
    let mut quote: Option<char> = None;

    for line in lines {
        let chars: Vec<char> = line.as_ref().chars().collect();
        let mut index = 0;
        while index < chars.len() {
            let ch = chars[index];
            let next = chars.get(index + 1).copied();

            if in_block_comment {
                if ch == '*' && next == Some('/') {
                    in_block_comment = false;
                    index += 1;
                }
                index += 1;
                continue;
            }

            if let Some(open) = quote {
                if ch == '\\' {
                    // Skip whatever follows, even at the end of the line. A
                    // trailing backslash therefore consumes nothing and the
                    // quote state survives to the reset below.
                    index += 1;
                } else if ch == open {
                    quote = None;
                }
                index += 1;
                continue;
            }

            match ch {
                '"' | '\'' | '`' => quote = Some(ch),
                '/' if next == Some('/') => break,
                '/' if next == Some('*') => {
                    in_block_comment = true;
                    index += 1;
                }
                '(' => balance.paren += 1,
                ')' => balance.paren -= 1,
                '[' => balance.bracket += 1,
                ']' => balance.bracket -= 1,
                '{' => balance.brace += 1,
                '}' => balance.brace -= 1,
                _ => {}
            }
            index += 1;
        }

        if matches!(quote, Some('"') | Some('\'')) {
            quote = None;
        }
    }

    balance
}

/// True when any character is something other than ASCII whitespace.
///
/// Deliberately not `trim().is_empty()`: that also treats a non-breaking space
/// as blank, and a line containing one is content a model may have meant.
fn has_content(line: &str) -> bool {
    line.chars()
        .any(|ch| !matches!(ch, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' '))
}

/// A replacement hunk, paired with the file it applies to.
struct Group {
    op_index: usize,
    start_line: usize,
    end_line: usize,
}

/// What a repair pass concluded.
#[derive(Debug, Default)]
pub struct RepairOutcome {
    pub warnings: Vec<String>,
}

/// Repair the boundaries of every replacement in `ops`, in place.
///
/// Only `PUT start=end:` operations are considered. An insertion has no range
/// for its payload to overflow, and a `CUT` has no payload at all.
pub fn repair_boundaries(ops: &mut [Op], file_lines: &[&str]) -> RepairOutcome {
    let mut outcome = RepairOutcome::default();
    let groups: Vec<Group> = ops
        .iter()
        .enumerate()
        .filter_map(|(op_index, op)| match op {
            Op::Put {
                anchor: Anchor::Range { start, end },
                ..
            } => Some(Group {
                op_index,
                start_line: *start,
                end_line: *end,
            }),
            _ => None,
        })
        .collect();

    // Indentation is repaired first and separately, because it changes the
    // payload text that every later rule compares against.
    let mut indent_shifted = false;
    for group in &groups {
        if repair_indentation(ops, group, file_lines) {
            indent_shifted = true;
        }
    }
    if indent_shifted {
        outcome.warnings.push(
            "Auto-indented a replacement body to match unchanged structural rows in \
             its source range."
                .to_string(),
        );
    }

    for group in &groups {
        if let Some(warning) = repair_group(ops, group, file_lines) {
            outcome.warnings.push(warning);
        }
    }

    outcome
}

/// The payload of a replacement op.
fn payload<'a>(ops: &'a [Op], group: &Group) -> &'a [String] {
    match &ops[group.op_index] {
        Op::Put { body, .. } => body,
        _ => &[],
    }
}

fn payload_mut<'a>(ops: &'a mut [Op], group: &Group) -> &'a mut Vec<String> {
    match &mut ops[group.op_index] {
        Op::Put { body, .. } => body,
        _ => unreachable!("groups are built only from replacement ops"),
    }
}

/// The file lines the range covers.
fn range_lines<'a>(group: &Group, file_lines: &'a [&'a str]) -> &'a [&'a str] {
    let start = group.start_line.saturating_sub(1);
    let end = group.end_line.min(file_lines.len());
    if start >= end { &[] } else { &file_lines[start..end] }
}

/// Try each boundary rule in turn, and stop at the first that fires.
///
/// The order matters and is omp's. A two-sided echo is checked before anything
/// that reasons about delimiter counts, because a payload that restates both
/// neighbours is explained completely by that one reading, and letting a
/// count-based rule see it first would explain half of it and leave the rest.
fn repair_group(ops: &mut [Op], group: &Group, file_lines: &[&str]) -> Option<String> {
    if let Some(warning) = repair_two_sided_echo(ops, group, file_lines) {
        return Some(warning);
    }

    let delta = compute_balance(payload(ops, group))
        .minus(compute_balance(range_lines(group, file_lines)));

    // A balanced payload cannot be explained by a duplicated bracket, so the
    // remaining rules have nothing to prove and are skipped. omp forks hard
    // here: a zero-delta group is only ever eligible for the one-sided echo,
    // which is not ported (see the module note).
    if delta.is_zero() {
        return None;
    }

    repair_duplicate_suffix(ops, group, file_lines, delta)
        .or_else(|| repair_duplicate_prefix(ops, group, file_lines, delta))
}

/// How many leading payload rows exactly repeat the lines above the range.
fn leading_echo(payload: &[String], group: &Group, file_lines: &[&str]) -> usize {
    let limit = payload.len().min(group.start_line - 1);
    let mut best = 0;
    for count in 1..=limit {
        let above = &file_lines[group.start_line - 1 - count..group.start_line - 1];
        if payload[..count]
            .iter()
            .zip(above)
            .all(|(row, line)| row == line)
            && payload[..count].iter().any(|row| has_content(row))
        {
            best = count;
        }
    }
    best
}

/// How many trailing payload rows exactly repeat the lines below the range.
fn trailing_echo(payload: &[String], group: &Group, file_lines: &[&str]) -> usize {
    let limit = payload.len().min(file_lines.len().saturating_sub(group.end_line));
    let mut best = 0;
    for count in 1..=limit {
        let below = &file_lines[group.end_line..group.end_line + count];
        let tail = &payload[payload.len() - count..];
        if tail.iter().zip(below).all(|(row, line)| row == line)
            && tail.iter().any(|row| has_content(row))
        {
            best = count;
        }
    }
    best
}

/// The payload restated the lines on **both** sides of the range.
///
/// This is the common mistake: the model quoted the neighbours to show where
/// its replacement goes. Applying it literally duplicates both.
fn repair_two_sided_echo(ops: &mut [Op], group: &Group, file_lines: &[&str]) -> Option<String> {
    let body = payload(ops, group);
    let leading = leading_echo(body, group, file_lines);
    let trailing = trailing_echo(body, group, file_lines);
    if leading == 0 || trailing == 0 {
        return None;
    }
    // Strictly less, so the echoes can never claim the entire payload. A
    // payload that is *all* echo is not a boundary mistake, and dropping every
    // row would turn a replacement into a deletion.
    if leading + trailing >= body.len() {
        return None;
    }

    let dropped = compute_balance(&body[..leading]).plus(compute_balance(&body[body.len() - trailing..]));
    if !dropped.is_zero() {
        // The echo carries brackets, so removing it changes the balance. That
        // is only safe when the change is exactly the imbalance the payload
        // had, which is the case where the echo explains the whole discrepancy.
        let delta = compute_balance(body).minus(compute_balance(range_lines(group, file_lines)));
        if dropped != delta {
            return None;
        }
    }

    let body = payload_mut(ops, group);
    body.truncate(body.len() - trailing);
    body.drain(..leading);

    Some(format!(
        "Auto-repaired a replacement boundary echo at line {}: dropped {leading} leading \
         and {trailing} trailing payload line(s) already present outside the range. Issue \
         the payload as the final desired content for the selected range only, and never \
         restate unchanged lines bordering the range.",
        group.start_line
    ))
}

/// The payload ends with lines that already sit below the range.
///
/// Distinguished from an intentional repetition by the balance identity:
/// dropping exactly these rows has to account for the whole imbalance. A
/// payload that legitimately repeats its last statement leaves the balance
/// untouched, so nothing fires.
fn repair_duplicate_suffix(
    ops: &mut [Op],
    group: &Group,
    file_lines: &[&str],
    delta: Balance,
) -> Option<String> {
    let body = payload(ops, group);
    let limit = body.len().min(file_lines.len().saturating_sub(group.end_line));
    let mut best = 0;
    for count in 1..=limit {
        let below = &file_lines[group.end_line..group.end_line + count];
        let tail = &body[body.len() - count..];
        if tail.iter().zip(below).all(|(row, line)| row == line)
            && compute_balance(tail) == delta
        {
            best = count;
        }
    }
    if best == 0 {
        return None;
    }

    let body = payload_mut(ops, group);
    body.truncate(body.len() - best);

    Some(format!(
        "Auto-repaired a delimiter-balance mismatch in the replacement at line {}: dropped \
         {best} duplicated trailing payload line(s) already present below the range. Issue \
         the payload as the final desired content only, and never restate or omit a closing \
         bracket bordering the range.",
        group.start_line
    ))
}

/// The mirror of [`repair_duplicate_suffix`], for lines above the range.
///
/// Only tried when the suffix rule found nothing, so a payload matching on both
/// ends is treated as a trailing duplication.
fn repair_duplicate_prefix(
    ops: &mut [Op],
    group: &Group,
    file_lines: &[&str],
    delta: Balance,
) -> Option<String> {
    let body = payload(ops, group);
    let limit = body.len().min(group.start_line - 1);
    let mut best = 0;
    for count in 1..=limit {
        let above = &file_lines[group.start_line - 1 - count..group.start_line - 1];
        if body[..count].iter().zip(above).all(|(row, line)| row == line)
            && compute_balance(&body[..count]) == delta
        {
            best = count;
        }
    }
    if best == 0 {
        return None;
    }

    payload_mut(ops, group).drain(..best);

    Some(format!(
        "Auto-repaired a delimiter-balance mismatch in the replacement at line {}: dropped \
         {best} duplicated leading payload line(s) already present above the range. Issue \
         the payload as the final desired content only, and never restate or omit a closing \
         bracket bordering the range.",
        group.start_line
    ))
}

/// The whitespace a line starts with.
fn indent_of(line: &str) -> &str {
    let end = line
        .find(|ch: char| ch != ' ' && ch != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// True when `inner` is indented strictly deeper than `outer`.
fn is_deeper(inner: &str, outer: &str) -> bool {
    inner.len() > outer.len() && inner.starts_with(outer)
}

/// Restore a base indent the payload dropped uniformly.
///
/// A model rewriting a nested block sometimes returns it flush left, having
/// mentally extracted it from its surroundings. Applied literally the body
/// escapes the brace that still encloses it.
///
/// The trigger is narrow on purpose, because "add some indentation" is exactly
/// the kind of helpfulness that ruins a deliberate dedent. It requires that the
/// replacement be the same size as the range, that the line above the range
/// opens a block, that the original first line was inside that block, that the
/// payload's first line is not, and finally that a strict majority of payload
/// rows are unchanged copies of their source rows agreeing on one shift. That
/// last condition is the real evidence: the rows the model did not touch tell
/// us what indentation it dropped.
fn repair_indentation(ops: &mut [Op], group: &Group, file_lines: &[&str]) -> bool {
    let body = payload(ops, group);
    let range = range_lines(group, file_lines);
    if body.is_empty() || body.len() != range.len() {
        return false;
    }

    let preceding = if group.start_line >= 2 {
        file_lines.get(group.start_line - 2).copied().unwrap_or("")
    } else {
        ""
    };
    if !preceding.trim_end().ends_with('{') {
        return false;
    }

    let preceding_indent = indent_of(preceding);
    let first_source = range.first().copied().unwrap_or("");
    if !is_deeper(indent_of(first_source), preceding_indent) {
        return false;
    }
    if is_deeper(indent_of(&body[0]), preceding_indent) {
        return false;
    }

    // Infer the shift from rows the model left alone. A row it rewrote says
    // nothing about the indentation it dropped.
    let mut shift: Option<String> = None;
    let mut matches = 0;
    for (offset, row) in body.iter().enumerate() {
        let source = range[offset];
        if source.trim().is_empty() || source.trim_start() != row.trim_start() {
            continue;
        }
        let source_indent = indent_of(source);
        let row_indent = indent_of(row);
        if !source_indent.ends_with(row_indent) {
            return false;
        }
        let candidate = &source_indent[..source_indent.len() - row_indent.len()];
        match &shift {
            Some(existing) if existing != candidate => return false,
            _ => shift = Some(candidate.to_string()),
        }
        matches += 1;
    }

    let Some(shift) = shift.filter(|shift| !shift.is_empty()) else {
        return false;
    };
    if matches < 2 || matches * 2 <= body.len() {
        return false;
    }

    for row in payload_mut(ops, group) {
        if !row.trim().is_empty() {
            row.insert_str(0, &shift);
        }
    }
    true
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod repair_tests;
