//! Behaviour ported from omp's `patcher.test.ts` and the seen-line rules in
//! their `patcher.ts`.
//!
//! Two things here were corrections to my own earlier research, both found only
//! by reading their tests rather than their prose:
//!
//! - `enforceSeenLines` ships **off** in omp's settings, not on.
//! - A **column-clipped line still counts as seen**. omp tried excluding those
//!   and removed the exclusion; their test says so in its name.
//!
//! The reveal caps are the subtle part. Merging revealed lines into the seen
//! set makes a rejection self-healing, but only when every unseen line was
//! shown in full. Over either cap nothing merges, which is what stops a model
//! splitting one blind edit into cap-sized retries and walking past the guard.

use super::*;
use crate::parser::parse_ops;

const PATH: &str = "src/foo.rs";
const TEXT: &str = "one\ntwo\nthree\nfour\nfive\n";

fn ops(patch: &str) -> Vec<Op> {
    parse_ops(patch).expect("patch must parse").ops
}

/// A store that has seen the whole file.
fn store_with_full_read() -> (SnapshotStore, String) {
    let store = SnapshotStore::new();
    let all: Vec<usize> = (1..=6).collect();
    let tag = store.record(PATH, TEXT, Some(&all));
    (store, tag)
}

// ─── tag validation ──────────────────────────────────────────────────────────

#[test]
fn a_matching_tag_applies() {
    let (store, tag) = store_with_full_read();

    let prepared = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 2.=2:\n+TWO"),
        true,
        Parsing::default(),
    )
    .expect("a current tag must apply");

    assert_eq!(prepared.after, "one\nTWO\nthree\nfour\nfive\n");
}

/// A tag this session minted, against content that has since changed, is the
/// ordinary drift case: another agent, a formatter, or the user edited the file.
#[test]
fn a_tag_minted_here_but_now_stale_is_reported_as_drift() {
    let store = SnapshotStore::new();
    let old_tag = store.record(PATH, TEXT, Some(&[1, 2, 3, 4, 5, 6]));
    let changed = "one\nCHANGED\nthree\nfour\nfive\n";

    let error = prepare(
        &store,
        PATH,
        changed,
        Some(&old_tag),
        &ops("CUT 2"),
        true,
        Parsing::default(),
    )
    .expect_err("a stale tag must be refused");

    assert!(
        matches!(error, RejectReason::StaleTag { .. }),
        "expected drift, got {error:?}"
    );
    let message = error.message(PATH);
    assert!(
        message.contains("changed between the read and this edit"),
        "{message}"
    );
    assert!(
        message.contains("re-read"),
        "must say what to do: {message}"
    );
}

/// A tag nothing ever minted is a different mistake: the model invented it, or
/// carried it from a prior session. Telling it to "re-read because the file
/// changed" would be wrong and would not stop the behaviour.
#[test]
fn a_tag_never_minted_here_is_reported_as_invented() {
    let store = SnapshotStore::new();
    store.record(PATH, TEXT, Some(&[1, 2, 3, 4, 5, 6]));

    let error = prepare(
        &store,
        PATH,
        TEXT,
        Some("FFFF"),
        &ops("CUT 2"),
        true,
        Parsing::default(),
    )
    .expect_err("an unminted tag must be refused");

    assert!(
        matches!(error, RejectReason::UnknownTag { .. }),
        "expected an unknown tag, got {error:?}"
    );
    let message = error.message(PATH);
    assert!(message.contains("not from this session"), "{message}");
    assert!(
        message.contains("never invent"),
        "must name the actual mistake: {message}"
    );
}

/// The two rejections must not collapse into one. They need different fixes,
/// and a model told to re-read will keep inventing tags.
#[test]
fn the_two_tag_rejections_carry_different_messages() {
    let store = SnapshotStore::new();
    let old_tag = store.record(PATH, TEXT, Some(&[1, 2, 3, 4, 5, 6]));
    let changed = "one\nCHANGED\nthree\nfour\nfive\n";

    let stale = prepare(
        &store,
        PATH,
        changed,
        Some(&old_tag),
        &ops("CUT 2"),
        true,
        Parsing::default(),
    )
    .expect_err("stale")
    .message(PATH);
    let unknown = prepare(
        &store,
        PATH,
        changed,
        Some("FFFF"),
        &ops("CUT 2"),
        true,
        Parsing::default(),
    )
    .expect_err("unknown")
    .message(PATH);

    assert_ne!(stale, unknown);
}

