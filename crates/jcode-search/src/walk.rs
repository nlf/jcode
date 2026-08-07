//! Walking the filesystem and matching file contents.
//!
//! The one module here that does I/O. Built on `ignore` and `globset`, the
//! crates ripgrep itself uses, so gitignore handling, hidden-file rules, and
//! parallel traversal are not reimplemented.

use crate::paths::{has_glob_chars, split_path_and_selector, split_path_list, LineRange};
use crate::select::{Match, INTERNAL_TOTAL_CAP};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::path::{Path, PathBuf};

/// Files above this are not searched.
///
/// A minified bundle or a checked-in dataset produces matches nobody wants and
/// costs the whole output budget. omp's native grep uses the same 4 MB window.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One resolved search target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Path as the caller wrote it, for messages they read.
    pub original: String,
    /// The path with any selector removed.
    pub path: PathBuf,
    /// Line ranges from the selector, if any.
    pub ranges: Vec<LineRange>,
    /// True when the entry was a glob rather than a literal path.
    pub is_glob: bool,
}

/// What went wrong resolving or running a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    BadSelector(String),
    BadPattern(String),
    BadGlob(String),
    NotFound(String),
}

impl SearchError {
    pub fn message(&self) -> String {
        match self {
            Self::BadSelector(detail) => detail.clone(),
            Self::BadPattern(detail) => format!("Invalid regex: {detail}"),
            Self::BadGlob(detail) => format!("Invalid glob: {detail}"),
            Self::NotFound(path) => format!("Path not found: {path}"),
        }
    }
}

/// Split a caller's path argument into targets.
///
/// Selectors are peeled per entry, so `src/a.rs:1-50; src/b.rs` scopes the
/// first file and searches all of the second.
pub fn resolve_targets(raw: Option<&str>, root: &Path) -> Result<Vec<Target>, SearchError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        // No path means the workspace root, matching omp.
        return Ok(vec![Target {
            original: ".".to_string(),
            path: root.to_path_buf(),
            ranges: Vec::new(),
            is_glob: false,
        }]);
    };

    let mut targets = Vec::new();
    for entry in split_path_list(raw) {
        // A glob's `*` is never a selector, and globs can contain colons in
        // brace alternations, so they skip selector peeling entirely.
        let (path_text, ranges) = if has_glob_chars(&entry) {
            (entry.clone(), Vec::new())
        } else {
            let split = split_path_and_selector(&entry);
            // An existing file wins over the selector reading of its name, so a
            // file literally called `notes:1-2` is searched rather than
            // truncated (omp issue #4618).
            let literal = root.join(&entry);
            if literal.exists() {
                (entry.clone(), Vec::new())
            } else {
                let ranges = crate::paths::selector_line_ranges(split.selector.as_deref())
                    .map_err(|error| SearchError::BadSelector(error.message()))?
                    .unwrap_or_default();
                (split.path, ranges)
            }
        };

        let is_glob = has_glob_chars(&path_text);
        let path = if Path::new(&path_text).is_absolute() {
            PathBuf::from(&path_text)
        } else {
            root.join(&path_text)
        };
        targets.push(Target {
            original: entry,
            path,
            ranges,
            is_glob,
        });
    }

    Ok(targets)
}

/// Options shared by content search and file finding.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub hidden: bool,
    pub respect_gitignore: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            hidden: false,
            // Ignore-aware by default: searching target/ and node_modules is
            // the difference between a useful tool and one nobody trusts.
            respect_gitignore: true,
        }
    }
}

