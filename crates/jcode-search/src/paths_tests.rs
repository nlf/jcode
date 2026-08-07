//! Behaviour spec for path and selector parsing.
//!
//! Cases come from oh-my-pi's `test/tools/path-literal-colon-selector.test.ts`
//! and the documented behaviour in `path-utils.ts`, not from reading our own
//! implementation back to itself.

use super::*;

#[test]
fn a_bare_number_selects_from_that_line_onward() {
    assert_eq!(
        parse_line_range_chunk("50"),
        Ok(Some(LineRange {
            start: 50,
            end: None
        }))
    );
}

#[test]
fn a_range_is_inclusive_on_both_ends() {
    assert_eq!(
        parse_line_range_chunk("50-100"),
        Ok(Some(LineRange {
            start: 50,
            end: Some(100)
        }))
    );
}

/// `N+K` is a count, so `50+10` is 50 through 59 rather than 50 through 60.
/// Off by one here silently returns an extra line on every windowed read.
#[test]
fn a_count_form_spans_exactly_that_many_lines() {
    assert_eq!(
        parse_line_range_chunk("50+10"),
        Ok(Some(LineRange {
            start: 50,
            end: Some(59)
        }))
    );
}

/// `..` is accepted because models paste Rust range syntax.
#[test]
fn a_double_dot_is_an_alias_for_a_dash() {
    assert_eq!(
        parse_line_range_chunk("2724..2727"),
        parse_line_range_chunk("2724-2727")
    );
}

/// `301-` means "from 301 onward", the same as a bare `301`.
#[test]
fn a_trailing_dash_is_open_ended() {
    assert_eq!(
        parse_line_range_chunk("301-"),
        Ok(Some(LineRange {
            start: 301,
            end: None
        }))
    );
}

/// Models paste `L50` from line-number UIs.
#[test]
fn a_leading_l_is_accepted() {
    assert_eq!(
        parse_line_range_chunk("L50-L100"),
        Ok(Some(LineRange {
            start: 50,
            end: Some(100)
        }))
    );
}

#[test]
fn line_zero_is_refused_because_lines_are_one_indexed() {
    assert_eq!(parse_line_range_chunk("0-10"), Err(SelectorError::ZeroLine));
}

#[test]
fn a_backwards_range_is_refused() {
    assert_eq!(
        parse_line_range_chunk("100-50"),
        Err(SelectorError::Backwards {
            start: 100,
            end: 50
        })
    );
}

#[test]
fn a_zero_count_is_refused() {
    assert_eq!(
        parse_line_range_chunk("50+0"),
        Err(SelectorError::EmptyCount {
            start: 50,
            count: 0
        })
    );
}

/// Ordinary text must not be read as a selector, or a path containing a colon
/// gets truncated.
#[test]
fn text_that_is_not_a_range_is_not_a_selector() {
    for text in ["abc", "", "1-2-3", "50x", "x50"] {
        assert_eq!(
            parse_line_range_chunk(text),
            Ok(None),
            "{text:?} should not parse as a range"
        );
    }
}

#[test]
fn several_ranges_are_sorted_and_returned_together() {
    assert_eq!(
        parse_line_ranges("960-973,5-16"),
        Ok(Some(vec![
            LineRange {
                start: 5,
                end: Some(16)
            },
            LineRange {
                start: 960,
                end: Some(973)
            },
        ]))
    );
}

/// Overlapping ranges are merged so a consumer streaming each range in one
/// pass does not read the overlap twice and report duplicate matches.
#[test]
fn overlapping_ranges_are_merged() {
    assert_eq!(
        parse_line_ranges("1-10,5-20"),
        Ok(Some(vec![LineRange {
            start: 1,
            end: Some(20)
        }]))
    );
}

/// 1-5 and 6-9 describe one span. Keeping them separate would re-read the
/// boundary line.
#[test]
fn adjacent_ranges_are_merged() {
    assert_eq!(
        parse_line_ranges("1-5,6-9"),
        Ok(Some(vec![LineRange {
            start: 1,
            end: Some(9)
        }]))
    );
}

/// An open-ended range runs to EOF, so a later range is already inside it.
#[test]
fn an_open_ended_range_absorbs_later_ranges() {
    assert_eq!(
        parse_line_ranges("10-,50-60"),
        Ok(Some(vec![LineRange {
            start: 10,
            end: None
        }]))
    );
}

/// One bad chunk disqualifies the whole selector. Accepting the good half
/// would search a subset of what was asked for without saying so.
#[test]
fn one_unparseable_chunk_rejects_the_whole_list() {
    assert_eq!(parse_line_ranges("1-5,garbage"), Ok(None));
}

#[test]
fn is_line_in_ranges_honours_open_ends() {
    let ranges = vec![
        LineRange {
            start: 5,
            end: Some(10),
        },
        LineRange {
            start: 100,
            end: None,
        },
    ];
    assert!(!is_line_in_ranges(4, &ranges));
    assert!(is_line_in_ranges(5, &ranges));
    assert!(is_line_in_ranges(10, &ranges));
    assert!(!is_line_in_ranges(11, &ranges));
    assert!(is_line_in_ranges(1_000_000, &ranges));
}

#[test]
fn a_selector_is_split_off_the_path() {
    assert_eq!(
        split_path_and_selector("src/foo.ts:50-100"),
        SplitPath {
            path: "src/foo.ts".to_string(),
            selector: Some("50-100".to_string()),
        }
    );
}

/// The case omp's `path-literal-colon-selector` test exists for: a real file
/// whose name contains a colon must not be silently truncated.
#[test]
fn a_path_whose_suffix_is_not_selector_shaped_is_left_alone() {
    for path in ["notes:txt", "src/a:b.rs", "http://example.com"] {
        assert_eq!(
            split_path_and_selector(path),
            SplitPath {
                path: path.to_string(),
                selector: None,
            },
            "{path:?} should have been left intact"
        );
    }
}

