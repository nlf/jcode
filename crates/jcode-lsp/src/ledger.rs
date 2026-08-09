//! Suppressing diagnostics the model has already been told about.
//!
//! Ported from omp's `DiagnosticsLedger`. 51 lines there, and aimed squarely at
//! context waste: a file edited five times republishes the same unrelated warnings
//! five times, and each repeat costs tokens and teaches the model nothing.
//!
//! # Why identity strips the location
//!
//! A diagnostic that merely **moved** is not new. Insert a line at the top of a
//! file and every diagnostic below it shifts down one, so location-sensitive
//! comparison reports the entire file as freshly broken after a one-line edit.
//!
//! So identity is the message with its `path:line:col` prefix removed, leaving
//! severity, source, text and code — the parts that say what is wrong rather than
//! where. Two diagnostics with the same identity are the same problem.
//!
//! # What is deliberately kept
//!
//! **Suppression is not permanent.** A diagnostic that disappears and comes back is
//! reported again ([`Ledger::reduce`] forgets a file's set when it publishes
//! nothing). Without that, fixing an error and reintroducing it would leave the
//! model blind to it for the rest of the session, which is a far worse failure than
//! the repetition this exists to avoid.
//!
//! **Severity and code are part of identity.** The same text at `[warning]` rather
//! than `[error]` is a different diagnostic, and so is the same text with a
//! different code. Both changes mean the server reclassified something, which the
//! model needs to know.

use std::collections::{HashMap, HashSet};

/// The identity of a diagnostic: what is wrong, not where.
///
/// Strips a leading `path:line:col ` prefix, taking the **leftmost** position where a
/// `:digits:digits` followed by whitespace occurs.
///
/// # Leftmost, not rightmost, and this was a real bug
///
/// The first version searched from the right, reasoning that a path may itself
/// contain a colon (`fixtures/pkg:2/example.ts:12:5` is one of omp's own fixtures)
/// and that the last `:digits:digits` must therefore be the true location.
///
/// That is wrong, because a diagnostic *message* frequently contains a location too:
///
/// ```text
/// src/a.ts:12:5 [error] declared at src/b.ts:3:1 previously
/// src/c.ts:9:9 [error] declared at src/b.ts:3:1 previously
/// ```
///
/// Searching from the right strips through the embedded `src/b.ts:3:1` and leaves
/// `"previously"` for both — so two diagnostics about *different files* share an
/// identity, and the second is suppressed as already-reported. The model is then
/// never told about a real error. Found by an adversarial reviewer probing this
/// exact shape; measured before the fix, both messages returned `"previously"`.
///
/// Leftmost also handles the colon-in-path case, which is why nothing was traded
/// away: omp's `/^.*?:\d+:\d+\s+/` is lazy, so it extends `.*?` only as far as
/// needed to reach the first position where `:digits:digits` *and the whitespace
/// after it* both match. In `fixtures/pkg:2/example.ts:12:5 `, the candidate at
/// `pkg:2/...` fails because `2/example` is not `digits` followed by whitespace, so
/// the match moves on and lands correctly. The rightmost search was solving a problem
/// the lazy match had already solved, and introduced a worse one doing it.
///
/// An unparseable message keeps its full text. That is the safe direction: a
/// message we cannot decompose is compared whole, so at worst it fails to dedup.
/// Guessing at a prefix could strip real content and merge two different problems.
pub fn identity(message: &str) -> &str {
    strip_location_prefix(message).unwrap_or(message)
}

/// Find the end of a `path:line:col ` prefix and return what follows.
///
/// Scans colons left to right and takes the first that begins a valid
/// `:digits:digits<whitespace>` run, matching the semantics of omp's lazy regex. See
/// [`identity`] for why the direction is load-bearing.
fn strip_location_prefix(message: &str) -> Option<&str> {
    let mut search_from = 0usize;
    while let Some(offset) = message[search_from..].find(':') {
        let colon = search_from + offset;
        if let Some(rest) = location_after(message, colon) {
            return Some(rest);
        }
        search_from = colon + 1;
        if search_from >= message.len() {
            break;
        }
    }
    None
}

/// Given a colon at `colon`, if what follows is `line:col ` return the remainder.
fn location_after(message: &str, colon: usize) -> Option<&str> {
    let after = &message[colon + 1..];
    let (line, rest) = take_digits(after)?;
    if line.is_empty() {
        return None;
    }
    let rest = rest.strip_prefix(':')?;
    let (column, rest) = take_digits(rest)?;
    if column.is_empty() {
        return None;
    }
    // A space separates the location from the message. Requiring it means
    // `a:1:2` alone is not treated as a prefix with an empty message.
    rest.strip_prefix(' ')
}

fn take_digits(input: &str) -> Option<(&str, &str)> {
    let end = input
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(input.len());
    Some((&input[..end], &input[end..]))
}

/// What a reduce produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reduced {
    /// The messages worth showing: those not already reported for this file.
    pub messages: Vec<String>,
    /// Whether any of them is an error, recomputed over the reduced set.
    ///
    /// **Recomputed, not inherited.** A batch of five warnings and one error, where
    /// only a warning is new, must not be reported as errored: the caller uses this
    /// to decide whether the edit broke something.
    pub errored: bool,
}

/// Per-file memory of what has been reported.
#[derive(Debug, Default)]
pub struct Ledger {
    seen: HashMap<String, HashSet<String>>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reduce a file's diagnostics to the ones not already reported.
    ///
    /// The file's remembered set is **replaced** by what is current, not unioned
    /// with it. That is what makes suppression temporary: a diagnostic absent from
    /// this publish is forgotten, so if it returns it is reported again.
    pub fn reduce(&mut self, path: &str, messages: &[String]) -> Reduced {
        let previous = self.seen.get(path);
        let mut current = HashSet::with_capacity(messages.len());
        let mut fresh = Vec::new();

        for message in messages {
            let identity = identity(message).to_string();
            let already_reported = previous.is_some_and(|seen| seen.contains(&identity));
            current.insert(identity);
            if !already_reported {
                fresh.push(message.clone());
            }
        }

        if current.is_empty() {
            // Nothing is wrong with the file now, so nothing is remembered. This is
            // what lets a fixed-then-reintroduced error resurface.
            self.seen.remove(path);
        } else {
            self.seen.insert(path.to_string(), current);
        }

        Reduced {
            errored: fresh.iter().any(|message| is_error(message)),
            messages: fresh,
        }
    }

    /// Forget a file entirely.
    ///
    /// For a file that was deleted or renamed: keeping its set would suppress
    /// diagnostics for whatever later takes the path.
    pub fn forget(&mut self, path: &str) {
        self.seen.remove(path);
    }
}

/// Whether a formatted message is an error.
///
/// Matches the `[error]` marker the formatter emits. Deliberately not a substring
/// search for "error": a message whose *text* mentions the word ("cannot infer
/// error type") is not necessarily an error, and misclassifying a warning as one
/// tells the caller an edit broke the build when it did not.
fn is_error(message: &str) -> bool {
    message.contains("[error]")
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod ledger_tests;
