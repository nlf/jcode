//! End-to-end tests for `edit`'s hashline mode.
//!
//! These go through the real `EditTool` and a real filesystem, so they exercise
//! the whole seam: schema, dispatch, session store lookup, preflight, commit.
//! The library's own tests cover the patch semantics; these cover the wiring,
//! which is where the value is, since the library was already verified.

use super::*;
use crate::tool::edit::EditTool;
use crate::tool::{Tool, ToolExecutionMode};
use serde_json::json;

fn ctx(working_dir: std::path::PathBuf, session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "t".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Read a file through the real tool and return the tag it minted, which is the
/// only supported way to obtain one.
async fn read_tag(dir: &std::path::Path, session: &str, file: &str) -> String {
    let output = crate::tool::read::ReadTool::new()
        .execute(json!({ "file_path": file }), ctx(dir.to_path_buf(), session))
        .await
        .expect("read");
    let header = output.output.lines().next().unwrap_or_default();
    header
        .trim_start_matches(&format!("[{file}#"))
        .trim_end_matches(']')
        .to_string()
}

/// The whole point: read, patch against the tag, see the file change.
#[tokio::test]
async fn a_hashline_edit_applies_against_the_tag_read_minted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").expect("write");
    let tag = read_tag(temp.path(), "hl-apply", "f.txt").await;

    EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 2.=2:\n+TWO"),
            }),
            ctx(temp.path().to_path_buf(), "hl-apply"),
        )
        .await
        .expect("hashline edit");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "one\nTWO\nthree\n"
    );
}

/// A stale tag is the failure hashline exists to catch: the file changed since
/// the read, so the line numbers the model is anchoring to may mean something
/// else entirely. It must refuse rather than apply.
#[tokio::test]
async fn an_edit_against_a_stale_tag_is_refused_and_writes_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");
    let tag = read_tag(temp.path(), "hl-stale", "f.txt").await;

    // The file changes underneath, as a concurrent process would change it.
    std::fs::write(&path, "one\ntwo\nthree\n").expect("rewrite");

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-stale"),
        )
        .await
        .expect_err("a stale tag must be refused");

    assert!(
        error.to_string().to_lowercase().contains("read"),
        "the error should tell the model to re-read: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "one\ntwo\nthree\n",
        "a refused edit must not write"
    );
}

/// The preflight guarantee, at the level that matters to a user: when the
/// second file's section is bad, the first file must be untouched. This is the
/// failure mode the library's preflight() was built for, checked through the
/// tool rather than in isolation.
#[tokio::test]
async fn a_bad_section_leaves_earlier_files_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "alpha\n").expect("write a");
    std::fs::write(temp.path().join("b.txt"), "beta\n").expect("write b");
    let tag_a = read_tag(temp.path(), "hl-preflight", "a.txt").await;

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "a.txt",
                // a.txt's section is valid; b.txt's tag is fabricated.
                "input": format!(
                    "[a.txt#{tag_a}]\nPUT 1.=1:\n+ALPHA\n[b.txt#FFFF]\nPUT 1.=1:\n+BETA"
                ),
            }),
            ctx(temp.path().to_path_buf(), "hl-preflight"),
        )
        .await
        .expect_err("the fabricated tag must be refused");

    assert!(
        error.to_string().contains("b.txt"),
        "the error should name the section that failed: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("a.txt")).expect("read a"),
        "alpha\n",
        "the valid section was written despite a later section failing, \
         which is exactly what preflight exists to prevent"
    );
}

/// Two files in one call, both valid, both written.
#[tokio::test]
async fn one_call_can_patch_several_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "alpha\n").expect("write a");
    std::fs::write(temp.path().join("b.txt"), "beta\n").expect("write b");
    let tag_a = read_tag(temp.path(), "hl-multi", "a.txt").await;
    let tag_b = read_tag(temp.path(), "hl-multi", "b.txt").await;

    EditTool::new()
        .execute(
            json!({
                "file_path": "a.txt",
                "input": format!(
                    "[a.txt#{tag_a}]\nPUT 1.=1:\n+ALPHA\n[b.txt#{tag_b}]\nPUT 1.=1:\n+BETA"
                ),
            }),
            ctx(temp.path().to_path_buf(), "hl-multi"),
        )
        .await
        .expect("multi-file edit");

    assert_eq!(
        std::fs::read_to_string(temp.path().join("a.txt")).expect("read a"),
        "ALPHA\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("b.txt")).expect("read b"),
        "BETA\n"
    );
}