/// A Windows drive letter is a colon at index 1, and splitting there would
/// leave a one-character path.
#[test]
fn a_windows_drive_letter_is_not_a_selector() {
    assert_eq!(
        split_path_and_selector("C:/src/main.rs"),
        SplitPath {
            path: "C:/src/main.rs".to_string(),
            selector: None,
        }
    );
}

#[test]
fn a_display_mode_is_a_selector() {
    assert_eq!(
        split_path_and_selector("src/foo.ts:raw"),
        SplitPath {
            path: "src/foo.ts".to_string(),
            selector: Some("raw".to_string()),
        }
    );
}

/// Compound selectors are accepted in either order.
#[test]
fn a_compound_selector_is_split_as_one_unit() {
    assert_eq!(
        split_path_and_selector("src/foo.ts:1-50:raw"),
        SplitPath {
            path: "src/foo.ts".to_string(),
            selector: Some("1-50:raw".to_string()),
        }
    );
    assert_eq!(
        split_path_and_selector("src/foo.ts:raw:1-50"),
        SplitPath {
            path: "src/foo.ts".to_string(),
            selector: Some("raw:1-50".to_string()),
        }
    );
}

/// Search honours ranges but has no use for display modes, so a pure `raw`
/// selector means the whole file rather than an error.
#[test]
fn a_display_mode_alone_selects_no_ranges() {
    assert_eq!(selector_line_ranges(Some("raw")), Ok(None));
    assert_eq!(selector_line_ranges(Some("conflicts")), Ok(None));
    assert_eq!(selector_line_ranges(None), Ok(None));
}

#[test]
fn a_compound_selector_still_yields_its_ranges() {
    assert_eq!(
        selector_line_ranges(Some("raw:50-100")),
        Ok(Some(vec![LineRange {
            start: 50,
            end: Some(100)
        }]))
    );
}

/// A selector with impossible bounds must report the mistake rather than
/// quietly searching the whole file.
#[test]
fn an_impossible_range_in_a_selector_is_reported() {
    assert_eq!(
        selector_line_ranges(Some("100-50")),
        Err(SelectorError::Backwards {
            start: 100,
            end: 50
        })
    );
}

#[test]
fn glob_characters_are_detected() {
    for pattern in ["src/**/*.ts", "a?.rs", "[abc].rs", "{a,b}.rs"] {
        assert!(has_glob_chars(pattern), "{pattern:?} is a glob");
    }
    for literal in ["src/main.rs", "Cargo.toml", "a-b_c.rs"] {
        assert!(!has_glob_chars(literal), "{literal:?} is a literal path");
    }
}

#[test]
fn a_semicolon_list_splits_into_entries() {
    assert_eq!(
        split_path_list("src/**/*.ts; test/**/*.ts"),
        vec!["src/**/*.ts".to_string(), "test/**/*.ts".to_string()]
    );
}

/// A trailing semicolon is a typo. Treating the empty entry as a path would
/// search the working directory, turning a scoped search into a whole-repo one.
#[test]
fn empty_entries_are_dropped_rather_than_meaning_everything() {
    assert_eq!(split_path_list("src;"), vec!["src".to_string()]);
    assert_eq!(split_path_list("  ;  "), Vec::<String>::new());
}

/// The error text is what the model reads, so it must say what to do.
#[test]
fn selector_errors_explain_themselves() {
    assert!(SelectorError::ZeroLine.message().contains("1-indexed"));
    assert!(
        SelectorError::Backwards { start: 9, end: 2 }
            .message()
            .contains("end must be >= start")
    );
    assert!(
        SelectorError::EmptyCount { start: 9, count: 0 }
            .message()
            .contains("count must be >= 1")
    );
}

/// A path that is nothing but a selector has no path to search. Splitting it
/// would leave an empty path, which resolves to the working directory and
/// silently turns a scoped search into a whole-repo one.
///
/// Found by mutation testing: weakening the `colon == 0` guard broke nothing,
/// because the drive-letter test above is protected by its candidate not being
/// selector-shaped rather than by that guard.
#[test]
fn a_leading_colon_leaves_no_path_so_nothing_is_split() {
    for raw in [":50-100", ":raw", ":5"] {
        assert_eq!(
            split_path_and_selector(raw),
            SplitPath {
                path: raw.to_string(),
                selector: None,
            },
            "{raw:?} has no path before the colon and must not be split"
        );
    }
}

/// `C:50-100` splits into path `C` plus selector `50-100`, matching omp
/// (`path-utils.ts:306` guards only `colon <= 0`).
///
/// Pinned because it looks like a bug and is not worth "fixing" twice: a bare
/// drive letter with no separator is not a path anyone searches, whereas
/// `notes:50-100` in a Windows checkout is an ordinary scoped search that must
/// keep working. Real drive paths (`C:/src`, `C:\\src`) are protected by their
/// candidate not being selector-shaped, which the test above covers.
#[test]
fn a_bare_drive_letter_with_a_range_splits_like_any_other_path() {
    assert_eq!(
        split_path_and_selector("C:50-100"),
        SplitPath {
            path: "C".to_string(),
            selector: Some("50-100".to_string()),
        }
    );
}

/// A real Windows drive path keeps its separator, so the chunk after the last
/// colon is never selector-shaped.
#[test]
fn a_windows_drive_path_with_a_selector_splits_at_the_selector() {
    assert_eq!(
        split_path_and_selector("C:/src/main.rs:50-100"),
        SplitPath {
            path: "C:/src/main.rs".to_string(),
            selector: Some("50-100".to_string()),
        }
    );
}
