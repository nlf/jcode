//! Behaviour ported from the header-and-path layer of omp's `input.ts`, whose
//! recovery cases come from shapes they observed in real benchmark traces.
//!
//! The `Update File:` and `***` cases look like trivia. They are not: a model
//! trained on apply-patch conventions reaches for them reflexively, and every
//! one that fails to parse is a wasted turn. omp lists the exact variants in a
//! source comment, which is as close to a bug report as the format has.

use super::*;

fn header(line: &str) -> RawSection {
    parse_header_line(line, None)
        .expect("header must parse")
        .expect("line must be recognized as a header")
}

// ─── header shapes ───────────────────────────────────────────────────────────

#[test]
fn a_header_carries_a_path_and_a_tag() {
    let section = header("[src/foo.rs#1A2B]");
    assert_eq!(section.path, "src/foo.rs");
    assert_eq!(section.file_hash.as_deref(), Some("1A2B"));
}

/// A header without a tag is legal at this layer. The patcher decides whether
/// an untagged section may be applied; the splitter only reports what it saw.
#[test]
fn a_header_without_a_tag_is_legal_here() {
    let section = header("[src/foo.rs]");
    assert_eq!(section.path, "src/foo.rs");
    assert_eq!(section.file_hash, None);
}

/// Tags are compared uppercase, so a lowercase tag must normalize rather than
/// silently failing to match a snapshot recorded in uppercase.
#[test]
fn a_lowercase_tag_is_normalized_to_uppercase() {
    assert_eq!(header("[a.rs#1a2b]").file_hash.as_deref(), Some("1A2B"));
}

/// A `#` that is not a valid tag belongs to the path. Files with `#` in the
/// name exist, and a URL fragment can reach this code too.
#[test]
fn a_hash_that_is_not_a_valid_tag_stays_part_of_the_path() {
    for line in ["[weird#name.rs]", "[a.rs#XYZ]", "[a.rs#12345]", "[a.rs#12]"] {
        let section = header(line);
        assert_eq!(section.file_hash, None, "must not parse a tag from {line:?}");
        assert!(section.path.contains('#'), "path must keep the # from {line:?}");
    }
}

#[test]
fn a_non_bracketed_line_is_not_a_header() {
    for line in ["PUT 1.=1:", "+content", "", "  indented"] {
        assert_eq!(parse_header_line(line, None).unwrap(), None, "{line:?}");
    }
}

/// A line that opens like a header but never closes is malformed, and must
/// error rather than being reclassified as body. Silently treating it as
/// content writes the header text into the file.
#[test]
fn an_unclosed_bracket_is_an_error_not_body_content() {
    assert!(parse_header_line("[src/foo.rs#1A2B", None).is_err());
}

#[test]
fn an_empty_header_is_an_error() {
    assert!(parse_header_line("[]", None).is_err());
}

// ─── path recovery: the traces omp actually saw ──────────────────────────────

/// Every one of these is a shape omp lists in a source comment as observed from
/// a model. They come from apply-patch conventions, which models reach for out
/// of habit.
#[test]
fn apply_patch_keyword_noise_is_stripped_from_the_path() {
    for line in [
        "[Update File:foo.ts#1A2B]",
        "[Update:foo.ts#1A2B]",
        "[UpdateFile:foo.ts#1A2B]",
        "[Update/File:foo.ts#1A2B]",
        "[Update-file:foo.ts#1A2B]",
        "[Update(File):foo.ts#1A2B]",
        "[Add File:foo.ts#1A2B]",
        "[Delete File:foo.ts#1A2B]",
        "[Move to:foo.ts#1A2B]",
        "[***foo.ts#1A2B]",
        "[***Update File:foo.ts#1A2B]",
    ] {
        assert_eq!(header(line).path, "foo.ts", "failed to recover path from {line:?}");
    }
}

#[test]
fn keyword_noise_recovery_is_case_insensitive() {
    for line in ["[UPDATE FILE:foo.ts#1A2B]", "[update file:foo.ts#1A2B]"] {
        assert_eq!(header(line).path, "foo.ts", "{line:?}");
    }
}

/// The dangerous direction. A filename that merely *starts* with a keyword must
/// survive intact, or `update_config.rs` becomes `_config.rs` and the edit
/// lands on a file that does not exist — or worse, one that does.
#[test]
fn a_filename_beginning_with_a_keyword_is_not_mangled() {
    for (line, expected) in [
        ("[update_config.rs#1A2B]", "update_config.rs"),
        ("[updates.rs#1A2B]", "updates.rs"),
        ("[add_user.py#1A2B]", "add_user.py"),
        ("[deleted_items.go#1A2B]", "deleted_items.go"),
        ("[movement.ts#1A2B]", "movement.ts"),
    ] {
        assert_eq!(header(line).path, expected, "mangled {line:?}");
    }
}

/// A keyword followed by a colon *later in a real path* must not trigger
/// stripping either. The colon has to terminate the keyword block, not appear
/// arbitrarily downstream.
#[test]
fn a_keyword_with_intervening_path_text_is_not_stripped() {
    assert_eq!(header("[update/deeply/nested:thing.rs#1A2B]").path, "update/deeply/nested:thing.rs");
}

