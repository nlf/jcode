//! Applying a whole envelope across several files.
//!
//! Ported from oh-my-pi's `src/edit/apply-patch/index.ts` and the semantics
//! their `#4074-B` regression test pins.
//!
//! # Why this is not atomic
//!
//! omp is explicit (spec §6.1) that hunks apply in order and **not** atomically:
//! if hunk N fails, hunks `0..N-1` are already on disk. Preflighting every file
//! in memory first would be stricter, but it cannot be honest about a write
//! that fails partway through, and true cross-file atomicity needs a
//! transactional filesystem.
//!
//! What this does guarantee is that the caller is never told a partial patch
//! succeeded: application stops at the first failure, the result is an error,
//! and the outcome names what landed, what failed, and what was never tried.

use crate::apply::{apply_hunks, create_content, ApplyError};
use crate::envelope::{Hunk, Operation};
use crate::hunks::parse_diff_hunks;

/// What one file's hunk did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    Created { path: String, content: String },
    Deleted { path: String },
    Updated {
        path: String,
        /// Set when the hunk also moved the file.
        moved_to: Option<String>,
        content: String,
    },
}

impl FileOutcome {
    pub fn path(&self) -> &str {
        match self {
            Self::Created { path, .. } | Self::Deleted { path } | Self::Updated { path, .. } => {
                path
            }
        }
    }
}

/// A file the patch needs to read before it can be applied.
///
/// The caller supplies content rather than this module reading it, which keeps
/// the module pure and lets the tool layer own path resolution and permissions.
pub trait FileSource {
    /// Current content, or `None` when the file does not exist.
    fn read(&self, path: &str) -> Option<String>;
}

impl<F> FileSource for F
where
    F: Fn(&str) -> Option<String>,
{
    fn read(&self, path: &str) -> Option<String> {
        self(path)
    }
}

/// Why a file's hunk could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkError {
    /// An update or delete named a file that is not there.
    Missing,
    /// A create named a file that already exists.
    Exists,
    /// The diff body could not be parsed.
    Parse(String),
    /// The change could not be applied to the content.
    Apply(ApplyError),
}

impl HunkError {
    pub fn message(&self, path: &str) -> String {
        match self {
            Self::Missing => format!("{path}: file does not exist"),
            Self::Exists => format!("{path}: file already exists"),
            Self::Parse(detail) => format!("{path}: {detail}"),
            Self::Apply(error) => format!("{path}: {}", error.message()),
        }
    }
}

/// The result of applying an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPlan {
    /// Outcomes to write, in order, for the files that applied.
    pub outcomes: Vec<FileOutcome>,
    /// The first failure, if any. Everything after it was never attempted.
    pub failure: Option<(String, HunkError)>,
    /// Paths never attempted because an earlier file failed.
    pub skipped: Vec<String>,
}

impl PatchPlan {
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// The message for a failed patch.
    ///
    /// Names what landed, what failed and what was skipped, because a caller
    /// that has to re-issue needs to send exactly the missing work rather than
    /// the whole patch again.
    pub fn failure_message(&self) -> Option<String> {
        let (path, error) = self.failure.as_ref()?;
        let mut message = error.message(path);

        if !self.outcomes.is_empty() {
            let applied: Vec<&str> = self.outcomes.iter().map(FileOutcome::path).collect();
            message.push_str(&format!(
                "\n\nAlready applied, and still on disk: {}. \
                 Re-read these before retrying.",
                applied.join(", ")
            ));
        }
        if !self.skipped.is_empty() {
            message.push_str(&format!(
                "\n\nNOT applied, because {path} failed first: {}",
                self.skipped.join(", ")
            ));
        }
        Some(message)
    }
}

/// Work out what an envelope's hunks would do, stopping at the first failure.
///
/// Returns a plan rather than performing writes, so the caller can report and
/// commit separately, and so this stays testable without a filesystem.
pub fn plan(hunks: &[Hunk], source: &dyn FileSource) -> PatchPlan {
    let mut outcomes = Vec::new();

    for (index, hunk) in hunks.iter().enumerate() {
        let result = plan_one(hunk, source);
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => {
                // Everything after the failure is left unattempted rather than
                // applied around it: later hunks may depend on the one that
                // failed, and applying them would deepen the inconsistency.
                let skipped = hunks[index + 1..]
                    .iter()
                    .map(|hunk| hunk.path.clone())
                    .collect();
                return PatchPlan {
                    outcomes,
                    failure: Some((hunk.path.clone(), error)),
                    skipped,
                };
            }
        }
    }

    PatchPlan {
        outcomes,
        failure: None,
        skipped: Vec::new(),
    }
}

fn plan_one(hunk: &Hunk, source: &dyn FileSource) -> Result<FileOutcome, HunkError> {
    match hunk.op {
        Operation::Create => {
            // Refusing to overwrite is what keeps a create from silently
            // discarding a file the caller forgot was there.
            if source.read(&hunk.path).is_some() {
                return Err(HunkError::Exists);
            }
            Ok(FileOutcome::Created {
                path: hunk.path.clone(),
                content: create_content(&hunk.diff),
            })
        }
        Operation::Delete => {
            if source.read(&hunk.path).is_none() {
                return Err(HunkError::Missing);
            }
            Ok(FileOutcome::Deleted {
                path: hunk.path.clone(),
            })
        }
        Operation::Update => {
            let Some(content) = source.read(&hunk.path) else {
                return Err(HunkError::Missing);
            };
            let parsed = parse_diff_hunks(&hunk.diff)
                .map_err(|error| HunkError::Parse(error.message()))?;
            let updated = apply_hunks(&content, &parsed).map_err(HunkError::Apply)?;
            Ok(FileOutcome::Updated {
                path: hunk.path.clone(),
                moved_to: hunk.rename.clone(),
                content: updated,
            })
        }
    }
}

/// The summary omp emits for a successful patch (their spec §9.1).
pub fn summary(outcomes: &[FileOutcome]) -> String {
    let mut lines = vec!["Success. Updated the following files:".to_string()];
    for outcome in outcomes {
        // A rename stays an M on the ORIGINAL path, matching omp, rather than
        // becoming a delete plus an add. The destination is appended because
        // omitting it made a real agent report the move as missing: it read
        // "M old.txt", saw old.txt gone from disk, and called the output wrong.
        let line = match outcome {
            FileOutcome::Created { path, .. } => format!("A {path}"),
            FileOutcome::Deleted { path } => format!("D {path}"),
            FileOutcome::Updated {
                path,
                moved_to: Some(destination),
                ..
            } => format!("M {path} -> {destination}"),
            FileOutcome::Updated { path, .. } => format!("M {path}"),
        };
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod plan_tests;
