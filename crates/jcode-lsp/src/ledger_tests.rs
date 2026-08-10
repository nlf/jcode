//! Ledger tests. Group H, all nine of omp's cases plus the ones their fixtures
//! imply but do not assert.

use super::*;

const TYPE_ERROR: &str = "src/a.ts:12:5 [error] pyright: Broken import (E1)";
const TYPE_ERROR_SHIFTED: &str = "src/a.ts:99:27 [error] pyright: Broken import (E1)";
const PRIVATE_IMPORT: &str = "src/a.ts:3:1 [warning] pyright: Private import (E7)";
const PRIVATE_IMPORT_SHIFTED: &str = "src/a.ts:40:9 [warning] pyright: Private import (E7)";
const NEW_ERROR: &str = "src/a.ts:50:1 [error] pyright: Something else (E9)";

fn messages(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn the_first_report_for_a_file_passes_everything_through() {
    let mut ledger = Ledger::new();
    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));
    assert_eq!(reduced.messages, messages(&[TYPE_ERROR, PRIVATE_IMPORT]));
    assert!(reduced.errored, "one of them is an error");
}

#[test]
fn an_identical_second_report_is_fully_suppressed() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    assert!(reduced.messages.is_empty(), "nothing new to say");
    assert!(
        !reduced.errored,
        "errored is about the reduced set: nothing new means nothing to report"
    );
}

/// **The case the whole mechanism exists for.** Insert a line at the top of a file
/// and every diagnostic below shifts down, so location-sensitive comparison reports
/// the entire file as freshly broken after a one-line edit.
#[test]
fn a_diagnostic_that_only_moved_is_suppressed() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));

    let reduced = ledger.reduce(
        "a.ts",
        &messages(&[TYPE_ERROR_SHIFTED, PRIVATE_IMPORT_SHIFTED]),
    );
    assert!(
        reduced.messages.is_empty(),
        "shifted lines are the same problems, got {:?}",
        reduced.messages
    );
}

/// `errored` is recomputed over the **reduced** set. A batch where only a warning
/// is new must not be reported as errored: the caller uses this to decide whether
/// the edit broke something.
#[test]
fn only_genuinely_new_messages_survive_and_errored_is_recomputed() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));

    let reduced = ledger.reduce(
        "a.ts",
        &messages(&[TYPE_ERROR_SHIFTED, PRIVATE_IMPORT_SHIFTED, NEW_ERROR]),
    );
    assert_eq!(reduced.messages, messages(&[NEW_ERROR]));
    assert!(reduced.errored, "the new one is an error");
}

/// Only a new *warning*: not errored, even though an error is still present in the
/// file. Inheriting the batch's severity would tell the caller its edit broke
/// something when the breakage predates it.
#[test]
fn a_new_warning_alongside_an_old_error_is_not_reported_as_errored() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));

    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));
    assert_eq!(reduced.messages, messages(&[PRIVATE_IMPORT]));
    assert!(
        !reduced.errored,
        "the only new diagnostic is a warning, so this edit broke nothing"
    );
}

/// **Suppression must not be permanent.** Fixing an error and reintroducing it must
/// report it again, or the model is blind to it for the rest of the session — a far
/// worse failure than the repetition this exists to avoid.
#[test]
fn a_diagnostic_resurfaces_after_being_fixed() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    // Fixed: the file publishes nothing.
    let clean = ledger.reduce("a.ts", &[]);
    assert!(clean.messages.is_empty());

    // Reintroduced.
    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    assert_eq!(
        reduced.messages,
        messages(&[TYPE_ERROR]),
        "a reintroduced error must be reported again"
    );
}

/// A diagnostic that disappears while *others* remain must also resurface. This is
/// the harder version of the case above: the file's set is non-empty throughout, so
/// a ledger that only forgets on a fully clean file would still suppress it.
#[test]
fn a_diagnostic_resurfaces_even_when_the_file_was_never_fully_clean() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));
    // The error is fixed; the warning remains, so the file is never clean.
    ledger.reduce("a.ts", &messages(&[PRIVATE_IMPORT]));

    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR, PRIVATE_IMPORT]));
    assert_eq!(
        reduced.messages,
        messages(&[TYPE_ERROR]),
        "the set must be replaced per publish, not accumulated"
    );
}

#[test]
fn files_are_tracked_independently() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));

    let reduced = ledger.reduce("b.ts", &messages(&[TYPE_ERROR]));
    assert_eq!(
        reduced.messages,
        messages(&[TYPE_ERROR]),
        "the same problem in another file is news about that file"
    );
}

