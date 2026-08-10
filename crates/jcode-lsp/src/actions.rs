//! The read-only actions, as a dispatch over a client.
//!
//! Ported from the per-action arms of omp's `tool.ts`. **No tool name appears here**, deliberately,
//! for the same reason as [`crate::results`]: this dispatch is identical whether the actions ship
//! as one tool or two, so only registration depends on that decision.
//!
//! # What this layer is
//!
//! `(client, action, arguments) -> text a model reads`. It owns the LSP method names, the document
//! sync that has to happen before a position request, and the wording of each answer. It does not
//! own process lifetime ([`crate::registry`]), safety tiering, or the tool schema.
//!
//! # Why the document is opened first
//!
//! A position request against a document the server has never seen answers `null`, because the
//! server has no content to resolve the position against. omp opens the file before every position
//! request for exactly this reason. Skipping it produces "no definition found" for a symbol that is
//! plainly there, which reads as the language server being useless.
//!
//! # Writes
//!
//! None of these actions modifies a file, which is what makes them the read-only half. `request` is
//! deliberately **not** here: it can send an arbitrary method, so it cannot be auto-approved and
//! belongs with whatever tool carries the approval gate.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::Client;
use crate::correlation::RequestFailure;
use crate::position::{self, PositionError};
use crate::results::{Locations, hover_text, render_locations};

/// A read-only navigation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Hover,
    Symbols,
}

impl Action {
    /// The action name as a caller writes it.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "definition" => Self::Definition,
            "type_definition" => Self::TypeDefinition,
            "implementation" => Self::Implementation,
            "references" => Self::References,
            "hover" => Self::Hover,
            "symbols" => Self::Symbols,
            _ => return None,
        })
    }

    /// Every action name, for a schema enum and for error messages.
    ///
    /// A single list so the schema and the parser cannot disagree — a schema advertising an action
    /// the parser rejects is the failure mode `~/NLFCODE.md` item 4 names.
    pub const ALL: &'static [&'static str] = &[
        "definition",
        "type_definition",
        "implementation",
        "references",
        "hover",
        "symbols",
    ];

    /// The LSP method this action sends.
    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::Symbols => "textDocument/documentSymbol",
        }
    }

    /// The noun for "Found N x(s)", and for "No x found".
    fn noun(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::TypeDefinition => "type definition",
            Self::Implementation => "implementation",
            Self::References => "reference",
            Self::Hover => "hover",
            Self::Symbols => "symbol",
        }
    }

    /// Whether this action resolves a position within the file.
    ///
    /// `symbols` does not: it asks about the whole document. Sending it a position is harmless but
    /// requiring one would reject a legitimate call, which is why the distinction is here rather
    /// than in the caller.
    fn needs_position(self) -> bool {
        !matches!(self, Self::Symbols)
    }
}

/// What an action was asked to do.
#[derive(Debug, Clone)]
pub struct Request {
    pub action: Action,
    /// The file, already resolved to an absolute path by the caller.
    pub file: std::path::PathBuf,
    /// 1-based, as a person and a model both count lines.
    pub line: Option<usize>,
    /// The symbol on that line, resolved to a column by [`crate::position`].
    pub symbol: Option<String>,
    /// For `references`: whether to include the declaration itself.
    pub include_declaration: bool,
}

/// Why an action could not run.
#[derive(Debug)]
pub enum ActionError {
    /// The request itself is wrong: a missing line, an unresolvable symbol.
    ///
    /// Separated from a transport failure because the fixes are different: this one is the caller's
    /// to correct, and telling a model "the connection died" when it omitted a parameter sends it
    /// to retry rather than to fix the call.
    BadRequest(String),
    /// The file could not be read.
    Unreadable {
        path: std::path::PathBuf,
        detail: String,
    },
    /// The position could not be resolved within the line.
    Position(PositionError),
    /// The server failed to answer.
    Failed(RequestFailure),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(detail) => write!(f, "{detail}"),
            Self::Unreadable { path, detail } => {
                write!(f, "could not read {}: {detail}", path.display())
            }
            Self::Position(error) => write!(f, "{error}"),
            Self::Failed(failure) => write!(f, "{failure}"),
        }
    }
}

impl From<RequestFailure> for ActionError {
    fn from(failure: RequestFailure) -> Self {
        Self::Failed(failure)
    }
}

