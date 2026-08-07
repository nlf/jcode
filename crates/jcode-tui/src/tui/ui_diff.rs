use crate::{message::ToolCall, tui::ui::tools_ui};
use ratatui::prelude::*;

pub(super) fn diff_add_color() -> Color {
    Color::Rgb(100, 200, 100)
}

pub(super) fn diff_del_color() -> Color {
    Color::Rgb(200, 100, 100)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffLineKind {
    Add,
    Del,
}

#[derive(Clone, Debug)]
pub(super) struct ParsedDiffLine {
    pub kind: DiffLineKind,
    pub prefix: String,
    pub content: String,
}

pub(super) fn diff_change_counts(content: &str) -> (usize, usize) {
    let lines = collect_diff_lines(content);
    let additions = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Add)
        .count();
    let deletions = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Del)
        .count();
    (additions, deletions)
}

pub(super) fn diff_change_counts_for_tool(tool: &ToolCall, content: &str) -> (usize, usize) {
    let (additions, deletions) = diff_change_counts(content);
    if additions > 0 || deletions > 0 {
        return (additions, deletions);
    }

    match tools_ui::canonical_tool_name(&tool.name) {
        "edit" => {
            diff_counts_from_input_pair(&tool.input, "old_string", "new_string").unwrap_or((0, 0))
        }
        "write" => {
            let content = tool
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            diff_counts_from_strings("", content)
        }
        "multiedit" => diff_counts_from_multiedit(&tool.input).unwrap_or((0, 0)),
        "patch" => diff_counts_from_unified_patch_input(&tool.input).unwrap_or((0, 0)),
        "apply_patch" => diff_counts_from_apply_patch_input(&tool.input).unwrap_or((0, 0)),
        _ => (additions, deletions),
    }
}

fn diff_counts_from_input_pair(
    input: &serde_json::Value,
    old_key: &str,
    new_key: &str,
) -> Option<(usize, usize)> {
    let old = input.get(old_key)?.as_str()?;
    let new = input.get(new_key)?.as_str()?;
    Some(diff_counts_from_strings(old, new))
}

fn diff_counts_from_multiedit(input: &serde_json::Value) -> Option<(usize, usize)> {
    let edits = input.get("edits")?.as_array()?;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for edit in edits {
        let old = edit
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = edit
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if old.is_empty() && new.is_empty() {
            continue;
        }
        let (add, del) = diff_counts_from_strings(old, new);
        additions += add;
        deletions += del;
    }

    Some((additions, deletions))
}

fn diff_counts_from_unified_patch_input(input: &serde_json::Value) -> Option<(usize, usize)> {
    let patch_text = input.get("patch_text")?.as_str()?;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for line in patch_text.lines() {
        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("@@")
            || line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }

    Some((additions, deletions))
}

fn diff_counts_from_apply_patch_input(input: &serde_json::Value) -> Option<(usize, usize)> {
    let patch_text = input.get("patch_text")?.as_str()?;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for line in patch_text.lines() {
        if line.starts_with("***") || line.starts_with("@@") {
            continue;
        }

        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }

    Some((additions, deletions))
}

fn diff_counts_from_strings(old: &str, new: &str) -> (usize, usize) {
    use similar::ChangeTag;

    let diff = similar::TextDiff::from_lines(old, new);
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    (additions, deletions)
}

