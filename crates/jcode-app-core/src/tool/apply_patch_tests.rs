//! Behaviour tests for the `apply_patch` tool.
//!
//! Parsing and application moved to `jcode-patch`, ported from omp, and are
//! tested there against omp's spec. What remains here is what only the tool
//! layer can be wrong about: the delete guard, the filesystem, and whether a
//! partial patch reports honestly.

use super::*;
use crate::tool::{ToolContext, ToolExecutionMode};

fn ctx(dir: &std::path::Path, session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "c".to_string(),
        working_dir: Some(dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn run(dir: &std::path::Path, patch: &str) -> Result<ToolOutput> {
    ApplyPatchTool::new()
        .execute(serde_json::json!({ "patch_text": patch }), ctx(dir, "apply-patch-test"))
        .await
}

/// The whole reason for the port. A patch where the second file fails must not
/// apply the third, and must report an error naming what landed and what did
/// not, so the caller re-issues exactly the missing work.
#[tokio::test]
async fn a_multi_file_failure_stops_and_reports_honestly() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "a\n").expect("a");

    let error = run(
        temp.path(),
        "*** Begin Patch\n\
         *** Update File: a.txt\n@@\n-a\n+A\n\
         *** Update File: missing.txt\n@@\n-x\n+y\n\
         *** Add File: c.txt\n+new content\n\
         *** End Patch",
    )
    .await
    .expect_err("a partial patch must fail the call");

    let message = error.to_string();
    assert!(message.contains("missing.txt"), "{message}");
    assert!(message.contains("NOT applied"), "{message}");
    assert!(message.contains("c.txt"), "{message}");

    assert_eq!(
        std::fs::read_to_string(temp.path().join("a.txt")).expect("a.txt"),
        "A\n",
        "the first file landed, and the message says so"
    );
    assert!(
        !temp.path().join("c.txt").exists(),
        "the third entry must not be applied after the second failed"
    );
}

#[tokio::test]
async fn a_successful_multi_file_patch_applies_every_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "a\n").expect("a");
    std::fs::write(temp.path().join("b.txt"), "b\n").expect("b");

    let output = run(
        temp.path(),
        "*** Begin Patch\n\
         *** Update File: a.txt\n@@\n-a\n+A\n\
         *** Update File: b.txt\n@@\n-b\n+B\n\
         *** End Patch",
    )
    .await
    .expect("both files should apply");

    assert_eq!(std::fs::read_to_string(temp.path().join("a.txt")).unwrap(), "A\n");
    assert_eq!(std::fs::read_to_string(temp.path().join("b.txt")).unwrap(), "B\n");
    assert!(output.output.contains("M a.txt"), "{}", output.output);
    assert!(output.output.contains("M b.txt"), "{}", output.output);
}

#[tokio::test]
async fn a_create_writes_the_file_and_its_parents() {
    let temp = tempfile::tempdir().expect("tempdir");

    run(
        temp.path(),
        "*** Begin Patch\n*** Add File: nested/deep/new.txt\n+hello\n*** End Patch",
    )
    .await
    .expect("create should succeed");

    assert_eq!(
        std::fs::read_to_string(temp.path().join("nested/deep/new.txt")).expect("created"),
        "hello\n"
    );
}

#[tokio::test]
async fn a_move_writes_the_destination_and_removes_the_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("from.txt"), "content\n").expect("from");

    run(
        temp.path(),
        "*** Begin Patch\n*** Update File: from.txt\n*** Move to: to.txt\n@@\n-content\n+moved\n*** End Patch",
    )
    .await
    .expect("move should succeed");

    assert!(!temp.path().join("from.txt").exists(), "the source is gone");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("to.txt")).expect("destination"),
        "moved\n"
    );
}

/// A stale patch fails rather than applying to the wrong lines, and the file is
/// left alone.
#[tokio::test]
async fn a_stale_patch_fails_and_writes_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "actual\n").expect("a");

    let error = run(
        temp.path(),
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-something else\n+new\n*** End Patch",
    )
    .await
    .expect_err("a stale patch must fail");

    assert!(error.to_string().contains("Re-read"), "{error}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("a.txt")).unwrap(),
        "actual\n",
        "a failed patch must not write"
    );
}

/// An envelope with no sections is a mistake worth naming, not a silent no-op.
#[tokio::test]
async fn an_empty_envelope_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let error = run(temp.path(), "*** Begin Patch\n*** End Patch")
        .await
        .expect_err("no sections");

    assert!(error.to_string().contains("no file sections"), "{error}");
}

/// A malformed envelope reports the parse error rather than applying part of it.
#[tokio::test]
async fn a_malformed_envelope_reports_its_parse_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let error = run(temp.path(), "not a patch at all")
        .await
        .expect_err("malformed");

    assert!(error.to_string().contains("*** Begin Patch"), "{error}");
}

#[tokio::test]
async fn apply_patch_refuses_to_delete_a_protected_path() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().to_path_buf();
    let _home = crate::tool::home_override::HomeOverride::set(&home);

    // A credential file inside the protected ~/.ssh directory.
    let ssh = home.join(".ssh");
    std::fs::create_dir_all(&ssh).expect("ssh dir");
    let key = ssh.join("id_ed25519");
    std::fs::write(&key, "PRIVATE KEY").expect("key");

    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
        key.display()
    );
    let result = ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-gate".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(std::path::PathBuf::from("/tmp")),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await;


    // Now an Err rather than an Ok carrying refusal text: a patch that did not
    // apply must take the error branch, which is what the port changed.
    let error = result.expect_err("a refused delete must fail the call");
    assert!(
        error.to_string().contains("refused"),
        "expected a refusal: {error}"
    );
    assert!(
        key.exists(),
        "apply_patch must not delete a protected credential file"
    );
}

#[tokio::test]
async fn apply_patch_still_deletes_ordinary_files() {
    // The guard must not break the tool's normal job.
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("obsolete.rs");
    std::fs::write(&target, "fn old() {}\n").expect("file");

    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
        target.display()
    );
    ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-ok".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("ordinary delete should succeed");

    assert!(!target.exists(), "an ordinary file should still be deleted");
}
