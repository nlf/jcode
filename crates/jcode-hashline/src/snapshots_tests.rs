//! Behaviour ported from omp's `test/snapshots.test.ts`, whose 12 cases are the
//! specification, plus concurrency tests that have no counterpart there.
//!
//! omp cannot have the concurrency tests: their tool calls are sequential, so
//! an unsynchronised map is sound for them. jcode's `batch` runs sub-calls on a
//! `FuturesUnordered` sharing one `session_id`, so those are the cases where a
//! naive port would lose provenance silently.

use super::*;

const PATH: &str = "/tmp/__hashline-snapshots__.ts";
const OTHER: &str = "/tmp/__hashline-other__.ts";

/// Two texts omp records as both hashing to `1D84` (their issue #4075).
const COLLIDE_A: &str = "line one 263\nline two 4471\n";
const COLLIDE_B: &str = "line one 410\nline two 6970\n";

fn seen(snapshot: &Snapshot) -> Vec<usize> {
    snapshot
        .seen_lines
        .as_ref()
        .map(|lines| lines.iter().copied().collect())
        .unwrap_or_default()
}

// ─── omp's 12 cases ──────────────────────────────────────────────────────────

#[test]
fn the_tag_is_derived_from_whole_file_content() {
    let store = SnapshotStore::new();
    let text = "L1\nL2\nL3\n";

    let tag = store.record(PATH, text, None);

    assert_eq!(tag.len(), 4);
    assert_eq!(tag, compute_file_hash(text));
}

/// Read fusion: two reads of one file state must widen a single snapshot, not
/// create two. This is what lets a partial read followed by another partial
/// read accumulate provenance under one anchor.
#[test]
fn repeated_reads_of_identical_content_fuse_onto_one_tag() {
    let store = SnapshotStore::new();
    let text = "alpha\nbeta\ngamma\n";

    let first = store.record(PATH, text, None);
    let second = store.record(PATH, text, None);

    assert_eq!(second, first);
    assert_eq!(store.head(PATH).unwrap().hash, first);
    assert_eq!(store.by_hash(PATH, &first).unwrap().text, text);
}

/// The prior version has to survive, or an edit chain cannot recover: a stale
/// tag must still resolve to the text it named so anchors can be remapped.
#[test]
fn changed_content_mints_a_new_tag_and_retains_the_prior_version() {
    let store = SnapshotStore::new();
    let v1 = "one\ntwo\n";
    let v2 = "one\ntwo\nthree\n";

    let tag1 = store.record(PATH, v1, None);
    let tag2 = store.record(PATH, v2, None);

    assert_ne!(tag2, tag1);
    assert_eq!(store.head(PATH).unwrap().hash, tag2);
    assert_eq!(store.by_hash(PATH, &tag1).unwrap().text, v1);
    assert_eq!(store.by_hash(PATH, &tag2).unwrap().text, v2);
}

/// A file reverting to earlier content is a real case (undo, a reverted patch),
/// and the reverted state is now current, so it must become head.
#[test]
fn re_observing_an_older_version_promotes_it_back_to_head() {
    let store = SnapshotStore::new();
    let v1 = "x\n";
    let v2 = "y\n";

    let tag1 = store.record(PATH, v1, None);
    store.record(PATH, v2, None);

    assert_eq!(store.record(PATH, v1, None), tag1);
    assert_eq!(store.head(PATH).unwrap().hash, tag1);
}

#[test]
fn per_path_history_is_bounded_and_drops_the_oldest() {
    let store = SnapshotStore::with_options(SnapshotStoreOptions {
        max_versions_per_path: 2,
        ..Default::default()
    });

    let tag_a = store.record(PATH, "A\n", None);
    let tag_b = store.record(PATH, "B\n", None);
    let tag_c = store.record(PATH, "C\n", None);

    assert_eq!(store.by_hash(PATH, &tag_c).unwrap().text, "C\n");
    assert_eq!(store.by_hash(PATH, &tag_b).unwrap().text, "B\n");
    assert!(store.by_hash(PATH, &tag_a).is_none(), "oldest must be dropped");
}

#[test]
fn tracked_paths_are_bounded_and_evict_the_coldest() {
    let store = SnapshotStore::with_options(SnapshotStoreOptions {
        max_paths: 1,
        ..Default::default()
    });

    let tag = store.record(PATH, "first\n", None);
    store.record(OTHER, "second\n", None);

    assert!(store.by_hash(PATH, &tag).is_none());
    assert!(store.head(PATH).is_none());
}

