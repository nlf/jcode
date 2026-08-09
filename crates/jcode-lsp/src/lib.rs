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
//! Under construction, bottom up. `framing` and `jsonrpc` are done and tested.

pub mod framing;
pub mod jsonrpc;

pub use framing::{encode, Framed, FramingError, MessageFramer, MAX_BODY_BYTES};
pub use jsonrpc::{DecodeError, Incoming, RequestId, ResponseError, METHOD_NOT_FOUND};
