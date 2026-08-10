//! Recovery tests, ported from oh-my-pi's `recovery-session-chain.test.ts`.
//!
//! Their assertions are the specification, so each test below names the
//! behaviour rather than the mechanism, and the exact expected text is pinned
//! wherever they pinned it. Where a case exercises something jcode does not
//! have, it is recorded as such rather than silently dropped.

use super::*;
use crate::format::compute_file_hash;
use crate::parser::parse_ops;

const PATH: &str = "/tmp/recovery-fixture.ts";

fn lines(rows: &[&str]) -> String {
    format!("{}\n", rows.join("\n"))
}

fn ops(patch: &str) -> Vec<Op> {
    parse_ops(patch).expect("fixture patch parses").ops
}

/// Two retained versions where a prior in-session edit rewrote line 5.
fn seed_two_snapshots() -> (SnapshotStore, String, String) {
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"]);
    let v1 = lines(&[
        "L1",
        "L2",
        "L3",
        "L4",
        "L5-CHANGED",
        "L6",
        "L7",
        "L8",
        "L9",
        "L10",
    ]);
    let h0 = store.record(PATH, &v0, None);
    store.record(PATH, &v1, None);
    (store, h0, v1)
}

#[test]
fn refuses_replay_when_the_anchor_line_itself_changed() {
    let (store, h0, v1) = seed_two_snapshots();
    // Anchored at the exact line a prior edit rewrote. Replaying would
    // overwrite "L5-CHANGED" with a payload authored against the old "L5",
    // which is corruption rather than recovery.
    let recovered = try_recover(&store, PATH, &v1, &h0, &ops("PUT 5=5:\n+L5-MODEL"));
    assert_eq!(recovered, None);
}

#[test]
fn replays_onto_current_when_every_anchor_line_is_unchanged() {
    let (store, h0, v1) = seed_two_snapshots();
    let recovered = try_recover(&store, PATH, &v1, &h0, &ops("PUT 3=3:\n+L3-MODEL"))
        .expect("line 3 is unchanged between the two versions");
    assert!(recovered.text.contains("L3-MODEL"));
    // The edit lands on top of current content, so the unrelated prior change
    // has to survive it.
    assert!(recovered.text.contains("L5-CHANGED"));
}

#[test]
fn a_zero_offset_recovery_against_an_older_snapshot_blames_the_session_chain() {
    let (store, h0, v1) = seed_two_snapshots();
    let recovered =
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 3=3:\n+L3-MODEL")).expect("recovers");
    assert_eq!(recovered.warnings, vec![RECOVERY_SESSION_CHAIN_WARNING]);
}

#[test]
fn an_external_write_is_named_as_external_rather_than_as_a_session_chain() {
    // Only one version is retained, and the live file differs from it, so
    // nothing in this session can explain the drift.
    let store = SnapshotStore::new();
    let snapshot = lines(&["L1", "L2", "L3", "L4"]);
    let tag = store.record(PATH, &snapshot, None);
    let current = lines(&["L1", "L2", "L3", "L4", "APPENDED-BY-SOMEONE-ELSE"]);

    let recovered =
        try_recover(&store, PATH, &current, &tag, &ops("PUT 2=2:\n+L2-MODEL")).expect("recovers");
    assert_eq!(recovered.warnings, vec![RECOVERY_EXTERNAL_WARNING]);
}

#[test]
fn recovers_anchors_shifted_by_a_prior_insertion() {
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4", "L5", "L6"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["L1", "L2", "INSERTED", "L3", "L4", "L5", "L6"]);
    store.record(PATH, &v1, None);

    let recovered =
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 5=5:\n+L5-MODEL")).expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&["L1", "L2", "INSERTED", "L3", "L4", "L5-MODEL", "L6"])
    );
    assert_eq!(recovered.warnings, vec![RECOVERY_LINE_REMAP_WARNING]);
}