/// A deleted or renamed file must be forgotten, or its set suppresses diagnostics
/// for whatever later takes the path.
#[test]
fn forgetting_a_file_makes_its_diagnostics_new_again() {
    let mut ledger = Ledger::new();
    ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    ledger.forget("a.ts");

    let reduced = ledger.reduce("a.ts", &messages(&[TYPE_ERROR]));
    assert_eq!(reduced.messages, messages(&[TYPE_ERROR]));
}

// =============================================================================
// identity
// =============================================================================

#[test]
fn identity_strips_the_location_and_keeps_the_problem() {
    assert_eq!(
        identity("src/a.ts:12:5 [error] pyright: Broken import (E1)"),
        "[error] pyright: Broken import (E1)"
    );
    assert_eq!(
        identity(TYPE_ERROR),
        identity(TYPE_ERROR_SHIFTED),
        "the same problem at a different location has one identity"
    );
}

/// **A colon in the path must not be mistaken for the line number.** This is omp's
/// own fixture (`fixtures/pkg:2/example.ts:12:5`), and splitting on the first colon
/// would leave `2/example.ts:12:5` in the identity — defeating dedup entirely for
/// such paths, silently.
#[test]
fn a_path_containing_colons_is_still_stripped_correctly() {
    let first = "fixtures/pkg:2/example.ts:12:5 [error] pyright: Broken import (E1)";
    let shifted = "fixtures/pkg:2/example.ts:99:27 [error] pyright: Broken import (E1)";

    assert_eq!(identity(first), "[error] pyright: Broken import (E1)");
    assert_eq!(identity(first), identity(shifted));
}

/// A Windows path has a drive colon too, which is the same hazard.
#[test]
fn a_windows_drive_path_is_stripped_correctly() {
    assert_eq!(
        identity("C:/src/a.ts:12:5 [error] rust-analyzer: mismatched types"),
        "[error] rust-analyzer: mismatched types"
    );
}

/// Severity and code are part of the problem. A reclassification is news.
#[test]
fn severity_and_code_changes_are_different_diagnostics() {
    let base = identity("src/a.ts:1:1 [error] pyright: Broken import (E1)");
    assert_ne!(
        identity("src/a.ts:1:1 [warning] pyright: Broken import (E1)"),
        base,
        "a downgrade to warning is a different diagnostic"
    );
    assert_ne!(
        identity("src/a.ts:1:1 [error] pyright: Broken import (E2)"),
        base,
        "a different code is a different diagnostic"
    );
}

/// An unparseable message keeps its full text. The safe direction: at worst it
/// fails to dedup, where guessing at a prefix could strip real content and merge
/// two different problems.
#[test]
fn a_message_with_no_location_prefix_is_kept_whole() {
    let message = "pyright: Broken import (E1)";
    assert_eq!(identity(message), message);
    assert_eq!(identity(""), "");
    assert_eq!(
        identity("[error] no location here"),
        "[error] no location here"
    );
}

/// A location-shaped string with nothing after it is not a prefix: stripping it
/// would leave an empty identity, and every such message would then dedup against
/// every other.
#[test]
fn a_bare_location_with_no_message_is_not_stripped() {
    assert_eq!(identity("src/a.ts:1:1"), "src/a.ts:1:1");
}

/// Partial location shapes must not be stripped either.
#[test]
fn an_incomplete_location_is_not_stripped() {
    // Line but no column.
    assert_eq!(
        identity("src/a.ts:12 [error] thing"),
        "src/a.ts:12 [error] thing"
    );
    // Non-numeric where the line should be.
    assert_eq!(
        identity("src/a.ts:ab:5 [error] thing"),
        "src/a.ts:ab:5 [error] thing"
    );
}

/// The message text may itself contain a `n:m ` sequence. Scanning from the right
/// would find *that* rather than the real prefix, so the location must be the
/// leftmost valid candidate... which is exactly what this asserts is handled.
#[test]
fn a_location_shape_inside_the_message_does_not_confuse_identity() {
    let with_range = "src/a.ts:12:5 [error] expected 1:2 got 3:4 here";
    let shifted = "src/a.ts:99:1 [error] expected 1:2 got 3:4 here";
    assert_eq!(
        identity(with_range),
        identity(shifted),
        "two reports of the same problem must share an identity even when the \
         message text contains its own colon-number pairs"
    );
    // And the message survives intact rather than being cut at the inner pair.
    assert!(
        identity(with_range).contains("expected 1:2 got 3:4"),
        "got {:?}",
        identity(with_range)
    );
}

