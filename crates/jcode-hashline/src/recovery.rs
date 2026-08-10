//! Rescuing an edit whose tag went stale, by proving its anchors still name
//! the same lines in the file as it is now.
//!
//! Ported from oh-my-pi's `recovery.ts`, with their
//! `recovery-session-chain.test.ts` as the specification.
//!
//! # This is not a three-way merge, despite how the problem is usually stated
//!
//! The obvious design for "the file changed under an edit" is to apply the edit
//! to the text it was authored against and merge that result onto current
//! content. omp built that, and **removed it** (their test records the removal
//! of the "direct replay fallback"). What replaced it is narrower and refuses
//! far more often, which is the point: a merge produces an answer for inputs
//! where no correct answer exists, and the wrong answer silently overwrites
//! someone else's work.
//!
//! What happens instead is a translation of coordinates. A line diff between
//! the tagged snapshot and the current file yields the lines that did not
//! change; every anchor must map through that set, with its surrounding context
//! agreeing, and every anchor must move by the *same* distance. When all of
//! that holds, the edit is replayed verbatim onto the current text, because it
//! has been proven to be describing the same place. When any of it fails,
//! recovery declines and the model is told to re-read.
//!
//! So the edit's payload is never merged, reconciled, or rewritten. It either
//! lands exactly where it was aimed, or it does not land.
//!
//! # Why an anchor is not enough on its own
//!
//! An unchanged line is weak evidence. `}` is unchanged in a thousand places,
//! and a file that duplicates a block has whole regions that map ambiguously.
//! So context is checked, and how strictly depends on how identifiable the
//! anchor line is: a line whose text is unique in both files needs one
//! neighbour that moved with it, while a line whose text repeats needs every
//! neighbour it has and is never accepted with none. That asymmetry is what
//! stops a stale replacement being relocated onto the *second* copy of a
//! duplicated block, which is the corruption this guard exists to prevent.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::apply::apply_ops;
use crate::parser::{Anchor, Op};
use crate::snapshots::SnapshotStore;

/// A cached snapshot matched the tag, and the file changed outside this
/// session.
pub const RECOVERY_EXTERNAL_WARNING: &str =
    "Recovered from a stale file hash using a previous read snapshot (file changed \
     externally between read and edit).";

/// An earlier edit in this session advanced the file past the tag being used.
pub const RECOVERY_SESSION_CHAIN_WARNING: &str =
    "Recovered from a stale file hash using an earlier in-session snapshot (a prior \
     edit in this session advanced the hash).";

/// Anchors were not merely stale but had moved, and were relocated.
///
/// Distinct from the two above because the model should look harder at this
/// one: the edit landed at a different line number than it named.
pub const RECOVERY_LINE_REMAP_WARNING: &str =
    "Recovered by remapping stale line anchors to unchanged current lines (file \
     changed since the tagged read). Verify the diff matches your intent.";

/// A drifted tag was tolerated because the edit did not depend on line numbers.
pub const HEADTAIL_DRIFT_WARNING: &str =
    "Applied the edit despite a stale snapshot tag (file changed since your read), \
     because inserting at the start or end of a file does not depend on its \
     content. Re-read if the drift was unexpected.";

/// A recovered edit, and what to tell the model about how it was rescued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub text: String,
    /// 1-indexed first changed line, relative to the **current** text rather
    /// than to the snapshot the edit was authored against.
    pub first_changed_line: Option<usize>,
    /// A `MV` destination, when the recovered patch also moved the file.
    pub move_dest: Option<String>,
    pub removed: bool,
    pub warnings: Vec<String>,
}

/// True when no operation depends on a line number.
///
/// Inserting at the head or tail of a file is position-stable: drift cannot
/// move "the beginning". Such an edit is applied to the current content with a
/// warning rather than being sent through recovery, which would refuse it for
/// want of any anchor to prove.
pub fn has_anchor_scoped_op(ops: &[Op]) -> bool {
    ops.iter().any(|op| match op {
        Op::Cut { .. } => true,
        Op::Put { anchor, .. } => !matches!(anchor, Anchor::Bof | Anchor::Eof),
        Op::Rem | Op::Mv { .. } => false,
    })
}

