//! Behaviour ported from omp's `format-v2.test.ts` and the leniency rules in
//! their tokenizer, against the grammar in `grammar.lark`.
//!
//! The separator cases come straight from their
//! `"leniently accepts common range separator variants"` test, which enumerates
//! `-`, `.`, `=`, `..`, `…`, and a bare space. Those are not hypothetical: they
//! are what a model writes when it is thinking about line ranges rather than
//! about this format.

use super::*;

fn ops(body: &str) -> Vec<Op> {
    parse_ops(body).expect("body must parse").ops
}

fn put(anchor: Anchor, rows: &[&str]) -> Op {
    Op::Put {
        anchor,
        body: rows.iter().map(|r| r.to_string()).collect(),
    }
}

// ─── the canonical forms ─────────────────────────────────────────────────────

#[test]
fn a_range_replacement_carries_its_body_rows_in_order() {
    assert_eq!(
        ops("PUT 2.=2:\n+before\n+after"),
        vec![put(
            Anchor::Range { start: 2, end: 2 },
            &["before", "after"]
        )]
    );
}

#[test]
fn a_cut_deletes_a_range() {
    assert_eq!(ops("CUT 2.=3"), vec![Op::Cut { start: 2, end: 3 }]);
}

#[test]
fn a_bare_cut_deletes_one_line() {
    assert_eq!(ops("CUT 2"), vec![Op::Cut { start: 2, end: 2 }]);
}

#[test]
fn gap_locators_insert_before_and_after() {
    assert_eq!(ops("PUT <2:\n+x"), vec![put(Anchor::Before(2), &["x"])]);
    assert_eq!(ops("PUT >2:\n+x"), vec![put(Anchor::After(2), &["x"])]);
}

/// `<1` is the file head and `>$` the tail. Both need names because neither
/// can be expressed as a line anchor in an empty file.
#[test]
fn head_and_tail_have_dedicated_anchors() {
    assert_eq!(ops("PUT <1:\n+HEAD"), vec![put(Anchor::Bof, &["HEAD"])]);
    assert_eq!(ops("PUT >$:\n+TAIL"), vec![put(Anchor::Eof, &["TAIL"])]);
}

#[test]
fn rem_deletes_the_file_and_mv_renames_it() {
    assert_eq!(ops("REM"), vec![Op::Rem]);
    assert_eq!(
        ops("MV lib/greet.py"),
        vec![Op::Mv {
            dest: "lib/greet.py".to_string()
        }]
    );
}

#[test]
fn a_move_destination_can_be_quoted_for_paths_with_spaces() {
    assert_eq!(
        ops("MV \"my file.py\""),
        vec![Op::Mv {
            dest: "my file.py".to_string()
        }]
    );
}

// ─── leniency: the whole point ───────────────────────────────────────────────

/// Straight from omp's `"leniently accepts common range separator variants"`.
/// Every one of these must reach the same range, because every one is
/// something a model actually writes.
#[test]
fn every_common_range_separator_reaches_the_same_range() {
    for separator in ["-", ".", "=", "..", "…", " ", ".=", "-.", " - "] {
        let cut = ops(&format!("CUT 2{separator}3"));
        assert_eq!(
            cut,
            vec![Op::Cut { start: 2, end: 3 }],
            "CUT with separator {separator:?} must reach 2..=3"
        );

        let put_op = ops(&format!("PUT 2{separator}3:\n+middle"));
        assert_eq!(
            put_op,
            vec![put(Anchor::Range { start: 2, end: 3 }, &["middle"])],
            "PUT with separator {separator:?} must reach 2..=3"
        );
    }
}

/// A bare body row is the most common near-miss: the model writes the content
/// but forgets the sigil. Its position after a `:` header makes the intent
/// unambiguous, so recover and say so rather than failing the turn.
#[test]
fn a_bare_body_row_is_auto_prefixed_with_a_warning() {
    let parsed = parse_ops("PUT 2.=2:\nraw").expect("must recover");

    assert_eq!(
        parsed.ops,
        vec![put(Anchor::Range { start: 2, end: 2 }, &["raw"])]
    );
    assert!(
        parsed.warnings.iter().any(|w| w.contains("Auto-prefixed")),
        "the recovery must be reported: {:?}",
        parsed.warnings
    );
}

