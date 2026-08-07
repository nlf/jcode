//! Hashline: a line-anchored patch format whose sections are bound to a
//! content hash of the whole file.
//!
//! Ported from oh-my-pi's `@oh-my-pi/hashline`, behaviour-first: their tests
//! are the specification, not their code. See `docs/plans/OMP_TOOL_PORT.md`.
//!
//! This crate is deliberately pure and I/O-free so it can be tested in about a
//! second without compiling the agent, and so it carries no rebase surface.

pub mod format;
pub mod prefixes;
pub mod snapshots;

pub use format::{
    compute_file_hash, format_hashline_header, format_numbered_line, format_numbered_lines,
    FILE_HASH_LENGTH,
};
pub use prefixes::{
    is_read_metadata_line, parse_payload_text, strip_hashline_prefixes, strip_new_line_prefixes,
    strip_one_leading_hashline_prefix,
};
pub use snapshots::{Snapshot, SnapshotStore, SnapshotStoreOptions};
