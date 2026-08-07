use super::{Tool, ToolContext, ToolOutput};
use crate::message::{ContentBlock, ToolCall};
use crate::session::Session;
use crate::storage;
use crate::{logging, util};
use ::agentgrep::cli::{FindArgs, FullRegionMode, GrepArgs, OutlineArgs, SmartArgs};
use ::agentgrep::find::{FindResult, run_find};
use ::agentgrep::outline::run_outline;
use ::agentgrep::search::{GrepResult, run_grep};
use ::agentgrep::smart_dsl::{SmartQuery, parse_smart_query};
use ::agentgrep::smart_engine::{SmartResult, run_smart};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod args;
mod context;

#[cfg(test)]
use self::args::trace_or_smart_terms_owned;
use self::args::{
    build_find_args, build_grep_args, build_outline_args, build_smart_args_and_query,
    resolve_search_root, summarize_agentgrep_request,
};
use self::context::maybe_write_context_json;
#[cfg(test)]
use self::context::{
    collect_bash_exposure, collect_trace_exposure, tune_known_file, tune_known_region,
};
use ::agentgrep::render::{
    render_find_output, render_grep_output, render_outline_output, render_smart_output,
};

#[derive(Debug, Deserialize)]
struct AgentGrepInput {
    #[serde(default = "default_agentgrep_mode")]
    mode: String,
    // `pattern` accepted for legacy grep-tool calls aliased to agentgrep.
    #[serde(default, alias = "pattern")]
    query: Option<String>,
    // `file_path` accepted because agents frequently pass it instead of `file`.
    #[serde(default, alias = "file_path")]
    file: Option<String>,
    #[serde(default)]
    terms: Option<Vec<String>>,
    #[serde(default)]
    regex: Option<bool>,
    #[serde(default)]
    path: Option<String>,
    // `include` accepted for legacy grep-tool calls aliased to agentgrep.
    #[serde(default, alias = "include")]
    glob: Option<String>,
    #[serde(rename = "type", default)]
    file_type: Option<String>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    no_ignore: Option<bool>,
    #[serde(default)]
    max_files: Option<usize>,
    #[serde(default)]
    max_regions: Option<usize>,
    #[serde(default)]
    full_region: Option<String>,
    #[serde(default)]
    debug_plan: Option<bool>,
    #[serde(default)]
    debug_score: Option<bool>,
    #[serde(default)]
    paths_only: Option<bool>,
}

/// Default cap on rendered grep matches.
///
/// Generous enough that ordinary code searches are unaffected (most return far
/// fewer), while bounding the pathological case of a common string inside large
/// data files. The match header always reports the true total, so a caller who
/// needs more can raise `max_regions` knowing what they are asking for.
const DEFAULT_GREP_MAX_REGIONS: usize = 200;

fn default_agentgrep_mode() -> String {
    "grep".to_string()
}

