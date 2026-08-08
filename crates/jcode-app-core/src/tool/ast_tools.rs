//! The `ast_grep` and `ast_edit` tools.
//!
//! Ported from omp's `ast-grep.ts` and `ast-edit.ts`. These search and rewrite
//! by code structure rather than text, so a pattern matches a call or a
//! function regardless of how it is spaced, wrapped or named, and it does not
//! match the same characters inside a string or a comment.
//!
//! # Why `ast_edit` writes immediately
//!
//! omp defaults `dry_run` to true and has the model call twice. We do not need
//! that: every write here goes through the same approval gate as `edit` and
//! `write`, which shows the user the diff before anything lands. The plan is
//! still computed across every file before the first byte is written, so a
//! rewrite that is wrong in the last file cannot half-apply.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use jcode_ast::{
    plan, resolve_language, search, RewriteOptions, RewritePlan, SearchFailure, SearchOptions,
};
use jcode_search::{resolve_targets, WalkOptions};
use serde::Deserialize;
use serde_json::{json, Value};

/// Unknown fields are tolerated rather than rejected, matching the other tools:
/// one failed native call sends a model back to bash for the session.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AstGrepInput {
    #[serde(alias = "query")]
    pattern: Option<String>,
    path: Option<String>,
    language: Option<String>,
    #[serde(alias = "max_files")]
    head_limit: Option<usize>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AstEditInput {
    pattern: Option<String>,
    #[serde(alias = "rewrite")]
    replacement: Option<String>,
    path: Option<String>,
    language: Option<String>,
    max_files: Option<usize>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
}

pub struct AstGrepTool;

impl AstGrepTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AstGrepTool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AstEditTool;

impl AstEditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AstEditTool {
    fn default() -> Self {
        Self::new()
    }
}

fn walk_options(hidden: Option<bool>, gitignore: Option<bool>) -> WalkOptions {
    WalkOptions {
        hidden: hidden.unwrap_or(false),
        respect_gitignore: gitignore.unwrap_or(true),
    }
}

/// Language names are resolved up front so an unknown one is a clear error
/// rather than a silent whole-tree inference that matches nothing.
fn language_option(raw: Option<&str>) -> Result<Option<jcode_ast::SupportLang>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    resolve_language(raw)
        .map(Some)
        .map_err(|error| anyhow::anyhow!("{}", error.message()))
}

fn failure(error: SearchFailure) -> anyhow::Error {
    anyhow::anyhow!("{}", error.message())
}

#[async_trait]
impl Tool for AstGrepTool {
    fn name(&self) -> &str {
        "ast_grep"
    }

    fn description(&self) -> &str {
        // Kept under the description token cap: the pattern syntax belongs on
        // the `pattern` property, where it is read at the point of use.
        "Search code by structure, not text. Never matches inside strings or \
         comments."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "ast-grep pattern. `$N` captures one node, `$$$N` many. e.g. `fn $N() { $$$B }`"
                },
                "path": {
                    "type": "string",
                    "description": "File, directory or glob to search. Defaults to the workspace root."
                },
                "language": {
                    "type": "string",
                    "description": "Language to parse as, e.g. rust, python, typescript. Inferred per file when omitted."
                },
                "head_limit": {"type": "number", "description": "Maximum files to report."},
                "hidden": {"type": "boolean", "description": "Include hidden files."},
                "gitignore": {"type": "boolean", "description": "Respect .gitignore. Default true."}
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AstGrepInput = serde_json::from_value(input)?;
        let pattern = params
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("ast_grep requires 'pattern'"))?
            .to_string();

        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let targets = resolve_targets(params.path.as_deref(), &root)
            .map_err(|error| anyhow::anyhow!("{}", error.message()))?;

        let mut options = SearchOptions {
            language: language_option(params.language.as_deref())?,
            walk: walk_options(params.hidden, params.gitignore),
            ..SearchOptions::default()
        };
        if let Some(limit) = params.head_limit {
            options.file_limit = limit.max(1);
        }

        let result = tokio::task::spawn_blocking({
            let root = root.clone();
            move || search(&pattern, &targets, &root, &options)
        })
        .await?
        .map_err(failure)?;

        // Tags are minted per file so a structural search result can be edited
        // without a re-read, exactly as `grep` does. Only for files we could
        // read: a tag that does not match disk is worse than no tag.
        let store = super::hashline_store::for_session(&ctx.session_id);
        for file in &result.files {
            if let Ok(content) = std::fs::read_to_string(root.join(&file.path)) {
                store.record(&file.path, &content, None);
            }
        }

        let total_matches: usize = result.files.iter().map(|file| file.total).sum();
        let body = render_search(&result, &store);
        Ok(ToolOutput::new(body).with_title(format!(
            "ast_grep {} in {} file{}",
            total_matches,
            result.total_files,
            if result.total_files == 1 { "" } else { "s" }
        )))
    }
}

