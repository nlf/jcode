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
        .execute(
            json!({ "file_path": file }),
            ctx(dir.to_path_buf(), session),
        )
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

/// A stale tag means the file changed since the read, so the line numbers the
/// model is anchoring to may mean something else entirely. When the line it
/// targeted is still there, recovery relocates the edit onto it and says so.
///
/// This test used to assert refusal, which was correct before recovery existed
/// and is now the wrong half of the behaviour. The refusal case it was really
/// protecting is the test below.
#[tokio::test]
async fn an_edit_against_a_stale_tag_is_relocated_onto_its_real_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");
    let tag = read_tag(temp.path(), "hl-stale", "f.txt").await;

    // The file changes underneath, as a concurrent process would change it.
    // Line 1 is untouched, so the edit still has a provable target.
    std::fs::write(&path, "one\ntwo\nthree\n").expect("rewrite");

    let output = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-stale"),
        )
        .await
        .expect("line 1 is unchanged, so the edit can be placed");

    // The model is told the tag was stale, so it does not carry on believing
    // its view of the file is current.
    assert!(
        output.output.contains("Recovered from a stale file hash"),
        "a recovered edit must say so: {}",
        output.output
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "ONE\ntwo\nthree\n",
        "the edit lands on its target and the concurrent change survives"
    );
}

/// Boundary repair, reached through the tool rather than the library. The
/// payload restates the function signature and closing brace bordering the
/// range, which applied literally would duplicate both.
#[tokio::test]
async fn a_payload_that_restates_its_neighbours_is_repaired_before_it_lands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.js");
    std::fs::write(&path, "function f() {\n  old();\n}\n").expect("write");
    let tag = read_tag(temp.path(), "hl-repair", "f.js").await;

    let output = EditTool::new()
        .execute(
            json!({
                "file_path": "f.js",
                "input": format!(
                    "[f.js#{tag}]\nPUT 2.=2:\n+function f() {{\n+  fresh();\n+}}"
                ),
            }),
            ctx(temp.path().to_path_buf(), "hl-repair"),
        )
        .await
        .expect("the echo is repaired rather than refused");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "function f() {\n  fresh();\n}\n",
        "the signature and closing brace must not double"
    );
    // The model is told what was changed for it, so it can author the next
    // patch without the same mistake.
    assert!(
        output.output.contains("boundary echo"),
        "a repaired edit must say so: {}",
        output.output
    );
}

