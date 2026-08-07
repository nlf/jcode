//! Behaviour spec for envelope parsing.
//!
//! Cases taken from oh-my-pi's `test/core/apply-patch.test.ts` (their
//! `parseLegacyPatch` describe block) and their parser's documented rules.

use super::*;

fn wrap(body: &str) -> String {
    format!("*** Begin Patch\n{body}\n*** End Patch")
}

#[test]
fn a_patch_must_begin_with_the_marker() {
    let error = parse("bad").expect_err("a patch without the opening marker must be refused");
    assert!(
        error.message().contains("*** Begin Patch"),
        "{}",
        error.message()
    );
}

#[test]
fn a_patch_must_end_with_the_marker() {
    let error = parse("*** Begin Patch\nbad").expect_err("a truncated patch must be refused");
    assert!(
        error.message().contains("*** End Patch"),
        "{}",
        error.message()
    );
}

/// Markers padded with whitespace still parse: a model that emits a trailing
/// space should not have its whole patch rejected for it.
#[test]
fn whitespace_padded_markers_are_accepted() {
    let hunks = parse("*** Begin Patch \n*** Add File: foo\n+hi\n *** End Patch")
        .expect("padded markers should parse");

    assert_eq!(
        hunks,
        vec![Hunk {
            path: "foo".to_string(),
            op: Operation::Create,
            rename: None,
            diff: "hi\n".to_string(),
        }]
    );
}

/// An update hunk with no body says nothing about what to change. Accepting it
/// would apply an empty diff and report success.
#[test]
fn an_empty_update_hunk_is_refused() {
    let error = parse(&wrap("*** Update File: test.py")).expect_err("an empty update is refused");
    assert!(error.message().contains("empty"), "{}", error.message());
    assert!(
        error.line.is_some(),
        "the error should name the line it failed on"
    );
}

/// An envelope with no hunks is empty, not invalid. The caller sent a
/// well-formed patch that happens to do nothing.
#[test]
fn an_empty_patch_parses_to_no_hunks() {
    assert_eq!(parse(&wrap("")).expect("an empty patch is valid"), Vec::new());
}

#[test]
fn every_operation_parses_from_one_envelope() {
    let hunks = parse(&wrap(
        "*** Add File: path/add.py\n\
         +abc\n\
         +def\n\
         *** Delete File: path/delete.py\n\
         *** Update File: path/update.py\n\
         *** Move to: path/update2.py\n\
         @@ def f():\n\
         -    pass\n\
         +    return 123",
    ))
    .expect("a full patch should parse");

    assert_eq!(hunks.len(), 3);

    assert_eq!(hunks[0].path, "path/add.py");
    assert_eq!(hunks[0].op, Operation::Create);
    assert_eq!(hunks[0].diff, "abc\ndef\n");

    assert_eq!(hunks[1].path, "path/delete.py");
    assert_eq!(hunks[1].op, Operation::Delete);

    assert_eq!(hunks[2].path, "path/update.py");
    assert_eq!(hunks[2].op, Operation::Update);
    assert_eq!(hunks[2].rename.as_deref(), Some("path/update2.py"));
    assert!(hunks[2].diff.contains("@@ def f():"));
    assert!(hunks[2].diff.contains("-    pass"));
}

/// Models emit the shell form they would have typed. Rejecting it teaches
/// nothing the caller can act on.
#[test]
fn a_heredoc_wrapper_is_stripped() {
    let inner = "*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch";
    for opener in ["<<EOF", "<<'EOF'", "<<\"EOF\""] {
        let wrapped = format!("{opener}\n{inner}\nEOF\n");
        let hunks = parse(&wrapped).unwrap_or_else(|error| {
            panic!("{opener} should be stripped: {}", error.message())
        });
        assert_eq!(hunks.len(), 1, "{opener}");
        assert_eq!(hunks[0].path, "test.txt");
    }
}

/// Only a matching pair is stripped. A patch whose first line happens to look
/// like an opener but which does not end with EOF is left alone, and then
/// fails the begin-marker check honestly.
#[test]
fn an_unmatched_heredoc_opener_is_not_stripped() {
    let error = parse("<<EOF\n*** Begin Patch\n*** Delete File: x\n*** End Patch")
        .expect_err("without a closing EOF the first line is not the begin marker");
    assert!(error.message().contains("*** Begin Patch"));
}

