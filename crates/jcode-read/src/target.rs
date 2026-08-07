//! Guards on a write's target path.
//!
//! Ported from oh-my-pi's `src/tools/write.ts`, behaviour-first.
//!
//! A model that means to *read* `notes.md:50-100` and dispatches to `write`
//! instead does not get an error: it gets a file literally named
//! `notes.md:50-100`, sitting in the workspace, which nothing will ever look at
//! again. These guards turn that silent success into a refusal that names the
//! call the model meant to make.

use jcode_search::split_path_and_selector;

/// Why a write target was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Misfire {
    /// The path ends in a read selector and no such file exists.
    Selector { target: String, selector: String },
    /// The path is a semicolon-joined list of read selectors.
    SelectorList { target: String, count: usize },
}

impl Misfire {
    /// The message the model reads.
    ///
    /// Each names the call that was probably intended, because a refusal the
    /// caller cannot act on becomes a retry of the same mistake.
    pub fn message(&self) -> String {
        match self {
            Self::Selector { target, selector } => format!(
                "write target '{target}' ends with a read selector ':{selector}' and no \
                 such file exists, so this would create a literal file by that name. \
                 If you meant to read it, use read with path '{target}'. If you really \
                 do want this file, pass its contents: a non-empty write is never \
                 blocked."
            ),
            Self::SelectorList { target, count } => format!(
                "write target '{target}' is a semicolon-joined list of {count} read \
                 selectors, not a path. write creates one file; issue one read per \
                 path to read these ranges."
            ),
        }
    }
}

/// Whether the target is a `;`-joined list of read selectors.
///
/// Fires regardless of content, unlike the single-selector guard. The
/// non-empty-content escape exists for a lone selector-shaped *filename*, never
/// a list: honouring it here would silently create a nested directory tree
/// (`a.txt:1-2;b/`) in the workspace.
fn selector_list(target: &str) -> Option<usize> {
    // No `contains(';')` fast path: split on a string without one yields a
    // single segment, which the length check below already rejects. Mutation
    // testing showed the extra check was unreachable.
    let segments: Vec<&str> = target.split(';').collect();
    // Fewer than two segments is not a list. Without this, a lone
    // `notes.md:50-100` would be reported as a "list of 1" rather than falling
    // through to the single-selector guard, and the caller would be told to
    // issue one read per path for a single path.
    if segments.len() < 2 {
        return None;
    }
    // Every segment must carry its own selector. One that does not means this
    // is a path that happens to contain a semicolon, which is legal.
    for segment in &segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() || split_path_and_selector(trimmed).selector.is_none() {
            return None;
        }
    }
    Some(segments.len())
}

/// Check a write target for a misdispatched read.
///
/// `target_exists` reports whether the literal path is already on disk, and is
/// the escape hatch: a real file named `notes:1-2` stays writable. The caller
/// supplies it so this stays pure.
///
/// Non-empty content also passes the single-selector guard: a model that sent
/// contents meant to write a file, whatever it called it.
pub fn check(target: &str, content: &str, target_exists: bool) -> Option<Misfire> {
    if target_exists {
        return None;
    }

    if let Some(count) = selector_list(target) {
        return Some(Misfire::SelectorList {
            target: target.to_string(),
            count,
        });
    }

    if !content.is_empty() {
        return None;
    }

    let split = split_path_and_selector(target);
    split.selector.map(|selector| Misfire::Selector {
        target: target.to_string(),
        selector,
    })
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod target_tests;