/// An untagged section skips validation. The patcher decides policy; this layer
/// reports what it can prove, and with no tag it can prove nothing.
#[test]
fn an_untagged_section_skips_tag_validation() {
    let store = SnapshotStore::new();
    let prepared = prepare(
        &store,
        PATH,
        TEXT,
        None,
        &ops("CUT 2"),
        true,
        Parsing::default(),
    )
    .expect("no tag means no tag check");
    assert_eq!(prepared.after, "one\nthree\nfour\nfive\n");
}

// ─── the seen-line guard ─────────────────────────────────────────────────────

/// The failure the guard exists to prevent: a partial read mints a tag for the
/// whole file, so without provenance an anchor anywhere would validate.
#[test]
fn an_edit_to_a_line_the_read_never_displayed_is_refused() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    let error = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 5.=5:\n+FIVE"),
        true,
        Parsing::default(),
    )
    .expect_err("line 5 was never displayed");

    match error {
        RejectReason::UnseenLines { ref lines, .. } => assert_eq!(lines, &[5]),
        other => panic!("expected unseen lines, got {other:?}"),
    }
}

#[test]
fn an_edit_to_a_displayed_line_applies() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    let prepared = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 2.=2:\n+TWO"),
        true,
        Parsing::default(),
    )
    .expect("line 2 was displayed");

    assert_eq!(prepared.after, "one\nTWO\nthree\nfour\nfive\n");
}

/// The rejection shows what is actually at the unseen lines, and those lines
/// then count as seen: the error itself is the proof. A straight retry with the
/// same header succeeds, which turns a hard failure into one wasted call rather
/// than a re-read plus a retry.
#[test]
fn a_rejection_reveals_the_lines_and_a_retry_then_succeeds() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    let error = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 5.=5:\n+FIVE"),
        true,
        Parsing::default(),
    )
    .expect_err("first attempt is refused");

    let message = error.message(PATH);
    assert!(
        message.contains("5:five"),
        "must show the real content: {message}"
    );
    assert!(
        message.contains("count as seen"),
        "must say a retry will work: {message}"
    );

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 5.=5:\n+FIVE"),
        true,
        Parsing::default(),
    )
    .expect("the retry must now succeed");
}

/// Over the reveal cap, nothing merges. Without this a model could split one
/// blind edit into cap-sized retries and walk past the guard a slice at a time.
#[test]
fn an_over_cap_rejection_merges_nothing_and_the_retry_still_fails() {
    let store = SnapshotStore::new();
    let big: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    let tag = store.record(PATH, &big, Some(&[1]));

    let patch = format!("PUT 2.={}:\n+X", SEEN_LINE_REVEAL_CAP + 10);
    let error = prepare(
        &store,
        PATH,
        &big,
        Some(&tag),
        &ops(&patch),
        true,
        Parsing::default(),
    )
    .expect_err("far too many unseen lines");

    let message = error.message(PATH);
    assert!(message.contains("re-read the range"), "{message}");

    prepare(
        &store,
        PATH,
        &big,
        Some(&tag),
        &ops(&patch),
        true,
        Parsing::default(),
    )
    .expect_err("the retry must still fail, or the guard is walkable");
}

/// A very wide line truncates in the reveal, which flags the whole reveal, so
/// nothing merges. Otherwise a model receives an "ok to retry" signal while
/// part of each line remains unseen.
#[test]
fn a_column_clipped_reveal_merges_nothing() {
    let store = SnapshotStore::new();
    let wide = format!(
        "head\n{}\nfoot\n",
        "a".repeat(SEEN_LINE_REVEAL_MAX_COLUMNS + 100)
    );
    let tag = store.record(PATH, &wide, Some(&[1]));

    let error = prepare(
        &store,
        PATH,
        &wide,
        Some(&tag),
        &ops("PUT 2.=2:\n+X"),
        true,
        Parsing::default(),
    )
    .expect_err("line 2 was not displayed");
    assert!(error.message(PATH).contains("re-read the range"));

    prepare(
        &store,
        PATH,
        &wide,
        Some(&tag),
        &ops("PUT 2.=2:\n+X"),
        true,
        Parsing::default(),
    )
    .expect_err("a clipped reveal must not unlock the retry");
}

