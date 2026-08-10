//! Formatter tests, differential against omp's `utils.ts`.
//!
//! Every expectation here was **printed by a transcription of omp's own functions** running in
//! node, not written from reading their format string. That distinction is the reason this
//! module exists: the ledger deduplicates formatted messages, and its tests previously used
//! strings I had invented, so a transcription mistake would have produced a ledger that
//! deduplicated nothing in production while passing everything.

use super::*;
use serde_json::json;

/// A diagnostic at a 0-based line and column.
fn diagnostic(line: i64, column: i64, message: &str) -> Value {
    json!({
        "range": {
            "start": {"line": line, "character": column},
            "end": {"line": line, "character": column + 1}
        },
        "message": message,
        "severity": 1
    })
}

/// **The format matches omp, character for character.**
///
/// Expectations printed by omp's `formatDiagnostic`. The interesting rows are the last few: an
/// out-of-spec severity still renders (as `[unknown]`), an empty `source` produces no bracket
/// *and no stray space*, and the noise filter removes two lines while leaving the code suffix
/// attached to what remains.
#[test]
fn formatting_matches_omp() {
    let rust_error = json!({
        "range": {"start": {"line": 11, "character": 4}, "end": {"line": 11, "character": 5}},
        "message": "cannot find value `x`",
        "severity": 1
    });
    assert_eq!(
        format_diagnostic(&rust_error, "src/main.rs"),
        "src/main.rs:12:5 [error] cannot find value `x`"
    );

    let mut with_metadata = rust_error.clone();
    with_metadata["source"] = json!("rustc");
    with_metadata["code"] = json!("E0425");
    assert_eq!(
        format_diagnostic(&with_metadata, "src/main.rs"),
        "src/main.rs:12:5 [error] [rustc] cannot find value `x` (E0425)"
    );

    // A numeric code, which TypeScript uses.
    let numeric = json!({
        "range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}},
        "message": "Type mismatch",
        "severity": 1,
        "source": "ts",
        "code": 2322
    });
    assert_eq!(
        format_diagnostic(&numeric, "a.ts"),
        "a.ts:1:6 [error] [ts] Type mismatch (2322)"
    );

    // No severity means error, per omp's `severity ?? 1`.
    let unspecified = json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        "message": "defaults to error"
    });
    assert_eq!(
        format_diagnostic(&unspecified, "a.ts"),
        "a.ts:1:1 [error] defaults to error"
    );

    // Out of spec: rendered rather than dropped.
    let mut odd = unspecified.clone();
    odd["message"] = json!("odd");
    odd["severity"] = json!(7);
    assert_eq!(format_diagnostic(&odd, "a.ts"), "a.ts:1:1 [unknown] odd");

    // An empty source must not leave a stray space before the message.
    let mut blank_source = unspecified.clone();
    blank_source["message"] = json!("m");
    blank_source["severity"] = json!(1);
    blank_source["source"] = json!("");
    assert_eq!(
        format_diagnostic(&blank_source, "e.rs"),
        "e.rs:1:1 [error] m"
    );
}

/// **LSP positions are 0-based and this output is 1-based.**
///
/// A silent off-by-one in every diagnostic ever reported, if wrong. Given its own test because
/// it is the single most likely transcription error in the module and the least visible: every
/// line still looks plausible.
#[test]
fn positions_are_converted_to_one_based() {
    let at_origin = diagnostic(0, 0, "first character of the file");
    assert!(
        format_diagnostic(&at_origin, "a.rs").starts_with("a.rs:1:1 "),
        "the first character of a file is line 1 column 1, not 0:0"
    );

    let later = diagnostic(11, 4, "m");
    assert!(format_diagnostic(&later, "a.rs").starts_with("a.rs:12:5 "));
}

/// The noise filter removes exactly what omp's removes.
///
/// rustc appends a "for further information" line to many errors and some servers emit a bare
/// URL. Both cost tokens and tell a model nothing it can act on.
#[test]
fn noise_lines_are_stripped_like_omp() {
    let noisy = json!({
        "range": {"start": {"line": 4, "character": 8}, "end": {"line": 4, "character": 9}},
        "message": "error: aborting\nfor further information visit https://doc.rust-lang.org/x\nhttps://bare.example",
        "severity": 1,
        "source": "rustc",
        "code": "E0001"
    });
    assert_eq!(
        format_diagnostic(&noisy, "c.rs"),
        "c.rs:5:9 [error] [rustc] error: aborting (E0001)"
    );

    // A genuine multi-line message keeps its lines: the filter is two shapes, not "one line
    // only". rustc notes and TS related-information both matter to a model.
    let multiline = diagnostic(0, 0, "first line\nsecond line");
    assert_eq!(
        format_diagnostic(&multiline, "d.rs"),
        "d.rs:1:1 [error] first line\nsecond line"
    );

    // A URL *inside* a line is not noise, only a line that is nothing but a URL.
    assert_eq!(
        strip_noise("see https://example.com for details"),
        "see https://example.com for details"
    );
}

