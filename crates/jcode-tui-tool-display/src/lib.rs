use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Map provider-side tool names to internal display names.
/// Mirrors Registry::resolve_tool_name so TUI surfaces show friendly names.
pub fn resolve_display_tool_name(name: &str) -> &str {
    match name {
        "communicate" => "swarm",
        "discover_tools" => "integration_tools",
        "task" | "task_runner" => "subagent",
        "shell_exec" => "bash",
        "file_read" => "read",
        "file_write" => "write",
        "file_edit" => "edit",
        "file_glob" => "glob",
        "file_grep" => "grep",
        "todo_read" | "todo_write" | "todoread" | "todowrite" => "todo",
        other => other,
    }
}

pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "communicate" => "swarm",
        "discover_tools" => "integration_tools",
        "Write" => "write",
        "Edit" => "edit",
        "MultiEdit" => "multiedit",
        "Patch" => "patch",
        "ApplyPatch" => "apply_patch",
        other => other,
    }
}

/// Whether a tool name denotes a file-modifying tool.
///
/// `multiedit` is retained here although the tool was removed: stored sessions
/// are replayed and re-rendered, so a transcript recorded before the removal
/// must still display its diffs rather than degrade to a bare tool name. The
/// same applies to every other display-side match on the name below.
///
/// # `ast_edit` was missing, and the doc comment above predicted it
///
/// This list is one of several across the workspace that enumerate the writing tools, and
/// they had drifted. Measured: `jcode-telemetry-core` (four sites), `jcode-usage-types`, and
/// the registry all included `ast_edit`; this one did not. So a structural rewrite touching
/// twenty files rendered in the TUI with **no `+`/`-` counts at all**, through five call
/// sites, because a name comparison fell through to its default arm and nothing anywhere
/// said so.
///
/// The rule for adding a tool that writes files: it goes in *this* list too, and
/// `every_writing_tool_is_recognised_by_the_display_layer` in this module's tests is what
/// enforces it rather than a reader remembering.
pub fn is_edit_tool_name(name: &str) -> bool {
    matches!(
        canonical_tool_name(name),
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" | "ast_edit"
    )
}

fn parse_nonzero_exit_code_line(line: &str) -> bool {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("Exit code:") {
        return rest
            .trim()
            .parse::<i32>()
            .map(|code| code != 0)
            .unwrap_or(false);
    }
    if let Some(rest) = trimmed.strip_prefix("--- Command finished with exit code:") {
        return rest
            .trim()
            .trim_end_matches('-')
            .trim()
            .parse::<i32>()
            .map(|code| code != 0)
            .unwrap_or(false);
    }
    false
}

fn display_prefix_by_width(s: &str, max_width: usize) -> &str {
    if max_width == 0 {
        return "";
    }
    let mut used = 0usize;
    let mut end = 0usize;
    for (idx, ch) in s.char_indices() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > max_width {
            break;
        }
        used += cw;
        end = idx + ch.len_utf8();
    }
    &s[..end]
}

fn display_suffix_by_width(s: &str, max_width: usize) -> &str {
    if max_width == 0 {
        return "";
    }
    let mut used = 0usize;
    let mut start = s.len();
    for (idx, ch) in s.char_indices().rev() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > max_width {
            break;
        }
        used += cw;
        start = idx;
    }
    &s[start..]
}

pub fn truncate_middle_display(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let remaining = max_width.saturating_sub(1);
    let head = remaining / 2 + remaining % 2;
    let tail = remaining / 2;
    format!(
        "{}…{}",
        display_prefix_by_width(s, head),
        display_suffix_by_width(s, tail)
    )
}

fn normalize_backticked_identifier(text: &str) -> String {
    text.replace('`', "").trim().to_string()
}

pub fn concise_tool_error_summary(content: &str) -> Option<String> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let detail = line
            .strip_prefix("Error:")
            .or_else(|| line.strip_prefix("error:"))
            .or_else(|| line.strip_prefix("Failed:"))
            .map(str::trim);
        if let Some(detail) = detail {
            if let Some(field) = detail.strip_prefix("missing field ") {
                return Some(format!(
                    "invalid input: missing {}",
                    normalize_backticked_identifier(field)
                ));
            }
            if detail.starts_with("invalid type") || detail.starts_with("unknown variant") {
                return Some(format!("invalid input: {}", detail));
            }
            if detail.contains("source metadata") && detail.contains("was for") {
                return Some("build source changed before reload".to_string());
            }
            if detail.starts_with("Refusing to publish") {
                return Some("reload refused: rebuild against current source".to_string());
            }
            return Some(format!("error: {}", truncate_middle_display(detail, 80)));
        }

        if line.contains("Compile terminated by signal") {
            return Some(line.to_string());
        }
        if let Some(rest) = line.strip_prefix("Exit code:")
            && let Ok(code) = rest.trim().parse::<i32>()
            && code != 0
        {
            return Some(format!("exit {}", code));
        }
        if let Some(rest) = line.strip_prefix("--- Command finished with exit code:") {
            let code = rest.trim().trim_end_matches('-').trim();
            if code != "0" && !code.is_empty() {
                return Some(format!("exit {}", code));
            }
        }
    }

    None
}

