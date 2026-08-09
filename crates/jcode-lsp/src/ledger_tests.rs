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
