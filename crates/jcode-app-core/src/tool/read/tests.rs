use super::*;
use crate::tool::{ToolContext, ToolExecutionMode};
use serde_json::json;

fn make_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[test]
fn normalize_read_range_supports_start_and_end_lines() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "end_line": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("normalize range");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 11,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_supports_start_line_and_limit() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "limit": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("start_line + limit should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_prefers_end_line_over_limit() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "end_line": 20,
        "limit": 999
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("end_line should take precedence");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 11,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_rejects_start_line_and_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "offset": 20
    }))
    .expect("deserialize params");

    let err = normalize_read_range(&params).expect_err("mixed range styles should fail");
    assert!(
        err.to_string().contains("Use either start_line/end_line")
            || err.to_string().contains("not both"),
        "unexpected error: {err}"
    );
}

#[test]
fn normalize_read_range_accepts_matching_start_line_and_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "offset": 9,
        "limit": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("matching range styles should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_accepts_end_line_with_zero_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "end_line": 20,
        "offset": 0
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("redundant zero offset should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 0,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_rejects_invalid_end_before_start() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 20,
        "end_line": 10
    }))
    .expect("deserialize params");

    let err = normalize_read_range(&params).expect_err("invalid range should fail");
    assert!(
        err.to_string()
            .contains("greater than or equal to start_line"),
        "unexpected error: {err}"
    );
}

#[test]
fn read_tool_schema_avoids_openai_incompatible_combinators() {
    let schema = ReadTool::new().parameters_schema();

    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert!(schema.get("allOf").is_none());
    assert!(schema.get("not").is_none());
}

#[test]
fn read_tool_schema_advertises_what_the_implementation_accepts() {
    // This test previously asserted the opposite: that `offset` and `end_line`
    // were *hidden* to keep the schema minimal. That minimalism is what caused
    // NLFCODE.md item 4. `ReadInput` accepts both spellings, and the Anthropic
    // OAuth path advertises `offset`/`limit`/`pages`, so hiding them locally
    // left two surfaces disagreeing about one tool and left `pages` looking
    // unsupported. Advertise what is actually accepted.
    let schema = ReadTool::new().parameters_schema();
    let properties = schema["properties"]
        .as_object()
        .expect("read schema properties should be an object");

    for field in [
        "file_path",
        "start_line",
        "end_line",
        "offset",
        "limit",
        "pages",
    ] {
        assert!(
            properties.contains_key(field),
            "'{field}' is accepted by ReadInput and must be advertised"
        );
    }

    // Internal-only fields must still stay out of the advertised surface.
    for field in ["style", "next_offset"] {
        assert!(!properties.contains_key(field), "'{field}' is internal");
    }
}

#[test]
fn read_tool_description_advertises_supported_file_types() {
    let tool = ReadTool::new();
    let description = tool.description().to_lowercase();
    assert!(description.contains("text"), "description={description}");
    assert!(description.contains("image"), "description={description}");
    assert!(description.contains("pdf"), "description={description}");

    let schema = tool.parameters_schema();
    let file_path_description = schema["properties"]["file_path"]["description"]
        .as_str()
        .expect("file_path should have a description");
    assert!(
        file_path_description.starts_with("Path to a file."),
        "description={file_path_description}"
    );
    // The tool-selection steer lives here rather than in the tool description,
    // which is capped at ~20 tokens as always-on prompt cost.
    assert!(
        file_path_description.contains("bash"),
        "file_path must name bash as the wrong choice: {file_path_description}"
    );
}

