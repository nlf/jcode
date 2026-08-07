//! Behaviour spec for diff hunk parsing.
//!
//! The five core cases come verbatim from oh-my-pi's
//! `test/core/apply-patch.test.ts` (`parseDiffHunks` describe block); the rest
//! pin rules documented in their `diff.ts`.

use super::*;

fn only(diff: &str) -> DiffHunk {
    let hunks = parse_diff_hunks(diff).expect("should parse");
    assert_eq!(hunks.len(), 1, "expected exactly one hunk: {hunks:?}");
    hunks.into_iter().next().expect("checked above")
}

#[test]
fn a_simple_hunk_carries_its_context_and_both_sides() {
    let hunk = only("@@ def f():\n-    pass\n+    return 123");

    assert_eq!(hunk.change_context.as_deref(), Some("def f():"));
    assert_eq!(hunk.old_lines, vec!["    pass"]);
    assert_eq!(hunk.new_lines, vec!["    return 123"]);
}

#[test]
fn several_hunks_parse_from_one_body() {
    let hunks = parse_diff_hunks("@@\n-bar\n+BAR\n@@\n-qux\n+QUX").expect("should parse");
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].old_lines, vec!["bar"]);
    assert_eq!(hunks[1].old_lines, vec!["qux"]);
}

/// Context lines appear on both sides, which is what anchors the change.
#[test]
fn context_lines_appear_in_both_old_and_new() {
    let hunk = only("@@\n foo\n-bar\n+baz\n qux");

    assert_eq!(hunk.old_lines, vec!["foo", "bar", "qux"]);
    assert_eq!(hunk.new_lines, vec!["foo", "baz", "qux"]);
    assert!(hunk.has_context_lines);
}

#[test]
fn a_bare_marker_carries_no_context() {
    let hunk = only("@@\n+new line");
    assert_eq!(hunk.change_context, None);
    assert_eq!(hunk.new_lines, vec!["new line"]);
}

#[test]
fn an_end_of_file_marker_is_recorded() {
    let hunk = only("@@\n+line\n*** End of File");
    assert!(hunk.is_end_of_file);
    assert_eq!(hunk.new_lines, vec!["line"]);
}

/// A hunk with no context is a blind replacement, and callers weigh that
/// differently from one anchored by surrounding lines.
#[test]
fn a_hunk_without_context_says_so() {
    let hunk = only("@@\n-old\n+new");
    assert!(
        !hunk.has_context_lines,
        "only additions and removals, so nothing anchors it"
    );
}

/// A header naming a line number is a location hint, not context.
#[test]
fn a_unified_header_yields_a_line_hint() {
    let hunk = only("@@ -42,3 +42,3 @@\n-old\n+new");
    assert_eq!(hunk.old_start_line, Some(42));
    assert_eq!(hunk.change_context, None);
}

#[test]
fn a_unified_header_can_carry_both_a_hint_and_context() {
    let hunk = only("@@ -42,3 +42,3 @@ fn main()\n-old\n+new");
    assert_eq!(hunk.old_start_line, Some(42));
    assert_eq!(hunk.change_context.as_deref(), Some("fn main()"));
}

#[test]
fn a_bare_number_header_is_a_line_hint() {
    let hunk = only("@@ 42\n-old\n+new");
    assert_eq!(hunk.old_start_line, Some(42));
    assert_eq!(hunk.change_context, None, "a number is not context text");
}

#[test]
fn a_zero_line_number_is_refused() {
    let error = parse_diff_hunks("@@ -0,3 +0,3 @@\n-old\n+new").expect_err("line 0 is invalid");
    assert!(error.message().contains(">= 1"), "{}", error.message());
}

/// Stacked headers name a nested location: an outer function, an inner branch.
#[test]
fn stacked_headers_accumulate_context() {
    let hunk = only("@@ impl Foo\n@@ fn bar()\n-old\n+new");
    let context = hunk.change_context.expect("both headers contribute");
    assert!(context.contains("impl Foo"), "{context}");
    assert!(context.contains("fn bar()"), "{context}");
}