/// The compounding case: a bare row pasted from `read` output carries a line
/// number. Without stripping it, `3:replaced` writes the literal prefix into
/// the file — omp calls this out with its own test.
#[test]
fn a_bare_body_row_pasted_from_read_output_loses_its_line_prefix() {
    let parsed = parse_ops("PUT 2.=2:\n3:replaced").expect("must recover");

    assert_eq!(
        parsed.ops,
        vec![put(Anchor::Range { start: 2, end: 2 }, &["replaced"])],
        "the `3:` prefix must not become file content"
    );
}

/// An empty replacement body means deletion. The model expressed it awkwardly,
/// but unambiguously.
#[test]
fn an_empty_replacement_body_is_read_as_a_deletion() {
    let parsed = parse_ops("PUT 2.=3:").expect("must parse");

    assert_eq!(parsed.ops, vec![Op::Cut { start: 2, end: 3 }]);
    assert!(
        parsed
            .warnings
            .iter()
            .any(|w| w.contains("empty `PUT` body")),
        "the reinterpretation must be reported: {:?}",
        parsed.warnings
    );
}

/// Elision markers pasted into a body are metadata, never content. Writing a
/// `…` row into a file would replace real code with an ellipsis.
#[test]
fn read_metadata_rows_are_dropped_from_a_body() {
    assert_eq!(
        ops("PUT 2.=2:\n+kept\n…\n+also kept"),
        vec![put(
            Anchor::Range { start: 2, end: 2 },
            &["kept", "also kept"]
        )]
    );
}

#[test]
fn a_plus_alone_is_a_blank_line() {
    assert_eq!(
        ops("PUT 1.=1:\n+\n+after blank"),
        vec![put(
            Anchor::Range { start: 1, end: 1 },
            &["", "after blank"]
        )]
    );
}

/// A literal leading `+` or `-` in content is escaped by doubling the sigil, so
/// Markdown lists and diffs can be written as content.
#[test]
fn a_doubled_sigil_writes_one_literal_sigil() {
    assert_eq!(
        ops("PUT 1.=1:\n+- item\n++ item"),
        vec![put(
            Anchor::Range { start: 1, end: 1 },
            &["- item", "+ item"]
        )]
    );
}

// ─── refusals ────────────────────────────────────────────────────────────────

/// A payload row with no hunk above it is not recoverable: there is nothing to
/// say where it goes. Failing loudly beats guessing a location.
#[test]
fn a_payload_row_with_no_hunk_header_is_an_error() {
    let error = parse_ops("+orphaned row").expect_err("must not guess a location");
    assert!(error.contains("no preceding hunk header"), "{error}");
}

/// omp removed `DEL` and `COPY`. They must not be silently accepted as some
/// other op, and their rows must not be read as content.
#[test]
fn removed_keywords_are_not_recognized() {
    for header in ["DEL 2", "COPY 2", "SWAP 1.=2"] {
        assert!(
            parse_ops(header).is_err(),
            "{header:?} must not parse as an operation"
        );
    }
}

/// An inverted range is a mistake with no safe interpretation.
#[test]
fn an_inverted_range_is_refused() {
    assert!(parse_ops("CUT 9.=2").is_err());
}

/// A huge span is refused rather than expanded, so a mistyped line number
/// cannot allocate unboundedly.
#[test]
fn a_range_above_the_expansion_limit_is_refused() {
    let error = parse_ops("PUT 1.=100001:\n+x").expect_err("must refuse");
    assert!(error.contains("maximum"), "{error}");
}

/// Line numbers are 1-indexed, so `0` is not a line, and a leading zero is
/// more likely a typo or a quoted string than a reference.
#[test]
fn zero_and_leading_zero_line_numbers_are_not_ranges() {
    assert!(parse_ops("CUT 0").is_err());
    assert!(parse_ops("CUT 01").is_err());
}

/// Clipboard registers (`@name`) and the after-block form (`>N*`) are not
/// implemented. They must be refused rather than silently reinterpreted as a
/// line range, which would edit the wrong lines.
///
/// The message matters as much as the refusal. Without a dedicated one these
/// fall through to "no preceding hunk header", which tells a model its syntax
/// was unrecognized rather than that the feature does not exist — so it
/// retries the same thing instead of choosing a different op.
#[test]
fn unimplemented_register_and_after_block_forms_are_refused_by_name() {
    for header in ["PUT >2*:\n+x", "PUT >20 @fn", "CUT 5.=9 @fn"] {
        let error = parse_ops(header)
            .expect_err(&format!("{header:?} must be refused while unimplemented"));
        assert!(
            error.contains("not supported"),
            "{header:?} must say the feature is unsupported, not that the syntax \
             is unrecognized; got: {error}"
        );
    }
}

