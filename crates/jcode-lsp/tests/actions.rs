//! Action tests end to end, against a real server process.
//!
//! The unit tests in `src/actions_tests.rs` cover rendering and argument handling. These cover the
//! parts only a real exchange can: that the document is synced before a position request, that the
//! position sent is the one the symbol resolves to, and that a server's own error reaches the
//! caller intact.

use std::time::Duration;

use serde_json::{Value, json};

use jcode_lsp::actions::{self, Action, ActionError, Request};
use jcode_lsp::client::{Client, ServerSpec};

/// A project directory with one file in it.
fn project(name: &str, contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join(name);
    std::fs::write(&file, contents).expect("write");
    (dir, file)
}

/// A client whose server answers the given methods with the given results.
async fn client_answering(root: &std::path::Path, answers: Value) -> Client {
    Client::start(
        ServerSpec {
            name: "fake".to_string(),
            program: env!("CARGO_BIN_EXE_fake_lsp_server").to_string(),
            args: Vec::new(),
            root: root.to_path_buf(),
            env: vec![("FAKE_LSP_ANSWER".to_string(), answers.to_string())],
            settings: json!({}),
            init_options: json!({}),
        },
        Duration::from_secs(5),
    )
    .await
    .expect("the fake server must start")
}

fn timeout() -> Duration {
    Duration::from_secs(5)
}

/// **The document is opened before a position request.**
///
/// A position request against a document the server has never seen answers `null`, because there is
/// no content to resolve the position against. That renders as "No definition found" for a symbol
/// plainly present, which reads as the language server being useless rather than as a client bug.
///
/// Asserted through the server's own record of what it received, not through the answer: the answer
/// here is canned, so only the fixture's bookkeeping can show the `didOpen` happened.
#[tokio::test]
async fn a_position_request_syncs_the_document_first() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    let client = client_answering(
        dir.path(),
        json!({"textDocument/definition": [{
            "uri": "file:///x/helper.rs",
            "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 10}}
        }]}),
    )
    .await;

    let request = Request {
        action: Action::Definition,
        file: file.clone(),
        line: Some(2),
        symbol: Some("helper".to_string()),
        include_declaration: false,
    };
    let output = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect("the action must succeed");
    assert!(output.starts_with("Found 1 definition(s):"), "{output}");

    // The server's own record: it saw the file's contents before the request.
    let state = client
        .request("test/state", json!({}), timeout())
        .await
        .expect("state");
    let opened = state["didOpen"]
        .as_object()
        .expect("the fixture records didOpen per uri");
    assert_eq!(
        opened.len(),
        1,
        "the document was not opened before the position request: {state}"
    );
    assert!(
        opened.keys().next().expect("one uri").ends_with("main.rs"),
        "the wrong document was opened: {state}"
    );
}

/// **The position sent is the one the symbol resolves to.**
///
/// The whole reason the tool takes a symbol rather than a column: a model cannot count characters
/// reliably. If the resolved column were wrong the server would answer about the wrong token, and
/// the result would look like a plausible answer to a different question.
///
/// `helper` starts at character 4 of line 2, and LSP is 0-based, so the request must carry
/// `{line: 1, character: 4}`. Read back from the fixture's record of the request it received.
#[tokio::test]
async fn the_resolved_column_is_what_reaches_the_server() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    let client = client_answering(dir.path(), json!({"textDocument/definition": []})).await;

    let request = Request {
        action: Action::Definition,
        file,
        line: Some(2),
        symbol: Some("helper".to_string()),
        include_declaration: false,
    };
    let _ = actions::run(&client, &request, dir.path(), timeout()).await;

    let state = client
        .request("test/state", json!({}), timeout())
        .await
        .expect("state");
    let position = state["lastPosition"].clone();
    assert_eq!(
        position,
        json!({"line": 1, "character": 4}),
        "the symbol resolved to the wrong position, so the server answered about another token"
    );
}

/// A missing symbol is refused before anything is sent.
///
/// `resolve_column` would fall back to the first non-whitespace character, which for `definition`
/// means answering confidently about whatever happens to start the line. omp's own comment on that
/// fallback says callers who must not guess should refuse, and this is that refusal.
#[tokio::test]
async fn a_position_action_without_a_symbol_is_refused() {
    let (dir, file) = project("main.rs", "fn main() {}\n");
    let client = client_answering(dir.path(), json!({})).await;

    let request = Request {
        action: Action::Definition,
        file,
        line: Some(1),
        symbol: None,
        include_declaration: false,
    };
    let error = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect_err("a definition without a symbol must be refused");

    assert!(
        matches!(error, ActionError::BadRequest(_)),
        "expected a bad request rather than a guess: {error}"
    );
    assert!(error.to_string().contains("symbol"), "{error}");
}

