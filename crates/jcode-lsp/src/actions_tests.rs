//! Action-dispatch tests.
//!
//! Pure rendering and argument handling here; the end-to-end path against a real server is in
//! `tests/actions.rs`, which is where document sync can actually be observed.

use super::*;
use serde_json::json;

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from("/project")
}

/// Every advertised action parses.
///
/// The schema enum and the parser come from one list, so a schema advertising an action the parser
/// rejects is impossible. That mismatch -- a tool describing capabilities it does not have -- is
/// `~/NLFCODE.md` item 4, and this is the test that keeps the two honest.
#[test]
fn every_advertised_action_parses() {
    for name in Action::ALL {
        assert!(
            Action::parse(name).is_some(),
            "{name} is advertised in the schema and rejected by the parser"
        );
    }
    assert_eq!(Action::parse("rename"), None, "rename is a v2 write action");
    assert_eq!(Action::parse(""), None);
    assert_eq!(Action::parse("Definition"), None, "names are exact");
}

/// Only `symbols` works without a position.
///
/// A whole-document question needs no position, and requiring one would reject a legitimate call.
/// The rest need one, and answering `null` without saying why is the failure this prevents.
#[test]
fn only_the_document_wide_action_works_without_a_position() {
    assert!(!Action::Symbols.needs_position());
    for action in [
        Action::Definition,
        Action::TypeDefinition,
        Action::Implementation,
        Action::References,
        Action::Hover,
    ] {
        assert!(action.needs_position(), "{action:?} resolves a position");
    }
}

/// Each action sends the method LSP defines for it.
///
/// Transcribed from omp's per-action arms. A wrong method name produces `-32601` from a healthy
/// server, which the client correctly reports as a server error -- so the mistake would look like
/// the server lacking a capability rather than like our typo.
#[test]
fn each_action_sends_its_own_method() {
    assert_eq!(Action::Definition.method(), "textDocument/definition");
    assert_eq!(
        Action::TypeDefinition.method(),
        "textDocument/typeDefinition"
    );
    assert_eq!(
        Action::Implementation.method(),
        "textDocument/implementation"
    );
    assert_eq!(Action::References.method(), "textDocument/references");
    assert_eq!(Action::Hover.method(), "textDocument/hover");
    assert_eq!(Action::Symbols.method(), "textDocument/documentSymbol");
}

/// **Nothing found says so, rather than rendering as empty.**
///
/// omp's wording. A model shown blank output cannot tell "no references exist" from "the call
/// failed", and will usually retry the identical call.
#[test]
fn an_empty_answer_is_reported_in_words() {
    let file = std::path::PathBuf::from("/project/a.rs");
    assert_eq!(
        render(Action::Definition, &Value::Null, &root(), &file),
        "No definition found"
    );
    assert_eq!(
        render(Action::References, &json!([]), &root(), &file),
        "No reference found"
    );
    assert_eq!(
        render(Action::Hover, &json!({}), &root(), &file),
        "No hover information found"
    );
    assert_eq!(
        render(Action::Symbols, &json!([]), &root(), &file),
        "No symbols found"
    );
}