/// The refusal that hashline exists for: the model's target is the very thing
/// that changed, so there is nowhere safe to put the edit.
#[tokio::test]
async fn an_edit_whose_target_changed_is_refused_and_writes_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");
    let tag = read_tag(temp.path(), "hl-stale-target", "f.txt").await;

    // A concurrent process rewrites the exact line the model is replacing.
    std::fs::write(&path, "REPLACED\ntwo\n").expect("rewrite");

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-stale-target"),
        )
        .await
        .expect_err("the target no longer exists, so this must be refused");

    assert!(
        error.to_string().to_lowercase().contains("read"),
        "the error should tell the model to re-read: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "REPLACED\ntwo\n",
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

/// Every operation form the system prompt teaches must actually work.
///
/// The prompt was written by reading the parser, which is exactly the way to
/// end up documenting a syntax that does not execute. These run each documented
/// form through the real tool. If someone changes the parser, this fails rather
/// than leaving the prompt quietly lying to the model.
mod documented_syntax {
    use super::*;

    async fn apply(initial: &str, ops: &str, session: &str) -> Result<String, String> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("f.txt");
        std::fs::write(&path, initial).expect("write");
        let tag = read_tag(temp.path(), session, "f.txt").await;

        EditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "input": format!("[f.txt#{tag}]\n{ops}"),
                }),
                ctx(temp.path().to_path_buf(), session),
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(std::fs::read_to_string(&path).unwrap_or_default())
    }

    /// `PUT start=end:` replaces an inclusive range.
    #[tokio::test]
    async fn put_replaces_an_inclusive_range() {
        let out = apply("a\nb\nc\nd\n", "PUT 2=3:\n+X", "doc-put-range")
            .await
            .expect("PUT range");
        assert_eq!(out, "a\nX\nd\n");
    }

    /// `CUT start=end` deletes, with no body.
    #[tokio::test]
    async fn cut_deletes_a_range() {
        let out = apply("a\nb\nc\nd\n", "CUT 2=3", "doc-cut")
            .await
            .expect("CUT");
        assert_eq!(out, "a\nd\n");
    }

    /// `PUT <N:` inserts before line N.
    #[tokio::test]
    async fn put_before_inserts_above_the_line() {
        let out = apply("a\nb\n", "PUT <2:\n+X", "doc-before")
            .await
            .expect("PUT before");
        assert_eq!(out, "a\nX\nb\n");
    }

    /// `PUT >N:` inserts after line N.
    #[tokio::test]
    async fn put_after_inserts_below_the_line() {
        let out = apply("a\nb\n", "PUT >1:\n+X", "doc-after")
            .await
            .expect("PUT after");
        assert_eq!(out, "a\nX\nb\n");
    }

    /// `PUT >$:` appends at end of file.
    #[tokio::test]
    async fn put_at_eof_appends() {
        let out = apply("a\nb\n", "PUT >$:\n+X", "doc-eof")
            .await
            .expect("PUT eof");
        assert!(out.starts_with("a\nb\n"), "expected an append, got {out:?}");
        assert!(out.contains('X'), "the appended line is missing: {out:?}");
    }

    /// `PUT <1:` is the beginning of the file.
    #[tokio::test]
    async fn put_before_the_first_line_prepends() {
        let out = apply("a\nb\n", "PUT <1:\n+X", "doc-bof")
            .await
            .expect("PUT bof");
        assert_eq!(out, "X\na\nb\n");
    }

    /// Several operations in one section, which the prompt's example shows.
    /// This is also the claim most likely to be wrong: line numbers must all
    /// refer to the original file, not to positions shifted by earlier ops.
    #[tokio::test]
    async fn line_numbers_refer_to_the_original_file_throughout() {
        let out = apply(
            "a\nb\nc\nd\ne\n",
            // Replacing 1=1 with two lines shifts everything down. If the CUT
            // were interpreted against the shifted text it would delete the
            // wrong line.
            "PUT 1=1:\n+X\n+Y\nCUT 4=4",
            "doc-multi",
        )
        .await
        .expect("multiple ops");
        assert_eq!(
            out, "X\nY\nb\nc\ne\n",
            "the second op should have been resolved against the original numbering"
        );
    }

    /// The prompt's own example must parse. Its content is arbitrary, so this
    /// checks the shape rather than a result.
    #[tokio::test]
    async fn the_example_from_the_system_prompt_parses() {
        let initial: String = (1..=40)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let out = apply(
            &initial,
            "PUT 12=14:\n+    let total = items.len();\n+    println!(\"{total}\");\nCUT 20=22\nPUT >30:\n+// appended after line 30",
            "doc-example",
        )
        .await
        .expect("the documented example should apply");

        assert!(out.contains("let total = items.len();"), "PUT did not land");
        assert!(!out.contains("line 20\n"), "CUT did not remove line 20");
        assert!(
            out.contains("// appended after line 30"),
            "the insert after line 30 did not land"
        );
    }

    /// `REM` deletes the file. Documented in the prompt, so it must work; also
    /// the one op whose failure mode is destructive rather than a no-op.
    #[tokio::test]
    async fn rem_deletes_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("f.txt");
        std::fs::write(&path, "a\n").expect("write");
        let tag = read_tag(temp.path(), "doc-rem", "f.txt").await;

        EditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "input": format!("[f.txt#{tag}]\nREM"),
                }),
                ctx(temp.path().to_path_buf(), "doc-rem"),
            )
            .await
            .expect("REM");

        assert!(!path.exists(), "REM should have deleted the file");
    }

    /// `MV dest` moves the file: new path has the content, old path is gone.
    #[tokio::test]
    async fn mv_moves_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let from = temp.path().join("f.txt");
        let to = temp.path().join("g.txt");
        std::fs::write(&from, "a\n").expect("write");
        let tag = read_tag(temp.path(), "doc-mv", "f.txt").await;

        EditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "input": format!("[f.txt#{tag}]\nMV g.txt"),
                }),
                ctx(temp.path().to_path_buf(), "doc-mv"),
            )
            .await
            .expect("MV");

        assert!(!from.exists(), "the original path should be gone after MV");
        assert_eq!(
            std::fs::read_to_string(&to).expect("the destination should exist"),
            "a\n"
        );
    }
}