/// A symbol that is not on the line is an error naming the line.
///
/// Rather than a silent fallback: the caller has the wrong line or the wrong symbol, and either way
/// guessing produces an answer about the wrong token.
#[tokio::test]
async fn a_symbol_absent_from_the_line_is_an_error() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    let client = client_answering(dir.path(), json!({})).await;

    let request = Request {
        action: Action::Definition,
        file,
        line: Some(1),
        symbol: Some("helper".to_string()),
        include_declaration: false,
    };
    let error = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect_err("the symbol is on line 2, not line 1");
    assert!(
        matches!(error, ActionError::Position(_)),
        "expected a position error: {error}"
    );
}

/// `symbols` needs no position, and asks about the whole document.
#[tokio::test]
async fn the_document_wide_action_needs_no_position() {
    let (dir, file) = project("main.rs", "fn main() {}\n");
    let client = client_answering(
        dir.path(),
        json!({"textDocument/documentSymbol": [{
            "name": "main",
            "kind": 12,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 12}},
            "selectionRange": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}
        }]}),
    )
    .await;

    let request = Request {
        action: Action::Symbols,
        file,
        line: None,
        symbol: None,
        include_declaration: false,
    };
    let output = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect("symbols needs no position");
    assert!(output.contains("Function main @ line 1"), "{output}");
}

/// `references` sends `includeDeclaration`, and it reaches the server.
///
/// A caller asking to exclude the declaration and receiving it anyway would conclude the flag does
/// nothing, which is the sort of thing that gets worked around rather than reported.
#[tokio::test]
async fn references_forwards_the_declaration_flag() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    let client = client_answering(dir.path(), json!({"textDocument/references": []})).await;

    for include in [true, false] {
        let request = Request {
            action: Action::References,
            file: file.clone(),
            line: Some(2),
            symbol: Some("helper".to_string()),
            include_declaration: include,
        };
        let _ = actions::run(&client, &request, dir.path(), timeout()).await;

        let state = client
            .request("test/state", json!({}), timeout())
            .await
            .expect("state");
        assert_eq!(
            state["lastContext"]["includeDeclaration"],
            json!(include),
            "includeDeclaration={include} did not reach the server"
        );
    }
}

/// **A server that does not implement an action says so, and it is not a transport failure.**
///
/// `-32601` means the server is healthy and lacks the capability. Reporting it as a connection
/// problem would make a caller restart a working server, and several of omp's regressions are about
/// exactly that distinction.
#[tokio::test]
async fn an_unimplemented_action_reports_the_servers_own_error() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    // No canned answer for typeDefinition, so the fixture returns -32601.
    let client = client_answering(dir.path(), json!({})).await;

    let request = Request {
        action: Action::TypeDefinition,
        file,
        line: Some(2),
        symbol: Some("helper".to_string()),
        include_declaration: false,
    };
    let error = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect_err("the fixture does not implement typeDefinition");

    match error {
        ActionError::Failed(failure) => {
            assert!(
                failure.is_method_not_found(),
                "a healthy server lacking a method must not read as a transport failure: \
                 {failure}"
            );
        }
        other => panic!("expected a server error, got {other}"),
    }
}

/// A file that cannot be read is reported as such, before the server is troubled.
#[tokio::test]
async fn an_unreadable_file_is_reported_without_asking_the_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = client_answering(dir.path(), json!({})).await;

    let request = Request {
        action: Action::Symbols,
        file: dir.path().join("does-not-exist.rs"),
        line: None,
        symbol: None,
        include_declaration: false,
    };
    let error = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect_err("a missing file cannot be analysed");
    assert!(
        matches!(error, ActionError::Unreadable { .. }),
        "expected an unreadable-file error: {error}"
    );

    // And the server was never asked about it.
    let state = client
        .request("test/state", json!({}), timeout())
        .await
        .expect("state");
    assert!(
        state["didOpen"]
            .as_object()
            .map(|opened| opened.is_empty())
            .unwrap_or(true),
        "a missing file was opened against the server: {state}"
    );
}

/// Hover renders the server's text rather than a location list.
#[tokio::test]
async fn hover_renders_its_text() {
    let (dir, file) = project("main.rs", "fn main() {\n    helper();\n}\n");
    let client = client_answering(
        dir.path(),
        json!({"textDocument/hover": {"contents": {"kind": "markdown", "value": "fn helper()"}}}),
    )
    .await;

    let request = Request {
        action: Action::Hover,
        file,
        line: Some(2),
        symbol: Some("helper".to_string()),
        include_declaration: false,
    };
    let output = actions::run(&client, &request, dir.path(), timeout())
        .await
        .expect("hover");
    assert_eq!(output, "fn helper()");
}
