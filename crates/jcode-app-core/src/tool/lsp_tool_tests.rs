//! `lsp` tool tests.
//!
//! The protocol and action behaviour is tested in `jcode-lsp` against a real server process. These
//! cover the adapter: schema honesty, argument validation, and the "no server" messages, which are
//! the parts a caller meets first and the parts that would otherwise be untested prose.

use super::*;

fn tool() -> LspTool {
    LspTool::new()
}

fn ctx(working_dir: &std::path::Path) -> ToolContext {
    ToolContext {
        session_id: "test".to_string(),
        message_id: "test".to_string(),
        tool_call_id: "test".to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
    }
}

/// **The schema advertises exactly the actions the dispatch accepts.**
///
/// The enum comes from `Action::ALL`, which is also what the parser uses, so the two cannot
/// disagree. A tool describing a capability it does not have is `~/NLFCODE.md` item 4, and it is
/// worse than a missing feature: a model spends a turn discovering the lie.
#[test]
fn the_schema_advertises_only_real_actions() {
    let schema = tool().parameters_schema();
    let advertised = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("the action property must be an enum");

    assert!(!advertised.is_empty());
    for value in advertised {
        let name = value.as_str().expect("action names are strings");
        assert!(
            jcode_lsp::actions::Action::parse(name).is_some(),
            "the schema advertises {name:?}, which the dispatch rejects"
        );
    }

    // And the write action is not among them: `request` can send arbitrary methods, so it belongs
    // on an approval-gated tool rather than this one.
    assert!(
        !advertised.iter().any(|value| value == "request"),
        "a read-only tool must not advertise `request`"
    );
    assert!(!advertised.iter().any(|value| value == "rename"));
}

/// The tool is named `lsp`, and the name is what safety keys on.
#[test]
fn the_tool_is_named_lsp() {
    assert_eq!(tool().name(), "lsp");
}

/// **This tool is auto-allowed, and every action it offers is read-only.**
///
/// Both halves matter. Auto-allowing it is what makes navigation usable — a model does it dozens of
/// times a turn and prompting each time would make the tool worse than grep. And the reason that is
/// safe is that nothing here writes, which this asserts by name rather than by trusting the
/// description.
#[test]
fn the_tool_is_auto_allowed_because_it_only_reads() {
    let safety = jcode_base::safety::SafetySystem::new();
    assert_eq!(
        safety.classify("lsp"),
        jcode_base::safety::ActionTier::AutoAllowed,
        "prompting for a hover would make navigation unusable"
    );

    // The display side of this -- that `lsp` is not treated as an edit tool -- is asserted in
    // `jcode-tui-tool-display`, which is where that list lives. `app-core` does not depend on the
    // display crate, and adding a dependency to make one assertion reachable would be the wrong
    // trade: the test belongs beside the thing it constrains.
}

/// A missing or unknown action is refused with the alternatives listed.
///
/// A model told "invalid action" guesses again; a model given the list picks from it. The difference
/// is a wasted turn.
#[tokio::test]
async fn an_unknown_action_lists_the_alternatives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = tool()
        .execute(json!({"action": "goto", "file": "a.rs"}), ctx(dir.path()))
        .await
        .expect_err("goto is not an action");

    let text = error.to_string();
    assert!(text.contains("goto"), "{text}");
    assert!(
        text.contains("definition"),
        "the alternatives must be listed: {text}"
    );
}

/// Both required arguments are required.
#[tokio::test]
async fn the_required_arguments_are_enforced() {
    let dir = tempfile::tempdir().expect("tempdir");

    let error = tool()
        .execute(json!({"file": "a.rs"}), ctx(dir.path()))
        .await
        .expect_err("action is required");
    assert!(error.to_string().contains("action"), "{error}");

    let error = tool()
        .execute(json!({"action": "definition"}), ctx(dir.path()))
        .await
        .expect_err("file is required");
    assert!(error.to_string().contains("file"), "{error}");

    // An empty string is not a value.
    let error = tool()
        .execute(
            json!({"action": "definition", "file": "   "}),
            ctx(dir.path()),
        )
        .await
        .expect_err("a blank file is not a file");
    assert!(error.to_string().contains("file"), "{error}");
}

