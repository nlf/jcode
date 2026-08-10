//! The `grep` and `glob` tools.
//!
//! These were adapters onto `agentgrep`. They now use `jcode-search`, ported
//! from omp; agentgrep has since been deleted, along with its `find`, `trace`,
//! and `outline` modes.
//!
//! # Why the schema keeps its Claude-Code shape
//!
//! omp's `grep` takes one `path` carrying a semicolon list, and `case` meaning
//! case-*sensitive*. Ours takes `pattern`/`path`/`glob`/`type`/`-i`, which is
//! the shape models have priors for and the shape the curated OAuth builtins
//! use. omp's capabilities are added on top rather than replacing that: `path`
//! accepts a semicolon list and `file:line` selectors, and `skip` paginates.
//! Nothing that worked before stops working.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use jcode_search::{
    DEFAULT_FILE_LIMIT, SearchError, WalkOptions, find_files, group_by_file, render, render_paths,
    resolve_targets, search_contents, select,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Unknown fields are tolerated rather than rejected: one failed native call is
/// enough to send a model back to bash for the rest of the session.
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
    /// omp's spelling, inverted: `case: true` means case-sensitive.
    case: Option<bool>,
    /// Files to skip, for paging past the file limit.
    skip: Option<usize>,
    #[serde(rename = "-n")]
    _line_numbers: Option<bool>,
    multiline: Option<bool>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GlobInput {
    #[serde(alias = "query")]
    pattern: Option<String>,
    path: Option<String>,
    #[serde(alias = "max_files")]
    head_limit: Option<usize>,
    limit: Option<usize>,
    hidden: Option<bool>,
    gitignore: Option<bool>,
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

fn walk_options(hidden: Option<bool>, gitignore: Option<bool>) -> WalkOptions {
    WalkOptions {
        hidden: hidden.unwrap_or(false),
        respect_gitignore: gitignore.unwrap_or(true),
    }
}

fn search_error(error: SearchError) -> anyhow::Error {
    anyhow::anyhow!(error.message())
}

/// Combine an explicit `path` with a `glob` or `type` filter.
///
/// `glob` and `type` are Claude-Code's way of narrowing, and the ported engine
/// expresses both as path entries. Combining them here keeps one code path in
/// the engine rather than two notions of "which files".
fn combine_scope(
    path: Option<&str>,
    glob: Option<&str>,
    file_type: Option<&str>,
) -> Option<String> {
    let type_glob = file_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("**/*.{value}"));
    let filter = glob
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(type_glob);

    match (path.map(str::trim).filter(|p| !p.is_empty()), filter) {
        (Some(path), Some(filter)) => {
            // A filter is relative to the scope, so join rather than replace:
            // path="src", glob="**/*.rs" means Rust files under src, not every
            // Rust file in the workspace.
            let base = path.trim_end_matches('/');
            Some(format!("{base}/{}", filter.trim_start_matches("./")))
        }
        (Some(path), None) => Some(path.to_string()),
        (None, Some(filter)) => Some(filter),
        (None, None) => None,
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents by regex, ignore-aware and ranked. Not grep or rg in bash."
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
                    "description": "File, directory, glob, or file:lines selector. Several as a semicolon list."
                },
                "glob": {
                    "type": "string",
                    "description": "Only search files matching this glob."
                },
                "type": {
                    "type": "string",
                    "description": "Only search this file type, such as rs or ts."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "content (default) returns matching regions; files_with_matches returns paths only."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of files to return."
                },
                "skip": {
                    "type": "integer",
                    "description": "Files to skip, to page past a previous call's limit."
                },
                "-i": {
                    "type": "boolean",
                    "description": "Case-insensitive search."
                },
                "hidden": {
                    "type": "boolean",
                    "description": "Include hidden files."
                },
                "gitignore": {
                    "type": "boolean",
                    "description": "Respect gitignore. On by default."
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
        let pattern = params
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            // An empty pattern matches every line of every file, which is never
            // what was meant and costs the whole output budget.
            .ok_or_else(|| anyhow::anyhow!("grep requires 'pattern'"))?
            .to_string();

        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let scope = combine_scope(
            params.path.as_deref(),
            params.glob.as_deref(),
            params.file_type.as_deref(),
        );
        let targets = resolve_targets(scope.as_deref(), &root).map_err(search_error)?;

        // `-i` and omp's `case` say the same thing from opposite directions.
        // `-i` wins when both are present, since it is the one models send.
        let case_sensitive = match (params.case_insensitive, params.case) {
            (Some(insensitive), _) => !insensitive,
            (None, Some(sensitive)) => sensitive,
            (None, None) => true,
        };

        let options = walk_options(params.hidden, params.gitignore);
        let matches = tokio::task::spawn_blocking({
            let targets = targets.clone();
            let root = root.clone();
            let options = options.clone();
            move || search_contents(&pattern, &targets, &root, &options, case_sensitive)
        })
        .await??;

        let paths_only = matches!(
            params.output_mode.as_deref(),
            Some("files_with_matches") | Some("count")
        );

        let grouped = group_by_file(matches);
        let single_file = targets.len() == 1 && !targets[0].is_glob && targets[0].path.is_file();
        let selection = select(
            grouped,
            params.skip.unwrap_or(0),
            params.head_limit.unwrap_or(DEFAULT_FILE_LIMIT),
            single_file,
        );

        if paths_only {
            let paths: Vec<String> = selection
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect();
            return Ok(ToolOutput::new(render_paths(&paths, selection.total_files))
                .with_title(format!("grep {}", selection.total_files)));
        }

        // Tags are minted per file so a search result can be edited without a
        // re-read. Only for files whose content we can read: a tag that does
        // not match the file on disk is worse than none.
        let store = super::hashline_store::for_session(&ctx.session_id);
        for file in &selection.files {
            if let Ok(content) = std::fs::read_to_string(root.join(&file.path)) {
                store.record(&file.path, &content, None);
            }
        }
        let tags = |path: &str| store.head(path).map(|snapshot| snapshot.hash);

        let body = render(&selection, &tags);
        Ok(ToolOutput::new(body).with_title(format!(
            "grep {} in {} file{}",
            selection.files.iter().map(|f| f.total).sum::<usize>(),
            selection.total_files,
            if selection.total_files == 1 { "" } else { "s" }
        )))
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by name or pattern, skipping vendored dirs. Not find in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "Glob such as **/*.rs, or bare words to match file names by. Skips ignored and vendored directories."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Omit to search the whole workspace."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of paths to return."
                },
                "hidden": {
                    "type": "boolean",
                    "description": "Include hidden files."
                },
                "gitignore": {
                    "type": "boolean",
                    "description": "Respect gitignore. On by default."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: GlobInput = serde_json::from_value(input)?;
        let pattern = params
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("glob requires 'pattern'"))?
            .to_string();

        let root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // A bare word is not a glob. Models send `three` meaning "files called
        // something like three", so it becomes a substring match rather than a
        // literal path that matches nothing.
        let is_glob = jcode_search::has_glob_chars(&pattern);
        let scope = if is_glob {
            combine_scope(params.path.as_deref(), Some(&pattern), None)
        } else {
            params.path.clone().or_else(|| Some(".".to_string()))
        };

        let targets = resolve_targets(scope.as_deref(), &root).map_err(search_error)?;
        let options = walk_options(params.hidden, params.gitignore);
        let found = tokio::task::spawn_blocking({
            let targets = targets.clone();
            let root = root.clone();
            let options = options.clone();
            move || find_files(&targets, &root, &options)
        })
        .await??;

        let needle = pattern.to_lowercase();
        let mut paths: Vec<String> = found
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .filter(|path| {
                if is_glob {
                    return true;
                }
                // Match on the file name rather than the whole path, so a bare
                // word does not match every file under a directory that happens
                // to contain it.
                path.rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect();

        let total = paths.len();
        let limit = params
            .head_limit
            .or(params.limit)
            .unwrap_or(jcode_search::select::DEFAULT_FILE_LIMIT * 10);
        paths.truncate(limit);

        Ok(
            ToolOutput::new(render_paths(&paths, total)).with_title(format!(
                "glob {total} file{}",
                if total == 1 { "" } else { "s" }
            )),
        )
    }
}

#[cfg(test)]
#[path = "grep_glob_tests.rs"]
mod grep_glob_tests;