#[tokio::test]
async fn read_tool_supports_start_line_and_end_line() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 4
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(
        output.output.contains("2\ttwo"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("3\tthree"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("4\tfour"),
        "output={:?}",
        output.output
    );
    assert!(
        !output.output.contains("1\tone"),
        "output={:?}",
        output.output
    );
    assert!(
        !output.output.contains("5\tfive"),
        "output={:?}",
        output.output
    );
}

#[tokio::test]
async fn read_tool_continuation_hint_matches_start_line_style() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 3
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

/// Every parameter the advertised schema permits must deserialize.
///
/// Regression for NLFCODE.md item 4: the OAuth path advertised `Read` with
/// `offset`/`limit`/`pages` while the local schema declared `start_line`, and
/// `pages` was not in `ReadInput` at all, so serde silently dropped it and a
/// page request returned the whole document. One failed or silently-wrong
/// native call is enough to send a model back to bash for a session.
#[test]
fn every_advertised_parameter_is_accepted() {
    let schema = ReadTool::new().parameters_schema();
    let advertised: Vec<String> = schema["properties"]
        .as_object()
        .expect("object schema")
        .keys()
        .filter(|k| *k != "intent")
        .cloned()
        .collect();

    for name in &advertised {
        // A representative value for each advertised parameter.
        let value = match name.as_str() {
            "file_path" => json!("src/lib.rs"),
            "pages" => json!("1-3"),
            _ => json!(2),
        };
        let mut object = serde_json::Map::new();
        object.insert("file_path".to_string(), json!("src/lib.rs"));
        object.insert(name.clone(), value);
        let params: Result<ReadInput, _> = serde_json::from_value(Value::Object(object));
        assert!(
            params.is_ok(),
            "advertised parameter '{name}' is rejected by ReadInput"
        );
    }

    // The whole advertised surface at once, in the OAuth spelling.
    let oauth_style: ReadInput = serde_json::from_value(json!({
        "file_path": "doc.pdf", "offset": 0, "limit": 100, "pages": "2-5"
    }))
    .expect("the OAuth-advertised call shape must deserialize");
    assert_eq!(oauth_style.pages.as_deref(), Some("2-5"));
    assert!(normalize_read_range(&oauth_style).is_ok());
}

/// Both spellings of the same range must be accepted and mean the same thing.
#[test]
fn offset_and_start_line_are_interchangeable_spellings() {
    let by_offset: ReadInput =
        serde_json::from_value(json!({"file_path": "a.rs", "offset": 9, "limit": 5})).unwrap();
    let by_start_line: ReadInput =
        serde_json::from_value(json!({"file_path": "a.rs", "start_line": 10, "limit": 5})).unwrap();

    let a = normalize_read_range(&by_offset).expect("offset style");
    let b = normalize_read_range(&by_start_line).expect("start_line style");
    assert_eq!(a.offset, b.offset, "0-based 9 and 1-based 10 are one line");
    assert_eq!(a.limit, b.limit);
}

#[test]
fn page_selections_parse_into_one_based_pages() {
    assert_eq!(parse_page_selection("3").unwrap(), vec![3]);
    assert_eq!(parse_page_selection("2-5").unwrap(), vec![2, 3, 4, 5]);
    assert_eq!(
        parse_page_selection("1,4,9-11").unwrap(),
        vec![1, 4, 9, 10, 11]
    );
    // Overlaps and disorder are normalized rather than duplicated.
    assert_eq!(parse_page_selection("3,1-2,2").unwrap(), vec![1, 2, 3]);
    assert_eq!(parse_page_selection(" 2 - 4 ").unwrap(), vec![2, 3, 4]);
}

/// A bad selection must be an error, not a silent full-document read: the
/// model cannot tell it did not get what it asked for.
#[test]
fn invalid_page_selections_are_refused() {
    for spec in ["0", "5-2", "abc", "", "1-", "-3"] {
        assert!(
            parse_page_selection(spec).is_err(),
            "'{spec}' should be refused"
        );
    }
}

#[tokio::test]
async fn read_tool_supports_start_line_with_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "limit": 2
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(
        output.output.contains("2\ttwo"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("3\tthree"),
        "output={:?}",
        output.output
    );
    assert!(
        !output.output.contains("4\tfour"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

#[tokio::test]
async fn read_tool_prefers_end_line_over_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 3,
                "limit": 50
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(
        output.output.contains("2\ttwo"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("3\tthree"),
        "output={:?}",
        output.output
    );
    assert!(
        !output.output.contains("4\tfour"),
        "output={:?}",
        output.output
    );
    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

// --- file-not-found message -------------------------------------------------
//
// The old message was "File not found: <path>" and nothing else, which stated
// the one thing the caller already knew. These pin what was added, and equally
// that it is not added when it would be noise or a guess.

#[test]
fn a_relative_miss_reports_what_it_resolved_against() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let resolved = cwd.join("missing.txt");

    let message = file_not_found_message("missing.txt", &resolved, Some(cwd));

    assert!(message.contains("File not found: missing.txt"), "{message}");
    assert!(
        message.contains("working directory"),
        "a relative miss must say what it resolved against: {message}"
    );
    assert!(
        message.contains(&cwd.display().to_string()),
        "the working directory itself must appear: {message}"
    );
}

/// An absolute path never consulted the working directory, so naming it would
/// point at something that played no part in the failure.
#[test]
fn an_absolute_miss_does_not_mention_the_working_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let absolute = cwd.join("nope").join("missing.txt");

    let message = file_not_found_message(&absolute.display().to_string(), &absolute, Some(cwd));

    assert!(
        !message.contains("working directory"),
        "an absolute path did not use the working directory: {message}"
    );
}

/// The common "dropped or duplicated a directory level" mistake: the file is
/// really at the root, but was asked for one level deeper.
#[test]
fn a_file_one_level_up_is_suggested() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let real = cwd.join("config.toml");
    std::fs::write(&real, "x = 1\n").expect("write");
    std::fs::create_dir(cwd.join("src")).expect("mkdir");

    let resolved = cwd.join("src").join("config.toml");
    let message = file_not_found_message("src/config.toml", &resolved, Some(cwd));

    assert!(
        message.contains("Did you mean"),
        "a file one level up should be offered: {message}"
    );
    assert!(
        message.contains(&real.display().to_string()),
        "the suggestion must be the real path: {message}"
    );
}

/// Suggestions must only ever name a path that exists, so the model is never
/// sent somewhere invented.
#[test]
fn no_suggestion_is_offered_when_nothing_is_near() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let resolved = cwd.join("totally-absent-xyz.txt");

    let message = file_not_found_message("totally-absent-xyz.txt", &resolved, Some(cwd));

    assert!(
        !message.contains("Did you mean"),
        "nothing exists nearby, so nothing should be guessed: {message}"
    );
}

#[tokio::test]
async fn the_read_tool_surfaces_the_richer_message() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("notes.md");
    std::fs::write(&real, "hello\n").expect("write");
    std::fs::create_dir(temp.path().join("docs")).expect("mkdir");

    let tool = ReadTool::new();
    let error = tool
        .execute(
            json!({ "file_path": "docs/notes.md" }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect_err("reading a missing file must fail");
    let message = error.to_string();

    assert!(
        message.contains("File not found: docs/notes.md"),
        "{message}"
    );
    assert!(
        message.contains("working directory"),
        "the tool must carry the context, not just the helper: {message}"
    );
    assert!(
        message.contains(&real.display().to_string()),
        "and the suggestion: {message}"
    );
}

/// With tilde expansion in `resolve_path`, `~/` reaching the read tool is a
/// genuine miss rather than a resolution failure, so the message must not still
/// be echoing an unexpanded `~` back as the resolved location.
#[tokio::test]
async fn a_tilde_path_is_expanded_before_the_existence_check() {
    let tool = ReadTool::new();
    let error = tool
        .execute(
            json!({ "file_path": "~/definitely-not-a-real-file-xyz123.txt" }),
            make_ctx(std::path::PathBuf::from("/tmp")),
        )
        .await
        .expect_err("a missing file must fail");
    let message = error.to_string();

    assert!(
        !message.contains("/tmp/~"),
        "the tilde must not have been joined onto the working directory: {message}"
    );
}

/// `~/x` looks relative to `Path::is_relative`, but `resolve_path` expands it to
/// an absolute path before the working directory is ever consulted. Claiming it
/// resolved "against the working directory" is therefore false, and misleading
/// whenever the home directory is not the working directory.
#[test]
fn a_tilde_miss_does_not_claim_it_resolved_against_the_working_directory() {
    let home = dirs::home_dir().expect("home");
    let resolved = home.join("definitely-absent-xyz.txt");

    let message = file_not_found_message(
        "~/definitely-absent-xyz.txt",
        &resolved,
        Some(std::path::Path::new("/some/other/place")),
    );

    assert!(
        !message.contains("working directory"),
        "a tilde path is expanded, not resolved against the cwd: {message}"
    );
}

/// A relative path with no working directory has nothing to report.
#[test]
fn a_relative_miss_without_a_working_directory_says_nothing_extra() {
    let message = file_not_found_message("x/y.txt", std::path::Path::new("x/y.txt"), None);
    assert_eq!(message, "File not found: x/y.txt");
}

#[tokio::test]
async fn edit_reports_the_same_context_as_read() {
    use crate::tool::Tool as _;
    use crate::tool::edit::EditTool;

    let temp = tempfile::tempdir().expect("tempdir");
    let tool = EditTool::new();
    let error = tool
        .execute(
            json!({
                "file_path": "nope/missing.txt",
                "old_string": "a",
                "new_string": "b"
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect_err("editing a missing file must fail");
    let message = error.to_string();

    assert!(
        message.contains("File not found: nope/missing.txt"),
        "{message}"
    );
    assert!(
        message.contains("working directory"),
        "edit resolves paths the same way and should explain itself the same way: {message}"
    );
}

/// A caller that asked for a directory cannot use a file, so a same-named file
/// must not be offered as the suggestion. Without the kind filter the fuzzy
/// matcher would happily point `ls` at a regular file.
#[test]
fn a_directory_miss_is_not_offered_a_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    // A *file* named `build` sits where a directory named `build` was wanted.
    std::fs::write(cwd.join("build"), "not a directory\n").expect("write");
    std::fs::create_dir(cwd.join("src")).expect("mkdir");

    let resolved = cwd.join("src").join("build");
    let message = directory_not_found_message("src/build", &resolved, Some(cwd));

    assert!(message.starts_with("Directory not found:"), "{message}");
    assert!(
        !message.contains("Did you mean"),
        "the only nearby match is a file, which a directory lookup cannot use: {message}"
    );
}

/// The mirror of the above: a real directory one level up is a valid hit.
#[test]
fn a_directory_one_level_up_is_suggested() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let real = cwd.join("fixtures");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::create_dir(cwd.join("tests")).expect("mkdir");

    let resolved = cwd.join("tests").join("fixtures");
    let message = directory_not_found_message("tests/fixtures", &resolved, Some(cwd));

    assert!(
        message.contains(&real.display().to_string()),
        "a real directory one level up should be offered: {message}"
    );
}

/// And the file direction still ignores directories, so `read` is never sent to
/// a directory it cannot read.
#[test]
fn a_file_miss_is_not_offered_a_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    std::fs::create_dir(cwd.join("notes.md")).expect("mkdir a directory with a file-ish name");
    std::fs::create_dir(cwd.join("docs")).expect("mkdir");

    let resolved = cwd.join("docs").join("notes.md");
    let message = file_not_found_message("docs/notes.md", &resolved, Some(cwd));

    assert!(
        !message.contains("Did you mean"),
        "the only nearby match is a directory, which read cannot use: {message}"
    );
}

#[tokio::test]
async fn ls_reports_the_same_context_as_read() {
    use crate::tool::Tool as _;
    use crate::tool::ls::LsTool;

    let temp = tempfile::tempdir().expect("tempdir");
    let tool = LsTool::new();
    let error = tool
        .execute(
            json!({ "path": "nope_xyz" }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect_err("listing a missing directory must fail");
    let message = error.to_string();

    assert!(
        message.contains("Directory not found: nope_xyz"),
        "{message}"
    );
    assert!(
        message.contains("working directory"),
        "ls resolves paths the same way and should explain itself the same way: {message}"
    );
}

/// A context with its own session, so snapshot provenance from one test cannot
/// resolve in another. `make_ctx` deliberately shares one session id, which is
/// fine for tests that never touch the store.
fn make_ctx_in_session(working_dir: std::path::PathBuf, session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        ..make_ctx(working_dir)
    }
}

/// The header is the whole point of the read side: without it the model has no
/// tag to send back, and every hashline edit fails as unanchored.
#[tokio::test]
async fn read_stamps_its_output_with_a_hashline_header() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\ntwo\n").expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt" }),
            make_ctx_in_session(temp.path().to_path_buf(), "read-header"),
        )
        .await
        .expect("read");

    let first = output.output.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("[f.txt#") && first.ends_with(']'),
        "expected a [path#TAG] header, got {first:?}"
    );
}