#[test]
fn quoted_paths_are_unquoted() {
    assert_eq!(header("[\"src/my file.rs\"#1A2B]").path, "src/my file.rs");
    assert_eq!(header("['src/my file.rs'#1A2B]").path, "src/my file.rs");
}

/// Mismatched quotes are not a quoting attempt, so they stay literal.
#[test]
fn mismatched_quotes_are_left_alone() {
    assert_eq!(header("[\"src/foo.rs'#1A2B]").path, "\"src/foo.rs'");
}

// ─── cwd normalization ───────────────────────────────────────────────────────

/// An absolute path inside the working directory renders relative, so the model
/// sees the same spelling it would get from `read`.
#[test]
fn an_absolute_path_inside_the_cwd_becomes_relative() {
    assert_eq!(normalize_path("/work/src/foo.rs", Some("/work")), "src/foo.rs");
}

/// Outside the cwd it stays absolute. Rewriting it would produce a `../..`
/// chain that reads as an escape attempt rather than a location.
#[test]
fn an_absolute_path_outside_the_cwd_stays_absolute() {
    assert_eq!(normalize_path("/elsewhere/foo.rs", Some("/work")), "/elsewhere/foo.rs");
}

#[test]
fn a_relative_path_is_untouched_by_cwd_normalization() {
    assert_eq!(normalize_path("src/foo.rs", Some("/work")), "src/foo.rs");
}

#[test]
fn the_cwd_itself_normalizes_to_dot() {
    assert_eq!(normalize_path("/work", Some("/work")), ".");
}

// ─── section splitting ───────────────────────────────────────────────────────

#[test]
fn a_single_section_carries_its_body() {
    let sections = split_sections("[a.rs#1A2B]\nPUT 1.=1:\n+x", None).unwrap();

    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].path, "a.rs");
    assert_eq!(sections[0].body, "PUT 1.=1:\n+x");
}

#[test]
fn multiple_sections_split_at_each_header() {
    let sections = split_sections("[a.rs#1A2B]\nCUT 1.=1\n[b.rs#3C4D]\nCUT 2.=2", None).unwrap();

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].path, "a.rs");
    assert_eq!(sections[0].body, "CUT 1.=1");
    assert_eq!(sections[1].path, "b.rs");
    assert_eq!(sections[1].body, "CUT 2.=2");
}

/// A patch with no header at all is a single-file edit whose path came from
/// elsewhere, which is how a simpler call site would use this.
#[test]
fn a_headerless_patch_is_one_anonymous_section() {
    let sections = split_sections("PUT 1.=1:\n+x", None).unwrap();

    assert_eq!(sections.len(), 1);
    assert!(sections[0].path.is_empty());
    assert_eq!(sections[0].body, "PUT 1.=1:\n+x");
}

#[test]
fn empty_input_yields_no_sections() {
    assert!(split_sections("", None).unwrap().is_empty());
    assert!(split_sections("\n\n  \n", None).unwrap().is_empty());
}

/// A byte-order mark at the head of an authored patch must not turn the first
/// header into unrecognized body text.
#[test]
fn a_leading_byte_order_mark_is_ignored() {
    let sections = split_sections("\u{feff}[a.rs#1A2B]\nCUT 1.=1", None).unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].path, "a.rs");
}

// ─── same-path merging ───────────────────────────────────────────────────────

/// A model repeating a header for one file is harmless; the ops still apply in
/// the order written.
#[test]
fn adjacent_sections_for_one_path_merge_without_the_interleaved_flag() {
    let sections = split_sections("[a.rs#1A2B]\nCUT 1.=1\n[a.rs#1A2B]\nCUT 2.=2", None).unwrap();

    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].body, "CUT 1.=1\nCUT 2.=2");
    assert!(!sections[0].interleaved, "adjacent merge is not interleaved");
}

/// Non-adjacent merging moves later ops up, which reorders them relative to how
/// they were authored. The flag exists so a caller can refuse order-sensitive
/// operations rather than applying them in the wrong sequence.
#[test]
fn non_adjacent_sections_for_one_path_merge_and_set_the_interleaved_flag() {
    let sections =
        split_sections("[a.rs#1A2B]\nCUT 1.=1\n[b.rs#3C4D]\nCUT 9.=9\n[a.rs#1A2B]\nCUT 2.=2", None)
            .unwrap();

    assert_eq!(sections.len(), 2);
    let a = sections.iter().find(|s| s.path == "a.rs").expect("a.rs present");
    assert_eq!(a.body, "CUT 1.=1\nCUT 2.=2");
    assert!(
        a.interleaved,
        "another file's section sat between these, so the merge reordered them"
    );
}

/// A later tag is the fresher observation, typically because an intervening
/// edit advanced it, so it must win.
#[test]
fn a_later_tag_wins_when_sections_merge() {
    let sections = split_sections("[a.rs#1A2B]\nCUT 1.=1\n[a.rs#3C4D]\nCUT 2.=2", None).unwrap();

    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].file_hash.as_deref(), Some("3C4D"));
}

/// Anonymous sections must not merge with each other: without a path there is
/// no evidence they target the same file.
#[test]
fn anonymous_sections_do_not_merge() {
    let sections = split_sections("CUT 1.=1", None).unwrap();
    assert_eq!(sections.len(), 1);
}
