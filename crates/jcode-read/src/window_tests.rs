//! Behaviour spec for window resolution.
//!
//! Constants and rules from oh-my-pi's `src/tools/read.ts`, including the
//! asymmetric range padding their own telemetry justifies.

use super::*;

fn range(start: usize, end: Option<usize>) -> LineRange {
    LineRange { start, end }
}

fn request(ranges: Vec<LineRange>) -> Request {
    Request {
        ranges,
        limit: None,
    }
}

#[test]
fn no_range_reads_from_the_top_up_to_the_default_cap() {
    let windows = resolve(&request(Vec::new()), 10_000);
    assert_eq!(
        windows,
        vec![Window {
            start: 1,
            end: DEFAULT_MAX_LINES
        }]
    );
}

#[test]
fn a_short_file_is_shown_whole() {
    let windows = resolve(&request(Vec::new()), 12);
    assert_eq!(windows, vec![Window { start: 1, end: 12 }]);
}

#[test]
fn an_empty_file_yields_no_windows() {
    assert_eq!(resolve(&request(Vec::new()), 0), Vec::new());
}

#[test]
fn an_explicit_limit_overrides_the_default() {
    let windows = resolve(
        &Request {
            ranges: Vec::new(),
            limit: Some(5),
        },
        100,
    );
    assert_eq!(windows, vec![Window { start: 1, end: 5 }]);
}

/// A read from the top has nothing above it, so padding would only shift the
/// line the caller asked to start at.
#[test]
fn an_unconstrained_read_gets_no_leading_context() {
    let windows = resolve(&request(Vec::new()), 100);
    assert_eq!(windows[0].start, 1);
}

/// omp's asymmetry: one line above, three below.
#[test]
fn an_explicit_range_is_padded_asymmetrically() {
    let windows = resolve(&request(vec![range(50, Some(60))]), 1000);

    assert_eq!(
        windows,
        vec![Window {
            start: 49,
            end: 63
        }],
        "one line of leading context, three of trailing"
    );
}

/// An open-ended range already runs to the end of the file, so there is no
/// below to expand into.
#[test]
fn an_open_ended_range_gets_leading_context_only() {
    let windows = resolve(&request(vec![range(50, None)]), 100);
    assert_eq!(windows, vec![Window { start: 49, end: 100 }]);
}

#[test]
fn context_never_runs_past_the_ends_of_the_file() {
    let windows = resolve(&request(vec![range(1, Some(2))]), 3);
    assert_eq!(
        windows,
        vec![Window { start: 1, end: 3 }],
        "leading context cannot go below line 1, trailing cannot pass the end"
    );
}

#[test]
fn several_ranges_produce_several_windows() {
    let windows = resolve(&request(vec![range(10, Some(12)), range(500, Some(502))]), 1000);
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].start, 9);
    assert_eq!(windows[1].start, 499);
}

/// Two ranges that end up adjacent after padding are one span. An elision
/// marker between consecutive lines would claim content was omitted when none
/// was.
#[test]
fn windows_that_touch_after_padding_are_merged() {
    // 10-12 pads to 9-15; 16-18 pads to 15-21. They overlap at 15.
    let windows = resolve(&request(vec![range(10, Some(12)), range(16, Some(18))]), 100);
    assert_eq!(windows, vec![Window { start: 9, end: 21 }]);
}

#[test]
fn overlapping_ranges_are_merged() {
    let windows = resolve(&request(vec![range(10, Some(30)), range(20, Some(40))]), 100);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].start, 9);
    assert_eq!(windows[0].end, 43);
}

/// Ranges are returned in file order however they were written, so the output
/// reads top to bottom.
#[test]
fn windows_come_back_in_file_order() {
    let windows = resolve(&request(vec![range(500, Some(502)), range(10, Some(12))]), 1000);
    assert!(windows[0].start < windows[1].start);
}

