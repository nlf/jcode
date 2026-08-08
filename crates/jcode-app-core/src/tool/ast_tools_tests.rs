use super::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn ctx(temp: &TempDir) -> ToolContext {
    // Session id keyed on the directory: the hashline store is global, and a
    // shared id would let one test see another's snapshots.
    let session = format!("ast-test-{}", temp.path().display());
    ToolContext {
        session_id: session.clone(),
        message_id: session.clone(),
        tool_call_id: session,
        working_dir: Some(temp.path().to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn tree(files: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().expect("temp");
    for (name, body) in files {
        fs::write(temp.path().join(name), body).expect("write");
    }
    temp
}

#[tokio::test]
async fn ast_grep_finds_matches_and_tags_the_file() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let out = AstGrepTool::new()
        .execute(json!({"pattern": "one()"}), ctx(&temp))
        .await
        .expect("search");

    assert!(out.output.contains("a.rs"), "{}", out.output);
    // The tag is what makes a search result editable without a re-read; grep
    // mints one, so this must too or ast_grep is a dead end for editing.
    assert!(
        out.output.contains(" #"),
        "no hashline tag in output: {}",
        out.output
    );
}

/// The whole reason to use ast_grep over grep: the same characters inside a
/// string or comment are not code and must not match.
#[tokio::test]
async fn ast_grep_does_not_match_inside_strings_or_comments() {
    let temp = tree(&[(
        "a.rs",
        "fn a() {\n    // one()\n    let s = \"one()\";\n    one();\n}\n",
    )]);
    let out = AstGrepTool::new()
        .execute(json!({"pattern": "one()"}), ctx(&temp))
        .await
        .expect("search");

    assert!(out.title.unwrap_or_default().contains("ast_grep 1"));
}

#[tokio::test]
async fn ast_grep_requires_a_pattern() {
    let temp = tree(&[("a.rs", "fn a() {}\n")]);
    let error = AstGrepTool::new()
        .execute(json!({}), ctx(&temp))
        .await
        .expect_err("no pattern");

    assert!(error.to_string().contains("pattern"), "{error}");
}

#[tokio::test]
async fn an_unknown_language_is_named_rather_than_silently_ignored() {
    let temp = tree(&[("a.rs", "fn a() { one(); }\n")]);
    let error = AstGrepTool::new()
        .execute(
            json!({"pattern": "one()", "language": "cobol"}),
            ctx(&temp),
        )
        .await
        .expect_err("unknown language");

    assert!(error.to_string().contains("cobol"), "{error}");
}

#[tokio::test]
async fn ast_grep_says_so_when_nothing_matched() {
    let temp = tree(&[("a.rs", "fn a() { other(); }\n")]);
    let out = AstGrepTool::new()
        .execute(json!({"pattern": "one()"}), ctx(&temp))
        .await
        .expect("search");

    assert!(out.output.contains("No matches"), "{}", out.output);
}

#[tokio::test]
async fn ast_edit_rewrites_and_reports_a_diff() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let out = AstEditTool::new()
        .execute(
            json!({"pattern": "one()", "replacement": "two()"}),
            ctx(&temp),
        )
        .await
        .expect("rewrite");

    assert_eq!(
        fs::read_to_string(temp.path().join("a.rs")).expect("read"),
        "fn alpha() { two(); }\n"
    );
    assert!(out.output.contains("a.rs"), "{}", out.output);
    assert!(
        out.output.contains('-') && out.output.contains('+'),
        "no diff shown: {}",
        out.output
    );
}

/// After rewriting, the tag must describe what is now on disk. A stale tag
/// would tell the model its snapshot is current when the file has changed
/// underneath it, which is the exact failure hashline exists to prevent.
#[tokio::test]
async fn ast_edit_leaves_a_tag_matching_the_file_it_just_wrote() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let context = ctx(&temp);
    AstEditTool::new()
        .execute(
            json!({"pattern": "one()", "replacement": "two()"}),
            context.clone(),
        )
        .await
        .expect("rewrite");

    let store = super::super::hashline_store::for_session(&context.session_id);
    let snapshot = store.head("a.rs").expect("a tag was recorded");
    let on_disk = fs::read_to_string(temp.path().join("a.rs")).expect("read");
    assert_eq!(
        snapshot.text, on_disk,
        "the recorded snapshot does not match what is on disk"
    );
}

