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
//! `correlation`, `client` and `freshness` are done and tested.

pub mod client;
pub mod correlation;
pub mod framing;
pub mod freshness;
pub mod jsonrpc;
pub mod transport;

pub use client::{Capabilities, Client, PublishedDiagnostics, ServerSpec, DEFAULT_REQUEST_TIMEOUT};
pub use correlation::{Pendings, RequestFailure, ServerRequest, SharedPendings};
pub use framing::{encode, Framed, FramingError, MessageFramer, MAX_BODY_BYTES};
pub use freshness::{equivalent_uris, Decision, Freshness, FreshnessRequest, FreshnessWait, Observation};
pub use jsonrpc::{DecodeError, Incoming, RequestId, ResponseError, METHOD_NOT_FOUND};
pub use transport::{FromServer, Transport, WriteError, Writer};