/// **Both symbol shapes render.**
///
/// `documentSymbol` answers with nested `DocumentSymbol`s (carrying `selectionRange` and
/// `children`) or the older flat `SymbolInformation` (carrying `location`). Handling one shape shows
/// no symbols at all for servers that send the other, and both are current: rust-analyzer sends
/// nested, some others still send flat.
#[test]
fn both_symbol_shapes_render() {
    let file = std::path::PathBuf::from("/project/src/a.rs");

    // Nested, with a child.
    let nested = json!([{
        "name": "Thing",
        "kind": 23,
        "range": {"start": {"line": 4, "character": 0}, "end": {"line": 9, "character": 1}},
        "selectionRange": {"start": {"line": 4, "character": 7}, "end": {"line": 4, "character": 12}},
        "children": [{
            "name": "field",
            "kind": 8,
            "range": {"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 20}},
            "selectionRange": {"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 9}}
        }]
    }]);
    assert_eq!(
        render(Action::Symbols, &nested, &root(), &file),
        "Symbols in src/a.rs:\nStruct Thing @ line 5\n  Field field @ line 6"
    );

    // Flat, where the position lives under `location`.
    let flat = json!([{
        "name": "helper",
        "kind": 12,
        "location": {
            "uri": "file:///project/src/a.rs",
            "range": {"start": {"line": 41, "character": 0}, "end": {"line": 41, "character": 10}}
        }
    }]);
    assert_eq!(
        render(Action::Symbols, &flat, &root(), &file),
        "Symbols in src/a.rs:\nFunction helper @ line 42"
    );
}

/// A symbol's `detail` is shown when the server sends one.
///
/// rust-analyzer puts the signature there, which is most of the value of asking.
#[test]
fn a_symbol_detail_is_included() {
    let file = std::path::PathBuf::from("/project/a.rs");
    let with_detail = json!([{
        "name": "main",
        "kind": 12,
        "detail": "fn()",
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 1}},
        "selectionRange": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}
    }]);
    assert_eq!(
        render(Action::Symbols, &with_detail, &root(), &file),
        "Symbols in a.rs:\nFunction main fn() @ line 1"
    );
}

/// An unknown symbol kind renders rather than being dropped.
///
/// A server sending a kind outside 1..=26 has still told us about a symbol, and hiding it loses
/// information for the sake of tidiness.
#[test]
fn an_unknown_symbol_kind_still_renders() {
    assert_eq!(kind_name(Some(99)), "Unknown");
    assert_eq!(kind_name(None), "Unknown");
    assert_eq!(kind_name(Some(5)), "Class");
}

/// Locations render with their positions 1-based and paths relative.
#[test]
fn locations_render_relative_and_one_based() {
    let file = std::path::PathBuf::from("/project/a.rs");
    let result = json!([{
        "uri": "file:///project/src/lib.rs",
        "range": {"start": {"line": 9, "character": 3}, "end": {"line": 9, "character": 8}}
    }]);
    assert_eq!(
        render(Action::Definition, &result, &root(), &file),
        "Found 1 definition(s):\n  src/lib.rs:10:4"
    );
}

/// **A `languageId` is chosen from the extension, and an unknown one falls through.**
///
/// Servers pick a parser from this. A wrong value produces a document the server cannot make sense
/// of, and the resulting silence looks like the file having no symbols.
///
/// The fallback is the extension itself rather than a fixed guess: most servers for languages
/// outside this list expect exactly that, and a fixed `"plaintext"` would be wrong for all of them.
#[test]
fn a_language_id_is_derived_from_the_extension() {
    assert_eq!(language_id_for(std::path::Path::new("a.rs")), "rust");
    assert_eq!(
        language_id_for(std::path::Path::new("a.tsx")),
        "typescriptreact"
    );
    assert_eq!(language_id_for(std::path::Path::new("a.mjs")), "javascript");
    assert_eq!(language_id_for(std::path::Path::new("a.hpp")), "cpp");
    // Unknown: the extension itself.
    assert_eq!(language_id_for(std::path::Path::new("a.zz")), "zz");
    // No extension at all.
    assert_eq!(language_id_for(std::path::Path::new("Makefile")), "");
}

/// A bad request is distinguishable from a transport failure.
///
/// The fixes differ: one is the caller's to correct, the other is not. Telling a model "the
/// connection died" when it omitted a parameter sends it to retry instead of to fix the call.
#[test]
fn a_bad_request_reads_as_the_callers_mistake() {
    let error = ActionError::BadRequest("definition needs a line, and a symbol on it".to_string());
    let text = error.to_string();
    assert!(text.contains("needs a line"), "{text}");
    assert!(
        !text.contains("connection"),
        "a missing parameter must not read as a transport problem: {text}"
    );
}
