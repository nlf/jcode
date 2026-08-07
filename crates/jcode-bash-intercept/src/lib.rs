//! Intercepting bash commands that a dedicated tool does better.
//!
//! Ported from oh-my-pi's `src/tools/bash-interceptor.ts` and
//! `shell-tokenize.ts`, behaviour-first.
//!
//! Prompt text asking a model to prefer `read` over `cat` is advice it can
//! ignore, and does. This refuses the call and names the tool instead, which is
//! the difference between a preference and a rule.

pub mod tokenize;

pub use tokenize::{segments, skip_word, without_leading_assignments, Segment};