/// Absent provenance means the guard cannot judge, so it stands aside. This is
/// what lets a producer that does not record yet keep working instead of
/// blocking every edit to files it touched.
#[test]
fn absent_provenance_disables_the_guard_rather_than_blocking() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, None);

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 5.=5:\n+FIVE"),
        true,
        Parsing::default(),
    )
    .expect("no provenance means the guard cannot judge");
}

/// The guard is switchable. omp ships it off; we default it on, and the
/// difference is a policy decision that should be visible in a test.
#[test]
fn the_guard_can_be_disabled() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 5.=5:\n+FIVE"),
        false,
        Parsing::default(),
    )
    .expect("with the guard off, an unseen line applies");
}

/// Insert anchors are checked too: inserting beside an unseen line still means
/// placing content the model cannot see the context for.
#[test]
fn the_guard_covers_insert_anchors_not_only_ranges() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT >4:\n+X"),
        true,
        Parsing::default(),
    )
    .expect_err("line 4 was never displayed");
}

/// File-level ops have no line anchors, so the guard has nothing to judge and
/// must not invent an objection.
#[test]
fn file_level_ops_are_not_blocked_by_the_guard() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1]));

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("MV other.rs"),
        true,
        Parsing::default(),
    )
    .expect("a move anchors no lines");
}

// ─── no-op detection ─────────────────────────────────────────────────────────

/// A patch that changes nothing is reported rather than written. omp's issue
/// #2081 recorded 182 identical no-op repeats in 205 calls, so this is the
/// signal a loop guard needs.
#[test]
fn a_patch_that_changes_nothing_is_refused() {
    let (store, tag) = store_with_full_read();

    let error = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 2.=2:\n+two"),
        true,
        Parsing::default(),
    )
    .expect_err("the body is byte-identical");

    assert!(matches!(error, RejectReason::NoOp), "got {error:?}");
    assert!(
        error.message(PATH).contains("re-read"),
        "must suggest a way out"
    );
}

/// A move that changes no content is still a change, so it must not be
/// mistaken for a no-op.
#[test]
fn a_move_alone_is_not_a_no_op() {
    let (store, tag) = store_with_full_read();

    prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("MV other.rs"),
        true,
        Parsing::default(),
    )
    .expect("relocating a file is a change");
}

// ─── chaining ────────────────────────────────────────────────────────────────

/// The returned tag is what makes an edit chain work without a re-read: it
/// anchors the next edit against the content this one produced.
#[test]
fn the_result_carries_a_tag_for_the_next_edit() {
    let (store, tag) = store_with_full_read();

    let first = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 2.=2:\n+TWO"),
        true,
        Parsing::default(),
    )
    .expect("first edit");

    assert_eq!(first.new_tag, compute_file_hash(&first.after));
    assert_ne!(
        first.new_tag, tag,
        "content changed, so the tag must change"
    );
}

/// Recording the post-edit content with no provenance is what lets a chain
/// continue: you wrote those lines, so you have seen them.
#[test]
fn a_chained_edit_applies_against_the_new_tag() {
    let (store, tag) = store_with_full_read();

    let first = prepare(
        &store,
        PATH,
        TEXT,
        Some(&tag),
        &ops("PUT 2.=2:\n+TWO"),
        true,
        Parsing::default(),
    )
    .expect("first edit");
    store.record(PATH, &first.after, None);

    prepare(
        &store,
        PATH,
        &first.after,
        Some(&first.new_tag),
        &ops("PUT 3.=3:\n+THREE"),
        true,
        Parsing::default(),
    )
    .expect("the second edit anchors against the first edit's tag");
}

// ─── multi-section preflight ─────────────────────────────────────────────────

fn section<'a>(path: &'a str, text: &'a str, tag: &'a str, ops: &'a [Op]) -> SectionInput<'a> {
    SectionInput {
        path,
        current_text: text,
        expected_tag: Some(tag),
        ops,
    }
}

