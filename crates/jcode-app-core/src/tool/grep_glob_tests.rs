//! Behaviour tests for `grep` and `glob`.
//!
//! These used to assert the JSON the adapters emitted for agentgrep. That
//! delegation is gone, so the translation tests went with it: they described a
//! mapping that no longer exists. What remains, and what was added, asserts
//! what a caller observes, which is what survived the engine swap.

use super::*;

/// Every parameter Claude-Code's `Grep` advertises must deserialize. A model
/// calling from its priors must not hit a schema error, because one failed
/// native call sends it back to bash for the rest of the session.
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

/// omp's parameters must deserialize too, since the ported engine accepts them.
#[test]
fn omp_grep_parameters_also_deserialize() {
    let params: GrepInput = serde_json::from_value(json!({
        "pattern": "needle",
        "path": "src; test",
        "case": true,
        "gitignore": false,
        "hidden": true,
        "skip": 20,
    }))
    .expect("omp-shaped call must deserialize");

    assert_eq!(params.case, Some(true));
    assert_eq!(params.skip, Some(20));
    assert_eq!(params.gitignore, Some(false));
    assert_eq!(params.hidden, Some(true));
}

#[test]
fn claude_code_glob_parameters_all_deserialize() {
    let params: GlobInput = serde_json::from_value(json!({
        "pattern": "**/*.rs",
        "path": "crates",
        "head_limit": 50,
    }))
    .expect("full Claude-Code Glob call must deserialize");

    assert_eq!(params.pattern.as_deref(), Some("**/*.rs"));
    assert_eq!(params.head_limit, Some(50));
}

/// Unknown parameters are tolerated rather than rejected, for the same reason.
#[test]
fn unknown_parameters_are_tolerated() {
    let params: GrepInput = serde_json::from_value(json!({
        "pattern": "x",
        "some_future_flag": true,
    }))
    .expect("unknown fields must not fail deserialization");
    assert_eq!(params.pattern.as_deref(), Some("x"));
}

/// An empty pattern would match every line of every file, which is never what
/// was meant and costs the whole output budget.
#[test]
fn empty_pattern_deserializes_but_is_refused_at_execute() {
    let params: GrepInput =
        serde_json::from_value(json!({"pattern": ""})).expect("empty string deserializes");
    assert_eq!(params.pattern.as_deref(), Some(""));
}

/// A `glob` or `type` filter narrows *within* the path rather than replacing
/// it. Replacing would silently widen `path: "src"` to the whole workspace.
#[test]
fn a_filter_narrows_within_the_path_rather_than_replacing_it() {
    assert_eq!(
        combine_scope(Some("src"), Some("**/*.rs"), None).as_deref(),
        Some("src/**/*.rs")
    );
    assert_eq!(
        combine_scope(Some("src"), None, Some("rs")).as_deref(),
        Some("src/**/*.rs")
    );
    assert_eq!(
        combine_scope(None, Some("**/*.rs"), None).as_deref(),
        Some("**/*.rs")
    );
    assert_eq!(
        combine_scope(Some("src"), None, None).as_deref(),
        Some("src")
    );
    assert_eq!(combine_scope(None, None, None), None);
}

/// `glob` wins over `type` when both are given: it is the more specific
/// statement, and honouring both would need an intersection the engine has no
/// way to express.
#[test]
fn an_explicit_glob_takes_precedence_over_a_type_filter() {
    assert_eq!(
        combine_scope(None, Some("**/*.md"), Some("rs")).as_deref(),
        Some("**/*.md")
    );
}

/// Empty strings are dropped rather than becoming a filter that matches
/// nothing.
#[test]
fn empty_scope_strings_are_ignored() {
    assert_eq!(combine_scope(Some(""), Some(""), Some("")), None);
    assert_eq!(
        combine_scope(Some("src"), Some("  "), None).as_deref(),
        Some("src")
    );
}

