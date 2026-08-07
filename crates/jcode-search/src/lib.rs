//! Search: the engine behind the `grep` and `glob` tools.
//!
//! Ported from oh-my-pi's `src/tools/grep.ts` and `glob.ts`, behaviour-first:
//! their tests are the specification, not their code.
//!
//! Pure and I/O-light so it tests in about a second without compiling the
//! agent, matching `jcode-hashline`.

pub mod paths;

pub use paths::{
    has_glob_chars, is_line_in_ranges, parse_line_range_chunk, parse_line_ranges,
    selector_line_ranges, split_path_and_selector, split_path_list, LineRange, SelectorError,
    SplitPath,
};