/// The preflight guarantee: no section is written until every section
/// validates. This is what stops a five-file patch with a bad anchor in the
/// third from leaving the first two applied.
#[test]
fn one_failing_section_prevents_every_section_from_being_prepared() {
    let store = SnapshotStore::new();
    let a = "alpha\n";
    let b = "beta\n";
    let tag_a = store.record("a.txt", a, Some(&[1, 2]));
    let tag_b = store.record("b.txt", b, Some(&[1, 2]));

    let good = ops("PUT 1.=1:\n+ALPHA");
    // A stale tag on the second section.
    let bad = ops("PUT 1.=1:\n+BETA");

    let error = preflight(
        &store,
        &[
            section("a.txt", a, &tag_a, &good),
            section("b.txt", b, "FFFF", &bad),
        ],
        true,
        Parsing::default(),
    )
    .expect_err("the second section must fail");

    match error {
        PreflightError::Section { ref path, .. } => assert_eq!(path, "b.txt"),
        other => panic!("expected a section failure, got {other:?}"),
    }

    // Nothing was written, because preflight returns results rather than
    // committing. The proof is that the caller got no Prepared values at all.
    let _ = tag_b;
}

#[test]
fn every_section_validating_yields_one_prepared_result_each() {
    let store = SnapshotStore::new();
    let a = "alpha\n";
    let b = "beta\n";
    let tag_a = store.record("a.txt", a, Some(&[1, 2]));
    let tag_b = store.record("b.txt", b, Some(&[1, 2]));

    let ops_a = ops("PUT 1.=1:\n+ALPHA");
    let ops_b = ops("PUT 1.=1:\n+BETA");

    let prepared = preflight(
        &store,
        &[
            section("a.txt", a, &tag_a, &ops_a),
            section("b.txt", b, &tag_b, &ops_b),
        ],
        true,
        Parsing::default(),
    )
    .expect("both sections validate");

    assert_eq!(prepared.len(), 2);
    assert_eq!(prepared[0].after, "ALPHA\n");
    assert_eq!(prepared[1].after, "BETA\n");
}

/// Two sections targeting one file are refused rather than merged. Merging
/// would move the second section's ops up, reordering them against how they
/// were authored; if the model intended a sequence, applying it out of order
/// is worse than asking for a single header.
#[test]
fn two_sections_targeting_one_file_are_refused() {
    let store = SnapshotStore::new();
    let text = "one\ntwo\n";
    let tag = store.record("a.txt", text, Some(&[1, 2, 3]));

    let first = ops("PUT 1.=1:\n+ONE");
    let second = ops("PUT 2.=2:\n+TWO");

    let error = preflight(
        &store,
        &[
            section("a.txt", text, &tag, &first),
            section("a.txt", text, &tag, &second),
        ],
        true,
        Parsing::default(),
    )
    .expect_err("one file, two sections");

    match error {
        PreflightError::DuplicatePath { ref path } => assert_eq!(path, "a.txt"),
        other => panic!("expected a duplicate path, got {other:?}"),
    }
    assert!(
        error.message().contains("Merge their operations"),
        "must say what to do: {}",
        error.message()
    );
}

/// The duplicate check must run before any section is prepared, or the first
/// section's work is wasted and its side effects (seen-line merging) have
/// already happened.
#[test]
fn the_duplicate_check_runs_before_any_section_is_prepared() {
    let store = SnapshotStore::new();
    let text = "one\n";
    let tag = store.record("a.txt", text, Some(&[1, 2]));

    // The first section would fail on its own merits (no-op), but the
    // duplicate error must win because it is detected first.
    let noop = ops("PUT 1.=1:\n+one");
    let other = ops("PUT 1.=1:\n+ONE");

    let error = preflight(
        &store,
        &[
            section("a.txt", text, &tag, &noop),
            section("a.txt", text, &tag, &other),
        ],
        true,
        Parsing::default(),
    )
    .expect_err("must refuse");

    assert!(
        matches!(error, PreflightError::DuplicatePath { .. }),
        "the structural error must be reported before per-section validation: {error:?}"
    );
}

/// A failure has to name the file. With several sections the message alone
/// does not say which one failed, and "edit rejected" against an unnamed file
/// is not actionable.
#[test]
fn a_section_failure_names_the_file_it_came_from() {
    let store = SnapshotStore::new();
    let text = "one\n";
    // Recorded so the path is tracked: that makes "FFFF" an unknown *tag*
    // rather than an unknown path, which is the case worth naming.
    store.record("a.txt", text, Some(&[1, 2]));
    let bad = ops("PUT 1.=1:\n+ONE");

    let error = preflight(
        &store,
        &[section("a.txt", text, "FFFF", &bad)],
        true,
        Parsing::default(),
    )
    .expect_err("unknown tag");

    assert!(error.message().contains("a.txt"), "{}", error.message());
}