/// The tag has to resolve in the session's store, or `edit` cannot turn it back
/// into the content it was minted from. A header that looks right but resolves
/// to nothing would pass the test above and fail every real edit.
#[tokio::test]
async fn the_minted_tag_resolves_to_the_content_that_was_read() {
    let temp = tempfile::tempdir().expect("tempdir");
    let text = "one\ntwo\n";
    std::fs::write(temp.path().join("f.txt"), text).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt" }),
            make_ctx_in_session(temp.path().to_path_buf(), "read-resolves"),
        )
        .await
        .expect("read");

    let header = output.output.lines().next().unwrap_or_default();
    let tag = header
        .trim_start_matches("[f.txt#")
        .trim_end_matches(']')
        .to_string();
    let snapshot = crate::tool::hashline_store::for_session("read-resolves")
        .by_hash("f.txt", &tag)
        .expect("the tag read just minted did not resolve in its own session");

    assert_eq!(snapshot.text, text);
}

/// A partial read must record only the lines it showed. If it recorded them all,
/// the seen-line guard would let the model edit a line it never saw, which is
/// the exact failure the guard exists to prevent.
#[tokio::test]
async fn a_partial_read_records_only_the_lines_it_displayed() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\ntwo\nthree\nfour\n").expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt", "start_line": 2, "end_line": 3 }),
            make_ctx_in_session(temp.path().to_path_buf(), "read-partial"),
        )
        .await
        .expect("read");

    let header = output.output.lines().next().unwrap_or_default();
    let tag = header
        .trim_start_matches("[f.txt#")
        .trim_end_matches(']')
        .to_string();
    let snapshot = crate::tool::hashline_store::for_session("read-partial")
        .by_hash("f.txt", &tag)
        .expect("tag resolves");

    assert_eq!(
        snapshot.seen_lines,
        Some([2usize, 3].into_iter().collect()),
        "a partial read should record exactly the displayed lines"
    );
}

