use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

pub struct MultiEditTool;

impl MultiEditTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct MultiEditInput {
    file_path: String,
    edits: Vec<EditOperation>,
}

#[derive(Deserialize)]
struct EditOperation {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multiedit"
    }

    fn description(&self) -> &str {
        "Apply multiple edits to one file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "edits"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to edit"
                },
                "edits": {
                    "type": "array",
                    "description": "Array of edit operations to apply sequentially",
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "The text to find and replace"
                            },
                            "new_string": {
                                "type": "string",
                                "description": "The replacement text"
                            },
                            "replace_all": {
                                "type": "boolean",
                                "description": "Replace all occurrences (default: false)"
                            }
                        }
                    },
                    "minItems": 1
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: MultiEditInput = serde_json::from_value(input)?;

        let path = ctx.resolve_path(Path::new(&params.file_path));

        if !path.exists() {
            return Err(anyhow::anyhow!(super::read::file_not_found_message(
                &params.file_path,
                &path,
                ctx.working_dir.as_deref(),
            )));
        }

        let original_content = tokio::fs::read_to_string(&path).await?;
        let mut content = original_content.clone();
        let mut applied = Vec::new();
        let mut failed = Vec::new();

        for (i, edit) in params.edits.iter().enumerate() {
            if edit.old_string == edit.new_string {
                failed.push(format!("Edit {}: old_string equals new_string", i + 1));
                continue;
            }

            let occurrences = content.matches(&edit.old_string).count();

            if occurrences == 0 {
                failed.push(format!("Edit {}: old_string not found", i + 1));
                continue;
            }

            if occurrences > 1 && !edit.replace_all {
                failed.push(format!(
                    "Edit {}: found {} occurrences, use replace_all or be more specific",
                    i + 1,
                    occurrences
                ));
                continue;
            }

            // Apply the edit
            if edit.replace_all {
                content = content.replace(&edit.old_string, &edit.new_string);
                applied.push(format!(
                    "Edit {}: replaced {} occurrences",
                    i + 1,
                    occurrences
                ));
            } else {
                content = content.replacen(&edit.old_string, &edit.new_string, 1);
                applied.push(format!("Edit {}: replaced 1 occurrence", i + 1));
            }
        }

        // Nothing is written unless every edit succeeded.
        //
        // This used to write whatever survived and return Ok, with the failures
        // listed under a "Failed:" heading below a line reading "Edited
        // <path>". Two ways that goes wrong: a call where *every* edit failed
        // still rewrote the file and reported success, and a partial success
        // left the file in a state matching neither the old content nor what
        // was asked for, while the model read the first line and moved on.
        //
        // Edits in one call are usually one intended change, so applying half
        // is not a partial success but a corruption. The same reasoning as
        // hashline's preflight, which validates every section before writing
        // any file.
        if !failed.is_empty() {
            return Err(anyhow::anyhow!(
                "No edits applied to {}: {} of {} failed.\n{}\n\n\
                 Nothing was written. Re-read the file and retry with text that \
                 matches, or use `edit` with a hashline patch to anchor by line \
                 number instead of by matching text.",
                params.file_path,
                failed.len(),
                params.edits.len(),
                failed
                    .iter()
                    .map(|message| format!("  ✗ {message}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        // Write the result
        tokio::fs::write(&path, &content).await?;

        // Record the new content, so a later hashline edit in this session can
        // anchor to it. Keyed the same way `read` and `write` key it.
        let cwd = ctx.working_dir.as_deref().and_then(|dir| dir.to_str());
        let key = jcode_hashline::normalize_path(&params.file_path, cwd);
        super::hashline_store::for_session(&ctx.session_id).record(&key, &content, None);

        // Format output
        let mut output = format!("Edited {}\n\n", params.file_path);

        if !applied.is_empty() {
            output.push_str("Applied:\n");
            for msg in &applied {
                output.push_str(&format!("  ✓ {}\n", msg));
            }
        }

        output.push_str(&format!("\nTotal: {} applied\n", applied.len()));

        // Generate diff summary
        if !applied.is_empty() {
            output.push_str("\nDiff:\n");
            output.push_str(&generate_diff_summary(&original_content, &content));
        }

        super::config_edit_notice::append_config_edit_notice(
            &mut output,
            &path,
            &original_content,
            &content,
        );

        Ok(ToolOutput::new(output).with_title(params.file_path.clone()))
    }
}

/// Compact line-numbered diff, rendered by the shared tool renderer.
///
/// This used to trim every line, which removed the file's indentation along
/// with it; see `tool_diff` for why common-prefix dedent replaced that.
fn generate_diff_summary(old: &str, new: &str) -> String {
    super::tool_diff::render_diff(old, new, 1, super::tool_diff::DEFAULT_MAX_DIFF_LINES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_summary_single_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff_summary(old, new);

        // Compact format: "1- content" / "1+ content"
        assert!(diff.contains("1- hello world"), "Should show deleted line");
        assert!(diff.contains("1+ hello rust"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_summary_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nchanged two\nline three";
        let diff = generate_diff_summary(old, new);

        assert!(diff.contains("2- line two"), "Should show deleted line");
        assert!(diff.contains("2+ changed two"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_summary_multiple_edits() {
        let old = "line 1\nline 2\nline 3\nline 4\nline 5";
        let new = "line 1\nmodified 2\nline 3\nmodified 4\nline 5";
        let diff = generate_diff_summary(old, new);

        // Should show both changed lines with correct line numbers
        assert!(diff.contains("2- line 2"), "Should show line 2 deleted");
        assert!(diff.contains("2+ modified 2"), "Should show line 2 added");
        assert!(diff.contains("4- line 4"), "Should show line 4 deleted");
        assert!(diff.contains("4+ modified 4"), "Should show line 4 added");
    }

    #[test]
    fn test_generate_diff_summary_truncation() {
        // Create old and new with more than 30 changed lines
        let old = (1..=35)
            .map(|i| format!("old line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (1..=35)
            .map(|i| format!("new line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = generate_diff_summary(&old, &new);

        assert!(diff.contains("..."), "Should truncate after 30 lines");
    }

    #[test]
    fn test_generate_diff_summary_line_number_format() {
        let old = "old";
        let new = "new";
        let diff = generate_diff_summary(old, new);

        // Compact format: no padding
        assert!(
            diff.contains("1- old"),
            "Should have line number directly before minus"
        );
        assert!(
            diff.contains("1+ new"),
            "Should have line number directly before plus"
        );
    }
}


#[cfg(test)]
mod execute_tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn ctx(dir: std::path::PathBuf, session: &str) -> ToolContext {
        ToolContext {
            session_id: session.to_string(),
            message_id: "m".to_string(),
            tool_call_id: "t".to_string(),
            working_dir: Some(dir),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    /// The worst of the old behaviour: every edit failed, yet the file was
    /// rewritten and the output opened with "Edited <path>". A model reading
    /// the first line would believe its change had landed.
    #[tokio::test]
    async fn a_call_where_every_edit_fails_is_an_error_and_writes_nothing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("f.txt");
        std::fs::write(&path, "hello\n").expect("write");

        let error = MultiEditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "edits": [{"old_string": "NOPE", "new_string": "X"}],
                }),
                ctx(temp.path().to_path_buf(), "me-allfail"),
            )
            .await
            .expect_err("a call where nothing matched must not report success");

        assert!(
            error.to_string().contains("Nothing was written"),
            "the error should say the file is untouched: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "hello\n"
        );
    }

    /// A partial failure must not write either. Edits in one call are usually
    /// one intended change, so applying half leaves the file matching neither
    /// the old content nor the intent.
    #[tokio::test]
    async fn a_partial_failure_writes_nothing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("f.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");

        let error = MultiEditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "MISSING", "new_string": "X"},
                    ],
                }),
                ctx(temp.path().to_path_buf(), "me-partial"),
            )
            .await
            .expect_err("a partial failure must be refused");

        assert!(
            error.to_string().contains("1 of 2 failed"),
            "the error should say how many failed: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "alpha\nbeta\n",
            "the successful edit was written despite a sibling failing"
        );
    }

    /// The ordinary path must still work.
    #[tokio::test]
    async fn all_edits_succeeding_applies_them_in_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("f.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write");

        MultiEditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "beta", "new_string": "BETA"},
                    ],
                }),
                ctx(temp.path().to_path_buf(), "me-ok"),
            )
            .await
            .expect("all edits should apply");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "ALPHA\nBETA\n"
        );
    }

    /// The failure names hashline, since anchoring by line number is the
    /// remedy when matching text keeps failing.
    #[tokio::test]
    async fn the_failure_points_at_hashline_as_the_alternative() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("f.txt"), "hello\n").expect("write");

        let error = MultiEditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "edits": [{"old_string": "NOPE", "new_string": "X"}],
                }),
                ctx(temp.path().to_path_buf(), "me-hint"),
            )
            .await
            .expect_err("must fail");

        assert!(
            error.to_string().contains("hashline"),
            "the error should offer the line-anchored alternative: {error}"
        );
    }

    /// After a successful multiedit, a hashline edit can anchor to the result
    /// without a re-read, as it can after `write`.
    #[tokio::test]
    async fn a_hashline_edit_can_follow_a_multiedit() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("f.txt"), "alpha\nbeta\n").expect("write");

        MultiEditTool::new()
            .execute(
                json!({
                    "file_path": "f.txt",
                    "edits": [{"old_string": "alpha", "new_string": "ALPHA"}],
                }),
                ctx(temp.path().to_path_buf(), "me-chain"),
            )
            .await
            .expect("multiedit");

        let snapshot = crate::tool::hashline_store::for_session("me-chain")
            .head("f.txt")
            .expect("multiedit should record what it wrote");
        assert_eq!(snapshot.text, "ALPHA\nbeta\n");
    }
}