#[tokio::test]
async fn ast_edit_rewrites_across_files() {
    let temp = tree(&[
        ("a.rs", "fn a() { one(); }\n"),
        ("b.rs", "fn b() { one(); }\n"),
    ]);
    let out = AstEditTool::new()
        .execute(
            json!({"pattern": "one()", "replacement": "two()"}),
            ctx(&temp),
        )
        .await
        .expect("rewrite");

    for name in ["a.rs", "b.rs"] {
        assert!(
            fs::read_to_string(temp.path().join(name))
                .expect("read")
                .contains("two()"),
            "{name} was not rewritten"
        );
    }
    // Both files must be shown. A renderer that reports only the first is the
    // multi-file bug that hid hashline patches three times over.
    assert!(out.output.contains("a.rs"), "{}", out.output);
    assert!(out.output.contains("b.rs"), "{}", out.output);
}

#[tokio::test]
async fn ast_edit_writes_nothing_when_nothing_matched() {
    let temp = tree(&[("a.rs", "fn a() { other(); }\n")]);
    let out = AstEditTool::new()
        .execute(
            json!({"pattern": "one()", "replacement": "two()"}),
            ctx(&temp),
        )
        .await
        .expect("rewrite");

    assert!(out.output.contains("No changes"), "{}", out.output);
    assert_eq!(
        fs::read_to_string(temp.path().join("a.rs")).expect("read"),
        "fn a() { other(); }\n"
    );
}

#[tokio::test]
async fn ast_edit_requires_both_a_pattern_and_a_replacement() {
    let temp = tree(&[("a.rs", "fn a() {}\n")]);
    let error = AstEditTool::new()
        .execute(json!({"pattern": "one()"}), ctx(&temp))
        .await
        .expect_err("no replacement");

    assert!(error.to_string().contains("replacement"), "{error}");
}

/// `ast_edit` writes, so it must never be auto-allowed. `ast_grep` is read-only
/// and should be, or every structural search interrupts the user.
#[test]
fn only_the_read_only_tool_is_auto_allowed() {
    let safety = jcode_base::safety::SafetySystem::new();
    assert_eq!(
        safety.classify("ast_grep"),
        jcode_base::safety::ActionTier::AutoAllowed
    );
    assert_eq!(
        safety.classify("ast_edit"),
        jcode_base::safety::ActionTier::RequiresPermission,
        "a tool that rewrites files across a repo must go through approval"
    );
}

/// A rewrite that reformats must say so. The live agent run hit this on its
/// first multi-line call, noticed the dangling comma in the diff and had to
/// fix it by hand; the tool should have told it up front.
#[tokio::test]
async fn ast_edit_reports_when_it_reflows_code() {
    let temp = tree(&[(
        "a.rs",
        "fn other() {\n    log(\n        \"wrapped\",\n    );\n}\n",
    )]);
    let out = AstEditTool::new()
        .execute(
            json!({"pattern": "log($$$A)", "replacement": "trace($$$A)"}),
            ctx(&temp),
        )
        .await
        .expect("rewrite");

    assert!(
        out.output.contains("reflowed"),
        "reformatting was not disclosed: {}",
        out.output
    );
}

/// And an ordinary rename must not carry the warning, or it becomes noise that
/// stops being read.
#[tokio::test]
async fn an_ordinary_rewrite_carries_no_reflow_warning() {
    let temp = tree(&[("a.rs", "fn a() { log(x); }\n")]);
    let out = AstEditTool::new()
        .execute(
            json!({"pattern": "log($$$A)", "replacement": "trace($$$A)"}),
            ctx(&temp),
        )
        .await
        .expect("rewrite");

    assert!(!out.output.contains("reflowed"), "{}", out.output);
}
