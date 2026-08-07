//! Search: the engine behind the `grep` and `glob` tools.
//!
//! Ported from oh-my-pi's `src/tools/grep.ts` and `glob.ts`, behaviour-first:
//! their tests are the specification, not their code.
//!
//! Pure and I/O-light so it tests in about a second without compiling the
//! agent, matching `jcode-hashline`.

pub mod paths;
pub mod select;

pub use paths::{
    has_glob_chars, is_line_in_ranges, parse_line_range_chunk, parse_line_ranges,
    selector_line_ranges, split_path_and_selector, split_path_list, LineRange, SelectorError,
    SplitPath,
};

pub use select::{
    filter_to_ranges, group_by_file, interleave, pagination_message, select, FileMatches, Match,
    Selection, DEFAULT_FILE_LIMIT, INTERNAL_TOTAL_CAP, MULTI_FILE_PER_FILE_MATCHES,
    SINGLE_FILE_MATCHES,
};