/// Models omit the leading space on context lines constantly. Refusing would
/// reject most real patches.
#[test]
fn an_unprefixed_line_is_treated_as_context() {
    let hunk = only("@@\nfn main() {\n-    old\n+    new\n}");

    assert!(hunk.old_lines.contains(&"fn main() {".to_string()));
    assert!(hunk.new_lines.contains(&"fn main() {".to_string()));
    assert!(hunk.has_context_lines);
}

/// A body with no header at all is accepted, since the envelope already said
/// which file this is.
#[test]
fn a_body_without_a_header_still_parses() {
    let hunk = only("-old\n+new");
    assert_eq!(hunk.old_lines, vec!["old"]);
    assert_eq!(hunk.new_lines, vec!["new"]);
}

/// An elision marks omitted context, so the hunk is anchored rather than blind
/// even though the elided lines are not content.
#[test]
fn an_elision_marks_the_hunk_as_anchored() {
    for marker in ["...", "…"] {
        let hunk = only(&format!("@@\n foo\n{marker}\n-old\n+new"));
        assert!(hunk.has_context_lines, "{marker} should anchor the hunk");
        assert!(
            !hunk.old_lines.iter().any(|line| line == marker),
            "{marker} is not content: {:?}",
            hunk.old_lines
        );
    }
}

/// A model pasting from `git diff` brings metadata with it. Treating those
/// lines as context would look for `diff --git a/x b/x` in the file.
#[test]
fn unified_diff_metadata_is_skipped() {
    let hunk = only(
        "diff --git a/x.rs b/x.rs\n\
         index 1234567..89abcde 100644\n\
         --- a/x.rs\n\
         +++ b/x.rs\n\
         @@\n\
         -old\n\
         +new",
    );

    assert_eq!(hunk.old_lines, vec!["old"]);
    assert_eq!(hunk.new_lines, vec!["new"]);
}

/// Metadata is only metadata when it is not diff content. A context line
/// reading `--- separator` is part of the file.
#[test]
fn a_content_line_that_looks_like_metadata_is_kept() {
    let hunk = only("@@\n --- separator\n-old\n+new");
    assert!(
        hunk.old_lines.contains(&"--- separator".to_string()),
        "a space-prefixed line is content: {:?}",
        hunk.old_lines
    );
}

/// Pasting a multi-file patch into one file's body would apply the first file
/// and silently drop the rest.
#[test]
fn multi_file_markers_inside_one_body_are_refused() {
    let error = parse_diff_hunks("*** Update File: a.txt\n-old\n+new\n*** Update File: b.txt")
        .expect_err("a single-file body cannot carry several file markers");

    assert!(error.message().contains("file markers"), "{}", error.message());
}

/// A blank line before the next header ends the hunk rather than becoming a
/// trailing empty context line, which would then have to match a blank line in
/// the file.
#[test]
fn a_blank_line_before_the_next_header_ends_the_hunk() {
    let hunks = parse_diff_hunks("@@\n-a\n+A\n\n@@\n-b\n+B").expect("should parse");

    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].old_lines, vec!["a"], "no trailing blank context");
    assert_eq!(hunks[1].old_lines, vec!["b"]);
}

/// A blank line inside a hunk is a real context line: files contain blank
/// lines, and dropping them would misalign the match.
#[test]
fn a_blank_line_inside_a_hunk_is_context() {
    let hunk = only("@@\n foo\n\n bar\n-old\n+new");
    assert!(
        hunk.old_lines.contains(&String::new()),
        "the blank line belongs to the context: {:?}",
        hunk.old_lines
    );
}

#[test]
fn a_trailing_header_with_no_body_is_not_a_hunk() {
    let hunks = parse_diff_hunks("@@\n-old\n+new\n@@\n").expect("should parse");
    assert_eq!(hunks.len(), 1, "the dangling header is dropped: {hunks:?}");
}

#[test]
fn an_empty_body_yields_no_hunks() {
    assert_eq!(parse_diff_hunks("").expect("empty is valid"), Vec::new());
    assert_eq!(parse_diff_hunks("\n\n").expect("blank is valid"), Vec::new());
}

/// An end-of-file marker with nothing before it has no change to terminate.
#[test]
fn an_end_of_file_marker_alone_is_refused() {
    let error = parse_diff_hunks("@@\n*** End of File").expect_err("nothing to terminate");
    assert!(
        error.message().contains("does not contain any lines"),
        "{}",
        error.message()
    );
}