#[test]
fn an_empty_patch_preflights_to_no_prepared_sections() {
    let store = SnapshotStore::new();
    assert!(
        preflight(&store, &[], true, Parsing::default())
            .expect("nothing to do")
            .is_empty()
    );
}

// Recovery and repair have to compose. Each was verified alone, and the seam
// between them was not: recovery relocated the anchors and then applied
// directly, so a drifted edit got its line numbers fixed and its duplicated
// neighbours left in. These pin the composition in both drift paths.

#[test]
fn a_drifted_edit_is_both_relocated_and_repaired() {
    // Another agent inserted a line at the top, so the anchors have moved. The
    // payload also restates the signature and closing brace around its range,
    // the ordinary boundary-echo mistake. Fixing only the anchors would place
    // the edit correctly and still duplicate both neighbours.
    let store = SnapshotStore::new();
    let original = "function f() {\nold();\n}\n";
    let tag = store.record(PATH, original, None);
    let current = "// added by someone else\nfunction f() {\nold();\n}\n";

    let prepared = prepare(
        &store,
        PATH,
        current,
        Some(&tag),
        &ops("PUT 2.=2:\n+function f() {\n+fresh();\n+}"),
        false,
        Parsing::default(),
    )
    .expect("the anchors are provable, so this recovers");

    assert_eq!(
        prepared.after, "// added by someone else\nfunction f() {\nfresh();\n}\n",
        "the signature and brace must not be duplicated by a recovered edit"
    );
    // Both layers report, so the model learns the tag was stale *and* that its
    // payload restated lines it should not have.
    assert!(
        prepared.warnings.iter().any(|w| w.contains("Recovered")),
        "{:?}",
        prepared.warnings
    );
    assert!(
        prepared
            .warnings
            .iter()
            .any(|w| w.contains("boundary echo")),
        "{:?}",
        prepared.warnings
    );
}

#[test]
fn a_drifted_append_goes_through_the_same_pipeline() {
    // The position-stable path: `PUT >$:` cannot be moved by drift, so it
    // applies with a warning rather than going through recovery.
    //
    // Recorded as a regression guard rather than counted as evidence: it
    // passes against the old code too, because repair only ever touches a
    // replacement's range and a head/tail insert has none. What it pins is
    // that routing this path through the shared pipeline did not change its
    // behaviour or lose its warning.
    let store = SnapshotStore::new();
    let original = "one\ntwo\n";
    let tag = store.record(PATH, original, None);
    let current = "one\ntwo\nthree\n";

    let prepared = prepare(
        &store,
        PATH,
        current,
        Some(&tag),
        &ops("PUT >$:\n+four"),
        false,
        Parsing::default(),
    )
    .expect("appending does not depend on line numbers");

    assert_eq!(prepared.after, "one\ntwo\nthree\nfour\n");
    assert!(
        prepared
            .warnings
            .iter()
            .any(|w| w.contains("stale snapshot tag")),
        "{:?}",
        prepared.warnings
    );
}

#[test]
fn a_multi_line_refusal_keeps_its_own_closing_advice() {
    // A block anchor's refusal names the enclosing block and the exact header
    // to retry, then shows a context preview. Appending the generic "check the
    // line numbers" both contradicts that instruction and lands after the
    // preview, where it reads as another line of the file.
    let detailed = RejectReason::Unapplicable {
        detail: "`PUT 6*:` could not resolve a block. Use `PUT 5*:`.\n\n 5:fn b() {\n*6:\tx();"
            .to_string(),
    };
    let message = detailed.message("s.rs");
    assert!(
        !message.contains("Check the line numbers"),
        "generic advice must not follow a preview: {message}"
    );

    // A one-line refusal still gets it, because it has no advice of its own.
    let plain = RejectReason::Unapplicable {
        detail: "Line 99 does not exist (file has 2 lines).".to_string(),
    };
    assert!(plain.message("s.rs").contains("Check the line numbers"));
}





