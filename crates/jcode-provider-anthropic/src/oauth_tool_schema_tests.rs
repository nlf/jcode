//! Regression coverage for the curated Anthropic OAuth tool schemas.
//!
//! The OAuth (subscription) endpoint expects Claude-Code builtin tool *names*,
//! so `format_tools` hand-maintains a curated definition for a few of them.
//! Hand-maintained schemas drift from the real tools they stand in for, and the
//! failure is invisible until a model calls the tool and the handler rejects
//! the arguments. These tests pin the two drifts that reached users.

use super::*;
use jcode_message_types::ToolDefinition;
use serde_json::json;

fn tool_def(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("{name} description"),
        input_schema: json!({"type":"object","properties":{}}),
    }
}

#[test]
fn oauth_schedule_wakeup_forwards_the_real_schedule_schema() {
    // Regression for #706: the curated ScheduleWakeup definition advertised
    // delaySeconds/reason/prompt while the real `schedule` handler requires
    // `task`, so every call failed with "task is required for action=create".
    let real_schema = json!({
        "type": "object",
        "properties": {
            "action": {"type": "string"},
            "task": {"type": "string"},
            "wake_in_minutes": {"type": "integer"}
        },
        "required": ["intent"]
    });
    let registry = vec![ToolDefinition {
        name: "schedule".to_string(),
        description: "Schedule, list, or cancel future tasks.".to_string(),
        input_schema: real_schema.clone(),
    }];

    let formatted = format_tools(&registry, true, false);
    let scheduled = formatted
        .iter()
        .find(|t| t.name == "ScheduleWakeup")
        .expect("schedule must be advertised under its OAuth name");

    let props = scheduled.input_schema["properties"]
        .as_object()
        .expect("object schema");
    assert!(props.contains_key("task"), "{props:?}");
    assert!(
        !props.contains_key("delaySeconds"),
        "fabricated schema leaked back in: {props:?}"
    );
    assert_eq!(
        formatted
            .iter()
            .filter(|t| t.name == "ScheduleWakeup")
            .count(),
        1,
        "schedule must not be advertised twice"
    );
}

#[test]
fn oauth_bash_schema_advertises_the_justification_escape_hatch() {
    // Regression for #722: the destructive gate consumes `justification`,
    // so it has to be discoverable in the advertised schema.
    let formatted = format_tools(&[tool_def("bash")], true, false);
    let bash = formatted
        .iter()
        .find(|t| t.name == "Bash")
        .expect("Bash must be advertised");
    assert!(
        bash.input_schema["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("justification")),
        "{:?}",
        bash.input_schema
    );
}

/// `Glob` and `Grep` must reach the model now that local tools back them.
///
/// Before the `grep`/`glob` adapters existed, `has_backing` silently dropped
/// both curated definitions, so a model with strong Claude-Code priors found
/// its familiar search tools absent and fell back to `Bash` plus ripgrep for
/// the rest of the session (NLFCODE.md item 1).
#[test]
fn oauth_advertises_glob_and_grep_when_local_tools_back_them() {
    let registry = vec![tool_def("grep"), tool_def("glob")];
    let formatted = format_tools(&registry, true, false);
    let names: Vec<&str> = formatted.iter().map(|t| t.name.as_str()).collect();

    assert!(names.contains(&"Grep"), "Grep must be advertised: {names:?}");
    assert!(names.contains(&"Glob"), "Glob must be advertised: {names:?}");

    // The adapters stand in for the curated builtins, so they must not also be
    // forwarded under their local names: two tools doing the same job splits
    // the model's choice for no benefit.
    assert!(
        !names.contains(&"grep") && !names.contains(&"glob"),
        "adapters must not be advertised twice: {names:?}"
    );
}

/// The backing check must still work: advertising a tool with nothing behind
/// it resolves to "Unknown tool" at call time (#572).
#[test]
fn oauth_still_drops_curated_builtins_with_no_backing() {
    let formatted = format_tools(&[tool_def("read")], true, false);
    let names: Vec<&str> = formatted.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"Read"), "{names:?}");
    assert!(
        !names.contains(&"Grep") && !names.contains(&"Glob"),
        "unbacked builtins must not be advertised: {names:?}"
    );
}

/// Curation must not throw away a richer local description.
///
/// The curated strings are Claude-Code's terse stubs ("A powerful search tool
/// built on ripgrep"). The local descriptions carry the tool-selection
/// guidance that steers a model off bash, and that guidance is worthless if
/// curation overwrites it on the OAuth path.
#[test]
fn curated_builtins_inherit_a_richer_local_description() {
    let detailed = "Search file contents by regex across the workspace. Use this instead of \
                    grep or rg through bash: it respects ignore files and returns readable \
                    regions rather than raw lines.";
    let registry = vec![ToolDefinition {
        name: "grep".to_string(),
        description: detailed.to_string(),
        input_schema: json!({"type":"object","properties":{}}),
    }];

    let formatted = format_tools(&registry, true, false);
    let grep = formatted
        .iter()
        .find(|t| t.name == "Grep")
        .expect("Grep must be advertised");

    assert_eq!(
        grep.description, detailed,
        "the local description must survive curation"
    );
    // The schema stays curated: the OAuth endpoint expects the builtin shape.
    assert!(
        grep.input_schema["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("pattern")),
        "curated schema must be kept: {:?}",
        grep.input_schema
    );
}

/// A terse local description must not replace a better curated one.
#[test]
fn curation_keeps_the_better_description_when_local_is_terse() {
    let formatted = format_tools(&[tool_def("grep")], true, false);
    let grep = formatted.iter().find(|t| t.name == "Grep").unwrap();
    assert_eq!(
        grep.description, "A powerful search tool built on ripgrep.",
        "a stub local description must not win"
    );
}