/// Rendered with `jcode-search`'s own line formatter, not a private one.
///
/// This matters more than consistency. `format_match_line` emits `*LINE:text`
/// carrying the WHOLE SOURCE LINE, which is the shape `edit` accepts under a
/// hashline tag. A private renderer that printed the matched AST node instead
/// dropped the leading indentation and the trailing semicolon, so a model
/// editing straight from a search result wrote back a line missing both and
/// needed a second edit to repair it. Observed in a live run.
fn render_search(
    result: &jcode_ast::SearchResult,
    store: &std::sync::Arc<jcode_hashline::SnapshotStore>,
) -> String {
    if result.files.is_empty() {
        let mut body = String::from("No matches found.");
        if result.unsupported_files > 0 {
            body.push_str(&format!(
                "\n{} file(s) skipped: no grammar for their language.",
                result.unsupported_files
            ));
        }
        if result.incompatible_files > 0 {
            body.push_str(&format!(
                "\n{} file(s) skipped: the pattern is not valid for their language.",
                result.incompatible_files
            ));
        }
        return body;
    }

    let mut body = String::new();
    for file in &result.files {
        let snapshot = store.head(&file.path);
        let tag = snapshot.as_ref().map(|snapshot| snapshot.hash.clone());
        // Read from the snapshot rather than disk: it is the exact text the tag
        // names, so the lines shown cannot disagree with the tag above them.
        let source = snapshot.map(|snapshot| snapshot.text);
        body.push_str(&format!(
            "{}{}\n",
            file.path,
            tag.as_ref()
                .map(|tag| format!(" #{tag}"))
                .unwrap_or_default()
        ));
        // The file's own lines, not the match text: a structural match starts
        // mid-line and ends before the statement's terminator, so echoing the
        // node loses exactly the characters an edit has to reproduce.
        let source_lines: Vec<&str> = source
            .as_deref()
            .map(|text| text.lines().collect())
            .unwrap_or_default();
        let mut shown: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        for found in &file.matches {
            let span = found.text.lines().count().max(1);
            for line in found.line..found.line + span {
                shown.insert(line);
            }
        }
        let mut previous: Option<usize> = None;
        for line in shown {
            // A gap marker, so non-adjacent matches are not misread as a
            // contiguous block of the file.
            if previous.is_some_and(|last| line > last + 1) {
                body.push_str("...\n");
            }
            let text = source_lines.get(line.saturating_sub(1)).copied();
            match text {
                Some(text) => body.push_str(&jcode_search::format_match_line(
                    line,
                    text,
                    true,
                    tag.is_some(),
                )),
                None => continue,
            }
            body.push('\n');
            previous = Some(line);
        }
        if file.total > file.matches.len() {
            body.push_str(&format!(
                "  ... {} more match(es) in this file\n",
                file.total - file.matches.len()
            ));
        }
        body.push('\n');
    }

    if result.file_limit_reached {
        body.push_str(&format!(
            "Showing {} of {} files with matches.\n",
            result.files.len(),
            result.total_files
        ));
    }
    body.trim_end().to_string()
}

#[async_trait]
impl Tool for AstEditTool {
    fn name(&self) -> &str {
        "ast_edit"
    }