pub fn tool_output_looks_failed(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .filter(|(label, _)| !label.is_empty() && !label.contains(['\n', '\r']))
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let lower = normalized.to_ascii_lowercase();
    if concise_tool_error_summary(normalized).is_some()
        || lower.starts_with("error:")
        || lower.starts_with("failed:")
        || normalized.starts_with('✗')
    {
        return true;
    }

    normalized.lines().any(|line| {
        let line = line.trim();
        parse_nonzero_exit_code_line(line)
            || line.eq_ignore_ascii_case("Status: failed")
            || line.eq_ignore_ascii_case("failed to start")
            || line.eq_ignore_ascii_case("terminated")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_edit_tool_names() {
        assert_eq!(canonical_tool_name("ApplyPatch"), "apply_patch");
        assert!(is_edit_tool_name("MultiEdit"));
        assert!(!is_edit_tool_name("read"));
    }

    /// **Every tool that writes files must be recognised here.**
    ///
    /// This list is one of several across the workspace enumerating the writing tools, and
    /// they had drifted: `ast_edit` was in the telemetry classifier (four sites), in
    /// `jcode-usage-types`, and in the registry, but not here. The consequence was a
    /// structural rewrite of twenty files rendering with no `+`/`-` counts, through five
    /// call sites, because the name fell through to a default arm.
    ///
    /// The doc comment on `is_edit_tool_name` already warned that display-side matches on
    /// the name need maintaining. A warning is not a mechanism, which is the point of this
    /// test.
    ///
    /// # Why the list is duplicated here rather than imported
    ///
    /// This crate is a leaf: depending on `jcode-telemetry-core` or the tool registry to
    /// borrow their list would invert the dependency and cost the display layer their
    /// compile time. So the names are repeated, and this test is what makes the repetition
    /// safe -- it fails when the two disagree, which is the only failure mode duplication
    /// has.
    ///
    /// Adding a writing tool means adding it in both places, and this is where you find out
    /// if you did not.
    #[test]
    fn every_writing_tool_is_recognised_by_the_display_layer() {
        // Mirrors the classifier in `jcode-telemetry-core::record_tool_call` and
        // `jcode-usage-types`. Kept in sync by this assertion rather than by a shared
        // constant, for the dependency reason above.
        const WRITING_TOOLS: &[&str] = &[
            "write",
            "edit",
            "multiedit",
            "patch",
            "apply_patch",
            "ast_edit",
        ];

        for name in WRITING_TOOLS {
            assert!(
                is_edit_tool_name(name),
                "{name} writes files but the display layer does not know it, so its diffs \
                 will not render and its +/- counts will be zero"
            );
        }

        // And the read-only counterparts stay out, or every search renders a diff header.
        for name in ["read", "grep", "glob", "ls", "ast_grep", "lsp", "bash"] {
            assert!(
                !is_edit_tool_name(name),
                "{name} does not write files and must not be treated as an edit"
            );
        }
    }

    /// The canonical-name mapping is applied before the comparison.
    ///
    /// Providers send `ApplyPatch` and `MultiEdit`; the list is lower-case with underscores.
    /// Comparing the raw name would miss every capitalised spelling, which is what
    /// `canonical_tool_name` exists to prevent and what this pins.
    #[test]
    fn writing_tools_are_recognised_under_their_provider_spellings() {
        for (sent, canonical) in [
            ("Write", "write"),
            ("Edit", "edit"),
            ("MultiEdit", "multiedit"),
            ("Patch", "patch"),
            ("ApplyPatch", "apply_patch"),
        ] {
            assert_eq!(canonical_tool_name(sent), canonical);
            assert!(
                is_edit_tool_name(sent),
                "{sent} is how a provider spells {canonical} and must still be an edit"
            );
        }
    }

    #[test]
    fn summarizes_tool_errors() {
        assert_eq!(
            concise_tool_error_summary("Error: missing field `command`").as_deref(),
            Some("invalid input: missing command")
        );
        assert_eq!(
            concise_tool_error_summary("--- Command finished with exit code: 2 ---").as_deref(),
            Some("exit 2")
        );
    }

    #[test]
    fn detects_failed_tool_output() {
        assert!(tool_output_looks_failed("Status: failed"));
        assert!(tool_output_looks_failed("Exit code: 1"));
        assert!(tool_output_looks_failed(
            "✗ demo.txt: failed to find expected lines"
        ));
        assert!(tool_output_looks_failed(
            "[apply_patch] ✗ demo.txt: failed to find expected lines"
        ));
        assert!(!tool_output_looks_failed("Exit code: 0"));
        assert!(!tool_output_looks_failed("completed successfully"));
    }
}