/// Try to rescue `ops` against a file that no longer hashes to `expected_tag`.
///
/// Returns `None` when the anchors cannot be proven, which is not an error
/// condition so much as a refusal: the caller then reports the stale tag and
/// the model re-reads.
pub fn try_recover(
    store: &SnapshotStore,
    path: &str,
    current_text: &str,
    expected_tag: &str,
    ops: &[Op],
) -> Option<Recovered> {
    // On a 16-bit tag collision this is the most recently recorded of the
    // colliding versions. Recovery does not try the others: the anchors still
    // have to be proven against whichever text it picks, so a wrong guess
    // fails closed rather than applying somewhere unintended.
    let snapshot = store.by_hash(path, expected_tag)?;

    // Which banner to use if nothing moved. The tag naming the newest thing we
    // recorded, while the file differs from it, means somebody outside this
    // session wrote the file. The tag naming an older retained version means
    // our own earlier edit advanced the head.
    //
    // Comparing tags is sufficient, including under a 16-bit collision, and
    // that is worth stating because the obvious worry is unfounded. History is
    // newest-first and `by_hash` returns the first match, so if the head
    // carries this tag then the head *is* what was resolved, colliders or not.
    // Comparing text as well was tried and proved unreachable.
    let head = store.head(path);
    let externally_changed = head.as_ref().is_some_and(|head| head.hash == snapshot.hash);
    let base_warning = if externally_changed {
        RECOVERY_EXTERNAL_WARNING
    } else {
        RECOVERY_SESSION_CHAIN_WARNING
    };

    let remapped = remap_ops(&snapshot.text, current_text, ops)?;
    let applied = apply_ops(current_text, &remapped.ops).ok()?;

    // A recovery that changes nothing is a failure, not a success. Reporting it
    // as a no-op would tell the model its edit was redundant, when what
    // actually happened is that we could not place it.
    if !applied.removed && applied.text == current_text && applied.move_dest.is_none() {
        return None;
    }

    let warning = if remapped.offset == 0 {
        base_warning
    } else {
        RECOVERY_LINE_REMAP_WARNING
    };

    Some(Recovered {
        text: applied.text,
        first_changed_line: applied.first_changed_line,
        move_dest: applied.move_dest,
        removed: applied.removed,
        warnings: vec![warning.to_string()],
    })
}

/// Operations rewritten into current-file coordinates.
struct Remapped {
    ops: Vec<Op>,
    /// The single distance every anchor moved by.
    offset: isize,
}

/// Map every line that is identical in both texts, from its old number to its
/// new one.
///
/// Lines inside a changed region are deliberately absent: an anchor landing in
/// one has no proof it still names the same thing, and recovery refuses rather
/// than guessing which side of the change it belonged to.
fn build_line_map(previous: &str, current: &str) -> BTreeMap<usize, usize> {
    let previous_lines: Vec<&str> = previous.split('\n').collect();
    let current_lines: Vec<&str> = current.split('\n').collect();
    let mut map = BTreeMap::new();
    for (old_index, new_index, len) in equal_runs(&previous_lines, &current_lines) {
        for offset in 0..len {
            // Stored 1-indexed, matching how anchors are written.
            map.insert(old_index + offset + 1, new_index + offset + 1);
        }
    }
    map
}

/// Runs of identical lines, as `(old_index, new_index, len)` with 0-indexed
/// starts.
///
/// This is the one diff primitive recovery needs. It is written here rather
/// than taken from a diff crate because the requirement is unusually strict:
/// **a run reported as equal is treated as proof**, so an algorithm that splits
/// or drops matches to produce a prettier diff turns an acceptable recovery
/// into a refused one. A longest-common-subsequence walk over line identity is
/// exactly the guarantee wanted and nothing more.
///
/// The quadratic table is bounded below; larger inputs fall back to matching
/// only the common head and tail, which is strictly less generous and therefore
/// safe in the direction that matters.
fn equal_runs(previous: &[&str], current: &[&str]) -> Vec<(usize, usize, usize)> {
    // A common prefix and suffix are trimmed first. They are the overwhelming
    // majority of any real edit, and removing them keeps the table small enough
    // that the bound below is rarely reached.
    let mut head = 0;
    while head < previous.len() && head < current.len() && previous[head] == current[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < previous.len() - head
        && tail < current.len() - head
        && previous[previous.len() - 1 - tail] == current[current.len() - 1 - tail]
    {
        tail += 1;
    }

    let old_mid = &previous[head..previous.len() - tail];
    let new_mid = &current[head..current.len() - tail];

    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    let push = |old: usize, new: usize, runs: &mut Vec<(usize, usize, usize)>| {
        // Extend the previous run when this pair continues it, so callers see
        // maximal runs rather than one entry per line.
        if let Some(last) = runs.last_mut()
            && last.0 + last.2 == old
            && last.1 + last.2 == new
        {
            last.2 += 1;
            return;
        }
        runs.push((old, new, 1));
    };

    for index in 0..head {
        push(index, index, &mut runs);
    }

    /// Above this many cells the table is not built. Chosen so the worst case
    /// stays in the low milliseconds; beyond it the head and tail alone are
    /// used, which can only refuse recoveries that a full diff would have
    /// allowed.
    const MAX_TABLE_CELLS: usize = 4_000_000;

    if !old_mid.is_empty()
        && !new_mid.is_empty()
        && old_mid.len().saturating_mul(new_mid.len()) <= MAX_TABLE_CELLS
    {
        for (old_offset, new_offset) in common_subsequence(old_mid, new_mid) {
            push(head + old_offset, head + new_offset, &mut runs);
        }
    }

    for index in 0..tail {
        push(
            previous.len() - tail + index,
            current.len() - tail + index,
            &mut runs,
        );
    }

    runs
}

/// Index pairs of a longest common subsequence of two line slices.
fn common_subsequence(previous: &[&str], current: &[&str]) -> Vec<(usize, usize)> {
    let rows = previous.len();
    let columns = current.len();
    // `lengths[i][j]` is the LCS length of `previous[i..]` and `current[j..]`.
    let mut lengths = vec![0usize; (rows + 1) * (columns + 1)];
    let at = |row: usize, column: usize| row * (columns + 1) + column;
    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            lengths[at(row, column)] = if previous[row] == current[column] {
                lengths[at(row + 1, column + 1)] + 1
            } else {
                lengths[at(row + 1, column)].max(lengths[at(row, column + 1)])
            };
        }
    }

    let mut pairs = Vec::new();
    let (mut row, mut column) = (0, 0);
    while row < rows && column < columns {
        if previous[row] == current[column] {
            pairs.push((row, column));
            row += 1;
            column += 1;
        } else if lengths[at(row + 1, column)] >= lengths[at(row, column + 1)] {
            row += 1;
        } else {
            column += 1;
        }
    }
    pairs
}