/// A tab in the message survives formatting.
///
/// Sanitisation is [`crate::display`]'s job and happens at render time. Doing it here would
/// change the string the ledger deduplicates, so a server that varied its whitespace would
/// defeat the dedup -- and it would put tab handling in two places.
#[test]
fn formatting_leaves_whitespace_for_the_display_layer() {
    let tabbed = diagnostic(0, 0, "too many\targuments");
    assert_eq!(
        format_diagnostic(&tabbed, "f.go"),
        "f.go:1:1 [error] too many\targuments"
    );
}

/// **A code of `0` is kept here and dropped by omp.** A deliberate divergence.
///
/// omp writes `diagnostic.code ? ...`, and `0` is falsey in JavaScript, so a diagnostic whose
/// code is the number zero loses it. Verified against their formatter, which prints
/// `"a.ts:1:1 [warning] weird"` for exactly this input.
///
/// A code of 0 is legal in LSP, so this is a bug in theirs rather than a behaviour to
/// reproduce. Recorded here because their tests are otherwise authoritative and a future reader
/// comparing the two will find this row disagreeing.
#[test]
fn a_zero_code_is_kept_although_omp_drops_it() {
    let zero_code = json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        "message": "weird",
        "severity": 2,
        "code": 0
    });
    assert_eq!(
        format_diagnostic(&zero_code, "a.ts"),
        "a.ts:1:1 [warning] weird (0)",
        "a legal code of 0 must survive; omp loses it to a falsey check"
    );
}

/// Summaries match omp, including the wording and the plural-always `(s)`.
#[test]
fn summaries_match_omp() {
    assert_eq!(summarize(&[]), "no issues");

    let mixed: Vec<Value> = [1, 1, 2, 3, 4, 7]
        .iter()
        .map(|severity| {
            let mut d = diagnostic(0, 0, "m");
            d["severity"] = json!(severity);
            d
        })
        .collect();
    assert_eq!(
        summarize(&mixed),
        "2 error(s), 1 warning(s), 1 info(s), 1 hint(s)",
        "an out-of-spec severity is listed nowhere in the summary, as in omp"
    );
}

/// The summary can be recovered from formatted strings, which is what the ledger leaves behind.
///
/// By the time a summary is wanted the original `Diagnostic` values are gone: the ledger reduces
/// formatted messages. omp has the same pair of functions for the same reason.
#[test]
fn a_summary_can_be_rebuilt_from_formatted_messages() {
    let messages: Vec<String> = vec![
        "a.rs:1:1 [error] one".to_string(),
        "a.rs:2:1 [warning] two".to_string(),
        "b.rs:1:1 [error] three".to_string(),
    ];
    let (summary, errored) = summarize_formatted(&messages);
    assert_eq!(summary, "2 error(s), 1 warning(s)");
    assert!(errored, "two errors were reported but `errored` was false");

    let (clean, errored) = summarize_formatted(&["a.rs:1:1 [hint] tidy".to_string()]);
    assert_eq!(clean, "1 hint(s)");
    assert!(!errored, "a hint is not an error");

    assert_eq!(summarize_formatted(&[]), ("no issues".to_string(), false));
}

/// A severity marker is found after the path, not only at the start.
///
/// The path precedes it, so an anchored match would count nothing and every summary would read
/// "no issues" -- the worst possible failure for this function, since it would report a clean
/// file over a broken one.
#[test]
fn the_severity_marker_is_found_after_the_path() {
    let (summary, errored) =
        summarize_formatted(&["deep/nested/path.rs:120:44 [error] boom".to_string()]);
    assert_eq!(summary, "1 error(s)");
    assert!(errored);
}

