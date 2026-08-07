use super::*;

/// The regression this module exists for. A per-line trim removes *all* leading
/// whitespace, so nested code arrives at the model flush left and the structure
/// the indentation carried is gone.
///
/// The hunk has to span two depths for this to say anything: when every changed
/// line sits at the same depth, that depth *is* the common indent and removing
/// it is correct.
#[test]
fn nesting_inside_a_hunk_survives() {
    let old = "fn outer() {\n    if x {\n        old_call();\n    }\n}\n";
    let new = "fn outer() {\n    if y {\n        new_call();\n    }\n}\n";

    let diff = render_diff(old, new, 1, DEFAULT_MAX_DIFF_LINES);

    let depth = |needle: &str| -> usize {
        let line = diff
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} shown in {diff:?}"));
        let after_marker = line
            .split_once(['+', '-'])
            .map(|(_, rest)| rest)
            .expect("a row carries a marker");
        // The renderer puts one space after the marker; indentation is beyond it.
        after_marker.len() - after_marker.trim_start_matches(' ').len()
    };

    assert!(
        depth("new_call") > depth("if y"),
        "the call is nested one level inside the `if` and must stay that way: {diff:?}"
    );
}

/// The width motivation behind the original trim was real: a hunk indented deep
/// inside a file should not waste the pane on leading space. Common-prefix
/// dedent keeps that while preserving relative structure.
#[test]
fn a_uniformly_indented_hunk_renders_flush_left() {
    let old = "            alpha();\n            beta();\n";
    let new = "            alpha();\n            gamma();\n";

    let diff = render_diff(old, new, 1, DEFAULT_MAX_DIFF_LINES);

    for line in diff.lines() {
        let content = line
            .split_once(['+', '-'])
            .map(|(_, rest)| rest.trim_start_matches(' '))
            .unwrap_or(line);
        assert!(
            !content.is_empty(),
            "a uniformly indented hunk should not keep its common indent: {diff:?}"
        );
    }
    assert!(
        diff.contains("gamma();"),
        "the changed line is still shown: {diff:?}"
    );
}

/// Tabs and spaces must measure the same way, or a tab-indented file dedents
/// unpredictably.
#[test]
fn tab_indentation_is_measured_and_normalized() {
    let old = "\t\tfirst();\n\t\tsecond();\n";
    let new = "\t\tfirst();\n\t\tthird();\n";

    let diff = render_diff(old, new, 1, DEFAULT_MAX_DIFF_LINES);

    assert!(
        !diff.contains('\t'),
        "leading tabs are normalized to spaces: {diff:?}"
    );
    assert!(diff.contains("third();"), "{diff:?}");
}

/// Line numbers are right-aligned so content does not shift a column when a
/// hunk crosses a digit boundary.
#[test]
fn line_numbers_are_right_aligned_across_a_digit_boundary() {
    let mut old = String::new();
    let mut new = String::new();
    for i in 1..=11 {
        old.push_str(&format!("line{i}\n"));
        new.push_str(&format!("changed{i}\n"));
    }

    let diff = render_diff(&old, &new, 1, DEFAULT_MAX_DIFF_LINES);

    let nine = diff
        .lines()
        .find(|line| line.contains("changed9"))
        .expect("line 9 shown");
    let ten = diff
        .lines()
        .find(|line| line.contains("changed10"))
        .expect("line 10 shown");
    let col = |line: &str| line.find('+').expect("a + marker");
    assert_eq!(
        col(nine),
        col(ten),
        "the marker column must not move between 9 and 10:\n{nine:?}\n{ten:?}"
    );
}

/// Numbering starts where the compared region does, not at 1.
#[test]
fn numbering_honours_the_start_line() {
    let diff = render_diff("alpha\n", "beta\n", 42, DEFAULT_MAX_DIFF_LINES);
    assert!(diff.contains("42- alpha"), "{diff:?}");
    assert!(diff.contains("42+ beta"), "{diff:?}");
}

/// Unchanged lines carry no information the model needs here.
#[test]
fn unchanged_lines_are_omitted() {
    let diff = render_diff(
        "keep\nold\nkeep2\n",
        "keep\nnew\nkeep2\n",
        1,
        DEFAULT_MAX_DIFF_LINES,
    );
    assert!(!diff.contains("keep"), "{diff:?}");
    assert!(diff.contains("old") && diff.contains("new"), "{diff:?}");
}

#[test]
fn an_unchanged_file_produces_nothing() {
    assert!(render_diff("same\n", "same\n", 1, DEFAULT_MAX_DIFF_LINES).is_empty());
}

/// A long diff is capped, and says so rather than stopping silently.
#[test]
fn a_long_diff_is_truncated_and_says_so() {
    let old: String = (0..50).map(|i| format!("old{i}\n")).collect();
    let new: String = (0..50).map(|i| format!("new{i}\n")).collect();

    let diff = render_diff(&old, &new, 1, 5);

    assert!(diff.contains("truncated"), "{diff:?}");
    assert!(
        diff.lines().count() <= 6,
        "5 rows plus the marker: {diff:?}"
    );
}
