//! The `lsp` tool: semantic code navigation through a language server.
//!
//! The adapter over `jcode-lsp`. Everything hard lives in that crate — process lifecycle, framing,
//! request correlation, freshness, the client registry — and this is the translation between a tool
//! call and [`jcode_lsp::actions`].
//!
//! # What this fills that nothing else does
//!
//! `grep` and `ast_grep` match text and syntax. Neither resolves a *name* to its meaning, so
//! neither can answer "where is this defined" for a symbol that appears under the same spelling in
//! twenty places, or "what calls this" without also matching a comment. That is the gap this closes,
//! and it is the one capability `~/NLFCODE.md` names as genuinely missing.
//!
//! # Read-only, and why that decides the safety tier
//!
//! Every action here answers a question. None writes a file, none runs a build, and the worst a
//! wrong call can do is spend a language server's time. So it belongs in `AUTO_ALLOWED` alongside
//! `grep` and `ast_grep`: prompting for a `hover` would make the tool unusable, since navigation is
//! something a model does dozens of times in a turn.
//!
//! **`request` is deliberately not here.** It can send an arbitrary LSP method, including ones that
//! mutate, so it cannot be auto-approved and belongs on a separate approval-gated tool. That split
//! follows `ast_grep`/`ast_edit`, and it is why this file registers one read-only tool rather than
//! one tool with a dangerous corner.

use std::sync::Arc;
use std::time::Duration;

use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use jcode_lsp::Registry;
use jcode_lsp::actions::{self, Action, ActionError, Request};
use jcode_lsp::config;
use serde::Deserialize;
use serde_json::{Value, json};

/// How long to wait for one answer.
///
/// Generous because a cold `rust-analyzer` indexing a large workspace genuinely takes this long, and
/// a timeout here reads to a model as "the symbol does not exist" — the same failure as a wrong
/// answer, but slower. omp allows up to 300s for the same reason.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Unknown fields are tolerated rather than rejected, matching the other tools: one failed native
/// call sends a model back to bash for the rest of the session.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LspInput {
    action: Option<String>,
    file: Option<String>,
    line: Option<usize>,
    symbol: Option<String>,
    include_declaration: Option<bool>,
}

/// Semantic navigation over a language server.
pub struct LspTool {
    /// Shared across calls, so a cold start is paid once per project rather than per call.
    ///
    /// The whole value of the client depends on outliving a single tool call: a cold
    /// `rust-analyzer` takes tens of seconds, so a client per call would time out every time.
    registry: Arc<Registry>,
}

impl LspTool {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Registry::new()),
        }
    }
}

impl Default for LspTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        // Under the 20-token cap, which an existing test enforces -- my first version was 26 and
        // it caught it. The detail belongs on the properties, where it is read at the point of use.
        "Find a symbol's definition or uses, by meaning not text."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    // Straight from `Action::ALL`, so the schema cannot advertise an action the
                    // dispatch rejects.
                    "enum": Action::ALL,
                    // Also capped, at 25 tokens per parameter. The enum itself carries the
                    // names, so this only has to say what distinguishes them.
                    "description": "What to ask: where declared, what uses it, type and docs, \
                                    or a file outline."
                },
                "file": {
                    "type": "string",
                    "description": "The file to ask about."
                },
                "line": {
                    "type": "number",
                    "description": "1-based line holding the symbol. Not needed for `symbols`."
                },
                "symbol": {
                    "type": "string",
                    "description": "The name on that line, so the column is resolved for you. \
                                    `name#2` picks the second occurrence."
                },
                "include_declaration": {
                    "type": "boolean",
                    "description": "For `references`: include the declaration itself. Default true."
                }
            },
            "required": ["action", "file"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: LspInput = serde_json::from_value(input)?;

        let action_name = params
            .action
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("lsp requires 'action'"))?;
        let action = Action::parse(action_name).ok_or_else(|| {
            // Lists the alternatives rather than only rejecting: a model given "invalid action"
            // guesses again, and a model given the list picks from it.
            anyhow::anyhow!(
                "unknown lsp action {action_name:?}; expected one of {}",
                Action::ALL.join(", ")
            )
        })?;

        let file = params
            .file
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("lsp requires 'file'"))?;
        let file = ctx.resolve_path(std::path::Path::new(file));

        let session_root = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // **The root is the project containing the file, not the session's directory.**
        //
        // A language server's whole model is scoped to a root, and root markers live at the project
        // root -- so detecting from the session directory finds nothing whenever the file is
        // somewhere else. Caught by running the tool through the real binary rather than the library:
        // asking about `/tmp/lsp-accept/main.c` from a session rooted in the jcode checkout reported
        // "No language server handles main.c in this project", which is both a failure and a wrong
        // explanation -- clangd handles `.c` and was installed.
        //
        // The library tests all passed the project as the root directly, so none of them could see
        // this. It is the difference between testing a function and testing a tool.
        let defaults = config::defaults();
        let root = project_root_for(&file, &session_root, &defaults);

        // Which servers apply here, and which of them is installed. `detect` walks the filesystem
        // and spawns nothing.
        let (available, unavailable) = config::detect(&defaults, &root, None);
        let candidates = config::servers_for_file(&available, &file);
        let Some(server) = candidates.first() else {
            return Ok(ToolOutput::new(no_server_message(
                &file,
                &unavailable,
                &root,
            )));
        };

        let client = self
            .registry
            .get_or_start(&root, server, REQUEST_TIMEOUT)
            .await
            .map_err(|failure| anyhow::anyhow!("could not start {}: {failure}", server.name))?;

        let request = Request {
            action,
            file,
            line: params.line,
            symbol: params.symbol,
            // Defaults to true, matching omp: a caller asking for references usually wants the
            // declaration among them, and excluding it silently would look like a missing result.
            include_declaration: params.include_declaration.unwrap_or(true),
        };

        match actions::run(&client, &request, &root, REQUEST_TIMEOUT).await {
            Ok(output) => Ok(ToolOutput::new(output)),
            // A bad request is the caller's to fix, so it is an error rather than output: an error
            // tells a model to change the call, where text tells it to read the answer.
            Err(error @ (ActionError::BadRequest(_) | ActionError::Position(_))) => {
                Err(anyhow::anyhow!("{error}"))
            }
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }
}

