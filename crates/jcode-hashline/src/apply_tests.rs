//! Behaviour ported from omp's `format-v2.test.ts`, whose applier cases are the
//! specification for this module.
//!
//! Their four phantom-trailing-line tests are the ones worth reading twice.
//! `"a\nb\n"` splits into three elements, and whether the third is content
//! decides if a model that counted lines from a read deletes one line too many
//! at the end of every newline-terminated file. That is nearly every file.

use super::*;
use crate::parser::parse_ops;

/// Parse and apply in one step, the way a caller would.
fn apply(text: &str, patch: &str) -> String {
    let ops = parse_ops(patch).expect("patch must parse").ops;
    apply_ops(text, &ops).expect("patch must apply").text
}

fn try_apply(text: &str, patch: &str) -> Result<String, String> {
    let ops = parse_ops(patch).expect("patch must parse").ops;
    apply_ops(text, &ops).map(|result| result.text)
}

// ─── omp's applier cases ─────────────────────────────────────────────────────

#[test]
fn a_range_is_replaced_by_its_body_rows_in_order() {
    assert_eq!(apply("a\nb\nc", "PUT 2.=2:\n+before\n+after"), "a\nbefore\nafter\nc");
}

#[test]
fn a_single_line_is_deleted() {
    assert_eq!(apply("a\nb\nc", "CUT 2.=2"), "a\nc");
}

#[test]
fn a_range_is_deleted() {
    assert_eq!(apply("a\nb\nc\nd", "CUT 2.=3"), "a\nd");
}

#[test]
fn inserts_land_before_and_after_their_anchors() {
    assert_eq!(
        apply("a\nb\nc", "PUT <2:\n+before\nPUT >2:\n+after"),
        "a\nbefore\nb\nafter\nc"
    );
}

#[test]
fn head_and_tail_inserts_land_at_the_file_boundaries() {
    assert_eq!(apply("a\nb", "PUT <1:\n+HEAD"), "HEAD\na\nb");
    assert_eq!(apply("a\nb", "PUT >$:\n+TAIL"), "a\nb\nTAIL");
}

/// An empty replacement body deletes, which the parser already reinterprets.
/// Pinned here too because the applier is where the consequence lands.
#[test]
fn an_empty_replacement_body_deletes_the_range() {
    assert_eq!(apply("a\nb\nc\nd", "PUT 2.=3:"), "a\nd");
}

#[test]
fn an_out_of_bounds_anchor_is_refused() {
    let error = try_apply("a\nb", "PUT <4:\n+x").expect_err("must refuse");
    assert!(error.contains("does not exist"), "{error}");
}

// ─── the phantom trailing line ───────────────────────────────────────────────

/// `"a\nb\n"` splits into `["a", "b", ""]`, so line 3 is the trailing newline
/// rather than content. Deleting it would only strip the newline, which is not
/// what a model asking to cut line 3 means.
#[test]
fn cutting_the_phantom_trailing_line_is_ignored() {
    assert_eq!(apply("a\nb\n", "CUT 3"), "a\nb\n");
}

/// A range ending at the phantom ends at the last real line. Without this a
/// model that read a newline-terminated file and asked for `2-3` deletes one
/// line too many.
#[test]
fn a_cut_range_ending_at_the_phantom_ends_at_the_last_real_line() {
    assert_eq!(apply("a\nb\n", "CUT 2.=3"), "a\n");
}

#[test]
fn a_replace_range_ending_at_the_phantom_ends_at_the_last_real_line() {
    assert_eq!(apply("a\nb\n", "PUT 2.=3:\n+B"), "a\nB\n");
}

/// Insertion is different from deletion: the phantom is a legitimate place to
/// append, so an anchor there stays valid.
#[test]
fn inserting_after_the_phantom_is_still_allowed() {
    assert_eq!(apply("a\nb\n", "PUT >3:\n+tail"), "a\nb\n\ntail");
}

/// Without a trailing newline the last line is real content and must be
/// deletable. This is the case the phantom rules must not break.
#[test]
fn the_last_line_of_a_file_without_a_trailing_newline_is_deletable() {
    assert_eq!(apply("a\nb", "CUT 2"), "a");
}

/// Appending to a newline-terminated file must not leave a blank line before
/// the appended content, and must keep the file newline-terminated.
///
/// My first expectation here was `"a\nb\nc"`, which was wrong: dropping the
/// terminator would make every append rewrite the file's final byte and show
/// up as a spurious "no newline at end of file" in every diff.
#[test]
fn appending_to_a_newline_terminated_file_preserves_the_terminator() {
    assert_eq!(apply("a\nb\n", "PUT >$:\n+c"), "a\nb\nc\n");
}

// ─── original-line semantics ─────────────────────────────────────────────────

/// The property that makes multi-hunk patches authorable: every anchor names
/// the file as it was read. If edits shifted later anchors, a model would have
/// to simulate its own patch to write the second hunk.
#[test]
fn later_anchors_are_not_shifted_by_earlier_edits() {
    // Deleting line 2 would move line 4 up if anchors were applied serially.
    assert_eq!(apply("a\nb\nc\nd\ne", "CUT 2.=2\nCUT 4.=4"), "a\nc\ne");
}

/// An insertion that adds several lines must not displace a later anchor
/// either, which is the same property in the growing direction.
#[test]
fn a_large_insertion_does_not_displace_a_later_anchor() {
    assert_eq!(
        apply("a\nb\nc", "PUT <1:\n+x\n+y\n+z\nCUT 3.=3"),
        "x\ny\nz\na\nb"
    );
}