/// A tag is only meaningful for the path that minted it. Resolving across paths
/// would let an edit land on a different file that happens to collide.
#[test]
fn lookups_do_not_cross_paths() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, "shared\n", None);

    assert!(store.by_hash(OTHER, &tag).is_none());
}

#[test]
fn invalidate_drops_one_path_and_clear_drops_everything() {
    let store = SnapshotStore::new();
    let tag_a = store.record(PATH, "A\n", None);
    let tag_b = store.record(OTHER, "B\n", None);

    store.invalidate(PATH);
    assert!(store.by_hash(PATH, &tag_a).is_none());
    assert_eq!(store.by_hash(OTHER, &tag_b).unwrap().text, "B\n");

    store.clear();
    assert!(store.by_hash(OTHER, &tag_b).is_none());
}

/// After `MV`, tags minted against the source must still validate, or every
/// rename forces a re-read before any further edit.
#[test]
fn relocate_moves_history_and_provenance_to_the_new_path() {
    let store = SnapshotStore::new();
    let dest = "/tmp/__hashline-dest__.ts";
    let tag = store.record(PATH, "A\n", Some(&[1]));

    store.relocate(PATH, dest);

    assert!(store.by_hash(PATH, &tag).is_none());
    assert_eq!(store.by_hash(dest, &tag).unwrap().text, "A\n");
    assert_eq!(seen(&store.by_hash(dest, &tag).unwrap()), vec![1]);
    assert_eq!(store.head(dest).unwrap().hash, tag);
}

#[test]
fn find_by_hash_returns_every_retained_version_across_paths() {
    let store = SnapshotStore::new();
    let text = "shared\n";
    let tag = store.record(PATH, text, None);
    store.record(OTHER, text, None);

    let matches = store.find_by_hash(&tag);
    let mut paths: Vec<&str> = matches.iter().map(|m| m.path.as_str()).collect();
    paths.sort_unstable();

    assert_eq!(paths, vec![OTHER, PATH].into_iter().collect::<Vec<_>>());
    assert!(matches.iter().all(|m| m.hash == tag));

    let absent = if tag == "0000" { "FFFF" } else { "0000" };
    assert!(store.find_by_hash(absent).is_empty());
}

/// The heart of omp issue #4075. Sixteen-bit tags collide, so deduplication
/// must compare full text. Fusing on tag alone would attribute one text's seen
/// lines to the other and let the patcher resolve a tag to content the file
/// never held.
#[test]
fn colliding_texts_stay_separate_versions_with_separate_provenance() {
    assert_eq!(
        compute_file_hash(COLLIDE_A),
        compute_file_hash(COLLIDE_B),
        "fixture must actually collide, or this test proves nothing"
    );

    let store = SnapshotStore::new();
    let tag_a = store.record(PATH, COLLIDE_A, Some(&[1]));
    let tag_b = store.record(PATH, COLLIDE_B, Some(&[2]));
    assert_eq!(tag_a, tag_b);

    assert_eq!(store.by_content(PATH, COLLIDE_A).unwrap().text, COLLIDE_A);
    assert_eq!(store.by_content(PATH, COLLIDE_B).unwrap().text, COLLIDE_B);

    assert_eq!(seen(&store.by_content(PATH, COLLIDE_A).unwrap()), vec![1]);
    assert_eq!(seen(&store.by_content(PATH, COLLIDE_B).unwrap()), vec![2]);

    // Ambiguous by construction: `by_hash` yields the most recent collider.
    assert_eq!(store.by_hash(PATH, &tag_a).unwrap().text, COLLIDE_B);
    assert_eq!(store.head(PATH).unwrap().text, COLLIDE_B);
}

#[test]
fn identical_reads_of_a_colliding_text_still_fuse_and_union_provenance() {
    let store = SnapshotStore::new();
    let first = store.record(PATH, COLLIDE_A, Some(&[1]));
    let again = store.record(PATH, COLLIDE_A, Some(&[2]));

    assert_eq!(again, first);
    assert_eq!(seen(&store.by_content(PATH, COLLIDE_A).unwrap()), vec![1, 2]);
    assert!(
        store.by_content(PATH, COLLIDE_B).is_none(),
        "the other collider was never recorded, so it must not resolve"
    );
}

