//! Behaviour spec for rendering.
//!
//! Rules from oh-my-pi's `src/tools/read.ts`: two numbering shapes, the elision
//! marker, the brace-pair collapse, and the header anchor rule.

use super::*;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

fn window(start: usize, end: usize) -> Window {
    Window { start, end }
}

/// `N:content` is hashline's editable form, so a read can be patched without a
/// second read. `N|content` is display only. Confusing them means either an
/// unpatched read or a patch anchored to a line the model cannot edit.
#[test]
fn the_two_numbering_shapes_are_distinct() {
    assert_eq!(format_line(12, "code", Numbering::Hashline), "12:code");
    assert_eq!(format_line(12, "code", Numbering::Plain), "12|code");
    assert_eq!(format_line(12, "code", Numbering::None), "code");
}

/// Padding would shift the content a hashline anchor refers to.
#[test]
fn line_numbers_are_never_padded() {
    assert_eq!(format_line(7, "x", Numbering::Hashline), "7:x");
    assert_eq!(format_line(1000, "x", Numbering::Hashline), "1000:x");
}

#[test]
fn a_single_window_renders_its_lines_in_order() {
    let file = lines("one\ntwo\nthree\nfour");
    let out = render(&file, &[window(2, 3)], Numbering::Hashline);
    assert_eq!(out, "2:two\n3:three");
}

/// A gap between windows must be marked, or the output claims contiguity it
/// does not have and a later anchor lands on the wrong line.
#[test]
fn a_gap_between_windows_is_marked() {
    let file = lines("a\nb\nc\nd\ne\nf");
    let out = render(&file, &[window(1, 2), window(5, 6)], Numbering::Hashline);

    assert_eq!(out, "1:a\n2:b\n…\n5:e\n6:f");
}

#[test]
fn adjacent_windows_still_show_their_elision_when_rendered_separately() {
    // Merging happens in window resolution; render honours what it is given.
    let file = lines("a\nb\nc\nd");
    let out = render(&file, &[window(1, 1), window(3, 3)], Numbering::Plain);
    assert!(out.contains(ELISION), "{out}");
}

/// The clever bit: an elided body between a brace pair collapses to one line,
/// because `fn foo() {` … `}` tells the reader everything the elision would.
#[test]
fn an_elided_brace_body_collapses_to_one_line() {
    let file = lines("fn foo() {\n    body();\n    more();\n}\nafter");
    let out = render(&file, &[window(1, 1), window(4, 4)], Numbering::Hashline);

    assert_eq!(out, "1-4:fn foo() { … }");
}

/// The merged line's number is a RANGE, so an anchor built from it covers the
/// whole collapsed region rather than pointing at the head alone.
#[test]
fn a_merged_brace_line_carries_the_whole_range() {
    let merged = format_merged_brace(10, 25, "if (x) {", "}", Numbering::Hashline);
    assert_eq!(merged, "10-25:if (x) { … }");

    let plain = format_merged_brace(10, 25, "if (x) {", "}", Numbering::Plain);
    assert_eq!(plain, "10-25|if (x) { … }");
}

#[test]
fn brace_pairs_merge_for_each_opener() {
    assert!(can_merge_brace_pair("fn foo() {", "}"));
    assert!(can_merge_brace_pair("call(", ")"));
    assert!(can_merge_brace_pair("let xs = [", "]"));
}

/// Terminating punctuation after the closer is fine: `};`, `})`, `]);` are all
/// still just the end of the construct.
#[test]
fn trailing_punctuation_does_not_block_a_merge() {
    assert!(can_merge_brace_pair("const x = {", "};"));
    // `foo({` opens a paren AND a brace; the tail closes the brace first, then
    // the paren. The opener considered is the LAST character of the head.
    assert!(can_merge_brace_pair("foo({", "});"));
    assert!(can_merge_brace_pair("let xs = [", "]);"));
}

/// `} else {` must not merge: the body between them is doing something the
/// reader needs to see, and collapsing it would hide a whole branch.
#[test]
fn a_closer_that_continues_the_statement_does_not_merge() {
    assert!(!can_merge_brace_pair("if (x) {", "} else {"));
    assert!(!can_merge_brace_pair("if (x) {", "} catch (e) {"));
}

#[test]
fn mismatched_or_absent_braces_do_not_merge() {
    assert!(!can_merge_brace_pair("fn foo() {", ")"));
    assert!(!can_merge_brace_pair("plain line", "}"));
    assert!(!can_merge_brace_pair("", "}"));
    assert!(!can_merge_brace_pair("fn foo() {", "body();"));
}

#[test]
fn indentation_does_not_prevent_a_merge() {
    assert!(can_merge_brace_pair("    fn foo() {   ", "    }"));
}

/// A relative path collapses to its file name: edit's tag recovery rebinds a
/// bare [name#tag] onto the in-tree file it uniquely names, so the short form
/// is enough and costs fewer tokens.
#[test]
fn a_relative_path_anchors_on_its_file_name() {
    assert_eq!(header_anchor("src/deep/lib.rs"), "lib.rs");
    assert_eq!(header_anchor("lib.rs"), "lib.rs");
}

/// An absolute path must stay resolvable. Recovery refuses to redirect a write
/// outside the workspace, so a bare basename would resolve against the working
/// directory, miss, and fail the edit with "File not found".
#[test]
fn an_absolute_path_stays_whole() {
    assert_eq!(header_anchor("/etc/hosts"), "/etc/hosts");
    assert_eq!(header_anchor("~/.config/thing.toml"), "~/.config/thing.toml");
}

#[test]
fn rendering_no_windows_produces_nothing() {
    let file = lines("a\nb");
    assert_eq!(render(&file, &[], Numbering::Hashline), "");
}

/// A window past the end of the file renders what exists rather than panicking
/// on a missing index.
#[test]
fn a_window_past_the_end_renders_what_is_there() {
    let file = lines("a\nb");
    let out = render(&file, &[window(1, 10)], Numbering::Plain);
    assert_eq!(out, "1|a\n2|b");
}

#[test]
fn several_gaps_each_get_their_own_marker() {
    let file = lines("a\nb\nc\nd\ne\nf\ng");
    let out = render(
        &file,
        &[window(1, 1), window(3, 3), window(6, 6)],
        Numbering::Plain,
    );
    assert_eq!(out.matches(ELISION).count(), 2, "{out}");
}