/// A stub block resolver: a line ending in `{` opens a block that runs to the
/// next line that is only `}`. Enough to test the transform without pulling a
/// parser into this crate, which is the same reason `SyntaxCheck` is injected.
fn stub_blocks(_path: &str, text: &str, line: usize) -> Option<crate::blocks::BlockSpan> {
    let lines: Vec<&str> = text.split('\n').collect();
    let opener = lines.get(line.checked_sub(1)?)?;
    if !opener.trim_end().ends_with('{') {
        return None;
    }
    for candidate in (line + 1)..=lines.len() {
        if lines.get(candidate - 1).copied().unwrap_or("").trim() == "}" {
            return Some(crate::blocks::BlockSpan {
                start: line,
                end: candidate,
            });
        }
    }
    None
}

#[test]
fn a_block_anchor_survives_drift_exactly_as_a_line_range_does() {
    // The line number in `PUT 2*:` means line 2 of the file the model *read*,
    // which is the same thing it means in `PUT 2=2:`. Both must therefore
    // survive the same drift and produce the same file.
    //
    // Resolving a block against the current text instead looks obviously right,
    // because that is where the edit lands, and it is right while the tag is
    // current. Under drift it silently asks about a different line: here line 2
    // of the drifted file is `alpha`, which opens nothing, so the block op was
    // refused outright while the identical range edit recovered cleanly.
    let read = "alpha\nfn target() {\n\tbody();\n}\nomega\n";
    let drifted = "NEWTOP\nalpha\nfn target() {\n\tbody();\n}\nomega\n";
    let expected = "NEWTOP\nalpha\nfn target() {\n\tCHANGED();\n}\nomega\n";

    let store = SnapshotStore::new();
    let tag = store.record(PATH, read, None);
    let parsing = Parsing {
        syntax: None,
        blocks: Some(&stub_blocks),
    };

    // The model saw the block open on line 2 and replaces it whole.
    let block = prepare(
        &store,
        PATH,
        drifted,
        Some(&tag),
        &ops("PUT 2*:\n+fn target() {\n+\tCHANGED();\n+}"),
        false,
        parsing,
    )
    .expect("a block anchor must recover from drift");

    assert_eq!(block.after, expected);
    assert!(
        block.warnings.iter().any(|w| w.contains("Recovered")),
        "{:?}",
        block.warnings
    );

    // The same edit as an explicit range, for comparison rather than as a
    // separate assertion: if these ever diverge, the block path has grown a
    // rule about drift that the rest of the pipeline does not have.
    let range = prepare(
        &store,
        PATH,
        drifted,
        Some(&tag),
        &ops("PUT 3=3:\n+\tCHANGED();"),
        false,
        parsing,
    )
    .expect("the equivalent range recovers");

    assert_eq!(block.after, range.after);
}

#[test]
fn a_block_anchor_resolves_against_the_current_file_when_the_tag_is_current() {
    // The other half of the rule. With no drift there is nothing to translate,
    // and the snapshot and the file are the same text anyway.
    let text = "alpha\nfn target() {\n\tbody();\n}\nomega\n";
    let store = SnapshotStore::new();
    let tag = store.record(PATH, text, None);
    let parsing = Parsing {
        syntax: None,
        blocks: Some(&stub_blocks),
    };

    let prepared = prepare(
        &store,
        PATH,
        text,
        Some(&tag),
        &ops("PUT 2*:\n+fn target() {\n+\tCHANGED();\n+}"),
        false,
        parsing,
    )
    .expect("resolves");

    assert_eq!(prepared.after, "alpha\nfn target() {\n\tCHANGED();\n}\nomega\n");
    assert!(prepared.warnings.is_empty(), "{:?}", prepared.warnings);
}

#[test]
fn a_block_anchor_with_an_unrecognized_tag_still_resolves_against_the_file() {
    // A tag this store never minted means there is no snapshot to resolve
    // against, so the current text is the only text available. The edit is
    // refused for the unminted tag either way; what matters is that the missing
    // snapshot does not turn into a panic or a confusing block error that
    // hides the real reason.
    let text = "alpha\nfn target() {\n\tbody();\n}\nomega\n";
    let store = SnapshotStore::new();
    let parsing = Parsing {
        syntax: None,
        blocks: Some(&stub_blocks),
    };

    let error = prepare(
        &store,
        PATH,
        text,
        Some("FFFF"),
        &ops("PUT 2*:\n+fn target() {\n+\tCHANGED();\n+}"),
        false,
        parsing,
    )
    .expect_err("an unminted tag is refused");

    let message = error.message(PATH);
    assert!(
        message.contains("never invent"),
        "the tag must be the reported problem, not the block: {message}"
    );
}
