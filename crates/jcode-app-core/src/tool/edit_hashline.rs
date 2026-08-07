//! Hashline mode for the `edit` tool.
//!
//! `edit` takes one of two shapes. The old one is `old_string`/`new_string`
//! exact replacement; the new one is a hashline `input` string carrying
//! `[path#TAG]` headers and line-anchored operations.
//!
//! # Why this changed `edit`'s schema rather than adding a sibling tool
//!
//! A sibling `hashline_edit` would have left the model choosing between two
//! tools that do the same job, and every prompt, renderer, and permission rule
//! would need to learn the second name. Extending `edit` keeps one name.
//!
//! `file_path` stays a top-level parameter in hashline mode even though the
//! header already carries the path. 56 files in this workspace read
//! `file_path` off an edit call: diff rendering, permission prompts, the TUI's
//! file-touch surfaces, desktop2's scan. Dropping it to a header-only form
//! would break all of them at once for no gain.

use super::{ToolContext, ToolOutput};
use anyhow::Result;
use jcode_hashline::{parse_ops, preflight, split_sections, Prepared, SectionInput};
use std::path::Path;

/// Whether to refuse edits to lines no `read` displayed.
///
/// Defaults **off**, as omp ships `enforceSeenLines`. On, an edit to a line the
/// store never recorded as displayed is refused. That sounds strictly safer,
/// but our bash tool can put file content in front of the model without the
/// store seeing it, so the guard refuses edits the model is in fact
/// well-informed about.
fn enforce_seen_lines() -> bool {
    jcode_base::config::config().tools.edit_enforce_seen_lines
}

/// One section resolved against the filesystem, held across preflight so the
/// commit loop can write without re-reading.
struct Resolved {
    /// Path as the model authored it, for messages the model reads.
    authored: String,
    resolved: std::path::PathBuf,
    current_text: String,
    expected_tag: Option<String>,
    ops: Vec<jcode_hashline::Op>,
}

/// Apply a hashline patch. Returns the tool output on success.
pub async fn execute_hashline(
    input: &str,
    intent: Option<&str>,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let cwd = ctx
        .working_dir
        .as_deref()
        .and_then(|dir| dir.to_str())
        .map(str::to_string);
    let sections = split_sections(input, cwd.as_deref()).map_err(|error| anyhow::anyhow!(error))?;

    if sections.is_empty() {
        return Err(anyhow::anyhow!(
            "No hashline sections found. Each section starts with a [path#tag] header, \
             where the tag is the one `read` returned for that file."
        ));
    }

    // Resolve every section against disk before validating any of them, so a
    // missing file is reported before a stale tag in a later section.
    let mut resolved = Vec::with_capacity(sections.len());
    for section in &sections {
        // `split_sections` returns an anonymous section with an empty path when
        // the input carried no header at all. Without this check the empty path
        // resolves to the working directory and the failure surfaces as
        // "Is a directory (os error 21)", which tells the model nothing about
        // the format it actually got wrong.
        if section.path.is_empty() {
            return Err(anyhow::anyhow!(
                "This patch has no [path#tag] header. Start each section with the \
                 header `read` returned for that file, for example [src/lib.rs#A1B2], \
                 then the operations beneath it."
            ));
        }
        let path = ctx.resolve_path(Path::new(&section.path));
        if !path.exists() {
            return Err(anyhow::anyhow!(super::read::file_not_found_message(
                &section.path,
                &path,
                ctx.working_dir.as_deref(),
            )));
        }
        let current_text = tokio::fs::read_to_string(&path).await?;
        let ops = parse_ops(&section.body).map_err(|error| {
            anyhow::anyhow!("{} in section [{}]", error, section.path)
        })?;
        resolved.push(Resolved {
            authored: section.path.clone(),
            resolved: path,
            current_text,
            expected_tag: section.file_hash.clone(),
            ops: ops.ops,
        });
    }

    // Validate and apply in memory. Nothing is written until every section
    // succeeds, so a bad anchor in section three cannot leave one and two
    // written.
    let store = super::hashline_store::for_session(&ctx.session_id);
    let inputs: Vec<SectionInput<'_>> = resolved
        .iter()
        .map(|section| SectionInput {
            path: &section.authored,
            current_text: &section.current_text,
            expected_tag: section.expected_tag.as_deref(),
            ops: &section.ops,
        })
        .collect();
    let prepared = preflight(&store, &inputs, enforce_seen_lines())
        .map_err(|error| anyhow::anyhow!(error.message()))?;

    commit(prepared, &resolved, intent, ctx).await
}

/// Write the prepared sections and render the result.
async fn commit(
    prepared: Vec<Prepared>,
    resolved: &[Resolved],
    intent: Option<&str>,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    use crate::bus::{Bus, BusEvent, FileOp, FileTouch};

    let mut body = String::new();
    let mut written: Vec<String> = Vec::new();

    for (section, source) in prepared.iter().zip(resolved.iter()) {
        // Preflight guarantees validity, not that the disk will accept the
        // write. If one fails partway, say which ones already landed rather
        // than reporting a bare error for a partially applied patch.
        let write = if section.removed {
            tokio::fs::remove_file(&source.resolved).await
        } else if let Some(dest) = &section.move_dest {
            let dest = ctx.resolve_path(Path::new(dest));
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            match tokio::fs::write(&dest, &section.after).await {
                Ok(()) => tokio::fs::remove_file(&source.resolved).await,
                Err(error) => Err(error),
            }
        } else {
            tokio::fs::write(&source.resolved, &section.after).await
        };

        if let Err(error) = write {
            return Err(anyhow::anyhow!(
                "Failed writing {}: {error}.{}",
                section.path,
                if written.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Already written: {}. Re-read before retrying, \
                         since those files have changed.",
                        written.join(", ")
                    )
                }
            ));
        }
        written.push(section.path.clone());

        // The store must not keep claiming the pre-edit content is current, or
        // the next edit resolves a tag to text that is no longer on disk.
        let store = super::hashline_store::for_session(&ctx.session_id);
        if section.removed {
            store.invalidate(&section.path);
        } else if let Some(dest) = &section.move_dest {
            store.relocate(&section.path, dest);
        } else {
            // Record the post-edit content so a follow-up edit can anchor to
            // `new_tag` without re-reading. Seen lines carry over: the model has
            // not stopped seeing what it just wrote.
            store.record(&section.path, &section.after, None);
        }

        let diff = super::tool_diff::render_diff(&section.before, &section.after, 1, usize::MAX);
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: source.resolved.clone(),
            op: FileOp::Edit,
            intent: intent
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty()),
            summary: Some(if section.removed {
                format!("removed {}", section.path)
            } else if let Some(dest) = &section.move_dest {
                format!("moved {} to {dest}", section.path)
            } else {
                format!("edited {}", section.path)
            }),
            detail: None,
        }));

        body.push_str(&format!(
            "[{}#{}]\n{}\n",
            section.path, section.new_tag, diff
        ));
        for warning in &section.warnings {
            body.push_str(&format!("warning: {warning}\n"));
        }
    }

    // The title is what the TUI shows on the collapsed tool call. Naming only
    // the first path hides that a multi-file patch touched anything else, which
    // is the one thing about this tool a reviewer most needs to see.
    let title = match prepared.len() {
        0 => String::new(),
        1 => prepared[0].path.clone(),
        2 => format!("{} and {}", prepared[0].path, prepared[1].path),
        n => format!("{} and {} more files", prepared[0].path, n - 1),
    };
    Ok(ToolOutput::new(body).with_title(title))
}

#[cfg(test)]
#[path = "edit_hashline_tests.rs"]
mod edit_hashline_tests;