/// Every line an operation depends on being unchanged.
///
/// A range contributes **all** of its lines, not just its endpoints. The
/// interior of a replacement is content the model read and decided to replace;
/// if it changed underneath, the replacement is authored against something that
/// no longer exists, and the endpoints matching proves nothing about that.
fn anchor_lines(ops: &[Op]) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    for op in ops {
        match op {
            Op::Cut { start, end } => lines.extend(*start..=*end),
            Op::Put { anchor, .. } => match anchor {
                Anchor::Range { start, end } => lines.extend(*start..=*end),
                Anchor::Before(line) | Anchor::After(line) => {
                    lines.insert(*line);
                }
                // Head and tail do not name a line, so there is nothing to
                // prove and nothing to relocate.
                Anchor::Bof | Anchor::Eof => {}
            },
            Op::Rem | Op::Mv { .. } => {}
        }
    }
    lines
}

/// The nearest lines outside an anchor's contiguous run, on each side.
///
/// Computed per run rather than per anchor: the neighbours of a line in the
/// middle of a 200-line replacement are other anchors, which are equally
/// suspect and would make every large replacement unprovable. The lines just
/// outside the run are the only ones that carry independent information.
#[derive(Debug, Clone, Copy)]
struct Neighbours {
    before: Option<usize>,
    after: Option<usize>,
}

fn anchor_neighbours(
    anchors: &BTreeSet<usize>,
    line_count: usize,
) -> HashMap<usize, Neighbours> {
    let sorted: Vec<usize> = anchors.iter().copied().collect();
    let mut neighbours = HashMap::new();
    let mut index = 0;
    while index < sorted.len() {
        let mut last = index;
        while last + 1 < sorted.len() && sorted[last + 1] == sorted[last] + 1 {
            last += 1;
        }
        let start = sorted[index];
        let end = sorted[last];
        let shared = Neighbours {
            before: (start >= 2).then(|| start - 1),
            after: (end < line_count).then(|| end + 1),
        };
        for anchor in &sorted[index..=last] {
            neighbours.insert(*anchor, shared);
        }
        index = last + 1;
    }
    neighbours
}

/// Line texts occurring more than once, which is what makes an anchor
/// ambiguous.
fn duplicated_lines(lines: &[&str]) -> BTreeSet<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut duplicated = BTreeSet::new();
    for line in lines {
        if !seen.insert(line) {
            duplicated.insert((*line).to_string());
        }
    }
    duplicated
}

