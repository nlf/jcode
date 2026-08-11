//! Hashline: a line-anchored patch format whose sections are bound to a
//! content hash of the whole file.
//!
//! Ported from oh-my-pi's `@oh-my-pi/hashline`, behaviour-first: their tests
//! are the specification, not their code. See `docs/plans/OMP_TOOL_PORT.md`.
//!
//! This crate is deliberately pure and I/O-free so it can be tested in about a
//! second without compiling the agent, and so it carries no rebase surface.

pub mod apply;
pub mod blocks;
pub mod format;
pub mod input;
pub mod parser;
pub mod patcher;
pub mod prefixes;
pub mod recovery;
pub mod repair;
pub mod snapshots;

pub use apply::{ApplyResult, apply_ops};
pub use blocks::{BlockResolver, BlockSpan};
pub use format::{
    FILE_HASH_LENGTH, compute_file_hash, format_hashline_header, format_numbered_line,
    format_numbered_lines,
};
pub use input::{RawSection, header_paths, normalize_path, parse_header_line, split_sections};
pub use parser::{Anchor, Op, ParsedOps, parse_ops};
pub use patcher::{
    Parsing, PreflightError, Prepared, RejectReason, SEEN_LINE_REVEAL_CAP,
    SEEN_LINE_REVEAL_MAX_COLUMNS, SectionInput, preflight, prepare,
};
pub use prefixes::{
    is_read_metadata_line, parse_payload_text, strip_hashline_prefixes, strip_new_line_prefixes,
    strip_one_leading_hashline_prefix,
};
pub use recovery::{
    HEADTAIL_DRIFT_WARNING, RECOVERY_EXTERNAL_WARNING, RECOVERY_LINE_REMAP_WARNING,
    RECOVERY_SESSION_CHAIN_WARNING, Recovered, has_anchor_scoped_op, try_recover,
};
pub use repair::{RepairOutcome, compute_balance, repair_boundaries};
pub use snapshots::{Snapshot, SnapshotStore, SnapshotStoreOptions};