pub(super) fn generate_diff_lines_from_tool_input(tool: &ToolCall) -> Vec<ParsedDiffLine> {
    match tools_ui::canonical_tool_name(&tool.name) {
        "edit" => {
            let old = tool
                .input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = tool
                .input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            generate_diff_lines_from_strings(old, new)
        }
        "multiedit" => {
            let Some(edits) = tool.input.get("edits").and_then(|v| v.as_array()) else {
                return Vec::new();
            };
            let mut all_lines = Vec::new();
            for edit in edits {
                let old = edit
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new = edit
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                all_lines.extend(generate_diff_lines_from_strings(old, new));
            }
            all_lines
        }
        "write" => {
            let content = tool
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            generate_diff_lines_from_strings("", content)
        }
        "patch" => {
            let patch_text = tool
                .input
                .get("patch_text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            collect_diff_lines(patch_text)
        }
        "apply_patch" => {
            let patch_text = tool
                .input
                .get("patch_text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            collect_diff_lines(patch_text)
        }
        _ => Vec::new(),
    }
}

fn generate_diff_lines_from_strings(old: &str, new: &str) -> Vec<ParsedDiffLine> {
    use similar::ChangeTag;

    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        let content = change.value().trim_end_matches(['\n', '\r']);
        if content.trim().is_empty() {
            continue;
        }

        match change.tag() {
            ChangeTag::Delete => {
                lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Del,
                    prefix: format!("{}- ", change.old_index().unwrap_or(0) + 1),
                    content: content.to_string(),
                });
            }
            ChangeTag::Insert => {
                lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Add,
                    prefix: format!("{}+ ", change.new_index().unwrap_or(0) + 1),
                    content: content.to_string(),
                });
            }
            ChangeTag::Equal => {}
        }
    }

    strip_common_indent(&mut lines);
    pad_line_numbers(&mut lines);
    lines
}

pub(super) fn collect_diff_lines(content: &str) -> Vec<ParsedDiffLine> {
    let mut lines: Vec<ParsedDiffLine> = content.lines().filter_map(parse_diff_line).collect();
    strip_common_indent(&mut lines);
    pad_line_numbers(&mut lines);
    lines
}

fn parse_diff_line(raw_line: &str) -> Option<ParsedDiffLine> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() || trimmed == "..." {
        return None;
    }
    if trimmed.starts_with("diff --git ")
        || trimmed.starts_with("index ")
        || trimmed.starts_with("--- ")
        || trimmed.starts_with("+++ ")
        || trimmed.starts_with("@@ ")
        || trimmed.starts_with("\\ No newline")
    {
        return None;
    }

    if let Some(pos) = trimmed.find("- ") {
        let (prefix, content) = trimmed.split_at(pos + 2);
        if !prefix.is_empty() && prefix[..pos].chars().all(|c| c.is_ascii_digit()) {
            return Some(ParsedDiffLine {
                kind: DiffLineKind::Del,
                prefix: prefix.to_string(),
                content: content.to_string(),
            });
        }
    }
    if let Some(pos) = trimmed.find("+ ") {
        let (prefix, content) = trimmed.split_at(pos + 2);
        if !prefix.is_empty() && prefix[..pos].chars().all(|c| c.is_ascii_digit()) {
            return Some(ParsedDiffLine {
                kind: DiffLineKind::Add,
                prefix: prefix.to_string(),
                content: content.to_string(),
            });
        }
    }

    if let Some(rest) = raw_line.strip_prefix('+') {
        return Some(ParsedDiffLine {
            kind: DiffLineKind::Add,
            prefix: "+".to_string(),
            content: rest.trim_end().to_string(),
        });
    }
    if let Some(rest) = raw_line.strip_prefix('-') {
        return Some(ParsedDiffLine {
            kind: DiffLineKind::Del,
            prefix: "-".to_string(),
            content: rest.trim_end().to_string(),
        });
    }

    None
}

const TAB_WIDTH: usize = 4;

/// Visual width of a line's leading whitespace, with tabs expanded, so that
/// mixed tab/space indentation does not yield a bogus minimum.
fn indent_width(content: &str) -> usize {
    let mut width = 0usize;
    for ch in content.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += TAB_WIDTH - (width % TAB_WIDTH),
            _ => break,
        }
    }
    width
}

/// Re-indent a line to `width` columns fewer than it had, normalizing leading
/// tabs to spaces so indentation renders predictably in the TUI.
fn strip_indent(content: &str, width: usize) -> String {
    let original = indent_width(content);
    let rest = content.trim_start_matches([' ', '\t']);
    let remaining = original.saturating_sub(width);
    let mut out = " ".repeat(remaining);
    out.push_str(rest);
    out
}

/// Split a prefix into its leading line number and the rest (`"12- "` becomes
/// `("12", "- ")`). Returns `None` when there is no number to align, as for the
/// bare `"+"` / `"-"` prefixes that carry no line number at all.
fn split_line_number(prefix: &str) -> Option<(&str, &str)> {
    let digits = prefix.len() - prefix.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    Some(prefix.split_at(digits))
}

