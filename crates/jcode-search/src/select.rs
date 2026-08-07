//! Selecting and limiting search results.
//!
//! Ported from oh-my-pi's `src/tools/grep.ts`, behaviour-first.
//!
//! The interesting part of a search tool is not finding matches, which ripgrep
//! does. It is choosing which matches to show when there are more than fit: a
//! naive truncation returns 400 hits from one generated file and none from the
//! twelve files the caller cared about.

use crate::paths::LineRange;

/// Distinct files surfaced in one response. Further pages come via `skip`.
pub const DEFAULT_FILE_LIMIT: usize = 20;

/// Per-file cap when several files matched.
///
/// Keeps one hot file from crowding out the others. omp's value.
pub const MULTI_FILE_PER_FILE_MATCHES: usize = 20;

/// Per-file cap when the search was scoped to a single file.
///
/// Higher because there is no diversity to protect: the caller already said
/// which file they mean, so showing them 200 matches in it is what they asked
/// for.
pub const SINGLE_FILE_MATCHES: usize = 200;

/// Ceiling on matches collected before grouping.
///
/// Sized to cover the file window (20 files x 20 matches) with headroom for
/// counting totals, so the caller can be told how much they are not seeing.
pub const INTERNAL_TOTAL_CAP: usize = 2000;

/// One match, before selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: String,
    /// 1-indexed.
    pub line: usize,
    pub text: String,
}

/// Matches for one file, in line order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    pub path: String,
    pub matches: Vec<Match>,
    /// Matches found in this file before the per-file cap was applied.
    pub total: usize,
}

/// What a search returned, after selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    pub files: Vec<FileMatches>,
    /// Files that matched, including those past the file limit.
    pub total_files: usize,
    /// True when files were withheld, so the caller can offer a next page.
    pub file_limit_reached: bool,
    /// `skip` value that would fetch the next page.
    pub next_skip: usize,
}

/// Group matches by file, preserving first-seen file order.
///
/// First-seen order is the traversal order, which is stable and is what makes
/// `skip` paginate correctly: re-running a search must place the same files in
/// the same sequence or a second page silently repeats or drops files.
pub fn group_by_file(matches: Vec<Match>) -> Vec<FileMatches> {
    let mut files: Vec<FileMatches> = Vec::new();
    for item in matches {
        match files.iter_mut().find(|file| file.path == item.path) {
            Some(file) => {
                file.total += 1;
                file.matches.push(item);
            }
            None => {
                files.push(FileMatches {
                    path: item.path.clone(),
                    total: 1,
                    matches: vec![item],
                });
            }
        }
    }
    files
}

/// Apply the file window and per-file caps.
///
/// `single_file` raises the per-file cap, since a caller who named one file has
/// no diversity to protect.
pub fn select(
    files: Vec<FileMatches>,
    skip: usize,
    file_limit: usize,
    single_file: bool,
) -> Selection {
    let total_files = files.len();
    let per_file = if single_file {
        SINGLE_FILE_MATCHES
    } else {
        MULTI_FILE_PER_FILE_MATCHES
    };

    let windowed: Vec<FileMatches> = files
        .into_iter()
        .skip(skip)
        .take(file_limit)
        .map(|mut file| {
            file.matches.truncate(per_file);
            file
        })
        .collect();

    let next_skip = skip + windowed.len();
    Selection {
        // A file limit is only "reached" when files were actually withheld.
        // Reporting it whenever the window is full would offer a next page that
        // returns nothing.
        file_limit_reached: next_skip < total_files,
        files: windowed,
        total_files,
        next_skip,
    }
}

/// Interleave one match from each file in turn, then apply the cap.
///
/// The cap is applied *after* the rotation rather than as a per-file bound.
/// Trimming mid-rotation would favour whichever files sort first, which is the
/// bias the rotation exists to remove.
pub fn interleave(files: &[FileMatches], cap: Option<usize>) -> Vec<Match> {
    let mut selected = Vec::new();
    let mut cursor = 0usize;
    loop {
        let mut any = false;
        for file in files {
            if let Some(item) = file.matches.get(cursor) {
                selected.push(item.clone());
                any = true;
            }
        }
        if !any {
            break;
        }
        cursor += 1;
    }
    if let Some(cap) = cap
        && selected.len() > cap
    {
        selected.truncate(cap);
    }
    selected
}

/// Drop matches outside the selector's line ranges.
///
/// Filtering here rather than in the search means the ranges apply per path, so
/// two paths in one call can carry different selectors.
pub fn filter_to_ranges(matches: Vec<Match>, ranges: &[LineRange]) -> Vec<Match> {
    if ranges.is_empty() {
        return matches;
    }
    matches
        .into_iter()
        .filter(|item| crate::paths::is_line_in_ranges(item.line, ranges))
        .collect()
}

/// The message telling the caller how to get the next page.
///
/// Empty when nothing was withheld: an unconditional hint teaches the model to
/// paginate searches that are already complete.
pub fn pagination_message(selection: &Selection, skip: usize) -> String {
    if !selection.file_limit_reached {
        return String::new();
    }
    format!(
        "Showing files {}-{} of {}. Use skip={} for the next page, or narrow paths/pattern.",
        skip + 1,
        selection.next_skip,
        selection.total_files,
        selection.next_skip
    )
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod select_tests;