/// **A location inside the message is not the location prefix.**
///
/// Found by an adversarial reviewer. Diagnostics routinely cite a second place --
/// "declared at", "first defined here", "previous definition" -- and the identity
/// must be taken from the *leading* location only.
///
/// The first implementation searched from the right, so both messages below stripped
/// through the embedded `src/b.ts:3:1` and became `"previously"`. Two diagnostics
/// about different files then shared an identity, and the ledger suppressed the second
/// as already-reported: the model is never told about a real error, which is the worst
/// failure this module can have. Measured before the fix; both returned `"previously"`.
#[test]
fn a_location_inside_the_message_does_not_become_the_identity() {
    let first = "src/a.ts:12:5 [error] declared at src/b.ts:3:1 previously";
    let second = "src/c.ts:9:9 [error] declared at src/b.ts:3:1 previously";

    assert_eq!(
        identity(first),
        "[error] declared at src/b.ts:3:1 previously",
        "only the leading location may be stripped"
    );
    // Same identity here is correct: same problem, different place, which is exactly
    // what identity is for.
    assert_eq!(identity(first), identity(second));

    // But a genuinely different message must not collapse into it.
    let different = "src/c.ts:9:9 [error] declared at src/b.ts:3:1 elsewhere";
    assert_ne!(
        identity(first),
        identity(different),
        "different diagnostics must not share an identity"
    );
}

/// Two diagnostics differing only past an embedded location stay distinct in the
/// ledger, not just in `identity`.
///
/// The unit above proves the string function; this proves the consequence, since the
/// bug's damage was a suppressed report rather than a wrong string.
#[test]
fn diagnostics_differing_after_an_embedded_location_are_both_reported() {
    let mut ledger = Ledger::new();

    let first = ledger.reduce(
        "src/a.ts",
        &["src/a.ts:12:5 [error] declared at src/b.ts:3:1 previously".to_string()],
    );
    assert_eq!(first.messages.len(), 1);

    let second = ledger.reduce(
        "src/a.ts",
        &["src/a.ts:12:5 [error] declared at src/b.ts:3:1 elsewhere".to_string()],
    );
    assert_eq!(
        second.messages.len(),
        1,
        "a different diagnostic was suppressed as a duplicate: {second:?}"
    );
}

/// A colon inside the path still resolves correctly, which is what the rightmost
/// search was trying to protect.
///
/// `fixtures/pkg:2/example.ts:12:5` is omp's own fixture. Leftmost scanning handles it
/// because the candidate at `pkg:2` fails the `digits` + whitespace test, so nothing
/// was traded away by changing direction. This test is why that claim is checkable.
#[test]
fn a_colon_in_the_path_is_not_mistaken_for_the_line_number() {
    assert_eq!(
        identity("fixtures/pkg:2/example.ts:12:5 [error] boom"),
        "[error] boom"
    );
    // Two colons in the path.
    assert_eq!(identity("a:1/b:2/c.ts:9:3 [warning] hm"), "[warning] hm");
}

/// **Any whitespace separates the location, and all of it is consumed.**
///
/// omp's pattern ends `\s+`. Mine required exactly one literal space, and both halves
/// of that were wrong. Found by an adversarial reviewer.
///
/// A tab did not strip at all, so a tab-separated diagnostic kept its whole location
/// prefix and could never dedup -- and Go tooling emits tab-separated diagnostics, so
/// that was a language's worth of the ledger silently not working. A double space left
/// one behind, giving the identity `" [error] x"`, which never matches the
/// single-spaced form of the same diagnostic.
///
/// Values below are differential: printed from omp's actual regex in node, not from
/// what I expected it to do.
#[test]
fn any_whitespace_separates_the_location_from_the_message() {
    // (input, what omp's /^.*?:\d+:\d+\s+/ produces)
    let cases: &[(&str, &str)] = &[
        ("src/a.ts:12:5 [error] x", "[error] x"),
        ("src/a.ts:12:5\t[error] x", "[error] x"),
        ("src/a.ts:12:5  [error] x", "[error] x"),
        ("src/a.ts:12:5\n[error] x", "[error] x"),
        // No whitespace at all: not a location prefix, kept whole.
        ("src/a.ts:12:5[error] x", "src/a.ts:12:5[error] x"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            identity(input),
            *expected,
            "for {input:?}, which omp's regex maps to {expected:?}"
        );
    }
}

