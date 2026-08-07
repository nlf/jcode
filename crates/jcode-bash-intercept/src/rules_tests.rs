//! Behaviour spec for interception rules.
//!
//! Rules from oh-my-pi's `DEFAULT_BASH_INTERCEPTOR_RULES`. The cases that
//! matter are the ones where interception would be *wrong*: a piped read, a
//! command this cannot tokenize, a tool that is not registered.

use super::*;

const ALL: &[&str] = &["read", "grep", "glob", "edit", "write"];

fn decide(command: &str) -> Decision {
    check(command, ALL, DEFAULT_RULES)
}

fn blocked_tool(command: &str) -> Option<String> {
    match decide(command) {
        Decision::Block { tool, .. } => Some(tool),
        Decision::Allow => None,
    }
}

#[test]
fn file_readers_are_redirected_to_read() {
    for command in ["cat f.txt", "head -20 f.txt", "tail -f log", "less f.txt"] {
        assert_eq!(
            blocked_tool(command).as_deref(),
            Some("read"),
            "{command:?} should be redirected"
        );
    }
}

#[test]
fn searchers_are_redirected_to_grep() {
    for command in ["grep needle f.txt", "rg needle", "ag needle", "ack needle"] {
        assert_eq!(blocked_tool(command).as_deref(), Some("grep"), "{command:?}");
    }
}

#[test]
fn file_finders_are_redirected_to_glob() {
    assert_eq!(
        blocked_tool("find . -name '*.rs'").as_deref(),
        Some("glob")
    );
    assert_eq!(blocked_tool("fd --type f").as_deref(), Some("glob"));
}

/// A bare `find src` is a directory listing, not a pattern search, and glob is
/// not obviously better at it.
#[test]
fn a_find_without_a_pattern_flag_is_left_alone() {
    assert_eq!(decide("find src"), Decision::Allow);
}

#[test]
fn in_place_editors_are_redirected_to_edit() {
    for command in [
        "sed -i 's/a/b/' f.txt",
        "sed --in-place 's/a/b/' f.txt",
        "perl -pi -e 's/a/b/' f.txt",
        "awk -i inplace '{print}' f.txt",
    ] {
        assert_eq!(blocked_tool(command).as_deref(), Some("edit"), "{command:?}");
    }
}

/// A non-mutating sed is a filter, and no file tool replaces it.
#[test]
fn a_read_only_sed_is_left_alone() {
    assert_eq!(decide("sed 's/a/b/' f.txt"), Decision::Allow);
    // `build.sh | sed ...` blocks on nothing: the sed reads stdin, and the
    // first stage is not a redirectable command either.
    assert_eq!(decide("build.sh | sed 's/a/b/'"), Decision::Allow);
}

/// `cat f | sed ...` DOES block, on the cat rather than the sed. The cat is
/// redirectable even though the pipeline as a whole is not, and pointing at
/// `read` is the right answer: the model wanted the file's contents.
#[test]
fn a_redirectable_first_stage_blocks_even_when_piped_into_a_filter() {
    assert_eq!(
        blocked_tool("cat f.txt | sed 's/a/b/'").as_deref(),
        Some("read")
    );
}

/// The whole reason the tokenizer exists: a redirectable command hidden behind
/// another one must still be caught.
#[test]
fn a_command_in_a_later_segment_is_still_caught() {
    assert_eq!(blocked_tool("cd src && cat f.txt").as_deref(), Some("read"));
    assert_eq!(blocked_tool("mkdir x; grep needle f").as_deref(), Some("grep"));
}

/// `FOO=1 cat x` is a cat call. Matching only the leading word would miss it.
#[test]
fn leading_assignments_do_not_hide_a_command() {
    assert_eq!(blocked_tool("FOO=1 cat f.txt").as_deref(), Some("read"));
    assert_eq!(
        blocked_tool("A=1 B=2 grep needle f").as_deref(),
        Some("grep")
    );
}

/// A segment reading piped stdin cannot be replaced by a path-based tool.
/// Blocking it would leave the caller with no way to do the thing at all.
#[test]
fn a_piped_stage_is_not_intercepted() {
    assert_eq!(decide("ps aux | grep firefox"), Decision::Allow);
    assert_eq!(decide("build.sh | tail -20"), Decision::Allow);
}

/// The first stage of a pipeline reads a file, so it is still redirectable.
#[test]
fn the_first_stage_of_a_pipeline_is_still_intercepted() {
    assert_eq!(
        blocked_tool("cat f.txt | grep needle").as_deref(),
        Some("read")
    );
}

/// A tool that is not registered cannot be suggested: the caller would be left
/// with a refusal and nowhere to go.
#[test]
fn a_rule_whose_tool_is_missing_does_not_fire() {
    assert_eq!(check("cat f.txt", &["grep"], DEFAULT_RULES), Decision::Allow);
    assert_eq!(check("cat f.txt", &[], DEFAULT_RULES), Decision::Allow);
}

/// Commands the tokenizer refuses to read still get matched as a whole, so a
/// bare `cat` inside an unparseable line is caught.
#[test]
fn an_untokenizable_command_is_still_matched_whole() {
    assert_eq!(
        blocked_tool("cat $(ls | head -1)").as_deref(),
        Some("read"),
        "the leading cat is visible even though the substitution is not"
    );
}

/// Ordinary commands with no better tool are untouched. Over-blocking is worse
/// than under-blocking: it makes the shell unusable for its actual job.
#[test]
fn unrelated_commands_are_allowed() {
    for command in [
        "cargo test",
        "git status",
        "ls -la",
        "echo hello",
        "npm run build",
        "docker ps",
    ] {
        assert_eq!(decide(command), Decision::Allow, "{command:?}");
    }
}

/// A word merely containing a tool name is not that tool.
#[test]
fn a_command_that_merely_contains_a_name_is_not_intercepted() {
    assert_eq!(decide("cargo build --features cat"), Decision::Allow);
    assert_eq!(decide("./concatenate.sh"), Decision::Allow);
    assert_eq!(decide("git log --grep=fix"), Decision::Allow);
}

/// The message has to say what the tool gives, not just what is forbidden: an
/// instruction to obey is weaker than a reason to prefer.
#[test]
fn the_refusal_explains_what_the_tool_gives() {
    let Decision::Block { message, .. } = decide("cat f.txt") else {
        panic!("should be blocked");
    };

    assert!(message.contains("numbered lines"), "{message}");
    assert!(
        message.contains("Original command: cat f.txt"),
        "the refusal should quote what was attempted: {message}"
    );
}

/// A malformed rule is skipped rather than failing the command: a config
/// mistake should not make bash unusable.
#[test]
fn a_malformed_rule_is_skipped() {
    const BAD: &[Rule] = &[Rule {
        pattern: "(unclosed",
        tool: "read",
        message: "never seen",
    }];
    assert_eq!(check("cat f.txt", ALL, BAD), Decision::Allow);
}

#[test]
fn every_default_rule_has_a_valid_pattern_and_a_reason() {
    for rule in DEFAULT_RULES {
        assert!(
            regex::Regex::new(rule.pattern).is_ok(),
            "rule for {} has an invalid pattern: {}",
            rule.tool,
            rule.pattern
        );
        assert!(
            rule.message.contains(rule.tool),
            "the message should name the tool: {}",
            rule.message
        );
        assert!(
            rule.message.len() > 40,
            "a bare prohibition is not a reason: {}",
            rule.message
        );
    }
}
