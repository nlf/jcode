//! A live probe against a **real** language server, not the fake one.
//!
//! `#[ignore]`d, so it does not run in the normal suite: it needs a language server installed, and
//! which one is available differs per machine. Run it explicitly:
//!
//! ```text
//! cargo test -p jcode-lsp --test live_server -- --ignored --nocapture
//! ```
//!
//! # Why this exists when 300 tests already pass
//!
//! Everything else runs against `fake_lsp_server`, which answers what it is told to answer. That
//! proves the protocol handling and proves nothing about whether a real server understands what we
//! send it.
//!
//! It earned its place immediately. The first run against clangd found **two defects no fake-server
//! test could have**:
//!
//! - a `references` query about a symbol used once reported "Found 2 reference(s)", the same
//!   position under `/tmp/...` and `/private/tmp/...`, because macOS `/tmp` is a symlink;
//! - every location rendered as an absolute path, because the server answers with the resolved root
//!   and ours was the symlink.
//!
//! Both are now regression-tested in `src/results_tests.rs`. Neither was reachable from a fixture
//! that echoes back the paths it was given.
//!
//! It also produced the clearest possible evidence for the error path: on a machine whose
//! `rust-analyzer` was a rustup shim with no real binary behind it, the failure came back as
//! `Closed { detail: "... error: infinite recursion detected" }` with the server's stderr attached
//! — which is exactly what `stderr_tail` was written for.

use std::time::Duration;

use jcode_lsp::Registry;
use jcode_lsp::actions::{self, Action, Request};
use jcode_lsp::config;

/// A tiny C project, since clangd is the server most likely to be present.
///
/// Built here rather than checked in: a fixture in the repository would be one more thing to keep
/// in step, and this is three files.
fn c_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("main.c"),
        "int helper(int value) {\n    return value + 1;\n}\n\nint main(void) {\n    int result = helper(41);\n    return result;\n}\n",
    )
    .expect("write main.c");
    // clangd needs a compilation database, or it guesses flags and reports spurious diagnostics.
    std::fs::write(
        dir.path().join("compile_commands.json"),
        format!(
            r#"[{{"directory": "{}", "command": "clang -c main.c", "file": "{}/main.c"}}]"#,
            dir.path().display(),
            dir.path().display()
        ),
    )
    .expect("write compile_commands.json");
    dir
}

/// **Every read-only action, against a real server.**
///
/// Skips rather than fails when no server is installed: a machine without clangd should not report a
/// broken build. The skip is loud, so a run that proved nothing cannot be mistaken for a pass.
#[tokio::test]
#[ignore = "needs a real language server installed; run explicitly"]
async fn every_action_works_against_a_real_server() {
    let project = c_project();
    let root = project.path().to_path_buf();
    let file = root.join("main.c");

    let (available, unavailable) = config::detect(&config::defaults(), &root, None);
    let Some(server) = config::servers_for_file(&available, &file).first().copied() else {
        eprintln!(
            "SKIPPED: no language server for main.c on this machine (clangd: {:?})",
            unavailable.get("clangd")
        );
        return;
    };
    eprintln!("using {} at {}", server.name, server.resolved.display());

    let registry = Registry::new();
    let client = registry
        .get_or_start(&root, server, Duration::from_secs(120))
        .await
        .expect("the language server must complete a handshake");

    // A real server indexes before it can answer navigation, so the first attempts legitimately
    // return nothing. Polling with a ceiling is the honest way to wait for that.
    let mut answered = None;
    for _ in 0..12 {
        let request = Request {
            action: Action::Definition,
            file: file.clone(),
            line: Some(6),
            symbol: Some("helper".to_string()),
            include_declaration: true,
        };
        match actions::run(&client, &request, &root, Duration::from_secs(30)).await {
            Ok(output) if !output.starts_with("No ") => {
                answered = Some(output);
                break;
            }
            Ok(_) | Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    let definition = answered.expect("the server never answered a definition request");

    // `helper` is declared on line 1 and called on line 6, so a definition lookup from the call
    // must land on the declaration -- the whole point of the tool.
    assert!(
        definition.contains("main.c:1:"),
        "definition should point at the declaration on line 1: {definition}"
    );
    // And the path is relative to the project, which the symlinked-root fix is about.
    assert!(
        !definition.contains("/private/"),
        "a file inside the project rendered as an absolute path: {definition}"
    );

    let hover = actions::run(
        &client,
        &Request {
            action: Action::Hover,
            file: file.clone(),
            line: Some(6),
            symbol: Some("helper".to_string()),
            include_declaration: true,
        },
        &root,
        Duration::from_secs(30),
    )
    .await
    .expect("hover");
    assert!(
        hover.contains("helper"),
        "hover should name the symbol: {hover}"
    );

    let symbols = actions::run(
        &client,
        &Request {
            action: Action::Symbols,
            file: file.clone(),
            line: None,
            symbol: None,
            include_declaration: false,
        },
        &root,
        Duration::from_secs(30),
    )
    .await
    .expect("symbols");
    assert!(
        symbols.contains("helper") && symbols.contains("main"),
        "both functions should appear: {symbols}"
    );

    // `helper` is called exactly once, so excluding the declaration must yield exactly one -- the
    // assertion that caught the symlink duplicate.
    let references = actions::run(
        &client,
        &Request {
            action: Action::References,
            file: file.clone(),
            line: Some(1),
            symbol: Some("helper".to_string()),
            include_declaration: false,
        },
        &root,
        Duration::from_secs(30),
    )
    .await
    .expect("references");
    assert!(
        references.starts_with("Found 1 reference(s):"),
        "helper is called once, so one reference: {references}"
    );

    registry.shutdown_all().await;
}