#[test]
fn recovers_anchors_shifted_by_a_prior_deletion() {
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4", "L5", "L6"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["L1", "L3", "L4", "L5", "L6"]);
    store.record(PATH, &v1, None);

    let recovered =
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 5=5:\n+L5-MODEL")).expect("recovers");
    assert_eq!(recovered.text, lines(&["L1", "L3", "L4", "L5-MODEL", "L6"]));
    assert_eq!(recovered.warnings, vec![RECOVERY_LINE_REMAP_WARNING]);
}

#[test]
fn refuses_a_duplicate_line_remap_when_the_surrounding_context_no_longer_matches() {
    let store = SnapshotStore::new();
    let v0 = lines(&["start", "DUP", "mid", "DUP", "tail"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["start", "mid", "DUP", "CHANGED", "tail"]);
    store.record(PATH, &v1, None);

    assert_eq!(
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 4=4:\n+MODEL")),
        None
    );
}

#[test]
fn refuses_to_relocate_a_stale_replacement_onto_a_duplicate_of_its_target() {
    // The dangerous case: the block the edit targeted was changed, but an
    // identical copy of it still exists further down. Relocating there would
    // apply the edit to the wrong one and look entirely successful.
    let store = SnapshotStore::new();
    let block = ["head", "TARGET_A", "TARGET_B", "ctx1", "ctx2", "ctx3"];
    let mut v0_rows: Vec<&str> = block.to_vec();
    v0_rows.push("middle");
    v0_rows.extend_from_slice(&block);
    v0_rows.push("tail");
    let v0 = lines(&v0_rows);
    let tag = store.record(PATH, &v0, None);

    let mut current_rows = vec!["head", "CHANGED_A", "CHANGED_B", "ctx1", "ctx2", "ctx3", "middle"];
    current_rows.extend_from_slice(&block);
    current_rows.push("tail");
    let current = lines(&current_rows);

    assert_eq!(
        try_recover(
            &store,
            PATH,
            &current,
            &tag,
            &ops("PUT 2=3:\n+MODEL_A\n+MODEL_B")
        ),
        None
    );
    // The surviving copy must be untouched, which is the whole point.
    assert!(current.contains("TARGET_A\nTARGET_B"));
}

#[test]
fn refuses_an_isolated_unique_line_when_neither_neighbour_moved_with_it() {
    // The anchor line is unique and did map, but both its neighbours changed,
    // so nothing corroborates that it is still the same place.
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4", "T", "L6"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["X", "L1", "L2", "L3", "L4", "BEFORE", "T", "AFTER", "L6"]);
    store.record(PATH, &v1, None);

    assert_eq!(
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 5=5:\n+MODEL")),
        None
    );
}

#[test]
fn recovers_a_range_covering_a_duplicated_line_when_context_still_matches() {
    // A range spanning a duplicated line and a unique one still remaps through
    // a prior insertion: the strict branch is satisfied because every
    // neighbour the run has moved with it.
    let store = SnapshotStore::new();
    let v0 = lines(&["alpha", "DUP", "beta", "DUP", "omega"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["alpha", "INSERTED", "DUP", "beta", "DUP", "omega"]);
    store.record(PATH, &v1, None);

    let recovered = try_recover(&store, PATH, &v1, &h0, &ops("PUT 3=4:\n+B-MODEL\n+MODEL"))
        .expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&["alpha", "INSERTED", "DUP", "B-MODEL", "MODEL", "omega"])
    );
    assert_eq!(recovered.warnings, vec![RECOVERY_LINE_REMAP_WARNING]);
}

/// Two distinct texts sharing one 4-hex tag.
///
/// 16-bit tags collide within a few hundred candidates, so the search is cheap.
/// The texts share their head and tail so that a line-anchored edit against one
/// is plausible-but-wrong against the other.
fn colliding_texts() -> (String, String) {
    let text_for = |n: usize| lines(&["shared head", &format!("unique payload {n}"), "shared tail"]);
    let mut by_tag: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for n in 0.. {
        let text = text_for(n);
        let tag = compute_file_hash(&text);
        if let Some(prior) = by_tag.get(&tag) {
            return (text_for(*prior), text);
        }
        by_tag.insert(tag, n);
    }
    unreachable!("a 16-bit tag space collides long before the counter runs out")
}