/// The project root for a file: the nearest ancestor carrying a root marker.
///
/// Falls back to the session directory when nothing matches, which keeps the previous behaviour for
/// a file inside the session's own project and is the only sensible guess otherwise.
///
/// Only the markers of servers that handle *this* file are considered. Using every marker would stop
/// at the first ancestor with a `.git` or a `package.json`, which for a Rust file in a monorepo is
/// the wrong tree — the crate root is what `rust-analyzer` needs.
fn project_root_for(
    file: &std::path::Path,
    session_root: &std::path::Path,
    defaults: &config::Config,
) -> std::path::PathBuf {
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dotted = format!(".{extension}");
    let basename = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let markers: Vec<String> = defaults
        .servers
        .values()
        .filter(|server| {
            server.file_types.iter().any(|file_type| {
                let declared = file_type.to_ascii_lowercase();
                let bare = declared.strip_prefix('.').unwrap_or(&declared).to_string();
                (!extension.is_empty() && (declared == dotted || bare == extension))
                    || declared == basename
                    || bare == basename
            })
        })
        .flat_map(|server| server.root_markers.iter().cloned())
        .collect();

    if !markers.is_empty() {
        let mut directory = file.parent();
        while let Some(candidate) = directory {
            if config::has_root_markers(candidate, &markers) {
                return candidate.to_path_buf();
            }
            directory = candidate.parent();
        }
    }

    session_root.to_path_buf()
}

/// Why no server handled this file, in terms the reader can act on.
///
/// Three different situations, three different actions, and collapsing them into "no server" makes
/// all three undebuggable. This is the `status`-style distinction [`config::detect`] preserves,
/// surfaced where a caller meets it.
fn no_server_message(
    file: &std::path::Path,
    unavailable: &std::collections::BTreeMap<String, config::Unavailable>,
    root: &std::path::Path,
) -> String {
    let shown = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();

    // A server that handles this extension but is not installed is the actionable case, so it is
    // named first and specifically.
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dotted = format!(".{extension}");

    let defaults = config::defaults();
    let missing: Vec<&String> = defaults
        .servers
        .iter()
        .filter(|(name, server)| {
            server
                .file_types
                .iter()
                .any(|file_type| file_type.eq_ignore_ascii_case(&dotted))
                && matches!(
                    unavailable.get(*name),
                    Some(config::Unavailable::BinaryNotFound { .. })
                )
        })
        .map(|(name, _)| name)
        .collect();

    if !missing.is_empty() {
        return format!(
            "No language server running for {shown}. This project looks like it needs one of \
             these, and none is installed: {}. Install one, or use grep or ast_grep instead.",
            missing
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    format!(
        "No language server handles {shown} in this project. Nothing here matches its file type, \
         so grep or ast_grep is the tool for it."
    )
}

#[cfg(test)]
#[path = "lsp_tool_tests.rs"]
mod tests;