/// After an edit the store must describe the new content, or a second edit in
/// the same turn resolves its tag to text that is no longer on disk. The output
/// returns `new_tag` precisely so a follow-up needs no re-read.
#[tokio::test]
async fn a_second_edit_can_anchor_to_the_tag_the_first_returned() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");
    let tag = read_tag(temp.path(), "hl-chain", "f.txt").await;

    let output = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-chain"),
        )
        .await
        .expect("first edit");

    let next_tag = output
        .output
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("[f.txt#")
        .trim_end_matches(']')
        .to_string();

    EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{next_tag}]\nPUT 2.=2:\n+TWO"),
            }),
            ctx(temp.path().to_path_buf(), "hl-chain"),
        )
        .await
        .expect("second edit should anchor to the tag the first returned");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "ONE\nTWO\n"
    );
}

/// The old shape must keep working unchanged. This is the whole risk of
/// changing the schema rather than adding a sibling tool.
#[tokio::test]
async fn the_old_replacement_shape_still_works() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");

    EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "old_string": "two",
                "new_string": "TWO",
            }),
            ctx(temp.path().to_path_buf(), "hl-old"),
        )
        .await
        .expect("old-style edit");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "one\nTWO\n"
    );
}

/// Both shapes at once is ambiguous. Letting one silently win would apply half
/// of what was asked for.
#[tokio::test]
async fn passing_both_shapes_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\n").expect("write");

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "old_string": "one",
                "new_string": "ONE",
                "input": "[f.txt#ABCD]\nPUT 1.=1:\n+ONE",
            }),
            ctx(temp.path().to_path_buf(), "hl-both"),
        )
        .await
        .expect_err("both shapes at once must be refused");

    assert!(
        error.to_string().contains("not both"),
        "the error should say which to pick: {error}"
    );
}

/// Neither shape is a schema-level mistake the model can fix, so say so rather
/// than failing deserialization with a message about a missing field.
#[tokio::test]
async fn passing_neither_shape_explains_what_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\n").expect("write");

    let error = EditTool::new()
        .execute(
            json!({ "file_path": "f.txt" }),
            ctx(temp.path().to_path_buf(), "hl-neither"),
        )
        .await
        .expect_err("neither shape must be refused");

    let message = error.to_string();
    assert!(
        message.contains("input") && message.contains("old_string"),
        "the error should name both accepted shapes: {message}"
    );
}

/// A tag from another session still applies when it matches the file's actual
/// content, and that is correct rather than a leak.
///
/// The tag *is* the content hash. If it matches what is on disk right now, the
/// model is patching exactly the bytes it named, whichever session minted the
/// tag. Sessions are isolated for *staleness attribution*, checked below, not as
/// a capability boundary: hashline is a concurrency guard, not an authorization
/// one. Permissions are enforced elsewhere, on the path.
#[tokio::test]
async fn a_matching_tag_applies_across_sessions_because_the_tag_is_the_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\n").expect("write");
    let tag = read_tag(temp.path(), "hl-session-a", "f.txt").await;

    EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-session-b"),
        )
        .await
        .expect("a tag matching the file's content describes the right bytes");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "ONE\n",
        "the content matched the tag, so the edit should have applied"
    );
}

/// Where session isolation does show: a tag that does *not* match the file.
///
/// Within the minting session the store recognises it and can say "the file
/// changed since you read it". From another session it was never recorded, so
/// the honest report is that the tag is unknown. Same refusal either way; the
/// difference is which one the model is told, and only the first suggests a
/// re-read will fix it.
#[tokio::test]
async fn an_unmatched_tag_from_another_session_is_reported_as_unknown() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\n").expect("write");
    let tag = read_tag(temp.path(), "hl-attrib-a", "f.txt").await;
    std::fs::write(&path, "one\ntwo\n").expect("rewrite");

    let from_minting_session = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-attrib-a"),
        )
        .await
        .expect_err("stale tag")
        .to_string();

    let from_other_session = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-attrib-b"),
        )
        .await
        .expect_err("unknown tag")
        .to_string();

    assert_ne!(
        from_minting_session, from_other_session,
        "the store should distinguish a tag it minted from one it never saw"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "one\ntwo\n",
        "neither refusal should write"
    );
}

/// Input with no header at all is a common model mistake, and the message
/// should teach the format rather than report a parse failure.
#[tokio::test]
async fn input_without_a_header_explains_the_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\n").expect("write");

    let error = EditTool::new()
        .execute(
            json!({ "file_path": "f.txt", "input": "PUT 1.=1:\n+ONE" }),
            ctx(temp.path().to_path_buf(), "hl-noheader"),
        )
        .await
        .expect_err("headerless input must be refused");

    assert!(
        error.to_string().contains("[path#tag]") || error.to_string().contains("header"),
        "the error should teach the header format: {error}"
    );
}

/// The seen-line guard ships off, matching omp. Pinned as a test because it is
/// a deliberate choice rather than an oversight: `bash` can show the model file
/// content the store never recorded, so enforcing would refuse well-informed
/// edits.
#[test]
fn the_seen_line_guard_is_off_by_default() {
    assert!(
        !jcode_base::config::ToolConfig::default().edit_enforce_seen_lines,
        "enforce_seen_lines should default off, as omp ships it"
    );
}
