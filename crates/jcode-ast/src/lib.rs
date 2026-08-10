//! Structural search over source code.
//!
//! Ported from oh-my-pi's `ast_match` / `ast_grep` / `ast_edit`, behaviour-first.
//! The engine is the upstream `ast-grep-core` crate, which is what omp's own
//! `pi-natives` uses, so this is orchestration rather than a reimplementation.
//!
//! This is what replaces the `outline` / `trace` / `smart` modes deleted with
//! agentgrep.

pub mod matching;
pub mod rewrite;
pub mod search;
pub mod syntax;

pub use matching::{
    find, language_for_path, resolve_language, supported_languages, Match, MatchError, Matches,
    DEFAULT_MAX_MATCHES,
};

pub use ast_grep_language::SupportLang;
pub use rewrite::{plan, PendingFile, RewriteOptions, RewritePlan};
pub use syntax::parses_cleanly;
pub use search::{
    search, targets_for, FileMatches, SearchFailure, SearchOptions, SearchResult,
    DEFAULT_FILE_LIMIT, MULTI_FILE_PER_FILE_MATCHES,
};
