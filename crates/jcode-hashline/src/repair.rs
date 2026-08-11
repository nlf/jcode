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

use std::collections::{BTreeMap, BTreeSet};

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

    fn negated(self) -> Self {
        Self {
            paren: -self.paren,
            bracket: -self.bracket,
            brace: -self.brace,
        }
    }

    /// Whether this balance supplies at least what `target` asks for.
    ///
    /// A component asking for nothing is satisfied by anything, which is why
    /// every caller must also check that the requirement is not all-zero. A
    /// vacuous cover is how a rule ends up "justifying" a rewrite with no
    /// evidence at all.
    fn covers(self, target: Self) -> bool {
        fn component(candidate: i32, target: i32) -> bool {
            if target == 0 {
                return true;
            }
            candidate.signum() == target.signum() && candidate.abs() >= target.abs()
        }
        component(self.paren, target.paren)
            && component(self.bracket, target.bracket)
            && component(self.brace, target.brace)
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
    /// A closer spare was available but not applied, because this pass was the
    /// authored one. The caller re-runs with spares enabled only when the
    /// authored result turns out not to parse.
    pub spares_proposed: bool,
    /// A spare was nominated by the arithmetic but the payload did not say
    /// which side of the spared closer it belongs on. See [`Ambiguity`].
    pub ambiguity: Option<Ambiguity>,
}

/// A spare the arithmetic nominated but could not place.
///
/// Sparing a closer decides where the payload lands relative to it, and both
/// answers produce a file that parses. The syntax veto therefore cannot
/// arbitrate: it would accept whichever was tried first. When the payload's own
/// indentation does not settle it, the repair is abandoned and this is
/// reported, so the model is told what was ambiguous rather than having a coin
/// flip applied to its file.
#[derive(Debug, Clone)]
pub struct Ambiguity {
    pub message: String,
}

