//! Behaviour spec for applying hunks.
//!
//! Scenarios from oh-my-pi's `test/core/apply-patch.test.ts` and
//! `apply-patch-adverserial.test.ts`: ambiguity is rejected, hunks apply
//! regardless of order, out-of-range hints are refused, indentation is adjusted
//! and file shape survives.

use super::*;
use crate::hunks::parse_diff_hunks;

fn apply(content: &str, diff: &str) -> Result<String, ApplyError> {
    let hunks = parse_diff_hunks(diff).expect("the diff should parse");
    apply_hunks(content, &hunks)
}

#[test]
fn a_simple_replacement_applies() {
    let out = apply("alpha\nbeta\ngamma\n", "@@\n-beta\n+BETA").expect("should apply");
    assert_eq!(out, "alpha\nBETA\ngamma\n");
}

#[test]
fn context_lines_anchor_the_change() {
    let out = apply(
        "one\ntwo\nthree\n",
        "@@\n one\n-two\n+TWO\n three",
    )
    .expect("should apply");
    assert_eq!(out, "one\nTWO\nthree\n");
}

/// omp's "rejects ambiguous changeContext matches". Picking the first
/// occurrence would silently edit code the caller never looked at.
#[test]
fn an_ambiguous_target_is_refused_rather_than_guessed() {
    let content = "if (a) {\n  return foo;\n}\nif (b) {\n  return foo;\n}\n";
    let error = apply(content, "@@\n-  return foo;\n+  return bar;")
        .expect_err("two identical targets must be refused");

    assert!(
        matches!(error, ApplyError::Ambiguous { .. }),
        "expected ambiguity, got {error:?}"
    );
    assert!(
        error.message().contains("ambiguous"),
        "{}",
        error.message()
    );
}

/// The message has to say how to disambiguate, or the model retries the same
/// patch.
#[test]
fn the_ambiguity_message_says_how_to_fix_it() {
    let content = "x = 1;\nx = 1;\n";
    let error = apply(content, "@@\n-x = 1;\n+x = 2;").expect_err("ambiguous");
    let message = error.message();

    assert!(message.contains("context lines"), "{message}");
    assert!(message.contains("@@"), "{message}");
}

/// An @@ header naming the enclosing scope is exactly how to disambiguate, so
/// it must actually work.
#[test]
fn an_at_header_disambiguates_repeated_lines() {
    let content = "fn a() {\n  value = 1;\n}\nfn b() {\n  value = 1;\n}\n";
    let out = apply(content, "@@ fn b() {\n-  value = 1;\n+  value = 2;").expect("should apply");

    assert_eq!(out, "fn a() {\n  value = 1;\n}\nfn b() {\n  value = 2;\n}\n");
}

