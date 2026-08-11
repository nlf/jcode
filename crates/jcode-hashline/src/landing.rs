//! Landing an `insert after N:` where its indentation says it belongs.
//!
//! Ported from the after-insert landing correction in oh-my-pi's `apply.ts`.
//!
//! The body rows of an insertion carry an implicit claim: their leading
//! indentation says how deep the author expects the new lines to sit. When that
//! claim is shallower than the anchor line itself, the hunk is inserting a
//! sibling of some enclosing construct while anchored inside it. The usual
//! shape is anchoring on the last statement of a block and writing the body at
//! the parent's depth:
//!
//! ```text
//! function f() {          PUT >3:
//!     if (x) {            +    c();
//!         a();
//!     }                   the body is a sibling of `if`, but line 3 is inside it
//!     b();
//! }
//! ```
//!
//! Applied literally, `c();` lands inside the `if` it was written to follow.
//! Sliding the landing point forward across the closing lines below the anchor
//! puts it where its indentation names.
//!
//! # Why this is not "fix the indentation instead"
//!
//! The alternative reading is that the landing is right and the body is
//! under-indented, and re-indenting it would be the fix. That is the more
//! destructive guess: indentation is what the author actually wrote, whereas
//! the anchor is arithmetic they did in their head. Trusting the text over the
//! arithmetic is the same principle the repair layer runs on.
//!
//! # Conservative in five specific ways
//!
//! The shift fires only when the body and anchor indentation are comparable,
//! crosses only pure closing-delimiter lines, stops as soon as depth returns to
//! the body's claim, is abandoned when any other hunk targets a line it would
//! cross, and always warns. Each of those is a case where the arithmetic and
//! the indentation disagree and there is no third thing to break the tie.

use crate::parser::{Anchor, Op};
use crate::repair::is_structural_closer;

/// Slide mis-anchored insertions to the depth their body claims.
///
/// Returns one warning per hunk moved. Ops that are not after-anchor
/// insertions, and hunks whose body makes no depth claim, pass through
/// untouched.
pub fn repair_landings(ops: &mut [Op], file_lines: &[&str]) -> Vec<String> {
    // Lines any hunk explicitly targets. A shift never crosses one: if another
    // hunk is rewriting or deleting a closer, that closer's meaning after the
    // patch is not what this file shows, and sliding past it would be reasoning
    // about a line that is on its way out.
    let targeted = targeted_lines(ops);

    let mut warnings = Vec::new();
    for op in ops.iter_mut() {
        let Op::Put {
            anchor: Anchor::After(anchor),
            body,
        } = op
        else {
            continue;
        };
        let anchor_line = *anchor;
        let Some(target) = body_target_indent(body) else {
            continue;
        };
        let Some((landing, crossed)) =
            outward_landing(anchor_line, &target, file_lines, &targeted)
        else {
            continue;
        };

        *anchor = landing;
        warnings.push(format!(
            "Moved an insertion past {crossed} closing line(s) to after line {landing}: it was \
             anchored on line {anchor_line}, inside a block, but its indentation places it \
             outside. Anchor an insertion on the line it should directly follow, and indent the \
             body to the depth it should end up at."
        ));
    }
    warnings
}

/// Every line an op names, so a shift can refuse to cross one.
fn targeted_lines(ops: &[Op]) -> Vec<usize> {
    let mut lines = Vec::new();
    for op in ops {
        match op {
            Op::Cut { start, end } => lines.extend(*start..=*end),
            Op::Put { anchor, .. } => match anchor {
                Anchor::Range { start, end } => lines.extend(*start..=*end),
                Anchor::Before(line) | Anchor::After(line) | Anchor::Block(line) => {
                    lines.push(*line);
                }
                Anchor::Bof | Anchor::Eof => {}
            },
            Op::Rem | Op::Mv { .. } => {}
        }
    }
    lines
}

/// Where an insertion anchored on `anchor` should land, given its body depth.
///
/// The last closing line in the run below the anchor whose indentation still
/// covers `target`. `None` leaves the landing where it was.
fn outward_landing(
    anchor: usize,
    target: &str,
    file_lines: &[&str],
    targeted: &[usize],
) -> Option<(usize, usize)> {
    let anchor_text = file_lines.get(anchor.checked_sub(1)?)?;
    if !has_content(anchor_text) {
        return None;
    }
    // The shift exists for a body shallower than its anchor. Equal depth is
    // already right, and deeper belongs to the inward correction that only
    // block-lowered insertions need.
    if !is_deeper(indent_of(anchor_text), target) {
        return None;
    }

    let mut landing = anchor;
    let mut crossed = 0;
    for line in (anchor + 1)..=file_lines.len() {
        let text = file_lines.get(line - 1).copied().unwrap_or("");
        // Look past blank lines. A blank is never itself a landing, and this
        // needs no guard to ensure it: the landing only advances on a closer,
        // so a blank either sits mid-run and is overwritten by the closer after
        // it, or ends the run without having moved anything. omp writes an
        // explicit "never land after a blank" here; a sweep over both spellings
        // across blanks before, after and between crossed closers, and at end
        // of file, produced identical results in every case.
        if !has_content(text) {
            continue;
        }
        // Content is never crossed. This is the rule that keeps the whole
        // correction honest, and it is why an indentation-only language is
        // unaffected: with no closing lines there is nothing crossable, so a
        // Python body always stays exactly where it was anchored.
        if !is_structural_closer(text) {
            break;
        }
        let indent = indent_of(text);
        // A closer shallower than the body would mean escaping further than the
        // indentation asks.
        if !indent.starts_with(target) {
            break;
        }
        if targeted.contains(&line) {
            return None;
        }
        landing = line;
        crossed += 1;
        // Depth has returned to the body's own level; anything beyond would
        // leave the scope it named.
        if indent.len() == target.len() {
            break;
        }
    }
    (landing != anchor).then_some((landing, crossed))
}

/// The depth an insertion body claims: its shallowest non-blank row.
///
/// `None` when no claim can be read, which is the answer for an empty or
/// all-blank body, a body of pure closers (which rebalances delimiters rather
/// than living at a depth), and rows whose indentation cannot be compared
/// because one uses tabs where another uses spaces.
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

fn indent_of(line: &str) -> &str {
    let end = line
        .find(|ch: char| ch != ' ' && ch != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

fn is_deeper(inner: &str, outer: &str) -> bool {
    inner.len() > outer.len() && inner.starts_with(outer)
}

fn has_content(line: &str) -> bool {
    !line.trim().is_empty()
}

#[cfg(test)]
#[path = "landing_tests.rs"]
mod landing_tests;