    fn description(&self) -> &str {
        "Structural find-and-replace across files. For mechanical refactors."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "ast-grep pattern. `$N` captures one node, `$$$N` many. e.g. `log($ARG)`"
                },
                "replacement": {
                    "type": "string",
                    "description": "Replacement, may reuse the pattern's metavariables. e.g. `trace($ARG)`"
                },
                "path": {
                    "type": "string",
                    "description": "File, directory or glob to rewrite. Defaults to the workspace root."
                },
                "language": {
                    "type": "string",
                    "description": "Language to parse as. Inferred per file when omitted."
                },
                "max_files": {"type": "number", "description": "Maximum files to rewrite. Default 50."},
                "hidden": {"type": "boolean", "description": "Include hidden files."},
                "gitignore": {"type": "boolean", "description": "Respect .gitignore. Default true."}
            },
            "required": ["pattern", "replacement"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AstEditInput = serde_json::from_value(input)?;
        let pattern = params
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("ast_edit requires 'pattern'"))?
            .to_string();
        let replacement = params
            .replacement
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ast_edit requires 'replacement'"))?;

        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let targets = resolve_targets(params.path.as_deref(), &root)
            .map_err(|error| anyhow::anyhow!("{}", error.message()))?;

        let mut options = RewriteOptions {
            language: language_option(params.language.as_deref())?,
            walk: walk_options(params.hidden, params.gitignore),
            ..RewriteOptions::default()
        };
        if let Some(limit) = params.max_files {
            options.max_files = limit.max(1);
        }

        let plan = tokio::task::spawn_blocking({
            let root = root.clone();
            move || plan(&pattern, &replacement, &targets, &root, &options)
        })
        .await?
        .map_err(failure)?;

        if plan.is_empty() {
            return Ok(ToolOutput::new(no_change_message(&plan))
                .with_title("ast_edit 0 changes".to_string()));
        }

        // Written only after every file's rewrite is known, so a plan that is
        // wrong in the last file cannot leave the first half applied.
        let store = super::hashline_store::for_session(&ctx.session_id);
        let mut written = 0usize;
        for file in &plan.files {
            std::fs::write(&file.absolute, &file.after)
                .map_err(|error| anyhow::anyhow!("writing {}: {error}", file.path))?;
            // Re-recorded rather than dropped: the file still exists and the
            // model will likely edit it next, so it gets a tag matching what is
            // now on disk. A stale tag here would be a lie about disk contents.
            store.record(&file.path, &file.after, None);
            written += 1;
        }

        Ok(ToolOutput::new(render_plan(&plan, &store)).with_title(format!(
            "ast_edit {} replacement{} in {} file{}",
            plan.total_replacements,
            if plan.total_replacements == 1 { "" } else { "s" },
            written,
            if written == 1 { "" } else { "s" }
        )))
    }
}

/// "Nothing matched" and "nothing could be parsed" are different answers, and
/// only one of them means the pattern was wrong.
fn no_change_message(plan: &RewritePlan) -> String {
    let mut body = format!(
        "No changes. Searched {} file(s), no structural matches.",
        plan.files_searched
    );
    if plan.unsupported_files > 0 {
        body.push_str(&format!(
            "\n{} file(s) skipped: no grammar for their language.",
            plan.unsupported_files
        ));
    }
    if plan.incompatible_files > 0 {
        body.push_str(&format!(
            "\n{} file(s) skipped: the pattern is not valid for their language.",
            plan.incompatible_files
        ));
    }
    body
}

fn render_plan(plan: &RewritePlan, store: &std::sync::Arc<jcode_hashline::SnapshotStore>) -> String {
    let mut body = format!(
        "Rewrote {} match(es) in {} file(s):\n\n",
        plan.total_replacements,
        plan.files.len()
    );
    for file in &plan.files {
        let tag = store
            .head(&file.path)
            .map(|snapshot| format!(" #{}", snapshot.hash))
            .unwrap_or_default();
        body.push_str(&format!(
            "{}{} ({} replacement{})\n",
            file.path,
            tag,
            file.count,
            if file.count == 1 { "" } else { "s" }
        ));
        body.push_str(&super::tool_diff::render_diff(
            &file.before,
            &file.after,
            1,
            usize::MAX,
        ));
        body.push('\n');
    }
    if plan.limit_reached {
        body.push_str("\nA limit was reached: more files or matches remain unchanged.\n");
    }
    // Named explicitly, because the reformatting is the tool's doing and not
    // part of the change the caller asked for. Silence here would leave them
    // reading an unexplained whitespace diff.
    if plan.reflowed_matches > 0 {
        body.push_str(&format!(
            "\nNote: {} match(es) were reflowed onto fewer lines. The code is \
             equivalent, but review the diff if formatting matters.\n",
            plan.reflowed_matches
        ));
    }
    body.trim_end().to_string()
}

#[cfg(test)]
#[path = "ast_tools_tests.rs"]
mod ast_tools_tests;