/// Right-align line numbers to a common width, so content does not shift a
/// column when the number gains a digit (9 to 10, 99 to 100).
///
/// Per-hunk, not per-line: the width depends on the widest number in the set,
/// which a single line cannot know. Padding goes on the left so the digits stay
/// flush against their `+`/`-` marker.
fn pad_line_numbers(lines: &mut [ParsedDiffLine]) {
    let widest = lines
        .iter()
        .filter_map(|line| split_line_number(&line.prefix))
        .map(|(number, _)| number.len())
        .max()
        .unwrap_or(0);
    if widest == 0 {
        return;
    }

    for line in lines.iter_mut() {
        let Some((number, rest)) = split_line_number(&line.prefix) else {
            continue;
        };
        if number.len() >= widest {
            continue;
        }
        line.prefix = format!("{}{number}{rest}", " ".repeat(widest - number.len()));
    }
}

/// Strip the indentation common to every non-blank line, so a uniformly
/// indented hunk renders flush left while relative nesting is preserved.
fn strip_common_indent(lines: &mut [ParsedDiffLine]) {
    let common = lines
        .iter()
        .filter(|line| !line.content.trim().is_empty())
        .map(|line| indent_width(&line.content))
        .min()
        .unwrap_or(0);
    if common == 0 {
        return;
    }
    for line in lines.iter_mut() {
        if line.content.trim().is_empty() {
            line.content = String::new();
        } else {
            line.content = strip_indent(&line.content, common);
        }
    }
}

pub(super) fn tint_span_with_diff_color(span: Span<'static>, diff_color: Color) -> Span<'static> {
    let (dr, dg, db) = match diff_color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) => super::color_support::indexed_to_rgb(n),
        _ => return span,
    };

    let fg = span.style.fg.unwrap_or(Color::White);
    let (sr, sg, sb) = match fg {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) => super::color_support::indexed_to_rgb(n),
        Color::White => (255, 255, 255),
        Color::Black => (0, 0, 0),
        _ => return span,
    };

    let blend = |s: u8, d: u8| -> u8 { ((s as u16 * 70 + d as u16 * 30) / 100) as u8 };

    let tinted = Color::Rgb(blend(sr, dr), blend(sg, dg), blend(sb, db));
    Span::styled(span.content, span.style.fg(tinted))
}

#[cfg(test)]
mod tests {
    use super::{
        DiffLineKind, collect_diff_lines, diff_change_counts_for_tool,
        diff_counts_from_apply_patch_input, generate_diff_lines_from_strings,
    };
    use crate::message::ToolCall;
    use serde_json::json;

    #[test]
    fn apply_patch_counts_ignore_context_lines_with_plus_or_minus_prefixes() {
        let input = json!({
            "patch_text": "*** Begin Patch\n*** Update File: demo.txt\n@@\n  +context line\n  -context line\n+added line\n-deleted line\n*** End Patch\n"
        });

        assert_eq!(diff_counts_from_apply_patch_input(&input), Some((1, 1)));
    }

    #[test]
    fn write_tool_falls_back_to_content_diff_counts() {
        let tool = ToolCall {
            id: "tool_1".to_string(),
            name: "write".to_string(),
            input: json!({
                "file_path": "demo.txt",
                "content": "first line\nsecond line\n"
            }),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(diff_change_counts_for_tool(&tool, ""), (2, 0));
    }

    #[test]
    fn multiedit_pascal_case_falls_back_to_input_diff_counts() {
        let tool = ToolCall {
            id: "tool_2".to_string(),
            name: "MultiEdit".to_string(),
            input: json!({
                "file_path": "demo.txt",
                "edits": [
                    {"old_string": "two\n", "new_string": "TWO\n"},
                    {"old_string": "three\n", "new_string": "THREE\n"}
                ]
            }),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(diff_change_counts_for_tool(&tool, ""), (2, 2));
    }

    #[test]
    fn generated_diff_lines_use_old_and_new_line_numbers() {
        let lines =
            generate_diff_lines_from_strings("one\ntwo\nthree\n", "one\nthree\nfour\nfive\n");

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].kind, DiffLineKind::Del);
        assert_eq!(lines[0].prefix, "2- ");
        assert_eq!(lines[1].kind, DiffLineKind::Add);
        assert_eq!(lines[1].prefix, "3+ ");
        assert_eq!(lines[2].kind, DiffLineKind::Add);
        assert_eq!(lines[2].prefix, "4+ ");
    }