/// Clamping a wholly-out-of-range selector would show the last few lines and
/// imply they are what was asked for.
#[test]
fn a_range_past_the_end_of_the_file_is_dropped() {
    let windows = resolve(&request(vec![range(5000, Some(5010))]), 100);
    assert_eq!(windows, Vec::new());
}

#[test]
fn a_range_ending_past_the_file_is_clamped_to_it() {
    let windows = resolve(&request(vec![range(95, Some(500))]), 100);
    assert_eq!(windows, vec![Window { start: 94, end: 100 }]);
}

#[test]
fn a_window_reports_its_length() {
    assert_eq!(Window { start: 5, end: 9 }.len(), 5);
    assert_eq!(Window { start: 5, end: 5 }.len(), 1);
}

#[test]
fn the_outcome_records_every_line_shown() {
    let result = outcome(vec![Window { start: 3, end: 5 }], 100);
    assert_eq!(result.shown_lines, vec![3, 4, 5]);
}

#[test]
fn shown_lines_span_several_windows() {
    let result = outcome(
        vec![Window { start: 1, end: 2 }, Window { start: 10, end: 11 }],
        100,
    );
    assert_eq!(result.shown_lines, vec![1, 2, 10, 11]);
}

#[test]
fn an_outcome_that_reached_the_end_is_not_truncated() {
    let result = outcome(vec![Window { start: 1, end: 50 }], 50);
    assert!(!result.truncated);
    assert_eq!(result.continuation("f.rs"), "");
}

/// The hint has to be a call the model can copy, not a bare number it has to
/// assemble into one.
#[test]
fn a_truncated_outcome_names_the_call_that_continues_it() {
    let result = outcome(vec![Window { start: 1, end: 3000 }], 5000);

    assert!(result.truncated);
    let hint = result.continuation("src/lib.rs");
    assert!(hint.contains("2000 more lines"), "{hint}");
    assert!(
        hint.contains("src/lib.rs:3001-"),
        "the hint should be copyable: {hint}"
    );
}

/// omp's constants, pinned because they are a considered tradeoff between
/// output size and usefulness.
#[test]
fn the_limits_match_omps() {
    assert_eq!(DEFAULT_MAX_LINES, 3000);
    assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
    assert_eq!(RANGE_LEADING_CONTEXT_LINES, 1);
    assert_eq!(RANGE_TRAILING_CONTEXT_LINES, 3);
    // A const block, so the relationship between the constants is checked at
    // compile time: a build that violates it should not link.
    const {
        assert!(
            RANGE_TRAILING_CONTEXT_LINES > RANGE_LEADING_CONTEXT_LINES,
            "the padding is deliberately asymmetric"
        )
    };
}

#[test]
fn expanding_respects_which_sides_were_constrained() {
    let both = expand_with_context(50, 60, 1000, true, true);
    assert_eq!(both, Window { start: 49, end: 63 });

    let neither = expand_with_context(50, 60, 1000, false, false);
    assert_eq!(neither, Window { start: 50, end: 60 });
}

/// Windows that are merely adjacent, with no overlap, are still one span.
///
/// Found by mutation testing: `<= last.end + 1` could be weakened to
/// `<= last.end` with nothing failing, because the existing merge test used
/// ranges that overlap after padding rather than ones that only touch.
#[test]
fn windows_separated_by_nothing_are_merged() {
    let first = Window { start: 1, end: 10 };
    let second = Window { start: 11, end: 20 };

    // Constructed directly: after padding, real selectors rarely land exactly
    // adjacent, which is why this went untested.
    let merged = merge_for_test(vec![first, second]);
    assert_eq!(
        merged,
        vec![Window { start: 1, end: 20 }],
        "line 11 follows line 10, so there is nothing between them to elide"
    );
}

/// A real gap must survive, or the output claims contiguity it does not have.
#[test]
fn windows_with_a_real_gap_stay_separate() {
    let merged = merge_for_test(vec![
        Window { start: 1, end: 10 },
        Window { start: 20, end: 30 },
    ]);
    assert_eq!(merged.len(), 2, "lines 11-19 are genuinely missing");
}