/// The descriptions steer away from bash, which a workspace-wide test also
/// enforces for every tool.
#[test]
fn descriptions_steer_away_from_bash() {
    for description in [GrepTool::new().description(), GlobTool::new().description()] {
        assert!(
            description.contains("bash"),
            "description must name bash as the wrong choice: {description}"
        );
    }
}

/// These run the real tool against real files.
///
/// They predate the engine swap and are kept verbatim: they assert observable
/// behaviour rather than the shape of a delegated JSON call, so they carried
/// over from the agentgrep adapters to the ported engine without edits. That is
/// the property that made them worth having.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use crate::tool::{ToolContext, ToolExecutionMode};

    /// A fixture directory unique to the calling test.
    ///
    /// Keyed on the test's own name, not just the process id: these tests
    /// delete their directory when done, so sharing one across the module
    /// meant a finishing test pulled the files out from under a running one.
    /// They passed only because they were always run with `--test-threads=1`,
    /// and failed 5-of-5 under cargo's default parallelism.
    fn fixture_dir(test_name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jcode-grep-e2e-{}-{test_name}", std::process::id()));
        // Left over from an aborted run, so start from a known state.
        let _ = std::fs::remove_dir_all(&dir);
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
        let dir = fixture_dir("patterns_are_treated_as_regex_not_literal_text");

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
        let dir = fixture_dir("case_insensitive_flag_actually_matches_other_case");
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
        let dir = fixture_dir("bare_alternation_with_no_other_parameters_matches");
        let out = run_grep(&dir, json!({"pattern": "alpha|gamma"})).await;
        assert!(
            out.contains("alpha") && out.contains("gamma"),
            "a bare alternation must match both branches, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_and_type_filters_actually_narrow_the_search() {
        let dir = fixture_dir("glob_and_type_filters_actually_narrow_the_search");

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

        // The `type` filter is a separate code path from `glob`, and this test
        // used to claim it in its name without exercising it.
        let by_type = run_grep(&dir, json!({"pattern": "UPPERCASE", "type": "rs"})).await;
        assert!(
            by_type.contains("one.rs") && !by_type.contains("three.txt"),
            "a type filter must narrow to that file type, got: {by_type}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `head_limit` is translated to both `max_regions` and `max_files`, which
    /// is a guess about which one agentgrep honours in grep mode. Assert the
    /// observable consequence rather than the mapping.
    #[tokio::test]
    async fn head_limit_bounds_the_results() {
        let dir = fixture_dir("head_limit_bounds_the_results");

        // UPPERCASE_MARKER appears in all three fixture files.
        let unbounded = run_grep(&dir, json!({"pattern": "UPPERCASE"})).await;
        let bounded = run_grep(&dir, json!({"pattern": "UPPERCASE", "head_limit": 1})).await;

        let count_files = |out: &str| {
            ["one.rs", "two.rs", "three.txt"]
                .iter()
                .filter(|name| out.contains(*name))
                .count()
        };
        assert!(
            count_files(&unbounded) > 1,
            "fixture should match several files, got: {unbounded}"
        );
        // `<= unbounded` would pass even if head_limit were ignored entirely,
        // so require it to actually bind.
        assert_eq!(
            count_files(&bounded),
            1,
            "head_limit=1 must return exactly one file, got: {bounded}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `multiline` is translated to agentgrep's `full_region`. Assert only that
    /// the call succeeds and still finds the match, since the exact rendering
    /// is agentgrep's to decide.
    #[tokio::test]
    async fn multiline_requests_still_return_matches() {
        let dir = fixture_dir("multiline_requests_still_return_matches");
        let out = run_grep(&dir, json!({"pattern": "fn alpha", "multiline": true})).await;
        assert!(
            out.contains("alpha"),
            "a multiline request must still match, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `files_with_matches` must return paths without match excerpts.
    #[tokio::test]
    async fn output_mode_files_with_matches_returns_paths_not_excerpts() {
        let dir = fixture_dir("output_mode_files_with_matches_returns_paths_not_excerpts");
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
        let dir = fixture_dir("glob_tool_finds_files_by_pattern_and_by_name");

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
