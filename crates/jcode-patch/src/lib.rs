//! Patch application: the `apply_patch` tool's engine.
//!
//! Ported from oh-my-pi's `src/edit/apply-patch/` and `src/edit/modes/patch.ts`,
//! behaviour-first: their tests are the specification, not their code.
//!
//! Pure and I/O-free so it tests in about a second without compiling the agent,
//! matching `jcode-hashline` and `jcode-search`.

pub mod apply;
pub mod envelope;
pub mod fuzzy;
pub mod hunks;
pub mod plan;
pub mod shape;

pub use fuzzy::{
    adjust_indentation, count_leading_whitespace, find_closest_sequence, find_context_line,
    find_exact_sequence, find_fuzzy_sequence, leading_whitespace, levenshtein_distance,
    normalize_for_fuzzy, seek_sequence, similarity, SequenceMatch, DEFAULT_FUZZY_THRESHOLD,
};

pub use envelope::{parse, parse_streaming, Hunk, Operation, ParseError};

pub use hunks::{parse_diff_hunks, DiffHunk};

pub use shape::{
    detect_line_ending, has_trailing_newline, normalize_to_lf, restore_line_endings, strip_bom,
    LineEnding, TextShape, BOM,
};

pub use apply::{apply_hunk, apply_hunks, create_content, ApplyError};

pub use plan::{plan, summary, FileOutcome, FileSource, HunkError, PatchPlan};
