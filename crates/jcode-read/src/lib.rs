//! Reading files: choosing the window, and rendering it.
//!
//! Ported from oh-my-pi's `src/tools/read.ts`, behaviour-first: their tests and
//! their reasoning are the specification, not their code.
//!
//! Pure and I/O-free so it tests in about a second without compiling the agent,
//! matching `jcode-hashline`, `jcode-search` and `jcode-patch`.

pub mod window;

pub use window::{
    expand_with_context, outcome, resolve, Outcome, Request, Window, DEFAULT_MAX_BYTES,
    DEFAULT_MAX_LINES, RANGE_LEADING_CONTEXT_LINES, RANGE_TRAILING_CONTEXT_LINES,
};
