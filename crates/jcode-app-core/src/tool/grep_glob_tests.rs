//! Translation tests for the `grep`/`glob` adapters.
//!
//! These assert the mapping onto agentgrep rather than search quality: the
//! failure mode that matters is a model calling `Grep` from its Claude-Code
//! priors and getting an error, because one failed native call sends it back
//! to bash for the rest of the session.

use super::*;

/// Run a `Grep` call through the real translation, as `execute` does.
fn grepped(input: Value) -> Value {
    let params: GrepInput = serde_json::from_value(input).expect("grep input should deserialize");
    grep_delegation(params).expect("translation should succeed")
}

/// Run a `Glob` call through the real translation.
fn globbed(input: Value) -> Value {
    let params: GlobInput = serde_json::from_value(input).expect("glob input should deserialize");
    glob_delegation(params).expect("translation should succeed")
}

/// Every parameter Claude-Code's `Grep` advertises must deserialize. A model
/// calling from its priors must not hit a schema error.
#[test]
fn claude_code_grep_parameters_all_deserialize() {
    let full = json!({
        "pattern": "fn main",
        "path": "crates",
        "glob": "**/*.rs",
        "type": "rs",
        "output_mode": "content",
        "-B": 2,
        "-A": 2,
        "-C": 2,
        "context": 2,
        "-n": true,
        "-i": true,
        "head_limit": 20,
        "offset": 0,
        "multiline": false,
    });
    let params: GrepInput =
        serde_json::from_value(full).expect("full Claude-Code Grep call must deserialize");
    assert_eq!(params.pattern.as_deref(), Some("fn main"));
    assert_eq!(params.head_limit, Some(20));
    assert_eq!(params.case_insensitive, Some(true));
}

/// The minimal call, which is what a model actually sends most of the time.
#[test]
fn minimal_grep_call_is_regex_and_content_mode() {
    let delegated = grepped(json!({"pattern": "TODO"}));
    assert_eq!(delegated["mode"], "grep");
    assert_eq!(delegated["query"], "TODO");
    // Claude-Code's Grep is regex by default; agentgrep's grep is literal, so
    // failing to set this would silently change the meaning of every pattern.
    assert_eq!(delegated["regex"], true);
    assert_eq!(delegated["paths_only"], false);
}

#[test]
fn output_mode_maps_onto_paths_only() {
    for mode in ["files_with_matches", "count"] {
        let delegated = grepped(json!({"pattern": "x", "output_mode": mode}));
        assert_eq!(delegated["paths_only"], true, "{mode} should be paths-only");
    }
    let delegated = grepped(json!({"pattern": "x", "output_mode": "content"}));
    assert_eq!(delegated["paths_only"], false);
}

#[test]
fn case_insensitive_flag_is_folded_into_the_pattern() {
    let delegated = grepped(json!({"pattern": "Error", "-i": true}));
    assert_eq!(delegated["query"], "(?i)Error");
}

#[test]
fn scope_parameters_are_forwarded_and_empties_dropped() {
    let delegated = grepped(json!({
        "pattern": "x", "path": "crates", "glob": "**/*.rs", "type": "rs", "head_limit": 5
    }));
    assert_eq!(delegated["path"], "crates");
    assert_eq!(delegated["glob"], "**/*.rs");
    assert_eq!(delegated["type"], "rs");
    assert_eq!(delegated["max_regions"], 5);

    let sparse = grepped(json!({"pattern": "x", "path": ""}));
    assert!(
        sparse.get("path").is_none(),
        "an empty path must not narrow the search to nothing"
    );
}

/// A glob must reach agentgrep's `glob` filter, not its ranking query, or it
/// is matched literally against file names and finds nothing.
#[test]
fn glob_patterns_and_bare_words_take_different_routes() {
    for pattern in ["**/*.rs", "src/*.ts", "foo?.py", "[abc].rs", ".github"] {
        assert!(is_glob_pattern(pattern), "{pattern} should route to glob");
    }
    for pattern in ["config", "read tool", "AgentGrep"] {
        assert!(
            !is_glob_pattern(pattern),
            "{pattern} should route to ranking terms"
        );
    }

    // And the routing must show up in the delegated call itself.
    let as_glob = globbed(json!({"pattern": "**/*.rs"}));
    assert_eq!(as_glob["mode"], "find");
    assert_eq!(as_glob["glob"], "**/*.rs");
    assert!(as_glob.get("query").is_none());

    let as_words = globbed(json!({"pattern": "read tool", "head_limit": 7}));
    assert_eq!(as_words["query"], "read tool");
    assert_eq!(as_words["max_files"], 7);
    assert!(as_words.get("glob").is_none());
}

#[test]
fn claude_code_glob_parameters_all_deserialize() {
    let params: GlobInput = serde_json::from_value(json!({
        "pattern": "**/*.rs",
        "path": "crates",
        "head_limit": 10,
    }))
    .expect("full Claude-Code Glob call must deserialize");
    assert_eq!(params.pattern.as_deref(), Some("**/*.rs"));
    assert_eq!(params.path.as_deref(), Some("crates"));
    assert_eq!(params.head_limit, Some(10));
}

/// Unknown parameters must be ignored rather than rejected, so a prior-driven
/// call with an extra field still returns results.
#[test]
fn unknown_parameters_are_tolerated() {
    let parsed: Result<GrepInput, _> =
        serde_json::from_value(json!({"pattern": "x", "invented_by_the_model": true}));
    assert!(parsed.is_ok(), "unknown fields must not fail the call");
}