/// Sorting puts errors first and is stable for equal diagnostics.
///
/// A server may publish in any order. Without the tie-breaks the list reshuffles between
/// identical runs, which makes comparing two runs impossible.
#[test]
fn sorting_puts_the_worst_first_and_is_otherwise_stable() {
    let mut diagnostics = vec![
        {
            let mut d = diagnostic(5, 0, "a hint");
            d["severity"] = json!(4);
            d
        },
        diagnostic(9, 0, "an error later in the file"),
        {
            let mut d = diagnostic(1, 0, "a warning");
            d["severity"] = json!(2);
            d
        },
        diagnostic(2, 0, "an error earlier in the file"),
    ];
    sort_diagnostics(&mut diagnostics);

    let order: Vec<&str> = diagnostics
        .iter()
        .map(|d| d["message"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        order,
        vec![
            "an error earlier in the file",
            "an error later in the file",
            "a warning",
            "a hint",
        ],
        "errors first, then by position within a severity"
    );
}

/// Two diagnostics identical but for the message sort by message, not by input order.
#[test]
fn equal_positions_sort_by_message() {
    let mut diagnostics = vec![diagnostic(0, 0, "zebra"), diagnostic(0, 0, "aardvark")];
    sort_diagnostics(&mut diagnostics);
    assert_eq!(diagnostics[0]["message"], "aardvark");
}

/// A malformed diagnostic formats rather than panicking.
///
/// Servers send things outside the spec, and this crate's stated position is to tolerate them.
/// A missing range or message must produce a usable line, because the alternative -- dropping
/// it -- loses a real problem the server was trying to report.
#[test]
fn a_malformed_diagnostic_still_formats() {
    assert_eq!(
        format_diagnostic(&json!({}), "a.rs"),
        "a.rs:1:1 [error] ",
        "an empty diagnostic still names its file and severity"
    );
    assert_eq!(
        format_diagnostic(&json!({"message": "no range"}), "a.rs"),
        "a.rs:1:1 [error] no range"
    );
    // A range with no `start`, which some servers send for whole-file diagnostics.
    assert_eq!(
        format_diagnostic(&json!({"range": {}, "message": "whole file"}), "a.rs"),
        "a.rs:1:1 [error] whole file"
    );
}

/// **The first severity marker by position wins, not the worst one present.**
///
/// omp matches `/\[(error|warning|info|hint)\]/i`, which finds the earliest marker. My first
/// version looped over the four names and took the first found *anywhere*, which orders by
/// severity instead of position.
///
/// Expectations printed by omp's `summarizeDiagnosticMessages` in node:
///
/// ```text
/// "[warning] cast produces [error] string"  ->  1 warning(s), errored = false
/// "[hint] see [warning] above"              ->  1 hint(s),    errored = false
/// ```
///
/// Mine said "1 error(s), errored = true" for the first. A diagnostic quoting another diagnostic
/// is ordinary in TypeScript and rustc output, so this reported warnings as errors on real input
/// -- and `errored` carries that all the way to the caller.
///
/// Found by an adversarial reviewer on the seventh pass. It is the same transcribed-by-eye mistake
/// the formatter port existed to eliminate, made inside the port: faithful to the format string,
/// unfaithful to the regex beside it.
#[test]
fn the_first_marker_by_position_decides_the_severity() {
    let cases: &[(&str, &str, bool)] = &[
        (
            "src/a.ts:1:1 [warning] cast produces [error] string",
            "1 warning(s)",
            false,
        ),
        (
            "src/a.ts:1:1 [hint] see [warning] above",
            "1 hint(s)",
            false,
        ),
        ("src/a.ts:1:1 [error] plain", "1 error(s)", true),
    ];
    for (message, summary, errored) in cases {
        assert_eq!(
            summarize_formatted(&[message.to_string()]),
            (summary.to_string(), *errored),
            "for {message:?}, which omp's regex summarises as {summary:?}"
        );
    }
}

/// A bracketed word that is not a severity does not stop the scan.
///
/// The path may contain brackets, and a message may say `[note]` or `[deprecated]`. Stopping at the
/// first `[` would find nothing and report "no issues" over a real error, which is the worst
/// direction for this function to fail.
#[test]
fn a_non_severity_bracket_does_not_end_the_search() {
    let (summary, errored) =
        summarize_formatted(&["a.ts:1:1 [note] [deprecated] [error] boom".to_string()]);
    assert_eq!(summary, "1 error(s)");
    assert!(errored);

    // And a message with no marker at all is counted nowhere rather than guessed at.
    assert_eq!(
        summarize_formatted(&["a.ts:1:1 something unformatted".to_string()]),
        ("no issues".to_string(), false)
    );
}
