//! Turning a server's `Diagnostic` into the line a model reads.
//!
//! Ported from omp's `src/lsp/utils.ts`: `severityToString`, `sortDiagnostics`,
//! `stripDiagnosticNoise`, `formatDiagnostic`, `formatDiagnosticsSummary` and
//! `summarizeDiagnosticMessages`.
//!
//! # Why this belongs in the crate rather than in the tool adapter
//!
//! [`crate::ledger`] deduplicates **formatted** messages, and its identity function strips a
//! `path:line:col ` prefix that only exists because this module puts it there. So the ledger's
//! tests were written against strings I invented by reading omp's format string, rather than
//! against strings any code produces. That is one transcription mistake away from a ledger
//! that dedups nothing in production while passing every test.
//!
//! With this module the ledger's inputs can be *generated*, which is what
//! `ledger_tests::identity_of_a_real_formatted_diagnostic` now does.
//!
//! # The format, and the two things in it that matter
//!
//! ```text
//! src/main.rs:12:5 [error] [rustc] cannot find value `x` (E0425)
//! ^path      ^1-based    ^source        ^message        ^code
//! ```
//!
//! LSP positions are **0-based** and this output is **1-based**, because it is read by a human
//! or a model and every editor and compiler in existence is 1-based. Getting that wrong is a
//! silent off-by-one in every diagnostic, which is why it has its own test.
//!
//! The `[severity]` marker is load-bearing beyond display:
//! [`summarize_formatted`] parses it back out to count errors, and
//! [`crate::ledger`] keeps it inside the identity so a warning promoted to an error is a
//! different diagnostic. Changing the brackets breaks both.

use serde_json::Value;

/// A severity name, matching omp's `SEVERITY_NAMES`.
///
/// An absent severity means **error**, not "unknown": the LSP spec allows omitting it and omp
/// defaults `severity ?? 1`. Defaulting to something milder would let a server hide errors by
/// leaving the field out.
pub fn severity_name(severity: Option<i64>) -> &'static str {
    match severity.unwrap_or(1) {
        1 => "error",
        2 => "warning",
        3 => "info",
        4 => "hint",
        // Out of spec. omp yields "unknown" through its `?? "unknown"` fallback, so a server
        // sending 7 produces a line rather than being dropped.
        _ => "unknown",
    }
}

