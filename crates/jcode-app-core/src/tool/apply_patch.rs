//! The `apply_patch` tool.
//!
//! Codex-style `*** Begin Patch` envelopes. The parsing and application live in
//! `jcode-patch`, ported from omp; this file is the I/O and jcode integration:
//! path resolution, the delete guard, file-touch events, and the config notice.

use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use jcode_patch::{plan, summary, FileOutcome, HunkError, PatchPlan};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct ApplyPatchTool;

impl ApplyPatchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ApplyPatchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct ApplyPatchInput {
    #[serde(default)]
    intent: Option<String>,
    patch_text: String,
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        // Says what it is FOR, not just what it accepts. Asked to choose
        // between this and `edit` from the descriptions alone, a real agent
        // called the distinction ambiguous and inferred - correctly - that this
        // is a compatibility shim. Better to state that than make it guess.
        "Apply a Codex *** Begin Patch envelope given to you. Writing your own: use edit."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["patch_text"],
            "properties": {
                "intent": super::intent_schema_property(),
                "patch_text": {
                    "type": "string",
                    "description": "Patch envelope: *** Begin Patch, then Add/Delete/Update File sections, then *** End Patch."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ApplyPatchInput = serde_json::from_value(input)?;
        let hunks =
            jcode_patch::parse(&params.patch_text).map_err(|error| anyhow::anyhow!(error.message()))?;

        if hunks.is_empty() {
            return Err(anyhow::anyhow!(
                "This patch contains no file sections. Add at least one \
                 '*** Add File:', '*** Delete File:' or '*** Update File:' section."
            ));
        }

        // Read every file the patch names, once, before planning. Planning is
        // pure, so it cannot read for itself, and doing it here means a file
        // read twice by two hunks sees the same content both times.
        let mut contents: HashMap<String, String> = HashMap::new();
        for hunk in &hunks {
            if contents.contains_key(&hunk.path) {
                continue;
            }
            let resolved = ctx.resolve_path(Path::new(&hunk.path));
            if let Ok(text) = tokio::fs::read_to_string(&resolved).await {
                contents.insert(hunk.path.clone(), text);
            }
        }

        let result = plan(&hunks, &|path: &str| contents.get(path).cloned());

        // A patch can reach config.toml through any hunk, so watch the file
        // across the whole commit rather than threading content through each
        // branch.
        let config_watch = super::config_edit_notice::ConfigEditWatch::begin();
        let committed = commit(&result, &ctx, params.intent.as_deref()).await?;

        if let Some(message) = failure_message(&result, &committed) {
            return Err(anyhow::anyhow!(message));
        }

        let mut body = summary(&committed);
        for outcome in &committed {
            if let Some(diff) = render_diff(outcome, &contents) {
                // A moved file's diff is headed by where it ended up, not where
                // it came from: a real agent reported the pre-move path as
                // "slightly misleading" because the file is no longer there.
                let heading = match outcome {
                    FileOutcome::Updated {
                        path,
                        moved_to: Some(destination),
                        ..
                    } => format!("{path} -> {destination}"),
                    other => other.path().to_string(),
                };
                body.push_str(&format!("\n\n{heading}\n{diff}"));
            }
        }
        config_watch.finish(&mut body);

        let title = match committed.len() {
            1 => committed[0].path().to_string(),
            n => format!("{n} files"),
        };
        Ok(ToolOutput::new(body).with_title(title))
    }
}

/// Write the planned outcomes, stopping at the first that fails.
///
/// Returns what actually landed, which is not the same as what was planned: a
/// write can fail for reasons planning cannot see, and the caller has to be
/// told the truth about the disk rather than about the plan.
async fn commit(
    result: &PatchPlan,
    ctx: &ToolContext,
    intent: Option<&str>,
) -> Result<Vec<FileOutcome>> {
    let mut committed = Vec::new();

    for outcome in &result.outcomes {
        let resolved = ctx.resolve_path(Path::new(outcome.path()));

        match outcome {
            FileOutcome::Created { content, .. } => {
                if let Some(parent) = resolved.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&resolved, content).await?;
                publish(ctx, &resolved, outcome.path(), "created", intent, FileOp::Write);
            }
            FileOutcome::Deleted { path } => {
                // `resolve_path` passes absolute paths through unchanged, so a
                // patch can name any file on disk. The bash gate does not cover
                // this path, so the same absolute deny applies here (#604).
                // Only the catastrophic tier: ordinary file deletes are this
                // tool's normal job.
                let risk = jcode_command_risk::RiskContext::from_env(ctx.working_dir.clone());
                if jcode_command_risk::is_catastrophic_target(&resolved, &risk) {
                    return Err(anyhow::anyhow!(
                        "{path}: refused, this path is protected and must never be \
                         deleted by an agent"
                    ));
                }
                tokio::fs::remove_file(&resolved).await?;
                publish(ctx, &resolved, path, "deleted", intent, FileOp::Edit);
            }
            FileOutcome::Updated {
                path,
                moved_to,
                content,
            } => match moved_to {
                Some(destination) => {
                    let target = ctx.resolve_path(Path::new(destination));
                    if let Some(parent) = target.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::write(&target, content).await?;
                    tokio::fs::remove_file(&resolved).await?;
                    publish(ctx, &target, path, "moved", intent, FileOp::Edit);
                }
                None => {
                    tokio::fs::write(&resolved, content).await?;
                    publish(ctx, &resolved, path, "modified", intent, FileOp::Edit);
                }
            },
        }

        committed.push(outcome.clone());
    }

