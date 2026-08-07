//! Searching files for structural matches.
//!
//! Ported from oh-my-pi's `src/tools/ast-grep.ts`. The matching is
//! [`crate::matching`]; this adds file walking, per-file grouping and the
//! caps that keep a broad pattern from costing the whole output budget.
//!
//! Walking is delegated to `jcode-search`, which already carries gitignore
//! rules, hidden-file policy and the size ceiling. A second walker would drift
//! from the first.

use crate::matching::{find, language_for_path, Match, MatchError, DEFAULT_MAX_MATCHES};
use jcode_search::{find_files, resolve_targets, SearchError, Target, WalkOptions};
use std::path::Path;

/// Files reported in one response.
///
/// Matches `jcode-search`'s file window, so a structural search and a text
/// search return comparably sized results.
pub const DEFAULT_FILE_LIMIT: usize = 20;

/// Matches shown per file when several files matched.
pub const MULTI_FILE_PER_FILE_MATCHES: usize = 20;

/// One file's structural matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    /// Path relative to the search root.
    pub path: String,
    pub matches: Vec<Match>,
    /// Matches found before the per-file cap was applied.
    pub total: usize,
}

/// What a structural search found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResult {
    pub files: Vec<FileMatches>,
    /// Files that matched, including those past the file limit.
    pub total_files: usize,
    /// Files skipped because their language has no grammar, so the caller can
    /// tell "nothing matched" from "this pattern could not be applied here".
    pub unsupported_files: usize,
    /// Files whose language the pattern is not valid for.
    ///
    /// A whole-tree search infers the language per file, so a Rust pattern
    /// necessarily fails to compile against Python. That is expected and not an
    /// error; it becomes one only when NO file could use the pattern.
    pub incompatible_files: usize,
    pub file_limit_reached: bool,
}

/// Why a search could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchFailure {
    Pattern(MatchError),
    Path(SearchError),
}

impl SearchFailure {
    pub fn message(&self) -> String {
        match self {
            Self::Pattern(error) => error.message(),
            Self::Path(error) => error.message(),
        }
    }
}

/// Options for one search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Language to parse every file as. `None` infers it per file from the
    /// extension, which is what makes a whole-tree search possible.
    pub language: Option<ast_grep_language::SupportLang>,
    pub file_limit: usize,
    pub per_file_limit: usize,
    pub walk: WalkOptions,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            language: None,
            file_limit: DEFAULT_FILE_LIMIT,
            per_file_limit: MULTI_FILE_PER_FILE_MATCHES,
            walk: WalkOptions::default(),
        }
    }
}

/// Search files under `targets` for structural matches of `pattern`.
///
/// Reads files itself rather than taking content, because the set of files is
/// discovered during the walk.
pub fn search(
    pattern: &str,
    targets: &[Target],
    root: &Path,
    options: &SearchOptions,
) -> Result<SearchResult, SearchFailure> {
    if pattern.trim().is_empty() {
        return Err(SearchFailure::Pattern(MatchError::EmptyPattern));
    }

    let paths = find_files(targets, root, &options.walk).map_err(SearchFailure::Path)?;

    let mut files: Vec<FileMatches> = Vec::new();
    let mut unsupported = 0usize;
    let mut incompatible = 0usize;

    for path in paths {
        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        // A file whose language has no grammar cannot be parsed, which is not
        // the same as not matching. Counted so the caller can be told.
        let Some(language) = options
            .language
            .or_else(|| language_for_path(&display))
        else {
            unsupported += 1;
            continue;
        };

        let Ok(content) = std::fs::read_to_string(&path) else {
            // Binary or unreadable: skipped rather than failing the search, so
            // one bad file does not lose every other file's results.
            continue;
        };

        let found = match find(&content, pattern, language, DEFAULT_MAX_MATCHES) {
            Ok(found) => found,
            Err(MatchError::BadPattern { .. }) => {
                // Expected during inference: the pattern belongs to some other
                // language. Counted, not fatal.
                incompatible += 1;
                continue;
            }
            Err(error) => return Err(SearchFailure::Pattern(error)),
        };
        if found.matches.is_empty() {
            continue;
        }

        let total = found.matches.len();
        let mut matches = found.matches;
        matches.truncate(options.per_file_limit);
        files.push(FileMatches {
            path: display,
            matches,
            total,
        });
    }

    // Every candidate file rejected the pattern, so it is not a search that
    // found nothing: it is a pattern that cannot be used here, and saying so is
    // the difference between "narrow your search" and "fix your pattern".
    if files.is_empty() && incompatible > 0 && unsupported == 0 {
        return Err(SearchFailure::Pattern(MatchError::BadPattern {
            pattern: pattern.to_string(),
            language: options
                .language
                .map(|language| format!("{language:?}").to_lowercase())
                .unwrap_or_else(|| "any searched file's language".to_string()),
        }));
    }

    let total_files = files.len();
    files.truncate(options.file_limit);

    Ok(SearchResult {
        file_limit_reached: total_files > files.len(),
        files,
        total_files,
        unsupported_files: unsupported,
        incompatible_files: incompatible,
    })
}

/// Resolve a path argument into search targets.
///
/// Shares `jcode-search`'s resolution, so a structural search accepts the same
/// semicolon lists and globs a text search does.
pub fn targets_for(path: Option<&str>, root: &Path) -> Result<Vec<Target>, SearchFailure> {
    resolve_targets(path, root).map_err(SearchFailure::Path)
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