/// Repair the boundaries of every replacement in `ops`, in place.
///
/// Only `PUT start=end:` operations are considered. An insertion has no range
/// for its payload to overflow, and a `CUT` has no payload at all.
///
/// `apply_spares` enables the rules that keep a deleted closing bracket on the
/// grounds that it closes a block. Those are never safe on delimiter counting
/// alone, so the caller runs this twice: once without, and again with only if
/// the first result fails to parse. See [`repair_with_syntax_veto`].
pub fn repair_boundaries_with(
    ops: &mut [Op],
    file_lines: &[&str],
    apply_spares: bool,
) -> RepairOutcome {
    let mut outcome = RepairOutcome::default();
    // Only ranges that actually name lines in this file become groups.
    //
    // Every rule below indexes `file_lines` around a group's boundaries
    // directly, on the assumption that those boundaries exist. Nothing
    // upstream guarantees it: the parser accepts any line number a model
    // writes, and the applier is what rejects an out-of-range one, which
    // happens *after* this runs. So `PUT 99=99:` on a three-line file used to
    // panic here rather than reaching the applier's error message.
    //
    // Filtering rather than validating is deliberate. An impossible range is
    // not a repair problem, and refusing it here would mean inventing a second
    // error for something the applier already reports well. Skipping it leaves
    // that message intact and simply declines to guess at boundaries that do
    // not exist.
    let groups: Vec<Group> = ops
        .iter()
        .enumerate()
        .filter_map(|(op_index, op)| match op {
            Op::Put {
                anchor: Anchor::Range { start, end },
                ..
            } if *start >= 1 && *start <= *end && *end <= file_lines.len() => Some(Group {
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

    // The textual rules first. They are complete on their own evidence, so
    // they run in both passes and are unaffected by whether spares are on.
    let mut unrepaired: Vec<&Group> = Vec::new();
    for group in &groups {
        match repair_group(ops, group, file_lines) {
            Some(warning) => outcome.warnings.push(warning),
            None => unrepaired.push(group),
        }
    }

    // Then the closer spares, over whatever the textual rules left alone. They
    // need the projected state of the whole patch, so they cannot run inside
    // the loop above: whether this hunk may keep a closer depends on what the
    // other hunks delete and insert.
    let projection = Projection::build(ops, &groups, file_lines);
    for group in unrepaired {
        if let Some(spare) = find_suffix_closer_spare(ops, group, file_lines, &projection) {
            outcome.spares_proposed = true;
            if !apply_spares {
                continue;
            }
            // Sparing a trailing closer re-inserts it *after* the payload,
            // which claims the payload lives inside the block that closer
            // terminates. Both readings parse, so the veto cannot choose:
            // the payload has to say. It does so by carrying the unmatched
            // opener itself, or by sitting deeper than the closer. At the
            // closer's own depth the payload is a sibling of the block and
            // belongs after it, so guessing "inside" silently moves code into
            // a scope the model did not write it for.
            let kept_indent = indent_of(
                file_lines
                    .get(spare.keep_from - 1)
                    .copied()
                    .unwrap_or_default(),
            );
            let body = payload(ops, group);
            let payload_opens = compute_balance(body).covers(spare.balance.negated());
            let claims_inside = body_target_indent(body)
                .is_some_and(|indent| is_deeper(&indent, kept_indent));
            if !payload_opens && !claims_inside {
                outcome.ambiguity = Some(Ambiguity {
                    message: format!(
                        "Ambiguous edit at line {}: the range through line {} deletes {} \
                         closing line(s) the payload never restates, but the payload's \
                         indentation does not say whether it belongs inside that block or \
                         after it. Re-issue the range so the payload is the final desired \
                         content for exactly the lines it replaces, restating any closing \
                         bracket that should remain.",
                        group.start_line, group.end_line, spare.count
                    ),
                });
                continue;
            }
            narrow_range_end(ops, group, spare.keep_from);
            outcome.warnings.push(format!(
                "Auto-repaired a dropped closing line in the replacement at line {}: kept \
                 {} line(s) the range deleted but the payload never restated, because \
                 removing them would leave the enclosing block unterminated. Issue the \
                 payload as the final desired content for the whole range, including any \
                 closing bracket it ends with.",
                group.start_line, spare.count
            ));
            continue;
        }

        let Some(spare) = find_prefix_closer_spare(ops, group, file_lines, &projection) else {
            continue;
        };
        outcome.spares_proposed = true;
        if !apply_spares {
            continue;
        }
        // The mirror judgement. Sparing a *leading* closer puts the payload
        // after it, which claims the payload sits outside the block that closer
        // ends. At or above the closer's depth that is a sibling, which is the
        // claim being made; deeper or incomparable would put the payload inside
        // a block the range just closed, so refuse rather than guess.
        let closer_indent = indent_of(
            file_lines
                .get(group.start_line - 1)
                .copied()
                .unwrap_or_default(),
        );
        let Some(payload_indent) = body_target_indent(payload(ops, group)) else {
            continue;
        };
        if !closer_indent.starts_with(&payload_indent) {
            outcome.ambiguity = Some(Ambiguity {
                message: format!(
                    "Ambiguous edit at line {}: the range starts on {} closing line(s) the \
                     payload never restates, but the payload is indented deeper than they \
                     are, so it is unclear whether it belongs before or after them. \
                     Re-issue the range starting below the closing bracket, or restate it \
                     in the payload.",
                    group.start_line, spare.count
                ),
            });
            continue;
        }
        narrow_range_start(ops, group, group.start_line + spare.count);
        outcome.warnings.push(format!(
            "Auto-repaired a dropped closing line in the replacement at line {}: kept {} \
             leading line(s) the range deleted but the payload never restated, because \
             removing them would leave the block above unterminated; the payload lands \
             after them. Issue the payload as the final desired content for the whole \
             range, including any closing bracket it begins with.",
            group.start_line, spare.count
        ));
    }

    outcome
}

/// Repair boundaries with no syntax veto available.
///
/// Closer spares never fire here, because nothing can vouch for them. Use
/// [`repair_with_syntax_veto`] wherever a parse check exists.
pub fn repair_boundaries(ops: &mut [Op], file_lines: &[&str]) -> RepairOutcome {
    repair_boundaries_with(ops, file_lines, false)
}

/// Repair boundaries, letting a syntax check decide whether a closer may be
/// spared.
///
/// `parses` answers "does this text parse, as far as you can tell", and must
/// return false when it cannot tell. See `jcode_ast::parses_cleanly`.
/// `materialize` renders a candidate result so it can be offered for checking.
///
/// # Why a repair has to be shown to work, not argued
///
/// Keeping a deleted `}` is the one repair that cannot be justified textually.
/// Every other rule here matches lines against text already in the file; this
/// one asserts that a bracket is *syntax*, and delimiter counting cannot tell a
/// block closer from a brace in a regex, a string, or a sentence of prose.
///
/// So the arithmetic only ever nominates a candidate. The order below is what
/// makes it safe:
///
/// 1. Apply what the author wrote. **If that parses, return it untouched** and
///    no spare runs at all. A file that still parses is not missing a closer,
///    whatever the counting says.
/// 2. Only if it does not parse, try again with spares applied, and **keep that
///    result only if it parses**.
/// 3. Otherwise return the authored result unchanged.
///
/// The consequence worth stating: in a language nothing can parse, `parses`
/// returns false for everything, step 2's result is never accepted, and the
/// authored edit stands. Markdown prose full of braces is therefore never
/// "balanced" by this layer, which is exactly the corruption it must not cause.
pub fn repair_with_syntax_veto(
    ops: &mut [Op],
    file_lines: &[&str],
    parses: impl Fn(&str) -> bool,
    materialize: impl Fn(&[Op]) -> Option<String>,
) -> RepairOutcome {
    let authored_ops = ops.to_vec();
    let authored = repair_boundaries_with(ops, file_lines, false);
    if !authored.spares_proposed {
        return authored;
    }

    // The author's edit keeps the file parsing, so no delimiter heuristic may
    // second-guess its boundaries.
    if let Some(text) = materialize(ops)
        && parses(&text)
    {
        return authored;
    }

    // Re-run from the authored ops rather than continuing from the first pass:
    // this is a different reading of the same input, not an increment on it.
    let mut spared_ops = authored_ops;
    let spared = repair_boundaries_with(&mut spared_ops, file_lines, true);
    if let Some(text) = materialize(&spared_ops)
        && parses(&text)
    {
        ops.clone_from_slice(&spared_ops);
        return spared;
    }

    // A spare the payload could not place. The authored edit stands, but the
    // caller is told what was ambiguous, because a spare was the only reading
    // that would have made this file parse and the model is better placed than
    // this layer to say which side it meant.
    //
    // Only when the file parsed before the edit, mirroring omp. Otherwise the
    // file was already broken and this edit cannot be blamed for it, so
    // reporting an ambiguity would be noise attached to the wrong change. The
    // baseline is joined back from `file_lines`, which is how the caller split
    // it, so this is the original text rather than an approximation of it.
    if spared.ambiguity.is_some() && parses(&file_lines.join("\n")) {
        let mut authored = authored;
        authored.ambiguity = spared.ambiguity;
        return authored;
    }

    authored
}

/// Keep the tail of a replacement's range instead of deleting it.
///
/// The payload stays where it is and the range shrinks, so the lines from
/// `keep_from` onward survive untouched below the new content.
fn narrow_range_end(ops: &mut [Op], group: &Group, keep_from: usize) {
    if let Op::Put {
        anchor: Anchor::Range { end, .. },
        ..
    } = &mut ops[group.op_index]
    {
        *end = keep_from - 1;
    }
}

/// Keep the head of a replacement's range instead of deleting it.
///
/// The mirror of [`narrow_range_end`]: the lines before `payload_at` survive
/// untouched above the new content, which now starts lower down.
fn narrow_range_start(ops: &mut [Op], group: &Group, payload_at: usize) {
    if let Op::Put {
        anchor: Anchor::Range { start, .. },
        ..
    } = &mut ops[group.op_index]
    {
        *start = payload_at;
    }
}

/// A closing run the range would delete and the payload does not put back.
struct CloserSpare {
    /// First line of the run to keep, 1-indexed.
    keep_from: usize,
    count: usize,
    /// Net delimiters the kept run carries, used to ask whether the payload
    /// opens them itself.
    balance: Balance,
}

/// What the whole patch does to the file, as far as any one hunk can see.
///
/// A spare cannot be judged from its own hunk. If another hunk deletes the
/// opener this closer matches, the closer has to go too; if another hunk
/// inserts the same closer just below, keeping this one duplicates it. So the
/// rules below need the patch as a whole, projected as though every hunk
/// applied as authored.
struct Projection {
    deleted: BTreeSet<usize>,
    /// Lines inserted at a given anchor line, in patch order.
    inserted: BTreeMap<usize, Vec<String>>,
}

impl Projection {
    fn build(ops: &[Op], groups: &[Group], _file_lines: &[&str]) -> Self {
        let mut deleted = BTreeSet::new();
        let mut inserted: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for op in ops {
            match op {
                Op::Cut { start, end } => deleted.extend(*start..=*end),
                Op::Put { anchor, body } => match anchor {
                    Anchor::Range { start, end } => {
                        deleted.extend(*start..=*end);
                        inserted.entry(*start).or_default().extend(body.clone());
                    }
                    Anchor::Before(line) | Anchor::After(line) => {
                        inserted.entry(*line).or_default().extend(body.clone());
                    }
                    Anchor::Bof => inserted.entry(1).or_default().extend(body.clone()),
                    Anchor::Eof => {}
                },
                Op::Rem | Op::Mv { .. } => {}
            }
        }
        let _ = groups;
        Self { deleted, inserted }
    }

    /// Net brackets left open above `line` once the patch has applied.
    fn balance_above(&self, line: usize, file_lines: &[&str]) -> Balance {
        let mut projected: Vec<String> = Vec::new();
        for candidate in 1..line {
            if let Some(rows) = self.inserted.get(&candidate) {
                projected.extend(rows.clone());
            }
            if !self.deleted.contains(&candidate) {
                projected.push(file_lines.get(candidate - 1).copied().unwrap_or("").to_string());
            }
        }
        compute_balance(&projected)
    }

    /// The closing lines that will sit immediately below `line`.
    fn closers_below(&self, line: usize, file_lines: &[&str]) -> Vec<String> {
        let mut below = Vec::new();
        for candidate in (line + 1)..=file_lines.len() {
            if let Some(rows) = self.inserted.get(&candidate) {
                for row in rows {
                    if !is_structural_closer(row) {
                        return below;
                    }
                    below.push(row.clone());
                }
            }
            if self.deleted.contains(&candidate) {
                continue;
            }
            let text = file_lines.get(candidate - 1).copied().unwrap_or("");
            if !is_structural_closer(text) {
                return below;
            }
            below.push(text.to_string());
        }
        below
    }
}

/// A line that is nothing but closing brackets, optionally with one separator.
///
/// Deliberately excludes the JSX form omp also recognises elsewhere. A `</div>`
/// carries no brackets at all, so the balance arithmetic every spare rests on
/// would be satisfied vacuously and the rule could keep arbitrary lines.
fn is_structural_closer(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let body = trimmed
        .strip_suffix([';', ','])
        .unwrap_or(trimmed)
        .trim_end();
    !body.is_empty() && body.chars().all(|ch| matches!(ch, ')' | ']' | '}'))
}

/// Does the range end in closers the payload drops and nothing replaces?
///
/// The reading being tested is "the model wrote the block's new body but forgot
/// to restate the bracket that closes it". Every condition below is an attempt
/// to rule that reading out, because the alternative reading, that the model
/// meant to delete the closer, is equally available and far more destructive to
/// get wrong.
fn find_suffix_closer_spare(
    ops: &[Op],
    group: &Group,
    file_lines: &[&str],
    projection: &Projection,
) -> Option<CloserSpare> {
    let body = payload(ops, group);
    if body.is_empty() {
        return None;
    }

    // How many lines at the end of the range are pure closers.
    let mut suffix_length = 0;
    while suffix_length < group.end_line - group.start_line + 1 {
        let line = group.end_line - suffix_length;
        if !is_structural_closer(file_lines.get(line - 1).copied().unwrap_or("")) {
            break;
        }
        suffix_length += 1;
    }
    if suffix_length == 0 {
        return None;
    }

    let suffix_start = group.end_line - suffix_length + 1;
    let suffix: Vec<&str> = (suffix_start..=group.end_line)
        .map(|line| file_lines.get(line - 1).copied().unwrap_or(""))
        .collect();

    // Lines the payload already ends with are not missing, and lines an
    // adjacent hunk puts back below are already covered. Both would be
    // duplicated rather than restored.
    //
    // These two counts are computed but, in this port, never change the
    // outcome: the delta and opener checks below refuse everything they would
    // have refused, and a search over several thousand generated patches found
    // no input where removing either altered the result. omp needs them
    // because their spare also has a prefix form and a landing-shift rule that
    // can reach this point by other routes. They are kept because the day one
    // of those lands, this rule silently stops being conservative without
    // them, and a comment is cheaper than rediscovering that.
    let keep_start = payload_restates_suffix_head(body, &suffix);
    let covered = projected_covers_suffix_tail(&suffix, group, file_lines, projection);
    let keep_end = suffix_length.checked_sub(covered)?;
    if keep_start >= keep_end {
        return None;
    }

    let kept: Vec<&str> = suffix[keep_start..keep_end].to_vec();
    let kept_balance = compute_balance(&kept);
    let needed = kept_balance.negated();

    // The payload must actually be short by these brackets. This subsumes the
    // "a run that closes nothing" case, since a zero balance asks for nothing
    // and the opener check below then has nothing to find.
    let delta = compute_balance(body).minus(compute_balance(range_lines(group, file_lines)));
    if !delta.covers(needed) {
        return None;
    }

    // If a contiguous run of deleted lines just above took the matching opener
    // with it, the closer must go too. Keeping it would close a block that no
    // longer opens.
    if deleted_prefix_balance(group, file_lines, projection).covers(needed) {
        return None;
    }

    // And there must be an unclosed opener above it in the projected file for
    // these brackets to close.
    //
    // A zero requirement cannot reach here. `covers` is vacuously true for a
    // component asking for nothing, so an all-zero `needed` would pass this
    // check, which is the classic way a rule "justifies" a rewrite with no
    // evidence. It is excluded earlier instead: a kept run with zero balance
    // means the payload is not short anything, and the `delta.covers` check
    // above has already refused it. Instrumenting this branch over 8,000
    // generated patches fired it zero times, so an explicit guard here would
    // be unreachable code rather than a safeguard.
    let above = projection.balance_above(suffix_start, file_lines);
    let covered_below = compute_balance(&suffix[keep_end..]);
    if !above.plus(covered_below).covers(needed) {
        return None;
    }

    Some(CloserSpare {
        keep_from: suffix_start + keep_start,
        count: keep_end - keep_start,
        balance: kept_balance,
    })
}

/// Does the range *start* on closers the payload drops and nothing replaces?
///
/// The mirror mistake: the range began one line early, on the `}` that ends the
/// construct above it. The payload is the new body of what follows, so applying
/// it literally tears the closing bracket off the previous block.
///
/// Same burden of proof as the suffix form, plus one condition it does not
/// need: the spared closers must have a dangling opener above them in the
/// projected file. The payload cannot supply it, because the payload lands
/// *below* the closers either way.
fn find_prefix_closer_spare(
    ops: &[Op],
    group: &Group,
    file_lines: &[&str],
    projection: &Projection,
) -> Option<CloserSpare> {
    let body = payload(ops, group);
    if body.is_empty() {
        return None;
    }
    // A payload opening with a closer is restating the boundary itself. That is
    // an echo, with a different reading and its own rule; leave it alone.
    if is_structural_closer(&body[0]) {
        return None;
    }

    let range_length = group.end_line - group.start_line + 1;
    let mut prefix_length = 0;
    while prefix_length < range_length {
        let line = group.start_line + prefix_length;
        if !is_structural_closer(file_lines.get(line - 1).copied().unwrap_or("")) {
            break;
        }
        prefix_length += 1;
    }
    // A range that is *entirely* closers is not a boundary slip: there would be
    // no body for the payload to be replacing.
    if prefix_length == 0 || prefix_length >= range_length {
        return None;
    }

    let prefix: Vec<&str> = (group.start_line..group.start_line + prefix_length)
        .map(|line| file_lines.get(line - 1).copied().unwrap_or(""))
        .collect();
    // No zero-balance guard here, unlike the arithmetic elsewhere in this file.
    // Every line in `prefix` passed `is_structural_closer`, which requires at
    // least one of `)]}` and permits nothing else, so the run's balance is
    // strictly negative in some component and `needed` can never be the empty
    // requirement that `covers` satisfies vacuously. A guard was written here
    // first and then removed: a sweep over the rule found no input where it
    // changed the answer, and an unreachable check reads as a safeguard.
    let balance = compute_balance(&prefix);
    let needed = balance.negated();

    // The payload must actually be short by these brackets.
    let delta = compute_balance(body).minus(compute_balance(range_lines(group, file_lines)));
    if !delta.covers(needed) {
        return None;
    }

    // If the deleted run just above took the matching opener with it, the
    // closer must go too.
    if deleted_prefix_balance(group, file_lines, projection).covers(needed) {
        return None;
    }

    // And an opener must actually survive above for these to close.
    if !projection.balance_above(group.start_line, file_lines).covers(needed) {
        return None;
    }

    Some(CloserSpare {
        keep_from: group.start_line,
        count: prefix_length,
        balance,
    })
}

/// The depth an insertion body claims, as its shallowest non-blank row.
///
/// `None` when no claim can be read: an empty or all-blank body, a body of pure
/// closers (which re-balances delimiters rather than living anywhere), or rows
/// whose indentation styles cannot be compared because one mixes tabs where
/// another uses spaces.
fn body_target_indent(rows: &[String]) -> Option<String> {
    let non_blank: Vec<&String> = rows.iter().filter(|row| has_content(row)).collect();
    if non_blank.is_empty() {
        return None;
    }
    if non_blank.iter().all(|row| is_structural_closer(row)) {
        return None;
    }
    let mut target = indent_of(non_blank[0]).to_string();
    for row in &non_blank {
        let indent = indent_of(row);
        if indent.starts_with(&target) {
            continue;
        }
        if target.starts_with(indent) {
            target = indent.to_string();
        } else {
            return None;
        }
    }
    Some(target)
}

/// How many of the suffix's first lines the payload already ends with.
fn payload_restates_suffix_head(body: &[String], suffix: &[&str]) -> usize {
    let max = body.len().min(suffix.len());
    for count in (1..=max).rev() {
        if body[body.len() - count..]
            .iter()
            .zip(suffix)
            .all(|(row, line)| row == line)
        {
            return count;
        }
    }
    0
}

/// How many of the suffix's last lines will already exist below the range.
fn projected_covers_suffix_tail(
    suffix: &[&str],
    group: &Group,
    file_lines: &[&str],
    projection: &Projection,
) -> usize {
    let below = projection.closers_below(group.end_line, file_lines);
    let max = below.len().min(suffix.len());
    for count in (1..=max).rev() {
        if below[..count]
            .iter()
            .zip(&suffix[suffix.len() - count..])
            .all(|(row, line)| row == line)
        {
            return count;
        }
    }
    0
}

/// Net brackets removed by the run of deleted lines immediately above a range.
///
/// Contiguity is the point: a deleted opener two lines up with a surviving line
/// between them is not what this range's closer was matching.
fn deleted_prefix_balance(group: &Group, file_lines: &[&str], projection: &Projection) -> Balance {
    let mut deleted: Vec<String> = Vec::new();
    let mut inserted: Vec<String> = Vec::new();
    let mut line = group.start_line;
    while line > 1 && projection.deleted.contains(&(line - 1)) {
        line -= 1;
        deleted.insert(0, file_lines.get(line - 1).copied().unwrap_or("").to_string());
        if let Some(rows) = projection.inserted.get(&line) {
            let mut rows = rows.clone();
            rows.extend(inserted);
            inserted = rows;
        }
    }
    compute_balance(&deleted).minus(compute_balance(&inserted))
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