/// The tag hashes the whole file even when only part was shown. Hashing the
/// shown range instead would make two different reads of one unchanged file
/// disagree, and `edit` would report a spurious concurrent modification.
#[tokio::test]
async fn the_tag_covers_the_whole_file_not_the_displayed_range() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\ntwo\nthree\nfour\n").expect("write");
    let dir = temp.path().to_path_buf();

    let tag_of = |args: serde_json::Value| {
        let dir = dir.clone();
        async move {
            let output = ReadTool::new()
                .execute(args, make_ctx_in_session(dir, "read-whole-file"))
                .await
                .expect("read");
            let header = output.output.lines().next().unwrap_or_default().to_string();
            header
                .trim_start_matches("[f.txt#")
                .trim_end_matches(']')
                .to_string()
        }
    };

    assert_eq!(
        tag_of(json!({ "file_path": "f.txt" })).await,
        tag_of(json!({ "file_path": "f.txt", "start_line": 2, "end_line": 3 })).await,
        "reads of one unchanged file produced different tags"
    );
}

/// An empty file returns early, before the header is minted. Worth pinning: a
/// caller that assumes line one is always a header would misparse this.
#[tokio::test]
async fn an_empty_file_returns_its_own_message_without_a_header() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("empty.txt"), "").expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "empty.txt" }),
            make_ctx_in_session(temp.path().to_path_buf(), "read-empty"),
        )
        .await
        .expect("read");

    assert_eq!(output.output, "(empty file)");
}

