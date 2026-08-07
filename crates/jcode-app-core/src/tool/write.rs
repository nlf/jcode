use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct WriteInput {
    #[serde(default)]
    intent: Option<String>,
    file_path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a whole file, with content sent verbatim. Not `>` or a heredoc in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "content"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "content": {
                    "type": "string",
                    "description": "File content."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: WriteInput = serde_json::from_value(input)?;

        let path = ctx.resolve_path(Path::new(&params.file_path));

        // Refuse a write that is really a misdispatched read, before creating
        // any directories: `a.txt:1-2;b/c.txt:3-4` would otherwise be a nested
        // tree in the workspace, and `notes.md:50-100` a file nothing will ever
        // look at again. The check is skipped when the literal path exists, so
        // a real file with a colon in its name stays writable.
        if let Some(misfire) =
            jcode_read::check_write_target(&params.file_path, &params.content, path.exists())
        {
            return Err(anyhow::anyhow!(misfire.message()));
        }

        // Create parent directories if needed
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Check if file existed before and read old content for diff
        let existed = path.exists();
        let old_content = if existed {
            tokio::fs::read_to_string(&path).await.ok()
        } else {
            None
        };

        // Write the file
        tokio::fs::write(&path, &params.content).await?;

        // Record the new content so a hashline edit later in this turn can
        // anchor to it without a re-read.
        //
        // This is ergonomics, not safety. The tag is a hash of the file, so an
        // unrecorded write is already caught: the model's stale tag would not
        // match the new content and the edit would be refused, which is the
        // right answer. What recording buys is that the refusal is unnecessary
        // in the first place, and that the store's error can say "unknown tag"
        // rather than misattributing a write this session made to a concurrent
        // modification.
        //
        // No seen lines. The model authored this content, so it has seen all of
        // it, but `seen_lines` records what was *displayed* with line numbers,
        // and claiming that here would be a different assertion than `read`'s.
        // Keyed by the normalized path, matching what `read` records and what
        // a patch header resolves to. See the note in read.rs: keying by the
        // raw argument puts an absolute-path write and its own header under
        // different keys.
        let cwd = ctx.working_dir.as_deref().and_then(|dir| dir.to_str());
        let key = jcode_hashline::normalize_path(&params.file_path, cwd);
        super::hashline_store::for_session(&ctx.session_id).record(&key, &params.content, None);

        let _new_len = params.content.len();
        let line_count = params.content.lines().count();
        let diff = if let Some(old) = old_content.as_deref() {
            generate_diff_summary(old, &params.content)
        } else {
            generate_diff_summary("", &params.content)
        };
        let detail = build_file_touch_preview(&diff);

        // Publish file touch event for swarm coordination
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: path.to_path_buf(),
            op: FileOp::Write,
            intent: params
                .intent
                .clone()
                .filter(|value| !value.trim().is_empty()),
            summary: Some(if existed {
                format!("overwrote file ({} lines)", line_count)
            } else {
                format!("created new file ({} lines)", line_count)
            }),
            detail,
        }));

        let mut body = if existed {
            format!(
                "Updated {} ({} lines){}\n{}",
                params.file_path,
                line_count,
                if diff.is_empty() { "" } else { ":" },
                diff
            )
        } else {
            // For new files, show all lines as additions
            let diff = generate_diff_summary("", &params.content);
            format!(
                "Created {} ({} lines):\n{}",
                params.file_path, line_count, diff
            )
        };

        // A write that lands on the active config.toml states exactly which
        // settings changed and whether they are live, so neither the agent nor
        // the user has to guess whether the edit took effect.
        super::config_edit_notice::append_config_edit_notice(
            &mut body,
            &path,
            old_content.as_deref().unwrap_or(""),
            &params.content,
        );

        Ok(ToolOutput::new(body).with_title(params.file_path.clone()))
    }
}

/// Generate a compact diff: "42- old" / "42+ new" (max 20 lines).
///
/// Delegates to the shared renderer. `write` was the fifth copy of this
/// function and the consolidation missed it, leaving the per-line `.trim()`
/// that flattens indentation - the same defect fixed once in `ui_diff.rs` and
/// then again in the other four copies.
fn generate_diff_summary(old: &str, new: &str) -> String {
    const MAX_LINES: usize = 20;
    super::tool_diff::render_diff(old, new, 1, MAX_LINES)
}