#[test]
fn recovers_against_the_most_recently_retained_text_when_two_collide_on_one_tag() {
    let (older, newer) = colliding_texts();
    let tag = compute_file_hash(&older);
    assert_eq!(compute_file_hash(&newer), tag);
    assert_ne!(older, newer);

    let store = SnapshotStore::new();
    store.record(PATH, &older, None);
    store.record(PATH, &newer, None);

    // Live has drifted from both colliders, so recovery cannot shortcut. The
    // tag cannot name a unique base, so the most recent one is used.
    let current = format!("{newer}drifted trailer\n");
    let recovered =
        try_recover(&store, PATH, &current, &tag, &ops("PUT 2=2:\n+model payload")).expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&["shared head", "model payload", "shared tail", "drifted trailer"])
    );
}

#[test]
fn recovers_when_exactly_one_retained_text_carries_the_tag() {
    let (older, _) = colliding_texts();
    let store = SnapshotStore::new();
    let tag = store.record(PATH, &older, None);

    let current = format!("{older}drifted trailer\n");
    let recovered =
        try_recover(&store, PATH, &current, &tag, &ops("PUT 2=2:\n+model payload")).expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&["shared head", "model payload", "shared tail", "drifted trailer"])
    );
}

#[test]
fn refuses_when_the_tag_names_nothing_this_store_retained() {
    let store = SnapshotStore::new();
    let current = lines(&["L1", "L2"]);
    assert_eq!(
        try_recover(&store, PATH, &current, "FFFF", &ops("PUT 1=1:\n+X")),
        None
    );
}

#[test]
fn refuses_a_recovery_that_would_change_nothing() {
    // Placing a payload identical to what is already there is not a successful
    // recovery: the model should be told to re-read, not told its edit was
    // redundant.
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["L1", "L2", "L3", "L4", "APPENDED"]);
    store.record(PATH, &v1, None);

    assert_eq!(
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 2=2:\n+L2")),
        None
    );
}

#[test]
fn refuses_when_two_anchors_moved_by_different_distances() {
    // An insertion between the two anchors means the region they straddle
    // changed size, so the patch describes a layout that no longer exists.
    let store = SnapshotStore::new();
    let v0 = lines(&["A", "B", "C", "D", "E", "F"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["A", "B", "INSERTED", "C", "D", "E", "F"]);
    store.record(PATH, &v1, None);

    assert_eq!(
        try_recover(
            &store,
            PATH,
            &v1,
            &h0,
            &ops("PUT 1=1:\n+A-MODEL\n\nPUT 5=5:\n+E-MODEL")
        ),
        None
    );
}

#[test]
fn an_edit_anchored_only_at_head_or_tail_is_not_anchor_scoped() {
    // The caller uses this to apply position-stable edits on drift instead of
    // sending them through recovery, which would refuse them for want of any
    // anchor to prove.
    assert!(!has_anchor_scoped_op(&ops("PUT >$:\n+appended")));
    assert!(!has_anchor_scoped_op(&ops("PUT <1:\n+prepended")));
    assert!(has_anchor_scoped_op(&ops("PUT 3=3:\n+replaced")));
    assert!(has_anchor_scoped_op(&ops("CUT 2=3")));
}

#[test]
fn a_head_or_tail_only_patch_is_refused_by_recovery_itself() {
    // Belt and braces: even reached directly, recovery declines an edit whose
    // anchors it cannot prove, because there are none.
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["L1", "L2", "L3"]);
    store.record(PATH, &v1, None);

    assert_eq!(
        try_recover(&store, PATH, &v1, &h0, &ops("PUT >$:\n+appended")),
        None
    );
}

#[test]
fn the_first_changed_line_is_reported_against_the_current_file() {
    // Not against the snapshot the edit was authored on. A caller rendering a
    // diff uses this to point at the right row.
    let store = SnapshotStore::new();
    let v0 = lines(&["L1", "L2", "L3", "L4"]);
    let h0 = store.record(PATH, &v0, None);
    let v1 = lines(&["INSERTED", "L1", "L2", "L3", "L4"]);
    store.record(PATH, &v1, None);

    let recovered =
        try_recover(&store, PATH, &v1, &h0, &ops("PUT 3=3:\n+L3-MODEL")).expect("recovers");
    // Line 3 of the snapshot is line 4 of the current file.
    assert_eq!(recovered.first_changed_line, Some(4));
}