// ─── provenance semantics ────────────────────────────────────────────────────

/// `None` and `Some(empty)` are different answers. Absent provenance means the
/// guard is skipped, so a producer that records nothing must not accidentally
/// assert that nothing was seen.
#[test]
fn absent_provenance_is_distinct_from_empty_provenance() {
    let store = SnapshotStore::new();

    store.record(PATH, "a\n", None);
    assert!(store.head(PATH).unwrap().seen_lines.is_none());

    store.record(OTHER, "a\n", Some(&[]));
    assert_eq!(
        store.head(OTHER).unwrap().seen_lines,
        Some(BTreeSet::new()),
        "an explicit empty read is recorded provenance, not absent provenance"
    );
}

/// A tag minted before its body was formatted still needs provenance attached.
#[test]
fn provenance_can_be_attached_after_the_tag_was_minted() {
    let store = SnapshotStore::new();
    let tag = store.record(PATH, "a\nb\nc\n", None);

    store.record_seen_lines(PATH, &tag, &[2, 3]);

    assert_eq!(seen(&store.head(PATH).unwrap()), vec![2, 3]);
}

#[test]
fn attaching_provenance_to_an_unknown_tag_is_a_no_op() {
    let store = SnapshotStore::new();
    store.record(PATH, "a\n", None);

    store.record_seen_lines(PATH, "FFFF", &[1]);
    store.record_seen_lines("/nope", "0000", &[1]);

    assert!(store.head(PATH).unwrap().seen_lines.is_none());
}

/// Two partial reads of one state accumulate. Reading 1-50 then 200-250 must
/// leave both ranges editable, or the second read revokes the first.
#[test]
fn partial_reads_of_one_state_accumulate_provenance() {
    let store = SnapshotStore::new();
    let text = "x\n".repeat(300);

    store.record(PATH, &text, Some(&[1, 2, 3]));
    store.record(PATH, &text, Some(&[200, 201]));

    assert_eq!(seen(&store.head(PATH).unwrap()), vec![1, 2, 3, 200, 201]);
}

// ─── concurrency: no omp counterpart ─────────────────────────────────────────

/// `batch` drives sub-calls on a `FuturesUnordered` sharing one `session_id`,
/// so concurrent reads of one file are reachable in normal use. omp's store
/// mutates `seenLines` in place without synchronisation, which is sound only
/// because their calls are sequential. If provenance is lost here, the patcher
/// later rejects an edit the model was entitled to make.
#[test]
fn concurrent_reads_of_one_file_lose_no_provenance() {
    use std::thread;

    let store = SnapshotStore::new();
    let text = "line\n".repeat(64);

    thread::scope(|scope| {
        for line in 1..=64usize {
            let store = store.clone();
            let text = text.clone();
            scope.spawn(move || {
                store.record(PATH, &text, Some(&[line]));
            });
        }
    });

    let recorded = seen(&store.head(PATH).unwrap());
    assert_eq!(
        recorded,
        (1..=64).collect::<Vec<_>>(),
        "every concurrent read must survive the union"
    );
}

/// Concurrent records of *different* content must not corrupt the history or
/// exceed its bound.
#[test]
fn concurrent_records_of_distinct_content_keep_the_history_bounded() {
    use std::thread;

    let store = SnapshotStore::with_options(SnapshotStoreOptions {
        max_versions_per_path: 4,
        ..Default::default()
    });

    thread::scope(|scope| {
        for i in 0..32usize {
            let store = store.clone();
            scope.spawn(move || {
                store.record(PATH, &format!("version {i}\n"), None);
            });
        }
    });

    let inner = store.inner.lock().expect("poisoned");
    let history = inner.versions.get(PATH).expect("path recorded");
    assert!(
        history.len() <= 4,
        "history must stay bounded under concurrency, got {}",
        history.len()
    );
}

/// A clone shares one store, which is what makes a `batch` sub-call see the
/// parent's provenance. If cloning copied, every sub-call would start blind.
#[test]
fn clones_share_one_store() {
    let store = SnapshotStore::new();
    let clone = store.clone();

    let tag = store.record(PATH, "shared\n", Some(&[1]));
    clone.record_seen_lines(PATH, &tag, &[2]);

    assert_eq!(seen(&store.head(PATH).unwrap()), vec![1, 2]);
}