/// Prove every anchor still names the same place, then rewrite the operations
/// into current coordinates.
fn remap_ops(previous: &str, current: &str, ops: &[Op]) -> Option<Remapped> {
    let line_map = build_line_map(previous, current);
    let previous_lines: Vec<&str> = previous.split('\n').collect();
    let current_lines: Vec<&str> = current.split('\n').collect();

    let anchors = anchor_lines(ops);
    // The count includes the phantom trailing element a final newline produces,
    // so the last real line does have an `after` neighbour to be checked
    // against. Excluding it would refuse edits at the end of every file.
    let neighbours = anchor_neighbours(&anchors, previous_lines.len());

    let duplicated_previous = duplicated_lines(&previous_lines);
    let duplicated_current = duplicated_lines(&current_lines);

    for (&line, neighbours) in &neighbours {
        let mapped = *line_map.get(&line)?;

        let previous_text = previous_lines.get(line - 1)?;
        let current_text = current_lines.get(mapped - 1)?;
        // Ambiguity on *either* side is enough to demand the stricter check.
        // A line that was unique when read but now repeats can be relocated
        // onto the wrong copy just as easily as one that always repeated.
        let ambiguous = duplicated_previous.contains(*previous_text)
            || duplicated_current.contains(*current_text);

        let ok = if ambiguous {
            strict_context_holds(line, mapped, *neighbours, &line_map)
        } else {
            loose_context_holds(line, mapped, *neighbours, &line_map)
        };
        if !ok {
            return None;
        }
    }

    let mut offsets: Vec<isize> = Vec::new();
    let map_line = |line: usize, offsets: &mut Vec<isize>| -> Option<usize> {
        let mapped = *line_map.get(&line)?;
        offsets.push(mapped as isize - line as isize);
        Some(mapped)
    };

    let mut remapped = Vec::with_capacity(ops.len());
    for op in ops {
        let moved = match op {
            Op::Cut { start, end } => {
                let mapped_start = map_line(*start, &mut offsets)?;
                let mut mapped_end = mapped_start;
                for line in (*start + 1)..=*end {
                    mapped_end = map_line(line, &mut offsets)?;
                }
                Op::Cut {
                    start: mapped_start,
                    end: mapped_end,
                }
            }
            Op::Put { anchor, body } => {
                let anchor = match anchor {
                    Anchor::Range { start, end } => {
                        let mapped_start = map_line(*start, &mut offsets)?;
                        let mut mapped_end = mapped_start;
                        for line in (*start + 1)..=*end {
                            mapped_end = map_line(line, &mut offsets)?;
                        }
                        Anchor::Range {
                            start: mapped_start,
                            end: mapped_end,
                        }
                    }
                    Anchor::Before(line) => Anchor::Before(map_line(*line, &mut offsets)?),
                    Anchor::After(line) => Anchor::After(map_line(*line, &mut offsets)?),
                    Anchor::Bof => Anchor::Bof,
                    Anchor::Eof => Anchor::Eof,
                };
                Op::Put {
                    anchor,
                    body: body.clone(),
                }
            }
            Op::Rem => Op::Rem,
            Op::Mv { dest } => Op::Mv { dest: dest.clone() },
        };
        remapped.push(moved);
    }

    // Nothing was anchored, so nothing was proven. The caller handles
    // position-stable edits before reaching here; arriving with no offsets
    // means there was no evidence to gather.
    let first = *offsets.first()?;
    // One patch cannot straddle a changed region: if its anchors moved by
    // different distances, the region between them changed size, and an edit
    // authored across it is describing a layout that no longer exists.
    if offsets.iter().any(|offset| *offset != first) {
        return None;
    }

    Some(Remapped {
        ops: remapped,
        offset: first,
    })
}

/// For an anchor whose text is unique: one neighbour moving with it is enough.
fn loose_context_holds(
    line: usize,
    mapped: usize,
    neighbours: Neighbours,
    line_map: &BTreeMap<usize, usize>,
) -> bool {
    let offset = mapped as isize - line as isize;
    let follows = |neighbour: Option<usize>| {
        neighbour.is_some_and(|neighbour| {
            line_map.get(&neighbour).copied()
                == usize::try_from(neighbour as isize + offset).ok()
        })
    };
    follows(neighbours.after) || follows(neighbours.before)
}

/// For an anchor whose text repeats: every neighbour it has must move with it,
/// and it must have at least one.
///
/// The "at least one" is load-bearing. A duplicated line at a file edge with no
/// context is exactly the case where relocating onto the wrong copy is most
/// likely, so it is refused rather than accepted for lack of evidence against.
fn strict_context_holds(
    line: usize,
    mapped: usize,
    neighbours: Neighbours,
    line_map: &BTreeMap<usize, usize>,
) -> bool {
    let offset = mapped as isize - line as isize;
    let mut checked = false;
    for neighbour in [neighbours.before, neighbours.after].into_iter().flatten() {
        checked = true;
        if line_map.get(&neighbour).copied() != usize::try_from(neighbour as isize + offset).ok() {
            return false;
        }
    }
    checked
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;
