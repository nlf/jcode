//! Structural rewriting.
//!
//! Ported from oh-my-pi's `src/tools/ast-edit.ts`, behaviour-first.
//!
//! The shape that matters is compute-then-stage: every rewrite is worked out in
//! memory across every file, and nothing is written until the caller asks. omp
//! runs a dry pass first and applies second, and their `dry_run` defaults to
//! **true**. A structural rewrite across a glob is the most destructive thing
//! in the toolset, and it is the one operation where the model cannot see what
//! it is about to change without being shown.

use crate::matching::{find, language_for_path, MatchError, DEFAULT_MAX_MATCHES};
use crate::search::SearchFailure;
use ast_grep_core::{AstGrep, Pattern};
use ast_grep_language::SupportLang;
use jcode_search::{find_files, Target, WalkOptions};
use std::path::{Path, PathBuf};

/// Files a single rewrite may touch before stopping.
///
/// A pattern broad enough to hit hundreds of files is more likely a mistake
/// than an intention, and the cap turns that into a refusal rather than a
/// repo-wide rewrite.
pub const DEFAULT_MAX_FILES: usize = 50;

/// Replacements within one file before stopping.
pub const DEFAULT_MAX_REPLACEMENTS: usize = 200;

/// One file's pending rewrite.
///
/// Carries the full before and after so the caller can render a diff and, on
/// commit, invalidate the hashline snapshot without re-reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    /// Path relative to the search root, for display.
    pub path: String,
    /// Absolute path, for writing.
    pub absolute: PathBuf,
    pub before: String,
    pub after: String,
    /// Replacements made in this file.
    pub count: usize,
}

/// What a rewrite would do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RewritePlan {
    pub files: Vec<PendingFile>,
    /// Files considered, including those that did not match.
    pub files_searched: usize,
    pub total_replacements: usize,
    /// True when a cap stopped the rewrite, so the caller knows the plan is
    /// partial and must not be presented as the whole change.
    pub limit_reached: bool,
    /// Files whose language has no grammar.
    pub unsupported_files: usize,
    /// Files the pattern is not valid for. Expected when inferring per file.
    pub incompatible_files: usize,
}

impl RewritePlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Options for one rewrite.
#[derive(Debug, Clone)]
pub struct RewriteOptions {
    /// Language to parse every file as. `None` infers per file.
    pub language: Option<SupportLang>,
    pub max_files: usize,
    pub max_replacements: usize,
    pub walk: WalkOptions,
}

impl Default for RewriteOptions {
    fn default() -> Self {
        Self {
            language: None,
            max_files: DEFAULT_MAX_FILES,
            max_replacements: DEFAULT_MAX_REPLACEMENTS,
            walk: WalkOptions::default(),
        }
    }
}

/// Work out what rewriting `pattern` to `replacement` would do.
///
/// Returns a plan and writes nothing. Committing is the caller's separate,
/// explicit step, which is what lets the model be shown the change first.
pub fn plan(
    pattern: &str,
    replacement: &str,
    targets: &[Target],
    root: &Path,
    options: &RewriteOptions,
) -> Result<RewritePlan, SearchFailure> {
    if pattern.trim().is_empty() {
        return Err(SearchFailure::Pattern(MatchError::EmptyPattern));
    }

    let paths = find_files(targets, root, &options.walk).map_err(SearchFailure::Path)?;

    let mut plan = RewritePlan::default();

    for path in paths {
        if plan.files.len() >= options.max_files {
            plan.limit_reached = true;
            break;
        }

        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        let Some(language) = options.language.or_else(|| language_for_path(&display)) else {
            plan.unsupported_files += 1;
            continue;
        };
        let Ok(before) = std::fs::read_to_string(&path) else {
            continue;
        };
        plan.files_searched += 1;

        // Compiled here rather than passed as a string: upstream panics on a
        // pattern that is invalid for the language, and inference means that
        // happens routinely on a mixed tree.
        let Ok(compiled) = Pattern::try_new(pattern, language) else {
            plan.incompatible_files += 1;
            continue;
        };

        // Matching before rewriting, so a file with no matches is not rewritten
        // to identical content and reported as changed.
        let found = match find(&before, pattern, language, DEFAULT_MAX_MATCHES) {
            Ok(found) => found,
            Err(MatchError::BadPattern { .. }) => {
                plan.incompatible_files += 1;
                continue;
            }
            Err(error) => return Err(SearchFailure::Pattern(error)),
        };
        if found.matches.is_empty() {
            continue;
        }

        let count = found.matches.len().min(options.max_replacements);
        if found.matches.len() > options.max_replacements {
            plan.limit_reached = true;
        }

        let doc = AstGrep::new(before.as_str(), language);
        let mut edits = doc.root().replace_all(&compiled, replacement);
        // The cap has to actually cut the edit list. `replace_all` rewrites
        // every match, so counting alone would report a capped number while
        // writing an uncapped file.
        edits.truncate(options.max_replacements);
        let after = apply_edits(&before, edits);

        // A rewrite that changes nothing is not a change. Reporting it would
        // put a file in the plan whose diff is empty.
        if after == before {
            continue;
        }

        plan.total_replacements += count;
        plan.files.push(PendingFile {
            path: display,
            absolute: path,
            before,
            after,
            count,
        });
    }

    Ok(plan)
}

/// Apply upstream's edit list to the original text.
///
/// Edits arrive as byte ranges with replacement text, in source order. Applied
/// back to front so earlier offsets stay valid: applying forward would shift
/// every subsequent range by the length delta of the one before it.
fn apply_edits(source: &str, edits: Vec<ast_grep_core::source::Edit<String>>) -> String {
    let mut out = source.to_string();
    for edit in edits.into_iter().rev() {
        let start = edit.position;
        let end = start + edit.deleted_length;
        if start > out.len() || end > out.len() {
            continue;
        }
        // Upstream yields bytes; the source is valid UTF-8 and so is any
        // replacement derived from it.
        let Ok(inserted) = std::str::from_utf8(&edit.inserted_text) else {
            continue;
        };
        out.replace_range(start..end, inserted);
    }
    out
}

#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod rewrite_tests;