    #[test]
    fn uniformly_indented_hunk_renders_flush_left() {
        let lines = generate_diff_lines_from_strings("", "        one\n        two\n");

        assert_eq!(lines[0].content, "one");
        assert_eq!(lines[1].content, "two");
    }

    #[test]
    fn nested_indentation_keeps_relative_structure() {
        let lines =
            generate_diff_lines_from_strings("", "        if x {\n            body();\n        }\n");

        assert_eq!(lines[0].content, "if x {");
        assert_eq!(lines[1].content, "    body();");
        assert_eq!(lines[2].content, "}");
    }

    #[test]
    fn tab_indentation_is_measured_by_visual_width() {
        let lines = collect_diff_lines("+\tif x {\n+\t\tbody();\n+\t}\n");

        assert_eq!(lines[0].content, "if x {");
        assert_eq!(lines[1].content, "    body();");
        assert_eq!(lines[2].content, "}");
    }

    #[test]
    fn numbered_prefix_lines_keep_relative_indentation() {
        let lines = collect_diff_lines("12-     outer\n13+         inner\n");

        assert_eq!(lines[0].content, "outer");
        assert_eq!(lines[1].content, "    inner");
    }

    #[test]
    fn unindented_hunk_is_unchanged() {
        let lines = generate_diff_lines_from_strings("", "one\n    two\n");

        assert_eq!(lines[0].content, "one");
        assert_eq!(lines[1].content, "    two");
    }

    #[test]
    fn line_numbers_align_across_a_digit_boundary() {
        // 8 unchanged lines, then three added: numbers 9, 10, 11.
        let old = "a\n".repeat(8);
        let new = format!("{old}x\ny\nz\n");
        let lines = generate_diff_lines_from_strings(&old, &new);

        let prefixes: Vec<&str> = lines.iter().map(|l| l.prefix.as_str()).collect();
        assert_eq!(prefixes, vec![" 9+ ", "10+ ", "11+ "]);

        let widths: std::collections::HashSet<usize> =
            lines.iter().map(|l| l.prefix.chars().count()).collect();
        assert_eq!(widths.len(), 1, "every gutter must be the same width");
    }

    #[test]
    fn line_numbers_align_across_the_hundreds_boundary() {
        let old = "a\n".repeat(98);
        let new = format!("{old}x\ny\nz\n");
        let lines = generate_diff_lines_from_strings(&old, &new);

        let prefixes: Vec<&str> = lines.iter().map(|l| l.prefix.as_str()).collect();
        assert_eq!(prefixes, vec![" 99+ ", "100+ ", "101+ "]);
    }

    #[test]
    fn padding_goes_left_so_digits_stay_against_their_marker() {
        let old = "a\n".repeat(8);
        let new = format!("{old}x\ny\n");
        let lines = generate_diff_lines_from_strings(&old, &new);

        assert!(
            lines[0].prefix.starts_with(' ') && lines[0].prefix.contains("9+"),
            "the gap belongs before the number, not between it and the marker: {:?}",
            lines[0].prefix
        );
    }

    #[test]
    fn parsed_patch_line_numbers_are_aligned_too() {
        let lines = collect_diff_lines("9- old line\n10+ new line\n");

        assert_eq!(lines[0].prefix, " 9- ");
        assert_eq!(lines[1].prefix, "10+ ");
    }

    #[test]
    fn single_digit_hunks_gain_no_padding() {
        let lines = generate_diff_lines_from_strings("", "one\ntwo\n");

        assert_eq!(lines[0].prefix, "1+ ");
        assert_eq!(lines[1].prefix, "2+ ");
    }

    #[test]
    fn markers_without_line_numbers_are_left_alone() {
        let lines = collect_diff_lines("+added\n-removed\n");

        assert_eq!(lines[0].prefix, "+");
        assert_eq!(lines[1].prefix, "-");
    }
}