/// A refusal must not promise a feature nobody intends to build.
///
/// `>N*` is unbuilt but wanted, so "yet" is honest and tells a model to expect
/// it later. Clipboard registers were considered and declined on 2026-08-11, so
/// "yet" there would be a promise this codebase has decided not to keep, and
/// the alternative offered is the permanent answer rather than a stopgap.
#[test]
fn a_declined_feature_is_not_described_as_merely_pending() {
    let registers = parse_ops("CUT 5.=9 @fn").expect_err("refused");
    assert!(
        !registers.contains("yet"),
        "registers were declined, so the message must not imply they are coming: \
         {registers}"
    );
    assert!(
        registers.contains("CUT") && registers.contains("PUT"),
        "it has to say what to do instead: {registers}"
    );

    let after_block = parse_ops("PUT >2*:\n+x").expect_err("refused");
    assert!(
        after_block.contains("yet"),
        "`>N*` is still intended, so its message should say so: {after_block}"
    );
}

// ─── block anchors ───────────────────────────────────────────────────────────

/// `PUT N*:` and `CUT N*` survive parsing as a deferred anchor, because the
/// parser has neither the file nor its language and so cannot know where the
/// block ends. `crate::blocks::resolve` turns them into ranges later.
#[test]
fn a_block_anchor_parses_into_a_deferred_anchor() {
    assert_eq!(
        ops("PUT 2*:\n+A\n+B"),
        vec![put(Anchor::Block(2), &["A", "B"])]
    );
    assert_eq!(ops("CUT 7*"), vec![put(Anchor::Block(7), &[])]);
}

/// The `*` is what distinguishes a block from a range, and the separator
/// leniency makes that easy to blur: `2-3` is a range however it is spelled,
/// and only a bare `N*` is a block.
#[test]
fn a_plain_range_is_never_read_as_a_block() {
    assert_eq!(
        ops("PUT 2-3:\n+A"),
        vec![put(Anchor::Range { start: 2, end: 3 }, &["A"])]
    );
}

/// An empty `PUT N*:` body means deletion, exactly as it does for a range. The
/// range form rewrites to `CUT` at parse time, but a block cannot: its extent
/// is unknown until resolution, so the deletion is expressed as a block anchor
/// with no payload and becomes a `CUT` once the span is known.
#[test]
fn an_empty_block_body_is_a_deletion() {
    assert_eq!(ops("PUT 2*:"), vec![put(Anchor::Block(2), &[])]);
}

// ─── multiple hunks ──────────────────────────────────────────────────────────

#[test]
fn several_hunks_parse_in_order() {
    assert_eq!(
        ops("PUT 1.=1:\n+first\nCUT 5.=6\nPUT >9:\n+last"),
        vec![
            put(Anchor::Range { start: 1, end: 1 }, &["first"]),
            Op::Cut { start: 5, end: 6 },
            put(Anchor::After(9), &["last"]),
        ]
    );
}

/// Blank lines between hunks are formatting. Treating one as a body row would
/// insert a spurious empty line into the file.
#[test]
fn blank_lines_between_hunks_are_formatting_not_content() {
    assert_eq!(
        ops("CUT 1.=1\n\nCUT 5.=5"),
        vec![Op::Cut { start: 1, end: 1 }, Op::Cut { start: 5, end: 5 }]
    );
}

#[test]
fn an_empty_body_parses_to_no_operations() {
    assert_eq!(ops(""), vec![]);
    assert_eq!(ops("\n\n  \n"), vec![]);
}

/// `MV` is a file-level op that can follow line edits, so the edits apply to
/// the source and the result is written at the destination.
#[test]
fn a_move_can_follow_line_edits_in_one_section() {
    assert_eq!(
        ops("PUT 1.=1:\n+edited\nMV new/path.rs"),
        vec![
            put(Anchor::Range { start: 1, end: 1 }, &["edited"]),
            Op::Mv {
                dest: "new/path.rs".to_string()
            },
        ]
    );
}
