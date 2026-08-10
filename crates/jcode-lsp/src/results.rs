//! Turning a server's answer into something a model can act on.
//!
//! Ported from omp's `normalizeLocationResult`, `formatLocation` and the per-action output
//! shaping in `tool.ts`. **No tool name appears here**, deliberately: this layer is the same
//! whether the actions ship as one tool or two, which is why it could be built while that
//! question was still open.
//!
//! # Why normalizing is not optional
//!
//! `textDocument/definition` may answer with a `Location`, an array of them, a `LocationLink`, an
//! array of those, or `null` — five shapes for one question, all legal. A caller that handles
//! only the array-of-`Location` case works against `gopls` and returns "no definition found"
//! against `rust-analyzer`, which is worse than an error because it looks like an answer.
//!
//! [`Locations::from_result`] collapses all five.
//!
//! # `LocationLink` and the two ranges
//!
//! A `LocationLink` carries `targetRange` (the whole symbol, e.g. a function including its body)
//! and `targetSelectionRange` (just the name). omp prefers the selection range, and so does this:
//! jumping to a definition should land on the name, not on the first line of a 200-line function.
//! Falling back to `targetRange` when the selection is absent, since it is optional.

use serde_json::Value;

/// One place in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The URI as the server spelled it.
    pub uri: String,
    /// 0-based, as LSP sends it. Converted to 1-based only at the point of display, so nothing
    /// downstream has to remember which convention it is holding.
    pub line: i64,
    pub character: i64,
}

/// Locations from a navigation request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locations(pub Vec<Location>);

impl Locations {
    /// Collapse any of the five legal shapes into a list.
    ///
    /// `null`, a single `Location`, an array of them, a `LocationLink`, or an array of links. A
    /// caller handling only one shape silently reports "not found" against half the servers in
    /// `defaults.json`.
    ///
    /// Unrecognised entries are skipped rather than failing the request: a server sending one odd
    /// element among five good ones should produce four answers, not an error. That matches omp's
    /// `flatMap` returning `[]` for anything it does not recognise.
    pub fn from_result(result: &Value) -> Self {
        let entries: Vec<&Value> = match result {
            Value::Null => Vec::new(),
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        };

        Self(entries.iter().filter_map(|entry| location_of(entry)).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// One entry, as either a `Location` or a `LocationLink`.
fn location_of(entry: &Value) -> Option<Location> {
    // A `Location` has `uri`; a `LocationLink` has `targetUri`. Checked in that order, matching
    // omp, so a server sending both keys is read as a `Location`.
    if let Some(uri) = entry.get("uri").and_then(Value::as_str) {
        let start = entry.get("range").and_then(|range| range.get("start"));
        return Some(Location {
            uri: uri.to_string(),
            line: start
                .and_then(|start| start.get("line"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            character: start
                .and_then(|start| start.get("character"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
        });
    }

    let target = entry.get("targetUri").and_then(Value::as_str)?;
    // The selection range is the name; the target range is the whole symbol. Preferring the
    // selection means a jump lands on the identifier rather than on the first line of a long
    // function body.
    let range = entry
        .get("targetSelectionRange")
        .or_else(|| entry.get("targetRange"));
    let start = range.and_then(|range| range.get("start"));
    Some(Location {
        uri: target.to_string(),
        line: start
            .and_then(|start| start.get("line"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        character: start
            .and_then(|start| start.get("character"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// `path:line:col`, with the path relative to the project when it is inside it.
///
/// 1-based, like every other position a person reads. A relative path when possible because an
/// absolute one is mostly noise repeated on every line, and omp does the same.
pub fn format_location(location: &Location, root: &std::path::Path) -> String {
    let path = uri_to_path(&location.uri);
    let shown = path
        .strip_prefix(root)
        .map(|relative| relative.to_path_buf())
        .unwrap_or(path);
    format!(
        "{}:{}:{}",
        shown.display(),
        location.line + 1,
        location.character + 1
    )
}

/// A path from a `file://` URI, tolerating a lax server that sent a bare path.
///
/// Percent-decoded, because we encode when sending and a server that echoes our own URI back
/// returns it encoded. Without decoding, a project containing a space would show
/// `my%20project/src/main.rs` on every line.
pub fn uri_to_path(uri: &str) -> std::path::PathBuf {
    let Some(rest) = uri.strip_prefix("file://") else {
        // Not a file URI. omp returns the input unchanged here rather than failing, since a lax
        // server sending a plain path is more likely than a genuinely non-file location.
        return std::path::PathBuf::from(uri);
    };
    std::path::PathBuf::from(percent_decode(rest))
}

/// Decode `%XX` escapes, leaving anything malformed alone.
///
/// A stray `%` is a server bug, and a path that keeps it is more use than an error: the reader can
/// still see which file was meant.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    // Lossy rather than an error: a decoded path that is not valid UTF-8 is still worth showing.
    String::from_utf8_lossy(&out).into_owned()
}

/// The output for a navigation action.
///
/// `None` when nothing was found, which the caller reports as omp does: `"No definition found"`
/// rather than an empty list. A model reading an empty result cannot tell it from a failure.
pub fn render_locations(
    locations: &Locations,
    noun: &str,
    root: &std::path::Path,
) -> Option<String> {
    if locations.is_empty() {
        return None;
    }
    let mut out = format!("Found {} {noun}(s):", locations.len());
    for location in &locations.0 {
        out.push_str("\n  ");
        out.push_str(&format_location(location, root));
    }
    Some(out)
}

/// Hover text, flattened from the three shapes LSP allows.
///
/// `contents` may be a string, a `MarkupContent` object, or an array of either — and the array
/// form is deprecated but still emitted, notably by older `gopls`. Returning `None` for an
/// unrecognised shape would report "no hover information" for a server that answered.
pub fn hover_text(result: &Value) -> Option<String> {
    let contents = result.get("contents")?;
    let text = flatten_markup(contents);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn flatten_markup(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        // `MarkupContent { kind, value }`, or the deprecated `MarkedString { language, value }`.
        // Both carry the text in `value`, which is why one arm covers them.
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .map(flatten_markup)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