/// Run one action and render its answer.
///
/// `root` is the project root, used to shorten paths in the output.
pub async fn run(
    client: &Client,
    request: &Request,
    root: &Path,
    timeout: Duration,
) -> Result<String, ActionError> {
    let text = std::fs::read_to_string(&request.file).map_err(|error| ActionError::Unreadable {
        path: request.file.clone(),
        detail: error.to_string(),
    })?;

    let uri = crate::client::path_to_uri(&request.file);
    let language_id = language_id_for(&request.file);

    // Opened before anything else. A position request against a document the server has not seen
    // answers `null`, which is indistinguishable from "nothing there".
    client.open_document(&uri, language_id, &text).await?;

    let mut params = json!({"textDocument": {"uri": uri}});

    if request.action.needs_position() {
        let line = request.line.ok_or_else(|| {
            ActionError::BadRequest(format!(
                "{} needs a line, and a symbol on it",
                request.action.noun()
            ))
        })?;
        let symbol = request.symbol.as_deref().ok_or_else(|| {
            ActionError::BadRequest(format!(
                "{} needs a symbol, so the column can be resolved without the caller counting \
                 characters",
                request.action.noun()
            ))
        })?;

        // `resolve_column` parses the symbol spec itself, including the `name#2` and `$name`
        // forms, so the caller passes the raw string rather than pre-parsing it.
        let column =
            position::resolve_column(&text, line, Some(symbol)).map_err(ActionError::Position)?;

        // LSP is 0-based; the request is 1-based. The conversion happens here, once, rather than in
        // each action.
        params["position"] = json!({"line": line - 1, "character": column});
    }

    if request.action == Action::References {
        params["context"] = json!({"includeDeclaration": request.include_declaration});
    }

    let result = client
        .request(request.action.method(), params, timeout)
        .await?;

    Ok(render(request.action, &result, root, &request.file))
}

/// Render an answer, or say plainly that there was none.
fn render(action: Action, result: &Value, root: &Path, file: &Path) -> String {
    match action {
        Action::Hover => {
            hover_text(result).unwrap_or_else(|| format!("No {} information found", action.noun()))
        }
        Action::Symbols => render_symbols(result, root, file),
        _ => {
            let locations = Locations::from_result(result);
            render_locations(&locations, action.noun(), root)
                .unwrap_or_else(|| format!("No {} found", action.noun()))
        }
    }
}

/// Symbols, in either of the two legal shapes.
///
/// `documentSymbol` may answer with `DocumentSymbol` (nested, carrying `selectionRange` and
/// `children`) or the older flat `SymbolInformation` (carrying `location`). omp distinguishes them
/// by the presence of `selectionRange`, and so does this. A caller handling one shape shows no
/// symbols at all for servers that send the other.
fn render_symbols(result: &Value, root: &Path, file: &Path) -> String {
    let Some(items) = result.as_array().filter(|items| !items.is_empty()) else {
        return "No symbols found".to_string();
    };

    let shown = file
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| file.to_path_buf());
    let mut lines = vec![format!("Symbols in {}:", shown.display())];

    // The nested shape is identified by `selectionRange` on the first element, matching omp. Mixed
    // arrays are not a thing a server sends, and treating each element independently would be more
    // code for a case that cannot arise.
    let nested = items[0].get("selectionRange").is_some();
    for item in items {
        if nested {
            render_nested_symbol(item, 0, &mut lines);
        } else if let Some(line) = flat_symbol_line(item) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn render_nested_symbol(symbol: &Value, depth: usize, out: &mut Vec<String>) {
    let name = symbol.get("name").and_then(Value::as_str).unwrap_or("?");
    let kind = kind_name(symbol.get("kind").and_then(Value::as_i64));
    let line = symbol
        .get("range")
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let detail = symbol
        .get("detail")
        .and_then(Value::as_str)
        .filter(|detail| !detail.is_empty())
        .map(|detail| format!(" {detail}"))
        .unwrap_or_default();

    out.push(format!(
        "{}{kind} {name}{detail} @ line {line}",
        "  ".repeat(depth)
    ));

    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            render_nested_symbol(child, depth + 1, out);
        }
    }
}

fn flat_symbol_line(symbol: &Value) -> Option<String> {
    let name = symbol.get("name").and_then(Value::as_str)?;
    let kind = kind_name(symbol.get("kind").and_then(Value::as_i64));
    let line = symbol
        .get("location")
        .and_then(|location| location.get("range"))
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    Some(format!("{kind} {name} @ line {line}"))
}

/// The `SymbolKind` name, from omp's `symbolKindToName`.
///
/// Names rather than omp's icons: theirs come from a theme (`theme.format.bullet`), and this crate
/// holds no theme and should not learn about one. A name is also more use to a model than a glyph,
/// which it has to guess the meaning of.
fn kind_name(kind: Option<i64>) -> &'static str {
    match kind.unwrap_or(0) {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
}

/// The `languageId` for a file, from its extension.
///
/// Servers use this to choose a parser, and a wrong one produces a document the server cannot make
/// sense of. Unknown extensions fall back to the extension itself, which is what most servers
/// expect for languages not in this list and is better than a fixed guess.
fn language_id_for(path: &Path) -> &str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    match extension {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" => "python",
        "go" => "go",
        "c" => "c",
        "h" | "hpp" | "cc" | "cpp" | "cxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "sh" | "bash" => "shellscript",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        "zig" => "zig",
        "lua" => "lua",
        other => other,
    }
}

#[cfg(test)]
#[path = "actions_tests.rs"]
mod tests;
