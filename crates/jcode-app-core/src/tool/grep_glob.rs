//! `grep` and `glob`: the search tools models already know how to call.
//!
//! These are thin adapters onto [`AgentGrepTool`], not new search engines.
//! They exist because the Anthropic OAuth path advertises Claude-Code's
//! `Grep` and `Glob` builtins only when a local tool of that name backs them
//! (`has_backing`), and nothing did. Both were silently dropped from the
//! advertised toolset, so a model with strong `Grep`/`Glob` priors found them
//! missing and fell back to `Bash` plus ripgrep for the rest of the session.
//!
//! The translation is parameter names only: `pattern` -> `query`,
//! `output_mode` -> `paths_only`, `head_limit` -> `max_regions`. Search
//! behaviour stays in one place so these cannot drift from `agentgrep`.

use super::agentgrep::AgentGrepTool;
use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

/// Claude-Code `Grep` parameters. Unknown fields are tolerated rather than
/// rejected: a model calling from its priors should get results, not a schema
/// error, since one failed native call is enough to send it back to bash.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GrepInput {
    #[serde(alias = "query")]
    pattern: Option<String>,
    path: Option<String>,
    glob: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>,
    output_mode: Option<String>,
    head_limit: Option<usize>,
    #[serde(rename = "-i")]
    case_insensitive: Option<bool>,
    multiline: Option<bool>,
}

/// Claude-Code `Glob` parameters.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GlobInput {
    #[serde(alias = "query")]
    pattern: Option<String>,
    path: Option<String>,
    #[serde(alias = "max_files")]
    head_limit: Option<usize>,
}

pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents by regex. You MUST use this, not grep or rg in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for. Searches ignore-aware, skipping binaries and vendored trees."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Omit to search the whole workspace."
                },
                "glob": {
                    "type": "string",
                    "description": "Filter files by glob, such as **/*.rs."
                },
                "type": {
                    "type": "string",
                    "description": "Filter by file type, such as rs, py, ts, or md."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "content (default) returns matching regions; files_with_matches returns paths only."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return."
                },
                "-i": {
                    "type": "boolean",
                    "description": "Case-insensitive search."
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Allow the pattern to match across line boundaries."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: GrepInput = serde_json::from_value(input)?;
        AgentGrepTool::new()
            .execute(grep_delegation(params)?, ctx)
            .await
    }
}

/// Translate a Claude-Code `Grep` call into an agentgrep call.
///
/// Pure and separate from `execute` so the translation is testable without a
/// workspace, which is where the interesting failures are.
fn grep_delegation(params: GrepInput) -> Result<Value> {
    let pattern = params
        .pattern
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::anyhow!("grep requires 'pattern'"))?;

    // `files_with_matches` and `count` both want paths rather than excerpts;
    // agentgrep expresses that as `paths_only`.
    let paths_only = matches!(
        params.output_mode.as_deref(),
        Some("files_with_matches") | Some("count")
    );

    // A case-insensitive request is honoured by folding it into the pattern,
    // since agentgrep's grep mode has no separate flag for it.
    let query = if params.case_insensitive.unwrap_or(false) {
        format!("(?i){pattern}")
    } else {
        pattern
    };

    let mut delegated = json!({
        "mode": "grep",
        "query": query,
        // Claude-Code's `Grep` is regex by default; agentgrep's is literal, so
        // omitting this silently changes the meaning of every pattern.
        "regex": true,
        "paths_only": paths_only,
    });
    insert_opt(&mut delegated, "path", params.path);
    insert_opt(&mut delegated, "glob", params.glob);
    insert_opt(&mut delegated, "type", params.file_type);
    if let Some(limit) = params.head_limit {
        delegated["max_regions"] = json!(limit);
        delegated["max_files"] = json!(limit);
    }
    if params.multiline.unwrap_or(false) {
        delegated["full_region"] = json!("all");
    }
    Ok(delegated)
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by name or path pattern. You MUST use this, not find or ls in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "Glob such as **/*.rs, or bare words to rank file names by. Skips ignored and vendored directories."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Omit to search the whole workspace."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of paths to return."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: GlobInput = serde_json::from_value(input)?;
        AgentGrepTool::new()
            .execute(glob_delegation(params)?, ctx)
            .await
    }
}

/// Translate a Claude-Code `Glob` call into an agentgrep find call.
fn glob_delegation(params: GlobInput) -> Result<Value> {
    let pattern = params
        .pattern
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::anyhow!("glob requires 'pattern'"))?;

    let mut delegated = json!({
        "mode": "find",
        "paths_only": true,
    });

    // A real glob goes to agentgrep's `glob` filter; bare words are ranking
    // terms for find mode. Sending a glob as a query would match it literally
    // against file names and find nothing.
    if is_glob_pattern(&pattern) {
        delegated["glob"] = json!(pattern);
    } else {
        delegated["query"] = json!(pattern);
    }
    insert_opt(&mut delegated, "path", params.path);
    if let Some(limit) = params.head_limit {
        delegated["max_files"] = json!(limit);
    }
    Ok(delegated)
}

/// Whether this looks like a path pattern rather than ranking words.
fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*')
        || pattern.contains('?')
        || pattern.contains('[')
        || pattern.contains('/')
        || pattern.starts_with('.')
}

fn insert_opt(target: &mut Value, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        target[key] = json!(value);
    }
}

#[cfg(test)]
#[path = "grep_glob_tests.rs"]
mod grep_glob_tests;