/// omp pins that recovery remaps correctly when a file contains a lone
/// surrogate, because their diff works over UTF-16 code units.
///
/// **Not portable, and deliberately not faked.** A Rust `String` cannot hold a
/// lone surrogate at all, so the failure mode the test guards against does not
/// exist here. The nearest real question is whether content that is merely
/// unusual still remaps, which this covers instead.
#[test]
fn remaps_anchors_in_a_file_containing_unusual_scalar_values() {
    let store = SnapshotStore::new();
    let v0 = lines(&["head", "odd \u{fffd} replacement char", "target line", "tail"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&[
        "inserted above",
        "head",
        "odd \u{fffd} replacement char",
        "target line",
        "tail",
    ]);

    let recovered = try_recover(&store, PATH, &current, &tag, &ops("PUT 3=3:\n+model payload"))
        .expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&[
            "inserted above",
            "head",
            "odd \u{fffd} replacement char",
            "model payload",
            "tail"
        ])
    );
}

// The tests below exist because the ones above did not discriminate. Each was
// written after a deliberate break in the implementation went undetected, and
// each was confirmed to fail against that break. They pin the rules that decide
// whether an anchor is believed, which is where a wrong answer corrupts a file
// rather than merely refusing one.

#[test]
fn one_surviving_neighbour_is_enough_for_a_line_whose_text_is_unique() {
    // The anchor is unique, and only the line *after* it still corroborates the
    // move. Demanding both neighbours would refuse this, which is the stricter
    // and apparently safer reading, but it is wrong: a unique line plus one
    // agreeing neighbour already identifies the place, and requiring two makes
    // any edit next to another edit unrecoverable.
    let store = SnapshotStore::new();
    let v0 = lines(&["p1", "p2", "TARGET", "p4", "p5"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&["p1", "CHANGED", "TARGET", "p4", "p5"]);

    let recovered =
        try_recover(&store, PATH, &current, &tag, &ops("PUT 3=3:\n+MODEL")).expect("recovers");
    assert_eq!(
        recovered.text,
        lines(&["p1", "CHANGED", "MODEL", "p4", "p5"])
    );
}

#[test]
fn a_range_is_refused_when_a_line_inside_it_changed() {
    // Both endpoints of the range are unchanged, so checking only those would
    // accept. The interior is content the model read and chose to replace; if
    // it changed underneath, the replacement is authored against something that
    // no longer exists and the endpoints prove nothing about it.
    let store = SnapshotStore::new();
    let v0 = lines(&["A", "B", "C", "D", "E"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&["A", "B", "C-CHANGED", "D", "E"]);

    assert_eq!(
        try_recover(
            &store,
            PATH,
            &current,
            &tag,
            &ops("PUT 2=4:\n+X\n+Y")
        ),
        None
    );
}

#[test]
fn a_run_is_judged_by_the_lines_outside_it_not_by_its_own_members() {
    // Every line of the range survived, but both lines bracketing it changed,
    // so nothing independent confirms the range is still the same region.
    // Judging each anchor against its immediate neighbours would accept here,
    // because an anchor's neighbours are mostly other anchors, which are
    // equally suspect and prove nothing.
    let store = SnapshotStore::new();
    let v0 = lines(&["k1", "a", "b", "c", "k5"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&["CHANGED1", "a", "b", "c", "CHANGED5"]);

    assert_eq!(
        try_recover(&store, PATH, &current, &tag, &ops("PUT 2=4:\n+X")),
        None
    );
}

#[test]
fn a_line_that_became_ambiguous_is_judged_strictly_even_though_it_was_unique() {
    // "TARGET" is unique in the snapshot but repeats in the current file. Only
    // checking the snapshot for ambiguity would apply the loose rule and accept
    // on one neighbour, which is precisely how an edit gets relocated onto the
    // wrong copy of something that has since been duplicated.
    let store = SnapshotStore::new();
    let v0 = lines(&["A", "TARGET", "B", "C"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&["A-CHANGED", "TARGET", "B", "C", "TARGET"]);

    assert_eq!(
        try_recover(&store, PATH, &current, &tag, &ops("PUT 2=2:\n+MODEL")),
        None
    );
}

#[test]
fn a_duplicated_anchor_with_no_context_at_all_is_refused() {
    // An anchor whose text repeats and which has no neighbour on either side is
    // the case with the least evidence available, so it is refused rather than
    // accepted for want of anything arguing against it.
    //
    // Reaching that state takes some doing, and the shape is worth recording
    // because it is not the obvious one. A run covering a whole file usually
    // still has the phantom trailing element below it, which counts as a
    // neighbour. Only a file with no trailing newline, whose single line is the
    // anchor, genuinely has neither side. That line's text then repeats in the
    // file it drifted into, so there is nothing to tell the copies apart.
    let store = SnapshotStore::new();
    let tag = store.record(PATH, "a", None);

    assert_eq!(
        try_recover(&store, PATH, "a\na\n", &tag, &ops("PUT 1=1:\n+MODEL")),
        None
    );
}

#[test]
fn a_lone_line_with_no_context_is_refused_even_when_its_text_is_unique() {
    // Written expecting the opposite, on the reasoning that a unique line
    // identifies its own target and needs no corroboration. It does not behave
    // that way, and the actual rule is the better one.
    //
    // Both branches require at least one neighbour: the lenient branch accepts
    // when *one* neighbour agrees, which still means there has to be a
    // neighbour to agree. So a file whose only line is the anchor has no
    // evidence of any kind, and uniqueness of the text is not evidence that the
    // file is the same file. Refusing costs a re-read; accepting would place an
    // edit on the strength of nothing at all.
    let store = SnapshotStore::new();
    let tag = store.record(PATH, "solo", None);

    assert_eq!(
        try_recover(&store, PATH, "solo\nappended\n", &tag, &ops("PUT 1=1:\n+MODEL")),
        None
    );
}

#[test]
fn the_last_real_line_still_has_a_neighbour_below_it() {
    // A trailing newline leaves a phantom final element when the text is split,
    // and it counts as a line for neighbour purposes. Excluding it would leave
    // the last real line of every file with only one neighbour, so an edit
    // there would be refused whenever the line above it also changed.
    let store = SnapshotStore::new();
    let v0 = lines(&["A", "B", "LAST"]);
    let tag = store.record(PATH, &v0, None);
    let current = lines(&["A", "B-CHANGED", "LAST"]);

    let recovered =
        try_recover(&store, PATH, &current, &tag, &ops("PUT 3=3:\n+MODEL")).expect("recovers");
    assert_eq!(recovered.text, lines(&["A", "B-CHANGED", "MODEL"]));
}

#[test]
fn a_colliding_tag_is_still_blamed_on_whoever_actually_advanced_the_file() {
    // Written expecting a session-chain banner, on the reasoning that the edit
    // is anchored against the older of two colliders while the head is the
    // newer one. That was wrong, and the reason is worth keeping.
    //
    // History is newest-first and the tag lookup returns the first match, so a
    // tag the head carries always resolves to the head itself. The older
    // collider is never what recovery replays against, and there is no state in
    // which "the tag names an older version" and "the tag equals the head's
    // tag" are both true. The distinction is therefore decidable from tags
    // alone, and an extra text comparison is unreachable rather than safer.
    let (older, newer) = colliding_texts();
    let store = SnapshotStore::new();
    let tag = store.record(PATH, &older, None);
    store.record(PATH, &newer, None);
    assert_eq!(compute_file_hash(&newer), tag, "fixture must collide");

    let current = format!("{newer}drifted trailer\n");
    let recovered = try_recover(&store, PATH, &current, &tag, &ops("PUT 1=1:\n+replaced head"))
        .expect("recovers");
    // The head carries this tag, so the drift is attributed outside the session.
    assert_eq!(recovered.warnings, vec![RECOVERY_EXTERNAL_WARNING]);
}
