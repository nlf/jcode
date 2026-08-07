//! Validating a patch before it writes.
//!
//! Ported from the validation half of oh-my-pi's `patcher.ts`. This is what
//! separates hashline from a line-number editor: the tag proves the file has
//! not changed since it was read, and the seen-line guard proves the model
//! actually saw the lines it is editing.
//!
//! # Preflight, not atomicity
//!
//! Every section is validated and applied in memory before anything is written.
//! omp is explicit that this is *not* transactional — their comment reads
//! "Commits are non-atomic: when a later write fails, the sections before it
//! are already on disk". True atomicity across several files needs a
//! transactional filesystem. What preflight buys is that no write happens until
//! every section is known to be applicable, which eliminates the common failure
//! (a bad anchor in section three) without pretending to solve the rare one (a
//! disk error mid-commit).
//!
//! # Two rejections, not one
//!
//! A tag that was never minted here and a tag that has gone stale are different
//! mistakes needing different fixes, so they get different messages. Collapsing
//! them teaches a model to re-read when it should stop inventing tags.

use std::collections::BTreeSet;

use crate::apply::apply_ops;
use crate::format::compute_file_hash;
use crate::parser::{Anchor, Op};
use crate::snapshots::SnapshotStore;

/// Maximum unseen anchor lines revealed inline in a rejection.
///
/// Over this, **nothing** merges into the seen set. That asymmetry is
/// deliberate: it stops a model splitting one blind edit into cap-sized retries
/// and walking past the guard a slice at a time.
pub const SEEN_LINE_REVEAL_CAP: usize = 40;

/// Maximum characters of a revealed line.
///
/// A longer line is truncated *and* flags the whole reveal, so no line merges.
/// Without this a minified bundle line could dump a megabyte into the error,
/// and a partially-shown line would count as seen.
pub const SEEN_LINE_REVEAL_MAX_COLUMNS: usize = 512;

/// Why a patch was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// The tag was never minted in this session.
    UnknownTag { expected: String, actual: String },
    /// The tag was minted here, but the file has changed since.
    StaleTag { expected: String, actual: String },
    /// The edit anchors lines the producer never displayed.
    UnseenLines {
        lines: Vec<usize>,
        revealed: Vec<(usize, String)>,
        truncated: bool,
    },
    /// The patch applied cleanly but changed nothing.
    NoOp,
}

impl RejectReason {
    /// The message the model reads.
    ///
    /// Written to be actionable: each says what to do next, because a rejection
    /// a model cannot act on becomes a retry of the same thing.
    pub fn message(&self, path: &str) -> String {
        match self {
            Self::UnknownTag { expected, actual } => format!(
                "Edit rejected for {path}: tag #{expected} is not from this session. \
                 The file currently hashes to #{actual}. Re-read the file to get a \
                 current [path#tag] header; never invent a tag or reuse one from an \
                 earlier session."
            ),
            Self::StaleTag { expected, actual } => format!(
                "Edit rejected for {path}: the file changed between the read and this \
                 edit. The section is bound to #{expected}, but the file now hashes to \
                 #{actual}. If an earlier edit in this session changed it, use the tag \
                 from that edit's response; otherwise re-read the file."
            ),
            Self::UnseenLines {
                lines,
                revealed,
                truncated,
            } => {
                let list = lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut message = format!(
                    "Edit rejected for {path}: it anchors lines the last read never \
                     displayed ({list}). Editing lines you have not seen is how files \
                     get mangled."
                );
                if revealed.is_empty() {
                    return message;
                }
                message.push_str("\n\nWhat is actually there:\n");
                for (line, text) in revealed {
                    message.push_str(&format!("{line}:{text}\n"));
                }
                if *truncated {
                    message.push_str(
                        "\nToo much was unseen to show it all, so re-read the range \
                         before retrying.",
                    );
                } else {
                    message.push_str(
                        "\nThose lines now count as seen: retry with the same header.",
                    );
                }
                message
            }
            Self::NoOp => format!(
                "Edit produced no change to {path}. The body is byte-identical to what \
                 is already there; re-read the file before issuing another edit."
            ),
        }
    }
}

/// A validated patch, ready to write.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub path: String,
    pub before: String,
    pub after: String,
    /// Tag of the post-edit content, for anchoring the next edit without a
    /// re-read.
    pub new_tag: String,
    pub move_dest: Option<String>,
    pub removed: bool,
    pub warnings: Vec<String>,
}