/// Both tools must be missing their required parameter loudly, not silently
/// search for the empty string across the whole workspace.
#[test]
fn empty_pattern_is_an_error_not_a_workspace_wide_match() {
    let grep: GrepInput = serde_json::from_value(json!({"pattern": ""})).unwrap();
    assert!(
        grep_delegation(grep).is_err(),
        "an empty pattern must be an error, not a match-everything search"
    );
    let glob: GlobInput = serde_json::from_value(json!({})).unwrap();
    assert!(glob_delegation(glob).is_err(), "glob requires a pattern");
}

/// The descriptions have to name bash as the wrong choice, which is the whole
/// point of registering these (NLFCODE.md items 2 and 3).
#[test]
fn descriptions_steer_away_from_bash() {
    let grep = GrepTool::new();
    let glob = GlobTool::new();
    assert_eq!(grep.name(), "grep");
    assert_eq!(glob.name(), "glob");
    for description in [grep.description(), glob.description()] {
        assert!(
            description.contains("bash"),
            "description must name bash as the wrong choice: {description}"
        );
    }
}

/// Everything above checks the JSON this adapter *emits*. That is not the same
/// as checking what agentgrep *does* with it, and the difference is where the
/// PDF page-splitting bug lived: unit tests that stop at the boundary of the
/// code under change cannot see a false premise underneath it. These run the
/// real tool against real files.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::tool::{ToolContext, ToolExecutionMode};

    fn fixture_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jcode-grep-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // `a.c` vs `abc` distinguishes a regex search from a literal one, which
        // is the single most consequential translation detail here.
        std::fs::write(
            dir.join("one.rs"),
            "fn alpha() {}\nliteral a.c here\nliteral abc here\nUPPERCASE_MARKER\n",
        )
        .unwrap();
        std::fs::write(dir.join("two.rs"), "fn gamma() {}\nUPPERCASE_MARKER\n").unwrap();
        std::fs::write(dir.join("three.txt"), "not rust\nUPPERCASE_MARKER\n").unwrap();
        dir
    }

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            session_id: "grep-e2e".to_string(),
            message_id: "grep-e2e".to_string(),
            tool_call_id: "grep-e2e".to_string(),
            working_dir: Some(dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    async fn run_grep(dir: &std::path::Path, input: Value) -> String {
        GrepTool::new()
            .execute(input, ctx(dir))
            .await
            .expect("grep should succeed")
            .output
    }

    /// Claude-Code's `Grep` is regex by default. If the adapter fails to pass
    /// `regex: true` through, an alternation silently returns nothing and the
    /// model concludes the code it is looking for does not exist.
    #[tokio::test]
    async fn patterns_are_treated_as_regex_not_literal_text() {
        let dir = fixture_dir();

        let alternation = run_grep(&dir, json!({"pattern": "fn (alpha|gamma)"})).await;
        assert!(
            alternation.contains("alpha") && alternation.contains("gamma"),
            "an alternation must match both branches, got: {alternation}"
        );

        let anchored = run_grep(&dir, json!({"pattern": "^UPPERCASE"})).await;
        assert!(
            anchored.contains("UPPERCASE_MARKER"),
            "an anchor must be honoured as regex, got: {anchored}"
        );

        // `.` must match any character, not just a literal period.
        let wildcard = run_grep(&dir, json!({"pattern": "literal a.c here"})).await;
        assert!(
            wildcard.contains("abc here"),
            "`.` must match any character, got: {wildcard}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn case_insensitive_flag_actually_matches_other_case() {
        let dir = fixture_dir();
        let out = run_grep(&dir, json!({"pattern": "uppercase_marker", "-i": true})).await;
        assert!(
            out.contains("UPPERCASE_MARKER"),
            "-i must match across case, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bare alternation with no other parameters, which is the shape a model
    /// actually sends and the shape that returned zero matches in a live
    /// session while the tests above passed.
    #[tokio::test]
    async fn bare_alternation_with_no_other_parameters_matches() {
        let dir = fixture_dir();
        let out = run_grep(&dir, json!({"pattern": "alpha|gamma"})).await;
        assert!(
            out.contains("alpha") && out.contains("gamma"),
            "a bare alternation must match both branches, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_and_type_filters_actually_narrow_the_search() {
        let dir = fixture_dir();

        let rust_only = run_grep(&dir, json!({"pattern": "UPPERCASE", "glob": "**/*.rs"})).await;
        assert!(
            !rust_only.contains("three.txt"),
            "a glob filter must exclude non-matching files, got: {rust_only}"
        );

        let unfiltered = run_grep(&dir, json!({"pattern": "UPPERCASE"})).await;
        assert!(
            unfiltered.contains("three.txt"),
            "without a filter the .txt file must be searched, got: {unfiltered}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `files_with_matches` must return paths without match excerpts.
    #[tokio::test]
    async fn output_mode_files_with_matches_returns_paths_not_excerpts() {
        let dir = fixture_dir();
        let out = run_grep(
            &dir,
            json!({"pattern": "UPPERCASE", "output_mode": "files_with_matches"}),
        )
        .await;
        assert!(out.contains("one.rs"), "must still name the files: {out}");
        assert!(
            !out.contains("UPPERCASE_MARKER"),
            "paths-only mode must not include match text, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_tool_finds_files_by_pattern_and_by_name() {
        let dir = fixture_dir();

        let by_glob = GlobTool::new()
            .execute(json!({"pattern": "**/*.rs"}), ctx(&dir))
            .await
            .expect("glob should succeed")
            .output;
        assert!(by_glob.contains("one.rs"), "{by_glob}");
        assert!(
            !by_glob.contains("three.txt"),
            "a glob must exclude non-matching extensions: {by_glob}"
        );

        let by_name = GlobTool::new()
            .execute(json!({"pattern": "three"}), ctx(&dir))
            .await
            .expect("glob should succeed")
            .output;
        assert!(
            by_name.contains("three.txt"),
            "bare words must rank file names: {by_name}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