/// `write` records what it wrote, so an edit later in the same turn can anchor
/// to it without a re-read.
///
/// Safety does not depend on this: an unrecorded write would leave the model
/// holding a tag that no longer matches the file, and the edit would be refused,
/// which is correct. This is about not forcing a redundant read.
#[tokio::test]
async fn an_edit_can_follow_a_write_without_re_reading() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");

    let written = crate::tool::write::WriteTool::new()
        .execute(
            json!({ "file_path": "f.txt", "content": "one\ntwo\n" }),
            ctx(temp.path().to_path_buf(), "hl-write"),
        )
        .await
        .expect("write");

    // The tag write recorded is the hash of what it wrote, which the store can
    // now hand back for the path.
    let tag = crate::tool::hashline_store::for_session("hl-write")
        .head("f.txt")
        .expect("write should have recorded a snapshot")
        .hash;
    assert!(!written.output.is_empty(), "write should report something");

    EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[f.txt#{tag}]\nPUT 1.=1:\n+ONE"),
            }),
            ctx(temp.path().to_path_buf(), "hl-write"),
        )
        .await
        .expect("an edit should anchor to the tag write recorded");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "ONE\ntwo\n"
    );
}

/// The collapsed tool call in the TUI shows the title. A multi-file patch that
/// names only its first file hides the rest, which is precisely what a reviewer
/// needs to see. Found by running a real agent, not by a test.
#[tokio::test]
async fn the_title_names_every_file_a_patch_touched() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.txt"), "alpha\n").expect("a");
    std::fs::write(temp.path().join("b.txt"), "beta\n").expect("b");
    let tag_a = read_tag(temp.path(), "hl-title", "a.txt").await;
    let tag_b = read_tag(temp.path(), "hl-title", "b.txt").await;

    let output = EditTool::new()
        .execute(
            json!({
                "file_path": "a.txt",
                "input": format!(
                    "[a.txt#{tag_a}]\nPUT 1.=1:\n+ALPHA\n[b.txt#{tag_b}]\nPUT 1.=1:\n+BETA"
                ),
            }),
            ctx(temp.path().to_path_buf(), "hl-title"),
        )
        .await
        .expect("multi-file edit");

    let title = output.title.unwrap_or_default();
    assert!(
        title.contains("a.txt") && title.contains("b.txt"),
        "the title should name both files, got {title:?}"
    );
}

/// A tag read minted must be recognised as *stale* when the file changes, not
/// reported as belonging to another session.
///
/// Found by running a real agent: bash overwrote a file and the refusal said
/// "tag #2543 is not from this session", which is false and misleading. The
/// model is told to check for a session mixup when the real cause is that the
/// file changed underneath it.
///
/// The cause was a key mismatch. `read` records under the path as the model
/// wrote it, but a header carrying an absolute path is normalized to a
/// cwd-relative one before lookup, so the two never met.
#[tokio::test]
async fn a_changed_file_is_reported_as_stale_not_as_a_foreign_tag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("f.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write");

    // Read by absolute path, as an agent commonly does.
    let absolute = path.to_string_lossy().to_string();
    let output = crate::tool::read::ReadTool::new()
        .execute(
            json!({ "file_path": absolute }),
            ctx(temp.path().to_path_buf(), "hl-attrib-abs"),
        )
        .await
        .expect("read");
    let header = output.output.lines().next().unwrap_or_default();
    let tag = header
        .rsplit_once('#')
        .map(|(_, tag)| tag.trim_end_matches(']').to_string())
        .expect("a header with a tag");

    // Something else changes the file, as bash would.
    std::fs::write(&path, "CHANGED\n").expect("rewrite");

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "f.txt",
                "input": format!("[{absolute}#{tag}]\nPUT 1.=1:\n+X"),
            }),
            ctx(temp.path().to_path_buf(), "hl-attrib-abs"),
        )
        .await
        .expect_err("a changed file must be refused")
        .to_string();

    assert!(
        !error.contains("not from this session"),
        "a tag this session minted was reported as foreign: {error}"
    );
    assert!(
        error.contains("changed") || error.contains("stale"),
        "the refusal should say the file changed: {error}"
    );
}