/// **A file no server handles says which tool to use instead.**
///
/// The common case for an unusual extension, and the message decides whether the reader tries again
/// pointlessly or moves on. Naming grep and ast_grep costs nothing and saves a turn.
#[tokio::test]
async fn a_file_no_server_handles_points_at_grep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("notes.xyz");
    std::fs::write(&file, "nothing structural here\n").expect("write");

    let output = tool()
        .execute(
            json!({"action": "symbols", "file": "notes.xyz"}),
            ctx(dir.path()),
        )
        .await
        .expect("an unhandled file is an answer, not an error");

    let text = output.output.clone();
    assert!(text.contains("notes.xyz"), "{text}");
    assert!(
        text.contains("grep") || text.contains("ast_grep"),
        "the message must name the tool to use instead: {text}"
    );
}

/// **A handled file whose server is not installed says which to install.**
///
/// The actionable case, and the one that must not be confused with the above. "No language server
/// handles this" for a `.rs` file in a Rust project is misleading: rust-analyzer handles it and is
/// simply absent. Distinguishing them is why `detect` returns reasons rather than a filtered list.
#[tokio::test]
async fn a_missing_server_names_what_to_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A Rust project, so rust-analyzer's root markers match.
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("write");

    let output = tool()
        .execute(
            json!({"action": "symbols", "file": "main.rs"}),
            ctx(dir.path()),
        )
        .await;

    // On a machine with rust-analyzer installed this starts a real server and answers, which is a
    // pass either way: the assertion is about the message when it cannot.
    let Ok(output) = output else {
        return;
    };
    let text = output.output.clone();
    if text.contains("No language server") {
        assert!(
            text.contains("rust-analyzer"),
            "a missing server must be named so the reader can install it: {text}"
        );
        assert!(
            text.contains("Install") || text.contains("install"),
            "{text}"
        );
    }
}

/// `include_declaration` defaults to true.
///
/// Matching omp. A caller asking for references usually wants the declaration among them, and
/// excluding it by default would look like a missing result rather than a choice.
#[test]
fn the_declaration_is_included_by_default() {
    let schema = tool().parameters_schema();
    let described = schema["properties"]["include_declaration"]["description"]
        .as_str()
        .expect("include_declaration must be documented");
    assert!(
        described.contains("Default true"),
        "the default must be stated where a model reads it: {described}"
    );
}

/// The schema documents that `symbols` needs no line.
///
/// Otherwise a model supplies one for every action, which is harmless, or omits one for actions that
/// need it, which is not. Saying so in the property description is where it gets read.
#[test]
fn the_schema_says_which_action_needs_no_line() {
    let schema = tool().parameters_schema();
    let line = schema["properties"]["line"]["description"]
        .as_str()
        .expect("line must be documented");
    assert!(
        line.contains("symbols"),
        "the exception must be documented on the property: {line}"
    );
}

/// **The project root is the one containing the file, not the session's directory.**
///
/// A language server's model is scoped to a root, and root markers live at the project root. So
/// detecting from the session directory finds nothing whenever the file is somewhere else — which is
/// the ordinary case, since a session is rooted wherever it started.
///
/// Found by running the tool through the real binary. Asking about `/tmp/lsp-accept/main.c` from a
/// session rooted in the jcode checkout answered:
///
/// ```text
/// No language server handles /tmp/lsp-accept/main.c in this project. Nothing here matches its
/// file type, so grep or ast_grep is the tool for it.
/// ```
///
/// Both a failure and a wrong explanation: clangd handles `.c` and was installed. Every library test
/// passed the project as the root directly, so none could see it. That is the difference between
/// testing a function and testing a tool, and it is why this test exists.
#[test]
fn the_project_root_is_found_from_the_file_not_the_session() {
    let session = tempfile::tempdir().expect("tempdir");
    let elsewhere = tempfile::tempdir().expect("tempdir");

    // A C project, somewhere other than the session directory.
    std::fs::create_dir_all(elsewhere.path().join("src")).expect("mkdir");
    std::fs::write(elsewhere.path().join("compile_commands.json"), "[]").expect("write");
    let file = elsewhere.path().join("src/main.c");
    std::fs::write(&file, "int main(void) { return 0; }\n").expect("write");

    let defaults = jcode_lsp::config::defaults();
    let resolved = project_root_for(&file, session.path(), &defaults);

    assert_eq!(
        resolved,
        elsewhere.path(),
        "the root must be the project holding the file, not the session directory"
    );
}