#[derive(Debug, Serialize, Default)]
struct AgentGrepHarnessContext {
    version: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    known_regions: Vec<AgentGrepKnownRegion>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    known_files: Vec<AgentGrepKnownFile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    known_symbols: Vec<AgentGrepKnownSymbol>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    focus_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AgentGrepKnownRegion {
    path: String,
    start_line: usize,
    end_line: usize,
    body_confidence: f32,
    current_version_confidence: f32,
    prune_confidence: f32,
    source_strength: &'static str,
    reasons: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct AgentGrepKnownFile {
    path: String,
    structure_confidence: f32,
    body_confidence: f32,
    current_version_confidence: f32,
    prune_confidence: f32,
    source_strength: &'static str,
    reasons: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct AgentGrepKnownSymbol {
    path: String,
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    structure_confidence: f32,
    body_confidence: f32,
    current_version_confidence: f32,
    prune_confidence: f32,
    source_strength: &'static str,
    reasons: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy)]
struct RegionConfidenceProfile {
    body_confidence: f32,
    current_version_confidence: f32,
    prune_confidence: f32,
    source_strength: &'static str,
}

#[derive(Debug, Clone)]
struct PendingTraceRegion {
    path: String,
    kind: Option<&'static str>,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone)]
struct ToolExposureObservation {
    tool: ToolCall,
    content: String,
    timestamp: Option<DateTime<Utc>>,
    message_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct ExposureDescriptor {
    timestamp: Option<DateTime<Utc>>,
    message_index: usize,
    total_messages: usize,
    compaction_cutoff: Option<usize>,
}

pub struct AgentGrepTool;

impl AgentGrepTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AgentGrepTool {
    fn name(&self) -> &str {
        "agentgrep"
    }

    fn description(&self) -> &str {
        "Search code by symbol and structure, not just lines. Not grep in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "mode": {
                    "type": "string",
                    "enum": ["grep", "find", "outline", "trace"],
                    "description": "Mode: grep (default), find (file names), outline (one file), trace (relationship DSL)."
                },
                "query": {
                    "type": "string",
                    "description": "Search query. Required for grep (literal unless regex=true); optional ranking terms for find."
                },
                "file": {
                    "type": "string",
                    "description": "Single file to inspect. Required for outline."
                },
                "terms": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Trace DSL terms, e.g. [\"subject:auth_status\", \"relation:rendered\"]. Not for grep/find; use query."
                },
                "regex": {
                    "type": "boolean",
                    "description": "In grep mode, treat query as a regex. Defaults to false (literal)."
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search, relative to the workspace. Omit to search the whole workspace."
                },
                "glob": {
                    "type": "string",
                    "description": "Optional file glob filter such as **/*.rs. Omit to search everything."
                },
                "type": {
                    "type": "string",
                    "description": "Optional ripgrep file type filter, such as rs, py, js, ts, or md."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to return for find/trace-style modes."
                },
                "max_regions": {
                    "type": "integer",
                    "description": "Maximum number of matching regions to return."
                },
                "paths_only": {
                    "type": "boolean",
                    "description": "Return only matching paths instead of match excerpts where supported."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AgentGrepInput = serde_json::from_value(input)?;
        // The search shells out to ripgrep and walks/reads files (and for
        // trace/outline modes also loads the session and reads more files),
        // all of which is blocking work with no async yield points. Offload it
        // to the blocking pool so we never stall a tokio worker thread. When it
        // ran inline, a single poll of this future executed the whole search to
        // completion, freezing the TUI's select! render/input loop, which made
        // the first cold-cache search feel like it "takes forever" with no
        // spinner and an unresponsive interrupt. This mirrors how the sibling
        // grep/glob/ls tools offload their work.
        tokio::task::spawn_blocking(move || run_agentgrep_blocking(&params, &ctx))
            .await
            .map_err(|err| anyhow::anyhow!("agentgrep task failed to join: {err}"))?
    }
}

fn run_agentgrep_blocking(params: &AgentGrepInput, ctx: &ToolContext) -> Result<ToolOutput> {
    if ctx.working_dir.is_none() {
        let explicit_path = params.path.as_deref().or(params.file.as_deref());
        if explicit_path.is_none_or(|path| !Path::new(path).is_absolute()) {
            anyhow::bail!(
                "agentgrep requires a session working directory unless an absolute path is provided"
            );
        }
    }
    let context_path = maybe_write_context_json(params, ctx)?;
    let request = summarize_agentgrep_request(params, ctx, context_path.as_deref());
    // Resolve through the context so `path: "~"` or a relative path is compared
    // as the directory actually searched. `resolve_search_root` returns an
    // explicit `path` verbatim, which would otherwise never match home.
    let search_root = match params.path.as_deref().or(params.file.as_deref()) {
        Some(path) => Some(resolve_path_arg(ctx, path)),
        None => ctx.working_dir.clone(),
    };
    let started_at = std::time::Instant::now();
    let outcome = execute_linked_agentgrep(params, ctx, context_path.as_deref());
    let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    if let Some(path) = context_path {
        let _ = std::fs::remove_file(path);
    }

    match outcome {
        Ok(output) => {
            if elapsed_ms >= 2_000 {
                logging::warn(&format!(
                    "agentgrep slow mode={} elapsed_ms={} request={}",
                    params.mode, elapsed_ms, request
                ));
            }
            // A whole-home search is occasionally what was meant ("where is the
            // file that mentions foo"), so this warns rather than refusing. It
            // is appended to a *successful* result because the cost is the point:
            // the caller should see that the scope was enormous even when it
            // worked, and can narrow it next time.
            if let Some(root) = search_root.as_deref()
                && is_home_directory(root)
            {
                let mut output = output;
                output.output.push_str(&unscoped_home_search_note(root));
                Ok(output)
            } else {
                Ok(output)
            }
        }
        Err(err) => {
            let detail = err.to_string();
            let detail = util::truncate_str(detail.trim(), 600);
            logging::warn(&format!(
                "agentgrep failure mode={} elapsed_ms={} request={} error={}",
                params.mode, elapsed_ms, request, detail
            ));
            // ripgrep exits 2 when *any* path errored, even though it still
            // printed every match it found. The matches are lost inside the
            // agentgrep crate before they reach us, so they cannot be recovered
            // here; what can be fixed is the report. Unfiltered, this is
            // hundreds of near-identical "Operation not permitted" lines that
            // bury the real problem and read as a broken tool rather than an
            // over-broad search.
            if let Some(summary) =
                summarize_permission_failure(&err.to_string(), search_root.as_deref())
            {
                return Err(anyhow::anyhow!(summary));
            }
            Err(anyhow::anyhow!(
                "agentgrep {} failed after {}ms: {}",
                params.mode,
                elapsed_ms,
                err
            ))
        }
    }
}

/// Whether a search root is exactly the user's home directory.
///
/// Only an exact match counts. A subdirectory of home is an ordinary, usually
/// deliberate scope; it is the undivided `$HOME` sweep that is worth remarking
/// on, and warning about every path under home would be noise.
fn is_home_directory(root: &Path) -> bool {
    dirs::home_dir().is_some_and(|home| root == home)
}

fn unscoped_home_search_note(root: &Path) -> String {
    format!(
        "\n\nNote: this searched all of {}, which is slow and includes application \
         data, caches, and other noise. Pass `path` to scope the search when you \
         know roughly where to look.",
        root.display()
    )
}

/// Collapse ripgrep's per-directory permission errors into one actionable line.
///
/// Returns `None` when the failure was not dominated by permission errors, so
/// genuine failures keep their original message rather than being reinterpreted.
fn summarize_permission_failure(error: &str, root: Option<&Path>) -> Option<String> {
    let denied: Vec<&str> = error
        .lines()
        .filter(|line| {
            line.contains("Operation not permitted") || line.contains("Permission denied")
        })
        .collect();
    // One or two denied directories is normal noise inside a repo and not worth
    // rewriting the error for; a wall of them is the actual diagnosis.
    if denied.len() < 3 {
        return None;
    }

    let scope = root
        .map(|root| format!(" of {}", root.display()))
        .unwrap_or_default();
    // "Narrow the path" is the remedy for an over-broad scope, and unfollowable
    // when the caller already named one file: there is nothing narrower to ask
    // for, and the advice sends them looking for a mistake they did not make.
    let remedy = if root.is_some_and(|root| root.is_file()) {
        "This search was already scoped to a single file, so the unreadable \
         directories are not reachable from it: the walk is being run against \
         a wider root than the `path` given, which is a bug worth reporting."
    } else {
        "Re-run with `path` set to a narrower directory to get results."
    };
    Some(format!(
        "Search{scope} could not complete: {} directories were unreadable \
         (on macOS, privacy-protected locations such as Mail, Messages, Safari \
         and app containers). Matches outside those directories were found but \
         discarded, because ripgrep reports a partial failure for the whole run. \
         {remedy}",
        denied.len()
    ))
}

fn execute_linked_agentgrep(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> Result<ToolOutput> {
    // `file` scopes to one file exactly as `path` does, and the scope builder
    // already honours `path.or(file)`. Reading only `path` here left a `file`
    // scope unlabelled once the search root became the file itself.
    let exact_file = exact_search_file_path(ctx, params.path.as_deref().or(params.file.as_deref()));
    match params.mode.as_str() {
        "grep" => {
            let args = build_grep_args(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref())?;
            let result = filter_grep_result_to_exact_file(
                run_grep(&root, &args).map_err(anyhow::Error::msg)?,
                exact_file.as_deref(),
            );
            // Bound the rendered matches by default. `find` and `outline` already
            // default to 5 files / 6 regions, but grep passed `None` straight
            // through, so one unscoped query over a repo containing large data
            // files rendered every match: a search for a common key across 2,027
            // benchmark transcripts produced 923k chars in a single call. The
            // header still reports the true total, so the caller sees that more
            // matches exist and can raise the cap deliberately.
            let max_regions = params.max_regions.or(Some(DEFAULT_GREP_MAX_REGIONS));
            Ok(
                ToolOutput::new(render_grep_output(&result, &args, max_regions))
                    .with_title("agentgrep grep"),
            )
        }
        "find" => {
            let args = build_find_args(params, ctx)?;
            let root = resolve_search_root(ctx, args.path.as_deref())?;
            let result =
                filter_find_result_to_exact_file(run_find(&root, &args), exact_file.as_deref());
            Ok(ToolOutput::new(render_find_output(&result, &args)).with_title("agentgrep find"))
        }
        "outline" => {
            let args = build_outline_args(params, ctx, context_json_path)?;
            let root = resolve_search_root(ctx, args.path.as_deref())?;
            let result = run_outline(&root, &args).map_err(anyhow::Error::msg)?;
            Ok(ToolOutput::new(render_outline_output(&result)).with_title("agentgrep outline"))
        }
        "trace" | "smart" => {
            let (args, query) = build_smart_args_and_query(params, ctx, context_json_path)?;
            let root = resolve_search_root(ctx, args.path.as_deref())?;
            let result = filter_smart_result_to_exact_file(
                run_smart(&root, &query, &args).map_err(anyhow::Error::msg)?,
                exact_file.as_deref(),
            );
            Ok(ToolOutput::new(render_smart_output(&result, &args))
                .with_title(format!("agentgrep {}", params.mode)))
        }
        _ => Err(anyhow::anyhow!(
            "Unsupported agentgrep mode: {}. Use grep, find, outline, or trace.",
            params.mode
        )),
    }
}

fn resolve_path_arg(ctx: &ToolContext, path: &str) -> PathBuf {
    ctx.resolve_path(Path::new(path))
}

/// The file a single-file scope names, as the searcher will report it.
///
/// Returns `None` unless `path` resolves to an existing file, so a directory
/// scope is unaffected.
///
/// The reported path is relative to the search root, and for a file scope the
/// root *is* the file, so the searcher strips it to the empty string. Callers
/// compare against that rather than against a file name: matching on the bare
/// name kept every same-named file in the tree, so `src/main.rs` in a workspace
/// of crates retained each crate's `main.rs`.
fn exact_search_file_path(ctx: &ToolContext, path: Option<&str>) -> Option<String> {
    let path = path?;
    let resolved = resolve_path_arg(ctx, path);
    if !resolved.is_file() {
        return None;
    }
    Some(resolved.display().to_string())
}

/// Whether a result path denotes the single file a scope named.
///
/// Accepts both spellings the searcher can produce for a file root: the empty
/// string (the root stripped from itself) and the full path.
fn is_exact_file_match(result_path: &str, exact_file: &str) -> bool {
    result_path.is_empty() || result_path == exact_file
}

/// How a single-file result should be labelled in the output.
///
/// A file root strips to the empty string, which would render a match with no
/// file name against it. Restore the name the caller asked about so the result
/// still says where it came from.
fn exact_file_display_name(exact_file: &str) -> String {
    Path::new(exact_file)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| exact_file.to_string())
}

fn filter_grep_result_to_exact_file(
    mut result: GrepResult,
    exact_file: Option<&str>,
) -> GrepResult {
    let Some(exact_file) = exact_file else {
        return result;
    };

    result
        .files
        .retain(|file| is_exact_file_match(&file.path, exact_file));
    // A file root strips to the empty string; label it so the match still
    // names the file it came from.
    let display = exact_file_display_name(exact_file);
    for file in &mut result.files {
        if file.path.is_empty() {
            file.path = display.clone();
        }
    }
    result.total_files = result.files.len();
    result.total_matches = result.files.iter().map(|file| file.matches.len()).sum();
    result
}

fn filter_find_result_to_exact_file(
    mut result: FindResult,
    exact_file: Option<&str>,
) -> FindResult {
    let Some(exact_file) = exact_file else {
        return result;
    };

    result
        .files
        .retain(|file| is_exact_file_match(&file.path, exact_file));
    // A file root strips to the empty string; label it so the match still
    // names the file it came from.
    let display = exact_file_display_name(exact_file);
    for file in &mut result.files {
        if file.path.is_empty() {
            file.path = display.clone();
        }
    }
    result
}

fn filter_smart_result_to_exact_file(
    mut result: SmartResult,
    exact_file: Option<&str>,
) -> SmartResult {
    let Some(exact_file) = exact_file else {
        return result;
    };

    result
        .files
        .retain(|file| is_exact_file_match(&file.path, exact_file));
    // A file root strips to the empty string; label it so the match still
    // names the file it came from.
    let display = exact_file_display_name(exact_file);
    for file in &mut result.files {
        if file.path.is_empty() {
            file.path = display.clone();
        }
    }
    result.summary.total_files = result.files.len();
    result.summary.total_regions = result.files.iter().map(|file| file.regions.len()).sum();
    result.summary.best_file = result.files.first().map(|file| file.path.clone());
    result
}

fn normalized_agentgrep_glob(glob: Option<&str>) -> Option<&str> {
    let glob = glob?.trim();
    if glob.is_empty() {
        return None;
    }

    if is_match_all_glob(glob) {
        return None;
    }

    Some(glob)
}

fn normalized_agentgrep_glob_owned(glob: Option<&str>) -> Option<String> {
    normalized_agentgrep_glob(glob).map(ToOwned::to_owned)
}

fn is_match_all_glob(glob: &str) -> bool {
    matches!(glob, "*" | "**" | "**/*" | "./*" | "./**" | "./**/*")
}

#[cfg(test)]
#[path = "agentgrep_tests.rs"]
mod tests;
