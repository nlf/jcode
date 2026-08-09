//! A Language Server Protocol client.
//!
//! Ported from omp's `packages/coding-agent/src/lsp`, with their tests as the
//! specification. The plan, its two review passes, and what was deliberately
//! left out are in `docs/plans/LSP_TOOL_PORT.md`.
//!
//! # What this crate is not
//!
//! It holds no jcode types and knows nothing about tools, sessions, or output
//! formatting. Those live in the adapter (`jcode-app-core/src/tool/lsp.rs`), so
//! that the client can be tested in about a second rather than behind a
//! 24-second `app-core` compile.
//!
//! # Status
//!
//! Under construction, bottom up. `framing`, `jsonrpc`, `transport`,
//! `correlation`, `client`, `freshness`, `ledger` and `position` are done and
//! tested.

pub mod client;
pub mod correlation;
pub mod display;
pub mod framing;
pub mod freshness;
pub mod jsonrpc;
pub mod ledger;
pub mod position;
pub mod transport;

pub use client::{Capabilities, Client, DEFAULT_REQUEST_TIMEOUT, PublishedDiagnostics, ServerSpec};
pub use correlation::{Pendings, RequestFailure, ServerRequest, SharedPendings};
pub use display::{block, expand_tabs, inline, truncate};
pub use framing::{Framed, FramingError, MAX_BODY_BYTES, MessageFramer, encode};
pub use freshness::{
    Decision, Freshness, FreshnessRequest, FreshnessWait, Observation, equivalent_uris,
};
pub use jsonrpc::{DecodeError, Incoming, METHOD_NOT_FOUND, RequestId, ResponseError};
pub use ledger::{Ledger, Reduced};
pub use position::{PositionError, SymbolSpec, parse_symbol, resolve_column};
pub use transport::{FromServer, Transport, WriteError, Writer};