fn build_file_touch_preview(diff: &str) -> Option<String> {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let mut preview = lines
        .by_ref()
        .take(FILE_TOUCH_PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut truncated = lines.next().is_some();

    if preview.len() > FILE_TOUCH_PREVIEW_MAX_BYTES {
        preview = crate::util::truncate_str(&preview, FILE_TOUCH_PREVIEW_MAX_BYTES)
            .trim_end()
            .to_string();
        truncated = true;
    }

    if truncated {
        preview.push_str("\n…");
    }

    Some(preview)
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
        // Equal lines should not appear
        assert!(
            !diff.contains("line one"),
            "Should not show unchanged lines"
        );
    }

    #[test]
    fn test_generate_diff_summary_new_file() {
        let old = "";
        let new = "line one\nline two\nline three";
        let diff = generate_diff_summary(old, new);

        assert!(diff.contains("1+ line one"), "Should show line 1 added");
        assert!(diff.contains("2+ line two"), "Should show line 2 added");
        assert!(diff.contains("3+ line three"), "Should show line 3 added");
    }

    #[test]
    fn test_generate_diff_summary_truncation() {
        // Create old and new with more than 20 changed lines
        let old = (1..=25)
            .map(|i| format!("old line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (1..=25)
            .map(|i| format!("new line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = generate_diff_summary(&old, &new);

        assert!(diff.contains("..."), "Should truncate after 20 lines");
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

    /// `write` was the fifth `generate_diff` copy, and the consolidation into
    /// `tool_diff` reached the other four. Its per-line `.trim()` flattened
    /// indentation exactly like the bug fixed once in `ui_diff.rs`, so a diff
    /// of nested code arrived at the model with its structure gone.
    ///
    /// The shared renderer removes only the indentation *common* to every
    /// shown line, so a uniformly indented hunk still renders flush - that is
    /// the width-reclaiming behaviour the dedent exists for. What must survive
    /// is the *relative* nesting between lines, which the old per-line trim
    /// destroyed.
    #[test]
    fn diff_summary_preserves_relative_indentation() {
        // The changed lines sit at two different depths, which is what makes
        // the relative nesting observable at all.
        let old = "fn f() {\n    if a {\n        old_inner();\n    }\n}\n";
        let new = "fn f() {\n    if a {\n        new_inner();\n    }\n    tail();\n}\n";

        let diff = generate_diff_summary(old, new);

        assert!(
            diff.contains("+     new_inner();") && diff.contains("+ tail();"),
            "the depth difference between the two added lines must survive: {diff:?}"
        );
    }

    #[test]
    fn test_generate_diff_summary_empty_result() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff_summary(old, new);

        assert!(diff.is_empty(), "No changes should produce empty diff");
    }
}

#[cfg(test)]
mod misfire_tests {
    use super::*;
    use crate::tool::{ToolContext, ToolExecutionMode};

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            session_id: "write-misfire".to_string(),
            message_id: "m".to_string(),
            tool_call_id: "t".to_string(),
            working_dir: Some(dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    /// A model meaning to READ `notes.md:50-100` and dispatching to write does
    /// not get an error without this: it gets a file by that literal name,
    /// which nothing will ever look at again.
    #[tokio::test]
    async fn a_misdispatched_read_is_refused_rather_than_creating_a_file() {
        let temp = tempfile::tempdir().expect("tempdir");

        let error = WriteTool::new()
            .execute(
                json!({ "file_path": "notes.md:50-100", "content": "" }),
                ctx(temp.path()),
            )
            .await
            .expect_err("a selector-shaped target with no content must be refused");

        assert!(error.to_string().contains("use read"), "{error}");
        assert!(
            !temp.path().join("notes.md:50-100").exists(),
            "the literal file must not have been created"
        );
    }

    /// omp's #6809. The directory tree is the damage: `b/` would be created as
    /// a real directory in the workspace.
    #[tokio::test]
    async fn a_semicolon_joined_read_list_creates_no_directories() {
        let temp = tempfile::tempdir().expect("tempdir");

        let error = WriteTool::new()
            .execute(
                json!({ "file_path": "a.txt:1-2;b/c.txt:3-4", "content": "x" }),
                ctx(temp.path()),
            )
            .await
            .expect_err("a selector list must be refused even with content");

        assert!(error.to_string().contains("one read per path"), "{error}");
        assert!(
            !temp.path().join("a.txt:1-2;b").exists(),
            "no directory tree should have been created"
        );
    }

    /// Non-empty content means the model meant to write a file, whatever it
    /// called it.
    #[tokio::test]
    async fn content_makes_a_selector_shaped_name_writable() {
        let temp = tempfile::tempdir().expect("tempdir");

        WriteTool::new()
            .execute(
                json!({ "file_path": "odd:1-2", "content": "real content" }),
                ctx(temp.path()),
            )
            .await
            .expect("a non-empty write is never blocked");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("odd:1-2")).expect("written"),
            "real content"
        );
    }

    /// A real file with a colon in its name stays writable, including emptying
    /// it.
    #[tokio::test]
    async fn an_existing_file_can_still_be_truncated() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("odd:1-2"), "before").expect("seed");

        WriteTool::new()
            .execute(
                json!({ "file_path": "odd:1-2", "content": "" }),
                ctx(temp.path()),
            )
            .await
            .expect("an existing file is never blocked");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("odd:1-2")).expect("still there"),
            ""
        );
    }

    /// Ordinary writes are untouched.
    #[tokio::test]
    async fn an_ordinary_write_still_works() {
        let temp = tempfile::tempdir().expect("tempdir");

        WriteTool::new()
            .execute(
                json!({ "file_path": "deep/nested/new.txt", "content": "hello" }),
                ctx(temp.path()),
            )
            .await
            .expect("an ordinary write should succeed");

        assert_eq!(
            std::fs::read_to_string(temp.path().join("deep/nested/new.txt")).expect("written"),
            "hello"
        );
    }
}