/// omp's read takes one `path` carrying an inline selector. Ours accepts that
/// spelling alongside the explicit range parameters, so a model that has read
/// omp's docs or seen a `file:line` reference anywhere can use it directly.
#[tokio::test]
async fn an_inline_selector_scopes_the_read() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:50-52" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-basic"),
        )
        .await
        .expect("read");

    assert!(output.output.contains("50\tline 50"), "{}", output.output);
    assert!(output.output.contains("52\tline 52"), "{}", output.output);
    assert!(
        !output.output.contains("line 100"),
        "the selector should scope the read: {}",
        output.output
    );
}

/// omp pads an explicit range asymmetrically: one line above, three below.
/// That padding is why a follow-up edit's anchor usually lands inside what was
/// already shown.
#[tokio::test]
async fn a_selector_read_carries_omps_context_padding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:50-52" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-pad"),
        )
        .await
        .expect("read");

    assert!(output.output.contains("49\tline 49"), "one line above: {}", output.output);
    assert!(output.output.contains("55\tline 55"), "three below: {}", output.output);
    assert!(!output.output.contains("48\tline 48"), "no more above: {}", output.output);
    assert!(!output.output.contains("56\tline 56"), "no more below: {}", output.output);
}

/// Several ranges in one selector, which is the thing our explicit parameters
/// cannot express at all.
#[tokio::test]
async fn a_multi_range_selector_returns_both_windows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:10-12,100-102" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-multi"),
        )
        .await
        .expect("read");

    assert!(output.output.contains("10\tline 10"), "{}", output.output);
    assert!(output.output.contains("100\tline 100"), "{}", output.output);
    assert!(
        !output.output.contains("50\tline 50"),
        "the gap between windows must not be shown: {}",
        output.output
    );
    assert!(
        output.output.contains(jcode_read::ELISION),
        "the gap must be marked: {}",
        output.output
    );
}