#[test]
fn an_unrecognised_hunk_header_names_the_valid_ones() {
    let error = parse(&wrap("*** Frobnicate File: x.txt")).expect_err("unknown header");
    let message = error.message();

    assert!(message.contains("Frobnicate"), "{message}");
    assert!(message.contains("*** Add File:"), "{message}");
    assert!(message.contains("*** Delete File:"), "{message}");
    assert!(message.contains("*** Update File:"), "{message}");
}

/// Blank lines between hunks are separators, not content.
#[test]
fn blank_lines_between_hunks_are_ignored() {
    let hunks = parse(&wrap(
        "*** Delete File: a.txt\n\n\n*** Delete File: b.txt",
    ))
    .expect("blank separators are allowed");

    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].path, "a.txt");
    assert_eq!(hunks[1].path, "b.txt");
}

/// An add hunk's body ends at the first non-`+` line, so the next marker is
/// not swallowed into its contents.
#[test]
fn an_add_hunk_stops_at_the_next_marker() {
    let hunks = parse(&wrap(
        "*** Add File: a.txt\n+one\n+two\n*** Delete File: b.txt",
    ))
    .expect("should parse");

    assert_eq!(hunks[0].diff, "one\ntwo\n");
    assert_eq!(hunks.len(), 2, "the delete must not be absorbed");
}

/// `*** End of File` terminates a chunk, not the file's hunk, so it belongs in
/// the body for the diff parser to handle.
#[test]
fn an_end_of_file_marker_stays_inside_the_update_body() {
    let hunks = parse(&wrap(
        "*** Update File: a.txt\n@@\n-old\n+new\n*** End of File",
    ))
    .expect("should parse");

    assert_eq!(hunks.len(), 1);
    assert!(
        hunks[0].diff.contains("*** End of File"),
        "the marker belongs to the diff body: {:?}",
        hunks[0].diff
    );
}

/// A move applies only when it directly follows its update header. Elsewhere
/// it is ordinary body text.
#[test]
fn a_move_marker_only_counts_directly_after_its_header() {
    let hunks = parse(&wrap("*** Update File: a.txt\n@@\n-old\n*** Move to: b.txt"))
        .expect("should parse");

    assert_eq!(hunks[0].rename, None, "a move inside the body is body text");
    assert!(hunks[0].diff.contains("*** Move to: b.txt"));
}

#[test]
fn an_update_body_runs_until_the_next_file_marker() {
    let hunks = parse(&wrap(
        "*** Update File: a.txt\n@@\n-old\n+new\n*** Update File: b.txt\n@@\n-x\n+y",
    ))
    .expect("should parse");

    assert_eq!(hunks.len(), 2);
    assert!(!hunks[0].diff.contains("b.txt"), "bodies must not run together");
    assert_eq!(hunks[1].path, "b.txt");
}

/// Streaming is for rendering a patch that is still arriving, so a missing end
/// marker is expected rather than an error.
#[test]
fn streaming_tolerates_an_unfinished_patch() {
    let partial = "*** Begin Patch\n*** Add File: a.txt\n+one";
    assert!(parse(partial).is_err(), "strict parsing still refuses it");

    let hunks = parse_streaming(partial);
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].diff, "one\n");
}

#[test]
fn streaming_returns_nothing_for_text_that_is_not_a_patch() {
    assert_eq!(parse_streaming("just some prose"), Vec::new());
}

/// A streaming update with no body yet is a hunk in progress, not an error.
#[test]
fn streaming_keeps_an_update_whose_body_has_not_arrived() {
    let hunks = parse_streaming("*** Begin Patch\n*** Update File: a.txt");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].op, Operation::Update);
    assert_eq!(hunks[0].diff, "");
}

/// The line number makes an error actionable in a long patch.
#[test]
fn errors_inside_the_body_carry_a_line_number() {
    let error = parse(&wrap(
        "*** Delete File: a.txt\n*** Delete File: b.txt\n*** Nonsense: c.txt",
    ))
    .expect_err("unknown header");

    assert_eq!(
        error.line,
        Some(4),
        "Begin Patch is line 1, so the third hunk header is line 4"
    );
}

/// Blank separator lines still advance the reported line number, so an error
/// after one points at the real line in the caller's patch.
///
/// Found by mutation testing: stopping the counter on blank lines broke
/// nothing, because the existing line-number test had no blank lines in it.
#[test]
fn blank_separators_still_advance_the_reported_line_number() {
    let error = parse(&wrap("*** Delete File: a.txt\n\n\n*** Nonsense: c.txt"))
        .expect_err("unknown header");

    assert_eq!(
        error.line,
        Some(5),
        "Begin Patch is 1, the delete is 2, two blanks are 3 and 4, so the bad header is 5"
    );
}
