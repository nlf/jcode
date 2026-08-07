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

    for field in ["file_path", "start_line", "end_line", "offset", "limit", "pages"] {
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