/// A file with no project falls back to the session directory.
///
/// The only sensible guess, and it preserves the previous behaviour for a file inside the session's
/// own project.
#[test]
fn a_file_with_no_project_falls_back_to_the_session_root() {
    let session = tempfile::tempdir().expect("tempdir");
    let orphan = tempfile::tempdir().expect("tempdir");
    let file = orphan.path().join("lonely.c");
    std::fs::write(&file, "int main(void) { return 0; }\n").expect("write");

    let defaults = jcode_lsp::config::defaults();
    assert_eq!(
        project_root_for(&file, session.path(), &defaults),
        session.path(),
        "with no marker anywhere, the session directory is the only guess available"
    );
}

/// **Only the markers of servers handling this file are considered.**
///
/// Using every server's markers would stop at the first ancestor with a `.git` or a `package.json`,
/// which for a Rust file in a monorepo is the wrong tree: rust-analyzer needs the crate root, and
/// given the repository root it resolves imports against the wrong workspace.
#[test]
fn an_unrelated_marker_does_not_capture_the_root() {
    let session = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");

    // The discriminating shape: an unrelated marker **below** the real one, so a bottom-up walk
    // reaches it first. My first version put `package.json` above `Cargo.toml`, which passes either
    // way -- the walk finds the crate root before it ever sees the outer directory. Caught by
    // mutating the filter away and watching the test still pass.
    //
    // Here `src/` carries a `package.json` and the crate root carries `Cargo.toml`. Only filtering
    // markers to servers that handle `.rs` reaches the crate.
    std::fs::write(repo.path().join("Cargo.toml"), "[package]\n").expect("write");
    let src = repo.path().join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("package.json"), "{}").expect("write");
    let file = src.join("lib.rs");
    std::fs::write(&file, "pub fn f() {}\n").expect("write");

    let defaults = jcode_lsp::config::defaults();
    assert_eq!(
        project_root_for(&file, session.path(), &defaults),
        repo.path(),
        "a .rs file must root at its crate, not at a package.json that happens to sit beside it"
    );
}

/// **`execute` uses the file's project, verified through `execute` itself.**
///
/// The three tests above call `project_root_for` directly, and a mutation replacing `execute`'s call
/// to it with the session root survived all of them: they proved the helper works and said nothing
/// about whether anything uses it. Exactly the gap that let the original bug ship.
///
/// This one goes through the public entry point with a session rooted somewhere else, and asserts on
/// what a caller receives. It needs no language server installed: the assertion is that the tool does
/// *not* say "nothing here matches its file type", which is what it said when the root was wrong.
#[tokio::test]
async fn execute_resolves_the_root_from_the_file_not_the_session() {
    let session = tempfile::tempdir().expect("tempdir");
    let elsewhere = tempfile::tempdir().expect("tempdir");

    // A C project, somewhere other than the session directory.
    std::fs::write(elsewhere.path().join("compile_commands.json"), "[]").expect("write");
    let file = elsewhere.path().join("main.c");
    std::fs::write(&file, "int main(void) { return 0; }\n").expect("write");

    let output = tool()
        .execute(
            json!({"action": "symbols", "file": file.to_string_lossy()}),
            ctx(session.path()),
        )
        .await;

    // Three outcomes are all acceptable and all prove the root was resolved: a real answer, or a
    // "not installed" message naming clangd, or a start failure. What must not appear is the
    // wrong-extension message, which is what a session-rooted detect produced.
    let text = match &output {
        Ok(output) => output.output.clone(),
        Err(error) => error.to_string(),
    };
    assert!(
        !text.contains("Nothing here matches its file type"),
        "the root was taken from the session rather than from the file: {text}"
    );
}