/// Validate and apply a section in memory.
///
/// `enforce_seen_lines` mirrors omp's `enforceSeenLines`, which ships **off**
/// in their settings. On is the safer default here because our bash tool can
/// still show file content the store never recorded.
pub fn prepare(
    store: &SnapshotStore,
    path: &str,
    current_text: &str,
    expected_tag: Option<&str>,
    ops: &[Op],
    enforce_seen_lines: bool,
) -> Result<Prepared, RejectReason> {
    let actual_tag = compute_file_hash(current_text);

    if let Some(expected) = expected_tag
        && expected != actual_tag
    {
        // A tag we minted means the file drifted; one we never minted means the
        // model invented it or carried it from another session.
        let recognized = store.by_hash(path, expected).is_some();
        return Err(if recognized {
            RejectReason::StaleTag {
                expected: expected.to_string(),
                actual: actual_tag,
            }
        } else {
            RejectReason::UnknownTag {
                expected: expected.to_string(),
                actual: actual_tag,
            }
        });
    }

    // The guard runs only when the tag matches: only then do anchor line
    // numbers index the content the store recorded.
    if enforce_seen_lines
        && let Some(expected) = expected_tag
    {
        assert_seen_lines(store, path, expected, current_text, ops)?;
    }

    let applied = apply_ops(current_text, ops).map_err(|error| RejectReason::UnseenLines {
        lines: Vec::new(),
        revealed: vec![(0, error)],
        truncated: false,
    })?;

    if !applied.removed && applied.text == current_text && applied.move_dest.is_none() {
        return Err(RejectReason::NoOp);
    }

    Ok(Prepared {
        path: path.to_string(),
        before: current_text.to_string(),
        new_tag: compute_file_hash(&applied.text),
        after: applied.text,
        move_dest: applied.move_dest,
        removed: applied.removed,
        warnings: Vec::new(),
    })
}

/// Every line an operation touches.
fn anchor_lines(ops: &[Op]) -> Vec<usize> {
    let mut lines = BTreeSet::new();
    for op in ops {
        match op {
            Op::Cut { start, end } => lines.extend(*start..=*end),
            Op::Put { anchor, .. } => match anchor {
                Anchor::Range { start, end } => lines.extend(*start..=*end),
                Anchor::Before(line) | Anchor::After(line) => {
                    lines.insert(*line);
                }
                Anchor::Bof | Anchor::Eof => {}
            },
            Op::Rem | Op::Mv { .. } => {}
        }
    }
    lines.into_iter().collect()
}

/// Refuse an edit anchored on lines the producer never displayed.
///
/// When the rejection can show every unseen line in full, those lines are
/// merged into the seen set, so a straight retry succeeds: the error itself is
/// proof the model has now seen them. Over either cap, nothing merges.
fn assert_seen_lines(
    store: &SnapshotStore,
    path: &str,
    expected_tag: &str,
    current_text: &str,
    ops: &[Op],
) -> Result<(), RejectReason> {
    let Some(snapshot) = store.by_content(path, current_text) else {
        // No provenance recorded: the guard cannot judge, so it stands aside.
        return Ok(());
    };
    let Some(seen) = snapshot.seen_lines.as_ref() else {
        return Ok(());
    };
    if seen.is_empty() {
        return Ok(());
    }

    let unseen: Vec<usize> = anchor_lines(ops)
        .into_iter()
        .filter(|line| !seen.contains(line))
        .collect();
    if unseen.is_empty() {
        return Ok(());
    }

    let lines: Vec<&str> = current_text.split('\n').collect();
    let mut revealed = Vec::new();
    let mut column_truncated = false;

    for line in unseen.iter().take(SEEN_LINE_REVEAL_CAP) {
        // Out-of-range anchors get a better message from the applier.
        let Some(text) = lines.get(line.saturating_sub(1)) else {
            continue;
        };
        if text.chars().count() > SEEN_LINE_REVEAL_MAX_COLUMNS {
            let clipped: String = text.chars().take(SEEN_LINE_REVEAL_MAX_COLUMNS).collect();
            revealed.push((*line, format!("{clipped}…")));
            column_truncated = true;
        } else {
            revealed.push((*line, (*text).to_string()));
        }
    }

    let truncated = unseen.len() > revealed.len() || column_truncated;
    if !truncated {
        let merged: Vec<usize> = revealed.iter().map(|(line, _)| *line).collect();
        store.record_seen_lines(path, expected_tag, &merged);
    }

    Err(RejectReason::UnseenLines {
        lines: unseen,
        revealed,
        truncated,
    })
}

#[cfg(test)]
#[path = "patcher_tests.rs"]
mod patcher_tests;
