//! Deciding when a bash command should be refused in favour of a tool.
//!
//! Ported from oh-my-pi's `src/tools/bash-interceptor.ts` and their
//! `DEFAULT_BASH_INTERCEPTOR_RULES`, behaviour-first.

use crate::tokenize::{segments, without_leading_assignments};
use regex::Regex;

/// A command shape and the tool that does it better.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Matched against the command, anchored at its start.
    pub pattern: &'static str,
    /// Tool to suggest. Only fires when that tool is actually registered.
    pub tool: &'static str,
    /// Why the tool is better, in terms the model can act on.
    pub message: &'static str,
}

/// The default rules, from omp.
///
/// Each names a *capability* the shell command lacks rather than just
/// forbidding it: a model told "use read" without a reason has been given an
/// instruction to obey, and one told what it gains has been given a reason to
/// prefer.
pub const DEFAULT_RULES: &[Rule] = &[
    Rule {
        pattern: r"^\s*(grep|rg|ripgrep|ag|ack)\s+",
        tool: "grep",
        message: "Use the `grep` tool instead of grep/rg. It respects .gitignore, ranks results, and returns editable line anchors.",
    },
    Rule {
        pattern: r"^\s*(find|fd|locate)\s+.*(-name|-iname|-type|--type|-glob)",
        tool: "glob",
        message: "Use the `glob` tool instead of find/fd. It respects .gitignore and skips vendored directories.",
    },
    Rule {
        pattern: r"^\s*sed\s+(-i|--in-place)",
        tool: "edit",
        message: "Use the `edit` tool instead of sed -i. It verifies it is changing the content you read, and shows a diff.",
    },
    Rule {
        pattern: r"^\s*perl\s+.*-[pn]?i",
        tool: "edit",
        message: "Use the `edit` tool instead of perl -i. It verifies it is changing the content you read, and shows a diff.",
    },
    Rule {
        pattern: r"^\s*awk\s+.*-i\s+inplace",
        tool: "edit",
        message: "Use the `edit` tool instead of awk -i inplace. It verifies it is changing the content you read, and shows a diff.",
    },
];

/// Readers that only warrant interception when given a file to read.
const FILE_READERS: &[&str] = &["cat", "head", "tail", "less", "more"];

/// Flags that take a separate value, so the value is not a path.
const VALUE_FLAGS: &[&str] = &["-n", "-c", "-q", "--lines", "--bytes"];

/// Whether `command` is a reader invoked on a file.
///
/// Expressed as a predicate rather than a regex because the distinction is
/// "has an argument that is a path, not a flag and not a flag's value", and a
/// regex for that is either wrong or unreadable. Two attempts at one were wrong
/// in different ways: `head -n1` (stdin) matched, then `head -n 5` did.
///
/// This matters because a reader on stdin cannot be replaced by a path-based
/// tool at all, so blocking it leaves no way to do the thing.
fn reads_a_file(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    // A path-qualified invocation is still the same program.
    let program = program.rsplit('/').next().unwrap_or(program);
    if !FILE_READERS.contains(&program) {
        return false;
    }

    let mut skip_next = false;
    for word in words {
        if skip_next {
            skip_next = false;
            continue;
        }
        if VALUE_FLAGS.contains(&word) {
            skip_next = true;
            continue;
        }
        // `-n1`, `+5`, `--lines=3`: flag and value in one word.
        if word.starts_with('-') || word.starts_with('+') {
            continue;
        }
        // `cat -` is explicitly stdin.
        return word != "-";
    }
    false
}

/// What to do with a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it.
    Allow,
    /// Refuse, and say which tool to use instead.
    Block { message: String, tool: String },
}

impl Decision {
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Block { .. })
    }
}

/// The strings a rule is tested against.
///
/// The whole command, plus each non-piped segment, plus each segment with its
/// leading assignments stripped. Testing only the whole command would miss
/// `cd src && cat x`; testing only segments would miss a command this cannot
/// tokenize.
fn candidates(command: &str) -> Vec<String> {
    let mut candidates = vec![command.trim().to_string()];
    for segment in segments(command) {
        // A segment reading piped stdin cannot be replaced by a path-based
        // tool, so it is not a candidate however much it looks like one.
        if segment.piped_stdin {
            continue;
        }
        if let Some(stripped) = without_leading_assignments(&segment.text) {
            candidates.push(stripped);
        }
        candidates.push(segment.text);
    }
    candidates
}

/// Decide whether `command` should be refused.
///
/// `available_tools` gates every rule: suggesting a tool that is not registered
/// leaves the caller with a refusal and no way forward.
pub fn check(command: &str, available_tools: &[&str], rules: &[Rule]) -> Decision {
    let candidates = candidates(command);

    // The reader rule is a predicate rather than a pattern; see `reads_a_file`.
    if available_tools.contains(&"read")
        && candidates.iter().any(|candidate| reads_a_file(candidate))
    {
        return Decision::Block {
            message: format!(
                "Blocked: Use the `read` tool instead of cat/head/tail. It returns \
                 numbered lines, which is what makes the file editable, and handles \
                 binary and PDF files.\n\nOriginal command: {command}"
            ),
            tool: "read".to_string(),
        };
    }

    for rule in rules {
        if !available_tools.contains(&rule.tool) {
            continue;
        }
        let Ok(regex) = Regex::new(rule.pattern) else {
            // A malformed rule is skipped rather than failing the command: a
            // configuration mistake should not make bash unusable.
            continue;
        };
        for candidate in &candidates {
            if regex.is_match(candidate) {
                return Decision::Block {
                    message: format!(
                        "Blocked: {}\n\nOriginal command: {command}",
                        rule.message
                    ),
                    tool: rule.tool.to_string(),
                };
            }
        }
    }

    Decision::Allow
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod rules_tests;
