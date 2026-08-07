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

    let prepared = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 2.=2:\n+TWO"), true)
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

    let error = prepare(&store, PATH, changed, Some(&old_tag), &ops("CUT 2"), true)
        .expect_err("a stale tag must be refused");

    assert!(
        matches!(error, RejectReason::StaleTag { .. }),
        "expected drift, got {error:?}"
    );
    let message = error.message(PATH);
    assert!(message.contains("changed between the read and this edit"), "{message}");
    assert!(message.contains("re-read"), "must say what to do: {message}");
}

/// A tag nothing ever minted is a different mistake: the model invented it, or
/// carried it from a prior session. Telling it to "re-read because the file
/// changed" would be wrong and would not stop the behaviour.
#[test]
fn a_tag_never_minted_here_is_reported_as_invented() {
    let store = SnapshotStore::new();
    store.record(PATH, TEXT, Some(&[1, 2, 3, 4, 5, 6]));

    let error = prepare(&store, PATH, TEXT, Some("FFFF"), &ops("CUT 2"), true)
        .expect_err("an unminted tag must be refused");

    assert!(
        matches!(error, RejectReason::UnknownTag { .. }),
        "expected an unknown tag, got {error:?}"
    );
    let message = error.message(PATH);
    assert!(message.contains("not from this session"), "{message}");
    assert!(message.contains("never invent"), "must name the actual mistake: {message}");
}

/// The two rejections must not collapse into one. They need different fixes,
/// and a model told to re-read will keep inventing tags.
#[test]
fn the_two_tag_rejections_carry_different_messages() {
    let store = SnapshotStore::new();
    let old_tag = store.record(PATH, TEXT, Some(&[1, 2, 3, 4, 5, 6]));
    let changed = "one\nCHANGED\nthree\nfour\nfive\n";

    let stale = prepare(&store, PATH, changed, Some(&old_tag), &ops("CUT 2"), true)
        .expect_err("stale")
        .message(PATH);
    let unknown = prepare(&store, PATH, changed, Some("FFFF"), &ops("CUT 2"), true)
        .expect_err("unknown")
        .message(PATH);

    assert_ne!(stale, unknown);
}

/// An untagged section skips validation. The patcher decides policy; this layer
/// reports what it can prove, and with no tag it can prove nothing.
#[test]
fn an_untagged_section_skips_tag_validation() {
    let store = SnapshotStore::new();
    let prepared = prepare(&store, PATH, TEXT, None, &ops("CUT 2"), true)
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

    let error = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 5.=5:\n+FIVE"), true)
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

    let prepared = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 2.=2:\n+TWO"), true)
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

    let error = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 5.=5:\n+FIVE"), true)
        .expect_err("first attempt is refused");

    let message = error.message(PATH);
    assert!(message.contains("5:five"), "must show the real content: {message}");
    assert!(message.contains("count as seen"), "must say a retry will work: {message}");

    prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 5.=5:\n+FIVE"), true)
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
    let error = prepare(&store, PATH, &big, Some(&tag), &ops(&patch), true)
        .expect_err("far too many unseen lines");

    let message = error.message(PATH);
    assert!(message.contains("re-read the range"), "{message}");

    prepare(&store, PATH, &big, Some(&tag), &ops(&patch), true)
        .expect_err("the retry must still fail, or the guard is walkable");
}

/// A very wide line truncates in the reveal, which flags the whole reveal, so
/// nothing merges. Otherwise a model receives an "ok to retry" signal while
/// part of each line remains unseen.
#[test]
fn a_column_clipped_reveal_merges_nothing() {
    let store = SnapshotStore::new();
    let wide = format!("head\n{}\nfoot\n", "a".repeat(SEEN_LINE_REVEAL_MAX_COLUMNS + 100));
    let tag = store.record(PATH, &wide, Some(&[1]));

    let error = prepare(&store, PATH, &wide, Some(&tag), &ops("PUT 2.=2:\n+X"), true)
        .expect_err("line 2 was not displayed");
    assert!(error.message(PATH).contains("re-read the range"));

    prepare(&store, PATH, &wide, Some(&tag), &ops("PUT 2.=2:\n+X"), true)
        .expect_err("a clipped reveal must not unlock the retry");
}

/// Absent provenance means the guard cannot judge, so it stands aside. This is
/// what lets a producer that does not record yet keep working instead of
/// blocking every edit to files it touched.
#[test]
fn absent_provenance_disables_the_guard_rather_than_blocking() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, None);

    prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 5.=5:\n+FIVE"), true)
        .expect("no provenance means the guard cannot judge");
}

/// The guard is switchable. omp ships it off; we default it on, and the
/// difference is a policy decision that should be visible in a test.
#[test]
fn the_guard_can_be_disabled() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 5.=5:\n+FIVE"), false)
        .expect("with the guard off, an unseen line applies");
}

/// Insert anchors are checked too: inserting beside an unseen line still means
/// placing content the model cannot see the context for.
#[test]
fn the_guard_covers_insert_anchors_not_only_ranges() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1, 2]));

    prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT >4:\n+X"), true)
        .expect_err("line 4 was never displayed");
}

/// File-level ops have no line anchors, so the guard has nothing to judge and
/// must not invent an objection.
#[test]
fn file_level_ops_are_not_blocked_by_the_guard() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, TEXT, Some(&[1]));

    prepare(&store, PATH, TEXT, Some(&tag), &ops("MV other.rs"), true)
        .expect("a move anchors no lines");
}

// ─── no-op detection ─────────────────────────────────────────────────────────

/// A patch that changes nothing is reported rather than written. omp's issue
/// #2081 recorded 182 identical no-op repeats in 205 calls, so this is the
/// signal a loop guard needs.
#[test]
fn a_patch_that_changes_nothing_is_refused() {
    let (store, tag) = store_with_full_read();

    let error = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 2.=2:\n+two"), true)
        .expect_err("the body is byte-identical");

    assert!(matches!(error, RejectReason::NoOp), "got {error:?}");
    assert!(error.message(PATH).contains("re-read"), "must suggest a way out");
}

/// A move that changes no content is still a change, so it must not be
/// mistaken for a no-op.
#[test]
fn a_move_alone_is_not_a_no_op() {
    let (store, tag) = store_with_full_read();

    prepare(&store, PATH, TEXT, Some(&tag), &ops("MV other.rs"), true)
        .expect("relocating a file is a change");
}

// ─── chaining ────────────────────────────────────────────────────────────────

/// The returned tag is what makes an edit chain work without a re-read: it
/// anchors the next edit against the content this one produced.
#[test]
fn the_result_carries_a_tag_for_the_next_edit() {
    let (store, tag) = store_with_full_read();

    let first = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 2.=2:\n+TWO"), true)
        .expect("first edit");

    assert_eq!(first.new_tag, compute_file_hash(&first.after));
    assert_ne!(first.new_tag, tag, "content changed, so the tag must change");
}

/// Recording the post-edit content with no provenance is what lets a chain
/// continue: you wrote those lines, so you have seen them.
#[test]
fn a_chained_edit_applies_against_the_new_tag() {
    let (store, tag) = store_with_full_read();

    let first = prepare(&store, PATH, TEXT, Some(&tag), &ops("PUT 2.=2:\n+TWO"), true)
        .expect("first edit");
    store.record(PATH, &first.after, None);

    prepare(
        &store,
        PATH,
        &first.after,
        Some(&first.new_tag),
        &ops("PUT 3.=3:\n+THREE"),
        true,
    )
    .expect("the second edit anchors against the first edit's tag");
}
