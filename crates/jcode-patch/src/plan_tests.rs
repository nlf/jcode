//! Behaviour spec for multi-file orchestration.
//!
//! The central case is oh-my-pi's `#4074-B` regression
//! (`test/core/apply-patch-multi-file.test.ts`): a multi-file patch stops at
//! the first per-file failure, reports the aggregate as an error, and surfaces
//! both the failed file and the ones never attempted.

use super::*;
use crate::envelope::parse;
use std::collections::HashMap;

/// A filesystem that only exists in the test.
fn files(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(path, content)| (path.to_string(), content.to_string()))
        .collect()
}

fn source(files: &HashMap<String, String>) -> impl FileSource + '_ {
    move |path: &str| files.get(path).cloned()
}

fn plan_patch(patch: &str, files: &HashMap<String, String>) -> PatchPlan {
    let hunks = parse(patch).expect("the patch should parse");
    plan(&hunks, &source(files))
}

#[test]
fn a_single_update_produces_its_new_content() {
    let files = files(&[("a.txt", "alpha\n")]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-alpha\n+ALPHA\n*** End Patch",
        &files,
    );

    assert!(!result.failed());
    assert_eq!(
        result.outcomes,
        vec![FileOutcome::Updated {
            path: "a.txt".to_string(),
            moved_to: None,
            content: "ALPHA\n".to_string(),
        }]
    );
}

#[test]
fn a_create_produces_its_contents() {
    let files = files(&[]);
    let result = plan_patch(
        "*** Begin Patch\n*** Add File: new.txt\n+one\n+two\n*** End Patch",
        &files,
    );

    assert_eq!(
        result.outcomes,
        vec![FileOutcome::Created {
            path: "new.txt".to_string(),
            content: "one\ntwo\n".to_string(),
        }]
    );
}

/// Overwriting silently would discard a file the caller forgot was there.
#[test]
fn creating_over_an_existing_file_is_refused() {
    let files = files(&[("a.txt", "existing\n")]);
    let result = plan_patch(
        "*** Begin Patch\n*** Add File: a.txt\n+new\n*** End Patch",
        &files,
    );

    assert!(result.failed());
    assert!(matches!(result.failure, Some((_, HunkError::Exists))));
}

#[test]
fn deleting_a_missing_file_is_refused() {
    let files = files(&[]);
    let result = plan_patch(
        "*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch",
        &files,
    );

    assert!(matches!(result.failure, Some((_, HunkError::Missing))));
}

#[test]
fn updating_a_missing_file_is_refused() {
    let files = files(&[]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: gone.txt\n@@\n-a\n+b\n*** End Patch",
        &files,
    );

    assert!(matches!(result.failure, Some((_, HunkError::Missing))));
}

#[test]
fn a_move_is_recorded_with_its_destination() {
    let files = files(&[("a.txt", "alpha\n")]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: a.txt\n*** Move to: b.txt\n@@\n-alpha\n+ALPHA\n*** End Patch",
        &files,
    );

    match &result.outcomes[0] {
        FileOutcome::Updated { moved_to, .. } => {
            assert_eq!(moved_to.as_deref(), Some("b.txt"));
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

/// omp's #4074-B, and the case our previous implementation got wrong: the
/// third entry must NOT be applied after the second one failed.
#[test]
fn application_stops_at_the_first_failure() {
    let files = files(&[("a.txt", "a\n")]);
    let result = plan_patch(
        "*** Begin Patch\n\
         *** Update File: a.txt\n@@\n-a\n+A\n\
         *** Update File: missing.txt\n@@\n-x\n+y\n\
         *** Add File: c.txt\n+new content\n\
         *** End Patch",
        &files,
    );

    assert!(result.failed(), "the aggregate must report failure");
    assert_eq!(
        result.outcomes.len(),
        1,
        "only the first file should have been applied"
    );
    assert_eq!(result.outcomes[0].path(), "a.txt");
    assert_eq!(
        result.failure.as_ref().map(|(path, _)| path.as_str()),
        Some("missing.txt")
    );
    assert_eq!(
        result.skipped,
        vec!["c.txt".to_string()],
        "the third entry must be left unattempted, not applied around the failure"
    );
}

/// The caller has to re-issue exactly the missing work, so the message names
/// what landed, what failed and what was skipped.
#[test]
fn the_failure_message_names_applied_failed_and_skipped() {
    let files = files(&[("a.txt", "a\n")]);
    let result = plan_patch(
        "*** Begin Patch\n\
         *** Update File: a.txt\n@@\n-a\n+A\n\
         *** Update File: missing.txt\n@@\n-x\n+y\n\
         *** Add File: c.txt\n+new\n\
         *** End Patch",
        &files,
    );

    let message = result.failure_message().expect("a failed plan has a message");
    assert!(message.contains("missing.txt"), "{message}");
    assert!(message.contains("c.txt"), "{message}");
    assert!(message.contains("NOT applied"), "{message}");
    assert!(
        message.contains("a.txt") && message.contains("still on disk"),
        "the caller must know a.txt already landed: {message}"
    );
}

/// A failure on the very first file has nothing applied to report, and saying
/// so would be noise.
#[test]
fn a_first_file_failure_reports_nothing_as_applied() {
    let files = files(&[]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: missing.txt\n@@\n-x\n+y\n*** End Patch",
        &files,
    );

    let message = result.failure_message().expect("failed");
    assert!(
        !message.contains("still on disk"),
        "nothing landed, so nothing should be listed: {message}"
    );
}

#[test]
fn a_successful_plan_has_no_failure_message() {
    let files = files(&[("a.txt", "a\n")]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+A\n*** End Patch",
        &files,
    );

    assert_eq!(result.failure_message(), None);
    assert!(result.skipped.is_empty());
}

/// A patch that cannot apply to the content it found is a failure, not a
/// silent no-change.
#[test]
fn a_stale_hunk_fails_the_file() {
    let files = files(&[("a.txt", "actual content\n")]);
    let result = plan_patch(
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-something else\n+new\n*** End Patch",
        &files,
    );

    assert!(matches!(
        result.failure,
        Some((_, HunkError::Apply(ApplyError::NotFound { .. })))
    ));
}

#[test]
fn several_files_all_applying_produce_several_outcomes() {
    let files = files(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let result = plan_patch(
        "*** Begin Patch\n\
         *** Update File: a.txt\n@@\n-a\n+A\n\
         *** Update File: b.txt\n@@\n-b\n+B\n\
         *** End Patch",
        &files,
    );

    assert!(!result.failed());
    assert_eq!(result.outcomes.len(), 2);
}

/// omp's §9.1 summary. A rename reports as a modification of the original
/// path rather than a delete plus an add.
#[test]
fn the_summary_marks_each_file_by_operation() {
    let outcomes = vec![
        FileOutcome::Created {
            path: "added.txt".to_string(),
            content: String::new(),
        },
        FileOutcome::Updated {
            path: "changed.txt".to_string(),
            moved_to: Some("moved.txt".to_string()),
            content: String::new(),
        },
        FileOutcome::Deleted {
            path: "gone.txt".to_string(),
        },
    ];

    let summary = summary(&outcomes);
    assert!(summary.starts_with("Success."), "{summary}");
    assert!(summary.contains("A added.txt"), "{summary}");
    assert!(
        summary.contains("M changed.txt"),
        "a rename is an M on the original path: {summary}"
    );
    assert!(summary.contains("D gone.txt"), "{summary}");
    assert!(
        !summary.contains("moved.txt"),
        "the destination is not listed separately: {summary}"
    );
}
