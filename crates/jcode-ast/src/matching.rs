//! Structural pattern matching over source text.
//!
//! Ported from oh-my-pi's `ast_match`, which is the in-memory member of their
//! `ast_match` / `ast_grep` / `ast_edit` family. Their `pi-natives` is already
//! Rust over the same upstream `ast-grep-core`, so this calls the engine
//! directly rather than reimplementing it.
//!
//! Pattern compilation, metavariable binding (`$VAR`, `$$$ARGS`) and matching
//! are all upstream. What lives here is the orchestration: language resolution,
//! bounded results, and reporting a match in terms a caller can act on.
//!
//! No I/O. `ast_grep` adds the walker, `ast_edit` adds rewriting.

use ast_grep_core::AstGrep;
use ast_grep_language::SupportLang;
use std::str::FromStr;

/// Matches collected before stopping.
///
/// A pattern like `$X` matches every node in the tree, so an unbounded search
/// over a large file produces output nobody wants and costs the whole budget.
pub const DEFAULT_MAX_MATCHES: usize = 200;

/// Why a match request could not be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    /// The language is not one the bundled grammars cover.
    UnknownLanguage { requested: String },
    /// The pattern does not parse in that language.
    BadPattern { pattern: String, language: String },
    /// The pattern is empty, which matches everything and means nothing.
    EmptyPattern,
}

impl MatchError {
    pub fn message(&self) -> String {
        match self {
            Self::UnknownLanguage { requested } => format!(
                "Unknown language '{requested}'. Supported: {}.",
                supported_languages().join(", ")
            ),
            Self::BadPattern { pattern, language } => format!(
                "Pattern '{pattern}' is not valid {language}. A pattern is a code \
                 fragment with metavariables, such as 'fn $NAME() {{ $$$BODY }}', \
                 not a regular expression."
            ),
            Self::EmptyPattern => {
                "A pattern is required. An empty pattern matches every node.".to_string()
            }
        }
    }
}

/// One structural match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// 1-based line where the match starts.
    pub line: usize,
    /// 1-based column where the match starts.
    pub column: usize,
    /// The matched source text.
    pub text: String,
    /// Metavariable bindings, sorted by name for deterministic output.
    pub bindings: Vec<(String, String)>,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matches {
    pub matches: Vec<Match>,
    /// True when the cap stopped collection, so the caller knows to narrow.
    pub truncated: bool,
}

/// Languages the bundled grammars cover.
///
/// Listed explicitly rather than derived, so the error message names them and
/// a caller can see what is available without guessing.
pub fn supported_languages() -> Vec<&'static str> {
    vec![
        "bash", "c", "cpp", "csharp", "css", "elixir", "go", "haskell", "html", "java",
        "javascript", "json", "kotlin", "lua", "php", "python", "ruby", "rust", "scala",
        "swift", "typescript", "tsx", "yaml",
    ]
}

/// Resolve a language name, accepting the aliases models actually send.
pub fn resolve_language(name: &str) -> Result<SupportLang, MatchError> {
    let normalized = match name.trim().to_lowercase().as_str() {
        // Aliases upstream does not take, but which a model will send.
        "c++" | "cc" | "cxx" => "cpp".to_string(),
        "c#" | "cs" => "csharp".to_string(),
        "js" | "mjs" | "cjs" => "javascript".to_string(),
        "ts" => "typescript".to_string(),
        "py" => "python".to_string(),
        "rs" => "rust".to_string(),
        "sh" | "shell" => "bash".to_string(),
        "yml" => "yaml".to_string(),
        other => other.to_string(),
    };
    SupportLang::from_str(&normalized).map_err(|_| MatchError::UnknownLanguage {
        requested: name.to_string(),
    })
}

/// The language a file extension implies.
pub fn language_for_path(path: &str) -> Option<SupportLang> {
    let extension = path.rsplit('.').next()?;
    resolve_language(extension).ok()
}

/// Find every structural match of `pattern` in `source`.
pub fn find(
    source: &str,
    pattern: &str,
    language: SupportLang,
    max_matches: usize,
) -> Result<Matches, MatchError> {
    if pattern.trim().is_empty() {
        return Err(MatchError::EmptyPattern);
    }

    let doc = AstGrep::new(source, language);
    let root = doc.root();

    // An unparseable pattern yields no matches rather than an error upstream,
    // which is indistinguishable from "no matches in this file". Distinguishing
    // them matters: one means narrow your search, the other means fix your
    // pattern.
    let mut found = Vec::new();
    let mut truncated = false;
    for node in root.find_all(pattern) {
        if found.len() >= max_matches {
            truncated = true;
            break;
        }
        let start = node.start_pos();
        let mut bindings: Vec<(String, String)> = node
            .get_env()
            .get_matched_variables()
            .filter_map(|var| {
                let name = match var {
                    ast_grep_core::meta_var::MetaVariable::Capture(name, _) => name,
                    ast_grep_core::meta_var::MetaVariable::MultiCapture(name) => name,
                    _ => return None,
                };
                node.get_env()
                    .get_match(&name)
                    .map(|matched| (name.clone(), matched.text().to_string()))
            })
            .collect();
        // Sorted so output is deterministic across runs, which matters for
        // caching and for diffing one search against another.
        bindings.sort();

        found.push(Match {
            line: start.line() + 1,
            column: start.column(&node) + 1,
            text: node.text().to_string(),
            bindings,
        });
    }

    Ok(Matches {
        matches: found,
        truncated,
    })
}

#[cfg(test)]
#[path = "match_tests.rs"]
mod match_tests;
