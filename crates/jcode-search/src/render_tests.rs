//! Behaviour spec for rendering.
//!
//! The line format comes from oh-my-pi's `match-line-format.ts`, and the
//! grouped-tree shape from their `grep-renderer.test.ts` fixture
//! (`["# src/", "## file.ts#abcd", "*12│const needle = true;"]`).

use super::*;
use crate::select::{FileMatches, Match, Selection};

fn hit(path: &str, line: usize, text: &str) -> Match {
    Match {
        path: path.to_string(),
        line,
        text: text.to_string(),
    }
}

fn file(path: &str, matches: Vec<Match>) -> FileMatches {
    FileMatches {
        path: path.to_string(),
        total: matches.len(),
        matches,
    }
}

fn selection(files: Vec<FileMatches>) -> Selection {
    let total = files.len();
    Selection {
        files,
        total_files: total,
        file_limit_reached: false,
        next_skip: total,
    }
}

fn no_tags(_: &str) -> Option<String> {
    None
}

#[test]
fn a_matched_line_is_starred_and_context_is_not() {
    assert_eq!(format_match_line(12, "code", true, false), "*12|code");
    assert_eq!(format_match_line(12, "code", false, false), " 12|code");
}

/// Hashline mode uses `:`, which is the shape `edit` accepts, so a search
/// result can be patched without re-reading the file.
#[test]
fn hashline_mode_uses_the_editable_separator() {
    assert_eq!(format_match_line(12, "code", true, true), "*12:code");
    assert_eq!(format_match_line(12, "code", false, true), " 12:code");
}

/// Padding would shift the content, and a hashline anchor refers to the line
/// exactly as shown.
#[test]
fn line_numbers_are_never_padded() {
    let rendered = format_match_line(7, "code", true, true);
    assert_eq!(rendered, "*7:code", "no leading pad: {rendered:?}");
}

#[test]
fn a_shared_directory_prefix_is_folded() {
    let paths = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    assert_eq!(common_prefix(&paths), "src/");
}

#[test]
fn a_deeper_shared_prefix_folds_entirely() {
    let paths = vec![
        "packages/core/src/a.rs".to_string(),
        "packages/core/src/b.rs".to_string(),
    ];
    assert_eq!(common_prefix(&paths), "packages/core/src/");
}

/// Whole components, not characters: `src/foo` and `src/foobar` share `src/`.
/// A character-wise prefix would produce `src/foo`, which is not a directory.
#[test]
fn the_prefix_is_computed_per_component_not_per_character() {
    let paths = vec!["src/foo/a.rs".to_string(), "src/foobar/b.rs".to_string()];
    assert_eq!(common_prefix(&paths), "src/");
}

#[test]
fn unrelated_paths_share_no_prefix() {
    let paths = vec!["src/a.rs".to_string(), "test/b.rs".to_string()];
    assert_eq!(common_prefix(&paths), "");
}

#[test]
fn files_at_the_root_share_no_prefix() {
    let paths = vec!["a.rs".to_string(), "b.rs".to_string()];
    assert_eq!(common_prefix(&paths), "");
}

#[test]
fn the_rendered_shape_matches_omps_fixture() {
    let rendered = render(
        &selection(vec![file(
            "src/file.ts",
            vec![hit("src/file.ts", 12, "const needle = true;")],
        )]),
        &|_| Some("abcd".to_string()),
    );

    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "# src/");
    assert_eq!(lines[1], "## file.ts#abcd");
    assert_eq!(lines[2], "*12:const needle = true;");
}

/// Without a tag there is nothing to anchor to, so the display-only separator
/// is used and no tag is invented.
#[test]
fn a_file_without_a_tag_renders_in_plain_mode() {
    let rendered = render(
        &selection(vec![file("a.rs", vec![hit("a.rs", 3, "x")])]),
        &no_tags,
    );

    assert!(rendered.contains("# a.rs"), "{rendered}");
    assert!(rendered.contains("*3|x"), "{rendered}");
    // `#` is the header marker itself, so the check is that the FILENAME
    // carries no `#TAG` suffix rather than that `#` is absent entirely.
    let header = rendered.lines().next().unwrap_or_default();
    assert_eq!(
        header, "# a.rs",
        "an untagged file must not gain a tag suffix"
    );
}

/// A caller seeing 20 of 400 matches has to know to narrow. Reporting only the
/// shown count reads as a complete answer.
#[test]
fn a_capped_file_reports_the_true_match_count() {
    let mut capped = file("a.rs", vec![hit("a.rs", 1, "x"), hit("a.rs", 2, "y")]);
    capped.total = 400;

    let rendered = render(&selection(vec![capped]), &no_tags);
    assert!(
        rendered.contains("(2 of 400 matches)"),
        "the true total should be reported: {rendered}"
    );
}