/// Every file under `target` that should be searched.
fn walk_files(target: &Target, root: &Path, options: &WalkOptions) -> Vec<PathBuf> {
    // A glob is matched against paths relative to the root, so the walk starts
    // at the root and filters, rather than trying to walk a pattern.
    let (walk_root, matcher) = if target.is_glob {
        let pattern = target
            .path
            .strip_prefix(root)
            .map(|relative| relative.to_string_lossy().to_string())
            .unwrap_or_else(|_| target.path.to_string_lossy().to_string());
        let mut builder = GlobSetBuilder::new();
        if let Ok(glob) = Glob::new(&pattern) {
            builder.add(glob);
        }
        // No `**/` fallback: globset's `*` already crosses directory
        // separators, so `*.rs` matches `deep/nested/b.rs` on its own. Adding
        // one looked like it made bare extensions match nested files, but it
        // was dead code - removing it changed no test. Verified against
        // globset directly rather than assumed from shell glob semantics,
        // where `*` does NOT cross separators.
        (root.to_path_buf(), builder.build().ok())
    } else {
        (target.path.clone(), None)
    };

    if !walk_root.exists() {
        return Vec::new();
    }

    let mut files = Vec::new();
    let mut builder = WalkBuilder::new(&walk_root);
    builder
        .hidden(!options.hidden)
        .git_ignore(options.respect_gitignore)
        .git_global(options.respect_gitignore)
        .git_exclude(options.respect_gitignore)
        .parents(options.respect_gitignore);
    // `git_ignore` alone only applies inside a git repository, so a worktree
    // that is not yet a repo, or a subdirectory checked out on its own, would
    // silently search everything its .gitignore excludes. `ignore(true)` reads
    // the same files without requiring git, which is the behaviour a caller
    // means by "respect gitignore".
    builder.ignore(options.respect_gitignore);
    if options.respect_gitignore {
        builder.add_custom_ignore_filename(".gitignore");
    }
    let walker = builder.build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(matcher) = &matcher {
            let relative = path.strip_prefix(root).unwrap_or(path);
            if !matcher.is_match(relative) {
                continue;
            }
        }
        if std::fs::metadata(path)
            .map(|meta| meta.len() > MAX_FILE_BYTES)
            .unwrap_or(false)
        {
            continue;
        }
        files.push(path.to_path_buf());
    }

    files.sort();
    files
}

/// Find files matching the targets, without reading their contents.
pub fn find_files(
    targets: &[Target],
    root: &Path,
    options: &WalkOptions,
) -> Result<Vec<PathBuf>, SearchError> {
    let mut found = Vec::new();
    for target in targets {
        for path in walk_files(target, root, options) {
            if !found.contains(&path) {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// Search file contents for `pattern`.
///
/// Case-sensitive by default, matching omp (`grep.ts:1015`,
/// `!(caseSensitive ?? true)`). Their `case` parameter turns sensitivity *off*
/// when passed `false`, despite being described as "case-sensitive search".
/// Verified against their source rather than inferred from the name, because
/// the natural reading of the schema gives the opposite default.
pub fn search_contents(
    pattern: &str,
    targets: &[Target],
    root: &Path,
    options: &WalkOptions,
    case_sensitive: bool,
) -> Result<Vec<Match>, SearchError> {
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| SearchError::BadPattern(error.to_string()))?;

    let mut matches = Vec::new();
    for target in targets {
        for path in walk_files(target, root, options) {
            // Binary files are skipped rather than rendered: a match inside one
            // is noise, and its "line" can be megabytes long.
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let display = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            for (index, line) in content.lines().enumerate() {
                let line_number = index + 1;
                if !target.ranges.is_empty()
                    && !crate::paths::is_line_in_ranges(line_number, &target.ranges)
                {
                    continue;
                }
                if regex.is_match(line) {
                    matches.push(Match {
                        path: display.clone(),
                        line: line_number,
                        text: line.to_string(),
                    });
                    // Stop collecting well past what any window can show, so a
                    // pathological file cannot exhaust memory before selection
                    // gets a chance to trim.
                    if matches.len() >= INTERNAL_TOTAL_CAP {
                        return Ok(matches);
                    }
                }
            }
        }
    }

    Ok(matches)
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod walk_tests;