    Ok(committed)
}

/// The message for a patch that did not fully apply.
///
/// Built from what was actually committed rather than what was planned, so a
/// caller is never told a file landed when its write failed.
fn failure_message(result: &PatchPlan, committed: &[FileOutcome]) -> Option<String> {
    let (path, error) = result.failure.as_ref()?;
    let mut message = match error {
        HunkError::Missing => format!("{path}: file does not exist"),
        HunkError::Exists => format!("{path}: file already exists"),
        HunkError::Parse(detail) => format!("{path}: {detail}"),
        HunkError::Apply(inner) => format!("{path}: {}", inner.message()),
    };

    if !committed.is_empty() {
        let applied: Vec<&str> = committed.iter().map(FileOutcome::path).collect();
        message.push_str(&format!(
            "\n\nAlready applied, and still on disk: {}. Re-read these before retrying.",
            applied.join(", ")
        ));
    }
    if !result.skipped.is_empty() {
        message.push_str(&format!(
            "\n\nNOT applied, because {path} failed first: {}",
            result.skipped.join(", ")
        ));
    }
    Some(message)
}

fn render_diff(outcome: &FileOutcome, before: &HashMap<String, String>) -> Option<String> {
    let old = before.get(outcome.path()).map(String::as_str).unwrap_or("");
    let new = match outcome {
        FileOutcome::Created { content, .. } => content.as_str(),
        FileOutcome::Updated { content, .. } => content.as_str(),
        FileOutcome::Deleted { .. } => "",
    };
    let diff = super::tool_diff::render_diff(
        old,
        new,
        1,
        super::tool_diff::DEFAULT_MAX_DIFF_LINES,
    );
    (!diff.trim().is_empty()).then_some(diff)
}

fn publish(
    ctx: &ToolContext,
    resolved: &Path,
    display_path: &str,
    verb: &str,
    intent: Option<&str>,
    op: FileOp,
) {
    Bus::global().publish(BusEvent::FileTouch(FileTouch {
        session_id: ctx.session_id.clone(),
        path: resolved.to_path_buf(),
        op,
        intent: intent
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty()),
        summary: Some(format!("{verb} {display_path}")),
        detail: None,
    }));
}

/// Trim a diff for the file-touch event's preview.
#[allow(dead_code)]
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
#[path = "apply_patch_tests.rs"]
mod apply_patch_tests;