#[test]
fn a_file_showing_everything_reports_no_count() {
    let rendered = render(
        &selection(vec![file("a.rs", vec![hit("a.rs", 1, "x")])]),
        &no_tags,
    );
    assert!(
        !rendered.contains("matches)"),
        "an uncapped file needs no count: {rendered}"
    );
}

/// Non-adjacent matches must not read as a contiguous block of the file.
#[test]
fn a_gap_between_matches_is_marked() {
    let rendered = render(
        &selection(vec![file(
            "a.rs",
            vec![hit("a.rs", 10, "x"), hit("a.rs", 50, "y")],
        )]),
        &no_tags,
    );

    assert!(rendered.contains("\n...\n"), "expected a gap marker: {rendered}");
}

#[test]
fn adjacent_matches_have_no_gap_marker() {
    let rendered = render(
        &selection(vec![file(
            "a.rs",
            vec![hit("a.rs", 10, "x"), hit("a.rs", 11, "y")],
        )]),
        &no_tags,
    );

    assert!(!rendered.contains("..."), "no gap to mark: {rendered}");
}

#[test]
fn no_matches_says_so_rather_than_rendering_nothing() {
    assert_eq!(render(&selection(Vec::new()), &no_tags), "No matches found.");
}

#[test]
fn a_truncated_search_explains_how_to_page() {
    let mut sel = selection(vec![file("a.rs", vec![hit("a.rs", 1, "x")])]);
    sel.total_files = 30;
    sel.file_limit_reached = true;
    sel.next_skip = 20;

    let rendered = render(&sel, &no_tags);
    assert!(rendered.contains("skip=20"), "{rendered}");
    assert!(rendered.contains("of 30"), "{rendered}");
}

#[test]
fn a_file_list_folds_its_prefix_too() {
    let paths = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let rendered = render_paths(&paths, 2);

    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "# src/");
    assert_eq!(lines[1], "a.rs");
    assert_eq!(lines[2], "b.rs");
}

#[test]
fn a_truncated_file_list_says_how_many_are_hidden() {
    let paths = vec!["a.rs".to_string()];
    let rendered = render_paths(&paths, 50);

    assert!(rendered.contains("Showing 1 of 50 files"), "{rendered}");
}

#[test]
fn an_empty_file_list_says_so() {
    assert_eq!(render_paths(&[], 0), "No files found.");
}

/// A single file still folds its directory, so the header carries the path and
/// the file line is not left bare.
#[test]
fn one_file_folds_its_directory_into_the_header() {
    assert_eq!(common_prefix(&["src/deep/a.rs".to_string()]), "src/deep/");
    assert_eq!(common_prefix(&["a.rs".to_string()]), "");
}

/// Exact output, not just "contains". A `contains` assertion cannot see a
/// stray trailing blank line, which is how an unconditional pagination block
/// survived mutation testing.
#[test]
fn a_complete_search_renders_with_no_trailing_noise() {
    let rendered = render(
        &selection(vec![file("a.rs", vec![hit("a.rs", 3, "x")])]),
        &no_tags,
    );

    assert_eq!(rendered, "# a.rs\n*3|x\n");
}

#[test]
fn a_truncated_search_renders_its_hint_exactly_once() {
    let mut sel = selection(vec![file("a.rs", vec![hit("a.rs", 1, "x")])]);
    sel.total_files = 30;
    sel.file_limit_reached = true;
    sel.next_skip = 20;

    let rendered = render(&sel, &no_tags);
    assert_eq!(
        rendered.matches("skip=").count(),
        1,
        "the hint should appear once: {rendered}"
    );
    assert!(
        rendered.ends_with("narrow paths/pattern.\n"),
        "the hint should be last: {rendered:?}"
    );
}

/// Several matches in one file share a path, so the shared-component walk sees
/// identical inputs. Popping the file name is what keeps the prefix a
/// directory; without it the whole path folds and the file header goes empty.
#[test]
fn identical_paths_still_fold_to_a_directory_not_the_whole_path() {
    let paths = vec!["src/a.rs".to_string(), "src/a.rs".to_string()];
    assert_eq!(
        common_prefix(&paths),
        "src/",
        "the file name must not become part of the folded prefix"
    );
}

/// The rendered header for such a file must still name it.
#[test]
fn a_single_file_with_several_matches_keeps_its_name_in_the_header() {
    let rendered = render(
        &selection(vec![file(
            "src/a.rs",
            vec![hit("src/a.rs", 1, "x"), hit("src/a.rs", 2, "y")],
        )]),
        &no_tags,
    );

    assert_eq!(rendered, "# src/\n## a.rs\n*1|x\n*2|y\n");
}