/// The count form: `:50+10` is fifty through fifty-nine.
#[tokio::test]
async fn a_count_selector_spans_exactly_that_many_lines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:50+10" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-count"),
        )
        .await
        .expect("read");

    assert!(output.output.contains("59\tline 59"), "{}", output.output);
    // 59 plus three lines of trailing context, and no further.
    assert!(!output.output.contains("63\tline 63"), "{}", output.output);
}

/// A file genuinely named `notes:1-2` must be read, not truncated to `notes`.
#[tokio::test]
async fn an_existing_file_wins_over_the_selector_reading_of_its_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("notes:1-2"), "real file\n").expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "notes:1-2" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-literal"),
        )
        .await
        .expect("the literal file should be read");

    assert!(output.output.contains("real file"), "{}", output.output);
}

/// A selector with impossible bounds is a mistake worth reporting rather than
/// a filename to look for.
#[tokio::test]
async fn an_impossible_selector_is_reported() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("f.txt"), "one\ntwo\n").expect("write");

    let error = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:100-50" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-bad"),
        )
        .await
        .expect_err("a backwards range should be refused");

    assert!(
        error.to_string().contains("end must be >= start"),
        "{error}"
    );
}

/// A selector read still mints a hashline tag, so an edit can follow it.
#[tokio::test]
async fn a_selector_read_still_records_provenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:50-52" }),
            make_ctx_in_session(temp.path().to_path_buf(), "sel-tag"),
        )
        .await
        .expect("read");

    let header = output.output.lines().next().unwrap_or_default();
    assert!(header.starts_with("[f.txt#"), "{header}");

    let snapshot = crate::tool::hashline_store::for_session("sel-tag")
        .head("f.txt")
        .expect("a selector read should record provenance");
    // Seen lines are the padded window, not the raw request: those are the
    // lines actually put in front of the model.
    let seen = snapshot.seen_lines.expect("seen lines recorded");
    assert!(seen.contains(&49), "leading context counts as seen");
    assert!(seen.contains(&55), "trailing context counts as seen");
    assert!(!seen.contains(&1), "unshown lines must not be recorded");
}

/// The continuation hint must describe where reading actually stopped.
///
/// Found by a live agent quoting it back: a selector read populates no
/// offset/limit, so deriving the hint from the request produced `offset=5000`
/// on a 200-line file. The existing tests all used explicit ranges, where the
/// request and the stopping point happen to agree.
#[tokio::test]
async fn a_truncated_selector_read_hints_with_a_selector() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    let output = ReadTool::new()
        .execute(
            json!({ "file_path": "f.txt:50-52" }),
            make_ctx_in_session(temp.path().to_path_buf(), "hint-sel"),
        )
        .await
        .expect("read");

    assert!(
        output.output.contains("145 more lines"),
        "the count must reflect what is left after line 55: {}",
        output.output
    );
    assert!(
        output.output.contains("f.txt:56-"),
        "a selector read continues with a selector: {}",
        output.output
    );
    assert!(
        !output.output.contains("offset=5000"),
        "the hint must not come from the unpopulated request: {}",
        output.output
    );
}

/// The hint must never point past the end of the file, whichever spelling was
/// used, or the model asks for lines that cannot exist.
#[tokio::test]
async fn a_continuation_hint_never_points_past_the_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    std::fs::write(temp.path().join("f.txt"), body).expect("write");

    for args in [
        json!({ "file_path": "f.txt:10-12" }),
        json!({ "file_path": "f.txt", "limit": 5 }),
        json!({ "file_path": "f.txt", "start_line": 10, "end_line": 12 }),
    ] {
        let output = ReadTool::new()
            .execute(
                args.clone(),
                make_ctx_in_session(temp.path().to_path_buf(), "hint-bounds"),
            )
            .await
            .expect("read");

        for token in output.output.split_whitespace() {
            let number = token
                .trim_start_matches("offset=")
                .trim_start_matches("start_line=")
                .trim_start_matches("f.txt:")
                .trim_end_matches('-');
            if token.starts_with("offset=")
                || token.starts_with("start_line=")
                || token.starts_with("f.txt:")
            {
                let value: usize = number.parse().unwrap_or(0);
                assert!(
                    value <= 101,
                    "hint {token:?} points past a 100-line file, for {args}"
                );
            }
        }
    }
}