#[test]
fn a_missing_target_reports_the_closest_match() {
    let content = "alpha\nlet value = compute();\ngamma\n";
    let error = apply(content, "@@\n-let value = compute_all();\n+let value = 0;")
        .expect_err("not in the file");

    match &error {
        ApplyError::NotFound { closest, .. } => {
            let (line, text) = closest.as_ref().expect("something is always closest");
            assert_eq!(*line, 2);
            assert!(text.contains("compute()"), "{text}");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(
        error.message().contains("Re-read"),
        "the message should say what to do: {}",
        error.message()
    );
}

/// omp's "applies hunks regardless of order": each hunk is located
/// independently, so a patch listing them out of order still works.
#[test]
fn hunks_apply_regardless_of_order() {
    let content = "one\ntwo\nthree\nfour\n";
    let out = apply(content, "@@\n-four\n+FOUR\n@@\n-two\n+TWO").expect("should apply");

    assert_eq!(out, "one\nTWO\nthree\nFOUR\n");
}

#[test]
fn several_hunks_apply_in_one_pass() {
    let content = "a\nb\nc\nd\n";
    let out = apply(content, "@@\n-a\n+A\n@@\n-c\n+C").expect("should apply");
    assert_eq!(out, "A\nb\nC\nd\n");
}

#[test]
fn an_insertion_with_no_removals_adds_lines() {
    let content = "one\nthree\n";
    let out = apply(content, "@@ one\n+two").expect("should apply");
    assert_eq!(out, "one\ntwo\nthree\n");
}

/// omp's "rejects out-of-range line hints for insertions". A hint past the end
/// of the file describes a location that does not exist.
#[test]
fn an_out_of_range_line_hint_is_refused() {
    let error = apply("one\ntwo\n", "@@ 500\n+added").expect_err("hint is past the end");

    assert!(
        matches!(error, ApplyError::HintOutOfRange { .. }),
        "expected HintOutOfRange, got {error:?}"
    );
}

/// A context header that is not in the file is a stale patch, not a hint to
/// ignore.
#[test]
fn a_missing_context_header_is_refused() {
    let error = apply("one\ntwo\n", "@@ fn nowhere()\n-one\n+ONE")
        .expect_err("the context is not in the file");

    assert!(
        matches!(error, ApplyError::ContextNotFound { .. }),
        "expected ContextNotFound, got {error:?}"
    );
}

/// A patch that changes nothing is a mistake worth reporting: the model
/// believes it made an edit.
#[test]
fn a_patch_that_changes_nothing_is_refused() {
    let error = apply("alpha\n", "@@\n alpha").expect_err("nothing changed");
    assert!(matches!(error, ApplyError::NoOp), "got {error:?}");
}

/// Fuzzy matching earns its keep here: the code is genuinely present, just
/// indented differently from the patch.
#[test]
fn a_reindented_target_still_applies() {
    let content = "fn main() {\n        let x = 1;\n}\n";
    let out = apply(content, "@@\n-    let x = 1;\n+    let x = 2;").expect("should apply");

    assert!(out.contains("let x = 2;"), "{out}");
}

/// The replacement takes the file's indentation, not the patch's, so the edit
/// does not leave misaligned code behind.
#[test]
fn the_replacement_is_reindented_to_the_file() {
    let content = "fn main() {\n        let x = 1;\n}\n";
    let out = apply(content, "@@\n-    let x = 1;\n+    let x = 2;").expect("should apply");

    assert_eq!(
        out, "fn main() {\n        let x = 2;\n}\n",
        "the replacement should keep the file's 8-space indent"
    );
}

#[test]
fn a_crlf_file_stays_crlf() {
    let out = apply("alpha\r\nbeta\r\n", "@@\n-beta\n+BETA").expect("should apply");
    assert_eq!(out, "alpha\r\nBETA\r\n");
}

#[test]
fn a_bom_survives_an_edit() {
    let out = apply("\u{feff}alpha\nbeta\n", "@@\n-beta\n+BETA").expect("should apply");
    assert_eq!(out, "\u{feff}alpha\nBETA\n");
}

#[test]
fn a_file_without_a_trailing_newline_does_not_gain_one() {
    let out = apply("alpha\nbeta", "@@\n-beta\n+BETA").expect("should apply");
    assert_eq!(out, "alpha\nBETA");
}

/// Multi-line blocks are matched as a unit, so a patch replacing several lines
/// does not have to match them one at a time.
#[test]
fn a_multi_line_block_is_replaced_as_a_unit() {
    let content = "start\nold one\nold two\nend\n";
    let out = apply(content, "@@\n-old one\n-old two\n+new one\n+new two\n+new three")
        .expect("should apply");

    assert_eq!(out, "start\nnew one\nnew two\nnew three\nend\n");
}

#[test]
fn a_removal_with_no_replacement_deletes_lines() {
    let out = apply("a\nb\nc\n", "@@\n-b").expect("should apply");
    assert_eq!(out, "a\nc\n");
}

/// An exact match decides on its own. A file containing both an exact and an
/// approximate occurrence patches the one the caller actually wrote.
#[test]
fn an_exact_match_is_not_ambiguous_against_a_fuzzy_one() {
    let content = "let  x  =  1;\nlet x = 1;\n";
    let out = apply(content, "@@\n-let x = 1;\n+let x = 2;").expect("the exact match decides");

    assert_eq!(out, "let  x  =  1;\nlet x = 2;\n");
}

/// Create content is normalised so a file made by a patch has the same shape as
/// one made by any other tool.
#[test]
fn created_content_gains_a_trailing_newline() {
    assert_eq!(create_content("hello"), "hello\n");
    assert_eq!(create_content("hello\n"), "hello\n");
    assert_eq!(create_content(""), "");
    assert_eq!(create_content("a\r\nb"), "a\nb\n");
}


/// Two *approximate* occurrences are as ambiguous as two exact ones, and the
/// consequence is worse: neither is what the patch literally says, so picking
/// one is a guess on top of a guess.
///
/// Found by mutation testing: removing the second fuzzy search broke nothing,
/// because every other ambiguity test used exact duplicates and those are
/// caught by the earlier exact-count check.
#[test]
fn two_approximate_targets_are_also_refused() {
    // Neither line matches the patch exactly: both differ from it only in
    // spacing, so both are fuzzy candidates.
    let content = "let  value  =  1;\nother();\nlet value  =  1;\n";
    let error = apply(content, "@@\n-let value = 1;\n+let value = 2;")
        .expect_err("two fuzzy candidates must be refused");

    assert!(
        matches!(error, ApplyError::Ambiguous { .. }),
        "expected ambiguity, got {error:?}"
    );
}

/// A single fuzzy candidate still applies. The refusal above must be about
/// there being two, not about fuzzy matching being distrusted.
#[test]
fn one_approximate_target_still_applies() {
    let content = "let  value  =  1;\nother();\n";
    let out = apply(content, "@@\n-let value = 1;\n+let value = 2;")
        .expect("a single fuzzy candidate should apply");

    assert!(out.contains("value = 2;"), "{out}");
}