#[test]
fn hunks_apply_regardless_of_the_order_they_were_written_in() {
    let ascending = apply("a\nb\nc\nd", "CUT 1.=1\nCUT 3.=3");
    let descending = apply("a\nb\nc\nd", "CUT 3.=3\nCUT 1.=1");
    assert_eq!(ascending, descending);
    assert_eq!(ascending, "b\nd");
}

// ─── file operations ─────────────────────────────────────────────────────────

#[test]
fn rem_empties_the_file_and_reports_removal() {
    let ops = parse_ops("REM").unwrap().ops;
    let result = apply_ops("a\nb\n", &ops).unwrap();
    assert!(result.removed);
    assert_eq!(result.text, "");
}

#[test]
fn mv_reports_its_destination_without_changing_content() {
    let ops = parse_ops("MV new/path.rs").unwrap().ops;
    let result = apply_ops("a\nb\n", &ops).unwrap();
    assert_eq!(result.move_dest.as_deref(), Some("new/path.rs"));
    assert_eq!(result.text, "a\nb\n");
}

/// Line edits above a `MV` apply to the source, and the result is what gets
/// written at the destination.
#[test]
fn edits_above_a_move_apply_before_the_file_relocates() {
    let ops = parse_ops("PUT 1.=1:\n+edited\nMV new/path.rs").unwrap().ops;
    let result = apply_ops("a\nb\n", &ops).unwrap();
    assert_eq!(result.move_dest.as_deref(), Some("new/path.rs"));
    assert_eq!(result.text, "edited\nb\n");
}

// ─── change reporting ────────────────────────────────────────────────────────

#[test]
fn the_first_changed_line_is_reported_for_a_diff_view() {
    let ops = parse_ops("PUT 3.=3:\n+changed").unwrap().ops;
    let result = apply_ops("a\nb\nc\nd", &ops).unwrap();
    assert_eq!(result.first_changed_line, Some(3));
}

/// A patch that changes nothing must say so, so a caller can distinguish "no
/// change needed" from "applied", which is what drives the no-op loop guard.
#[test]
fn an_unchanged_file_reports_no_changed_line() {
    let result = apply_ops("a\nb", &[]).unwrap();
    assert_eq!(result.first_changed_line, None);
    assert_eq!(result.text, "a\nb");
}

// ─── content fidelity ────────────────────────────────────────────────────────

/// Indentation is the content most easily lost, and a diff that drops it is
/// unreadable. Body rows are literal after the sigil.
#[test]
fn body_rows_preserve_leading_whitespace_exactly() {
    assert_eq!(
        apply("fn f() {\n    old();\n}", "PUT 2.=2:\n+        deeply();"),
        "fn f() {\n        deeply();\n}"
    );
}

#[test]
fn a_blank_body_row_writes_a_blank_line() {
    assert_eq!(apply("a\nb", "PUT 1.=1:\n+first\n+\n+third"), "first\n\nthird\nb");
}

/// Replacing one line with many, and many with one, are both size changes the
/// splice must handle without disturbing neighbours.
#[test]
fn a_range_can_grow_or_shrink() {
    assert_eq!(apply("a\nb\nc", "PUT 2.=2:\n+x\n+y\n+z"), "a\nx\ny\nz\nc");
    assert_eq!(apply("a\nb\nc\nd", "PUT 2.=3:\n+one"), "a\none\nd");
}

/// Unicode content must survive a round trip; Rust indexes bytes where the
/// original indexes UTF-16 code units.
#[test]
fn unicode_content_survives_a_round_trip() {
    assert_eq!(apply("héllo\n日本語\n", "PUT 1.=1:\n+émoji 🎉"), "émoji 🎉\n日本語\n");
}

#[test]
fn an_empty_file_can_be_written_into() {
    assert_eq!(apply("", "PUT <1:\n+first line"), "first line");
}

/// Where a replacement body lands *within* its range is only observable when
/// another insertion targets a line inside that same range. Mutation testing
/// found this gap: moving the body from the range's start to its end left
/// every other test green, because a lone replacement reads identically either
/// way.
///
/// It is not cosmetic. The replacement occupies the whole range, so it must
/// come first; an insertion anchored anywhere inside that vanished range
/// follows it. Placing the body last would put replacement text *after*
/// content the model asked to insert mid-range, silently swapping two blocks.
#[test]
fn a_replacement_body_precedes_an_insertion_anchored_inside_the_same_range() {
    for (patch, expected) in [
        ("PUT 2.=3:\n+BODY\nPUT <2:\n+OTHER", "a\nBODY\nOTHER\nd"),
        ("PUT 2.=3:\n+BODY\nPUT >2:\n+OTHER", "a\nBODY\nOTHER\nd"),
        ("PUT 2.=3:\n+BODY\nPUT <3:\n+OTHER", "a\nBODY\nOTHER\nd"),
    ] {
        assert_eq!(apply("a\nb\nc\nd", patch), expected, "for {patch:?}");
    }
}

/// An insertion outside the replaced range keeps its natural position.
#[test]
fn an_insertion_before_a_replaced_range_precedes_the_replacement() {
    assert_eq!(
        apply("a\nb\nc", "PUT <2:\n+BEFORE\nPUT 2.=2:\n+REPLACED"),
        "a\nBEFORE\nREPLACED\nc"
    );
}