/// The same diagnostic spelled with different whitespace dedups.
///
/// The consequence of the unit above, and the reason it matters: a server that varies
/// its spacing between publishes would otherwise report the same problem twice.
#[test]
fn whitespace_spelling_does_not_defeat_deduplication() {
    let mut ledger = Ledger::new();

    let first = ledger.reduce("src/a.ts", &["src/a.ts:12:5 [error] boom".to_string()]);
    assert_eq!(first.messages.len(), 1);

    // Tab-separated, and at a different location: same problem.
    let second = ledger.reduce("src/a.ts", &["src/a.ts:99:1\t[error] boom".to_string()]);
    assert!(
        second.messages.is_empty(),
        "a tab-separated repeat was reported again: {second:?}"
    );
}

/// The location parser's edge cases, differential against omp's regex.
///
/// Written because changing `location_after` to consume any whitespace altered its
/// control flow, and "requiring at least one whitespace still rejects a bare `a:1:2`"
/// was a claim I believed rather than knew. It holds, and so do ten neighbours -- but
/// the way to establish that is to run omp's actual pattern over the awkward inputs
/// and compare, not to reason about it.
///
/// Each right-hand value was printed by `/^.*?:\d+:\d+\s+/` in node.
#[test]
fn the_location_parsers_edges_match_omps_regex() {
    let cases: &[(&str, &str)] = &[
        // No whitespace after the location: not a prefix, kept whole. This is the one
        // the whitespace requirement exists for.
        ("a:1:2", "a:1:2"),
        // Trailing whitespace and nothing else: the prefix is the entire string, so the
        // identity is empty. Odd, and omp agrees, so it stays.
        ("a:1:2 ", ""),
        ("a:1:2  ", ""),
        ("a:1:2\t", ""),
        // An empty path is still a location.
        (":1:2 x", "x"),
        // No colon before the digits: `1:2` is not `path:line:col`.
        ("1:2 x", "1:2 x"),
        // A missing column, and a missing line.
        ("a:1: x", "a:1: x"),
        ("a::2 x", "a::2 x"),
        // Leftmost wins, so the second location survives in the identity.
        ("a:1:2 x:3:4 y", "x:3:4 y"),
        // Zeroes are digits.
        ("a:0:0 x", "x"),
        // A digit run must end at the colon, not run into a word.
        ("a:1:2x y", "a:1:2x y"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            identity(input),
            *expected,
            "for {input:?}, which omp's regex maps to {expected:?}"
        );
    }
}

/// **Only the first line can carry the location prefix.**
///
/// omp's pattern is anchored and `.` does not match `\n`, so a location on a later line
/// is part of the message. My leftmost scan crossed newlines, and the damage is the same
/// over-merge as the round-one bug it was introduced to fix: a multi-line diagnostic
/// whose first line has no location got stripped down to whatever followed a location
/// further in, so two unrelated failures could share an identity and one would be
/// silently suppressed.
///
/// rustc notes and TypeScript related-information are both multi-line, so this is the
/// ordinary shape. Found by an adversarial reviewer on the third pass, in the area I had
/// asked them to look at hardest.
///
/// Values are differential, printed by omp's regex in node.
#[test]
fn a_location_on_a_later_line_is_part_of_the_message() {
    let cases: &[(&str, &str)] = &[
        // No location on line one: nothing is stripped, even though line two has one.
        (
            "Something failed\n at foo.ts:1:2 bar",
            "Something failed\n at foo.ts:1:2 bar",
        ),
        (
            "no location\nsecond.ts:3:4 message",
            "no location\nsecond.ts:3:4 message",
        ),
        // A location on line one strips, and the later lines survive untouched.
        (
            "src/a.ts:12:5 [error] x\n note: see src/b.ts:9:9 here",
            "[error] x\n note: see src/b.ts:9:9 here",
        ),
        (
            "src/a.ts:1:2 first\nsrc/b.ts:3:4 second",
            "first\nsrc/b.ts:3:4 second",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            identity(input),
            *expected,
            "for {input:?}, which omp's regex maps to {expected:?}"
        );
    }
}