/// Remove lines that carry no information for a model.
///
/// rustc appends "for further information visit <url>" to many diagnostics, and some servers
/// emit a bare URL line. Both cost tokens and say nothing a model can act on.
///
/// Deliberately conservative: only those two shapes, matching omp exactly. A more aggressive
/// filter risks removing the one line that explains the error, and this text is the model's
/// only view of the problem.
pub fn strip_noise(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("for further information visit") {
                return false;
            }
            // A bare URL on its own line.
            !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Format one diagnostic as the line a model reads.
///
/// `file` is used verbatim, so the caller decides whether it is absolute or relative to the
/// project. omp passes a cwd-relative path, which is what makes the output readable.
pub fn format_diagnostic(diagnostic: &Value, file: &str) -> String {
    let severity = severity_name(diagnostic.get("severity").and_then(Value::as_i64));

    // LSP is 0-based; this output is 1-based. See the module note.
    let start = diagnostic.get("range").and_then(|range| range.get("start"));
    let line = start
        .and_then(|start| start.get("line"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let column = start
        .and_then(|start| start.get("character"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;

    // `[rustc] ` when present, nothing when not -- including the trailing space, which is why
    // this is built as a whole fragment rather than interpolated conditionally.
    let source = diagnostic
        .get("source")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
        .map(|source| format!("[{source}] "))
        .unwrap_or_default();

    // A code may be a number (`2322`) or a string (`E0425`); both are rendered as-is. A
    // caller-facing difference from omp: their `diagnostic.code ? ...` treats the number 0 as
    // absent, since 0 is falsey in JavaScript. A code of 0 is legal and would be dropped
    // there; here it is kept, which is a divergence in our favour and is tested.
    let code = match diagnostic.get("code") {
        Some(Value::String(code)) if !code.is_empty() => format!(" ({code})"),
        Some(Value::Number(code)) => format!(" ({code})"),
        _ => String::new(),
    };

    let message = strip_noise(
        diagnostic
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );

    format!("{file}:{line}:{column} [{severity}] {source}{message}{code}")
}

/// Order diagnostics for display: worst first, then by position, then by message.
///
/// Severity ascends numerically (1 = error), so a plain sort puts errors first. The tie-breaks
/// exist so the output is stable: a server is free to publish in any order, and a list that
/// reshuffles between identical runs makes a diff of two runs unreadable.
pub fn sort_diagnostics(diagnostics: &mut [Value]) {
    diagnostics.sort_by(|left, right| {
        let key = |diagnostic: &Value| {
            let severity = diagnostic
                .get("severity")
                .and_then(Value::as_i64)
                .unwrap_or(1);
            let start = diagnostic.get("range").and_then(|range| range.get("start"));
            let line = start
                .and_then(|start| start.get("line"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let column = start
                .and_then(|start| start.get("character"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let message = diagnostic
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (severity, line, column, message)
        };
        key(left).cmp(&key(right))
    });
}

/// A one-line count, from the diagnostics themselves.
///
/// `"no issues"` when empty, which is a claim rather than a formatting choice: a caller must
/// only use it when the diagnostics are known to be fresh. See [`crate::freshness`] for why an
/// empty list and an unanswered server are not the same thing.
pub fn summarize(diagnostics: &[Value]) -> String {
    let mut counts = [0usize; 4];
    for diagnostic in diagnostics {
        match severity_name(diagnostic.get("severity").and_then(Value::as_i64)) {
            "error" => counts[0] += 1,
            "warning" => counts[1] += 1,
            "info" => counts[2] += 1,
            "hint" => counts[3] += 1,
            // "unknown" is counted nowhere, matching omp: their `if (sev in counts)` guard
            // skips it, so an out-of-spec severity appears in the list but not the summary.
            _ => {}
        }
    }
    render_counts(counts)
}

/// The same count, recovered from **already formatted** messages.
///
/// Needed because the ledger reduces formatted strings, so by the time a summary is wanted the
/// original `Diagnostic` values are gone. omp has the same pair of functions for the same
/// reason, and parses its own `[severity]` marker back out.
///
/// Returns the summary and whether any error survived, since a caller usually needs both and
/// deriving the second from the first would mean parsing the string it just built.
pub fn summarize_formatted(messages: &[String]) -> (String, bool) {
    let mut counts = [0usize; 4];
    for message in messages {
        if let Some(index) = first_severity_marker(message) {
            counts[index] += 1;
        }
    }
    (render_counts(counts), counts[0] > 0)
}

/// The index of the **first** `[severity]` marker by position, or `None`.
///
/// # Why position rather than a name-ordered search
///
/// omp matches `/\[(error|warning|info|hint)\]/i`, which finds the earliest marker in the string.
/// My first version looped over the four names and took the first that appeared *anywhere*, which
/// is a different function: it is ordered by severity, not by position.
///
/// Measured divergence, against omp's actual regex in node:
///
/// ```text
/// "src/a.ts:1:1 [warning] cast produces [error] string"
///   omp:  1 warning(s), errored = false
///   mine: 1 error(s),   errored = true
/// ```
///
/// A diagnostic quoting another diagnostic is ordinary -- TypeScript and rustc both do it -- so a
/// warning was being reported as an error, and `errored` drove that all the way out to the caller.
///
/// This is precisely the transcribed-by-eye class of mistake the formatter port was written to
/// eliminate, and I made it in the port itself: faithful to the format string, unfaithful to the
/// regex. Found by an adversarial reviewer on the seventh pass.
///
/// Returns the index into the severity arrays used by [`render_counts`], so the caller does not
/// re-map names to positions.
pub(crate) fn first_severity_marker(message: &str) -> Option<usize> {
    let lowered = message.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(offset) = lowered[from..].find('[') {
        let start = from + offset;
        for (index, name) in ["error", "warning", "info", "hint"].iter().enumerate() {
            if lowered[start..].starts_with(&format!("[{name}]")) {
                return Some(index);
            }
        }
        from = start + 1;
    }
    None
}

fn render_counts(counts: [usize; 4]) -> String {
    let mut parts = Vec::new();
    for (count, label) in counts.iter().zip(["error", "warning", "info", "hint"]) {
        if *count > 0 {
            parts.push(format!("{count} {label}(s)"));
        }
    }
    if parts.is_empty() {
        // omp's exact wording. Worth matching, because it appears in tool output a model reads
        // and gratuitous rewording is how a port stops being comparable to its original.
        "no issues".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