/// The closer spare, reached through the real tool with the real tree-sitter
/// probe behind it. The model adds a method and forgets the `};` that ends the
/// object, so the authored edit would leave it unterminated.
#[tokio::test]
async fn a_dropped_closing_line_is_kept_when_the_file_would_not_parse_without_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("h.js");
    std::fs::write(
        &path,
        "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n};\n",
    )
    .expect("write");
    let tag = read_tag(temp.path(), "hl-spare", "h.js").await;

    let output = EditTool::new()
        .execute(
            json!({
                "file_path": "h.js",
                "input": format!(
                    "[h.js#{tag}]\nPUT 5.=5:\n+\tb() {{\n+\t\treturn 2;\n+\t}},"
                ),
            }),
            ctx(temp.path().to_path_buf(), "hl-spare"),
        )
        .await
        .expect("the dropped closer is restored rather than the edit refused");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "const handlers = {\n\ta() {\n\t\treturn 1;\n\t},\n\tb() {\n\t\treturn 2;\n\t},\n};\n",
        "the object literal must still be terminated"
    );
    assert!(
        output.output.contains("dropped closing line"),
        "a spared closer must be reported: {}",
        output.output
    );
}

/// The same shape in a file the parser cannot judge. Braces in prose are not
/// syntax, so nothing may be restored on their behalf, and the edit applies
/// exactly as written.
#[tokio::test]
async fn a_brace_in_prose_is_never_restored_by_a_repair() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("notes.md");
    std::fs::write(&path, "Some prose.\n\nA closing thought }\n").expect("write");
    let tag = read_tag(temp.path(), "hl-prose", "notes.md").await;

    EditTool::new()
        .execute(
            json!({
                "file_path": "notes.md",
                "input": format!("[notes.md#{tag}]\nPUT 3.=3:\n+A different thought"),
            }),
            ctx(temp.path().to_path_buf(), "hl-prose"),
        )
        .await
        .expect("prose edits apply");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "Some prose.\n\nA different thought\n",
        "no brace may be resurrected in a file the parser cannot vouch for"
    );
}

/// A line number past the end of the file must be an error the model can act
/// on, not a crash. The repair layer runs before anything validates a model's
/// arithmetic, and it used to index the file around the range directly, so
/// this panicked inside the tool.
#[tokio::test]
async fn an_anchor_past_the_end_of_the_file_is_refused_not_a_crash() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("s.js");
    std::fs::write(&path, "const a = 1;\nconst b = 2;\n").expect("write");
    let tag = read_tag(temp.path(), "hl-oob", "s.js").await;

    let error = EditTool::new()
        .execute(
            json!({
                "file_path": "s.js",
                "input": format!("[s.js#{tag}]\nPUT 99.=99:\n+const c = 3;"),
            }),
            ctx(temp.path().to_path_buf(), "hl-oob"),
        )
        .await
        .expect_err("line 99 does not exist");

    // The message has to name the problem, since the model's next move is to
    // fix the number rather than re-read.
    let text = error.to_string();
    assert!(
        text.contains("99") && text.to_lowercase().contains("line"),
        "the error should name the bad line: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "const a = 1;\nconst b = 2;\n",
        "a refused edit must not write"
    );
}