/// Two multi-line diagnostics that differ only in a later line stay distinct.
///
/// The consequence of the unit above. Both messages below end in `bar` and have no
/// first-line location; an unbounded scan reduced both to `"bar"` and the ledger dropped
/// the second.
#[test]
fn multi_line_diagnostics_are_not_merged_by_a_shared_tail() {
    let mut ledger = Ledger::new();

    let first = ledger.reduce(
        "src/a.ts",
        &["Something failed\n at foo.ts:1:2 bar".to_string()],
    );
    assert_eq!(first.messages.len(), 1);

    let second = ledger.reduce(
        "src/a.ts",
        &["Something else failed\n at other.ts:5:6 bar".to_string()],
    );
    assert_eq!(
        second.messages.len(),
        1,
        "a distinct multi-line diagnostic was suppressed: {second:?}"
    );
}

/// **The identity of a diagnostic that was actually formatted, not one I typed.**
///
/// Every other test in this file uses a hand-written string like
/// `"src/a.ts:12:5 [error] pyright: Broken import (E1)"`. Those were transcribed from omp's
/// format string by eye, so the whole module rested on my having read a template correctly:
/// a mistake there would have produced a ledger that deduplicated nothing in production while
/// passing every test here.
///
/// `crate::format` exists partly to close that gap. This generates the input from a real
/// `Diagnostic` and asserts the identity comes out as expected, so the two modules are pinned
/// against each other rather than against my reading.
#[test]
fn identity_of_a_real_formatted_diagnostic() {
    let diagnostic = serde_json::json!({
        "range": {"start": {"line": 11, "character": 4}, "end": {"line": 11, "character": 5}},
        "message": "cannot find value `x`",
        "severity": 1,
        "source": "rustc",
        "code": "E0425"
    });

    let formatted = crate::format::format_diagnostic(&diagnostic, "src/main.rs");
    // Confirms the assumption the rest of this file makes about the shape.
    assert_eq!(
        formatted,
        "src/main.rs:12:5 [error] [rustc] cannot find value `x` (E0425)"
    );

    assert_eq!(
        identity(&formatted),
        "[error] [rustc] cannot find value `x` (E0425)",
        "the location is stripped and everything that identifies the problem is kept"
    );
}

/// The same diagnostic at a different position dedups; a different one does not.
///
/// The property the ledger exists for, now driven end to end from `Diagnostic` values through
/// the real formatter rather than from strings chosen to make it work.
#[test]
fn a_real_diagnostic_moving_position_is_deduplicated() {
    let at = |line: i64| {
        serde_json::json!({
            "range": {"start": {"line": line, "character": 4}, "end": {"line": line, "character": 5}},
            "message": "cannot find value `x`",
            "severity": 1,
            "source": "rustc",
            "code": "E0425"
        })
    };

    let mut ledger = Ledger::new();
    let first = crate::format::format_diagnostic(&at(11), "src/main.rs");
    assert_eq!(ledger.reduce("src/main.rs", &[first]).messages.len(), 1);

    // Same problem, twenty lines down after an edit above it.
    let moved = crate::format::format_diagnostic(&at(31), "src/main.rs");
    assert!(
        ledger.reduce("src/main.rs", &[moved]).messages.is_empty(),
        "the same problem at a new position must not be reported twice"
    );

    // A different severity is a different diagnostic: the server reclassified it.
    let mut promoted = at(31);
    promoted["severity"] = serde_json::json!(2);
    let promoted = crate::format::format_diagnostic(&promoted, "src/main.rs");
    assert_eq!(
        ledger.reduce("src/main.rs", &[promoted]).messages.len(),
        1,
        "a warning is not the same diagnostic as an error"
    );
}

/// A formatted multi-line diagnostic keeps its later lines in the identity.
///
/// Ties the newline bound in `strip_location_prefix` to what the formatter actually emits:
/// rustc notes arrive as extra lines in the message, and the formatter preserves them, so the
/// identity has to treat them as content.
#[test]
fn a_formatted_multi_line_diagnostic_keeps_its_notes_in_the_identity() {
    let diagnostic = serde_json::json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        "message": "mismatched types\nnote: expected `u32`, found `String`",
        "severity": 1,
        "source": "rustc"
    });
    let formatted = crate::format::format_diagnostic(&diagnostic, "a.rs");
    let identity = identity(&formatted);

    assert!(
        identity.contains("note: expected"),
        "the note is part of what makes this diagnostic distinct: {identity:?}"
    );
    assert!(
        !identity.starts_with("a.rs"),
        "the leading location must still be stripped: {identity:?}"
    );
}
