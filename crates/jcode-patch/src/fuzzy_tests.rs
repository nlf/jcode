//! Behaviour spec for fuzzy matching.
//!
//! Values and rules from oh-my-pi's `src/edit/modes/replace.ts`.

use super::*;

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn identical_strings_have_no_distance() {
    assert_eq!(levenshtein_distance("abc", "abc"), 0);
    assert_eq!(similarity("abc", "abc"), 1.0);
}

#[test]
fn distance_counts_single_edits() {
    assert_eq!(levenshtein_distance("abc", "abd"), 1, "substitution");
    assert_eq!(levenshtein_distance("abc", "abcd"), 1, "insertion");
    assert_eq!(levenshtein_distance("abc", "ab"), 1, "deletion");
}

#[test]
fn an_empty_string_costs_the_other_length() {
    assert_eq!(levenshtein_distance("", "abc"), 3);
    assert_eq!(levenshtein_distance("abc", ""), 3);
    assert_eq!(similarity("", ""), 1.0);
}

/// Multi-byte characters count as one edit, not one per byte. Comparing bytes
/// would score an accented identifier as wildly different from itself.
#[test]
fn distance_is_measured_in_characters_not_bytes() {
    assert_eq!(levenshtein_distance("café", "cafe"), 1);
    assert_eq!(levenshtein_distance("日本語", "日本"), 1);
}

#[test]
fn normalizing_collapses_indentation_and_interior_runs() {
    assert_eq!(normalize_for_fuzzy("    let  x   =  1;  "), "let x = 1;");
    assert_eq!(normalize_for_fuzzy("\tlet\tx = 1;"), "let x = 1;");
}

#[test]
fn leading_whitespace_is_reported_exactly() {
    assert_eq!(leading_whitespace("    code"), "    ");
    assert_eq!(leading_whitespace("\t\tcode"), "\t\t");
    assert_eq!(leading_whitespace("code"), "");
    assert_eq!(count_leading_whitespace("    code"), 4);
}

#[test]
fn an_exact_sequence_is_found_at_its_index() {
    let haystack = lines("a\nb\nc\nd");
    let found = find_exact_sequence(&haystack, &lines("b\nc"), 0).expect("should match");
    assert_eq!(found.start, 1);
    assert_eq!(found.score, 1.0);
}

#[test]
fn a_missing_sequence_is_not_found() {
    let haystack = lines("a\nb\nc");
    assert!(find_exact_sequence(&haystack, &lines("x\ny"), 0).is_none());
}

#[test]
fn a_needle_longer_than_the_haystack_is_not_found() {
    let haystack = lines("a");
    assert!(find_exact_sequence(&haystack, &lines("a\nb"), 0).is_none());
    assert!(find_fuzzy_sequence(&haystack, &lines("a\nb"), 0, DEFAULT_FUZZY_THRESHOLD).is_none());
}

/// The point of fuzzy matching: a patch written against a differently indented
/// copy still applies, because the code is genuinely there.
#[test]
fn a_reindented_block_still_matches() {
    let haystack = lines("fn main() {\n        let x = 1;\n}");
    let needle = lines("    let x = 1;");

    assert!(
        find_exact_sequence(&haystack, &needle, 0).is_none(),
        "indentation differs, so it is not an exact match"
    );
    let found = find_fuzzy_sequence(&haystack, &needle, 0, DEFAULT_FUZZY_THRESHOLD)
        .expect("fuzzy matching should absorb indentation");
    assert_eq!(found.start, 1);
}

/// Matching too loosely is worse than failing: the edit lands somewhere the
/// caller never looked.
#[test]
fn a_genuinely_different_block_is_refused() {
    let haystack = lines("let alpha = compute_total();");
    let needle = lines("let beta = fetch_remote_config();");

    assert!(
        find_fuzzy_sequence(&haystack, &needle, 0, DEFAULT_FUZZY_THRESHOLD).is_none(),
        "different code must not match"
    );
}

/// Every line must clear the threshold on its own. Averaging across the block
/// would let one badly wrong line hide inside a run of good ones.
#[test]
fn one_bad_line_disqualifies_the_whole_match() {
    let haystack = lines("let a = 1;\nlet b = 2;\ntotally different here\nlet d = 4;");
    let needle = lines("let a = 1;\nlet b = 2;\nlet c = 3;");

    assert!(
        find_fuzzy_sequence(&haystack, &needle, 0, DEFAULT_FUZZY_THRESHOLD).is_none(),
        "a block containing one wrong line must not match"
    );
}

/// An exact match is unambiguous, so it wins over an approximate one elsewhere.
#[test]
fn seeking_prefers_an_exact_match_over_a_fuzzy_one() {
    let haystack = lines("  let x = 1;\nlet x = 1;");
    let found = seek_sequence(&haystack, &lines("let x = 1;"), 0, DEFAULT_FUZZY_THRESHOLD)
        .expect("should match");

    assert_eq!(found.start, 1, "the exact match is at index 1");
    assert_eq!(found.score, 1.0);
}

#[test]
fn searching_can_start_past_an_earlier_match() {
    let haystack = lines("target\nfiller\ntarget");
    let found = find_exact_sequence(&haystack, &lines("target"), 1).expect("should match");
    assert_eq!(found.start, 2);
}

/// Ties go to the earliest match: moving an edit further from where the caller
/// was looking needs a reason.
#[test]
fn equally_good_fuzzy_matches_resolve_to_the_first() {
    let haystack = lines("let x = 1;\nlet x = 1;");
    let found = find_fuzzy_sequence(&haystack, &lines("let  x  =  1;"), 0, DEFAULT_FUZZY_THRESHOLD)
        .expect("should match");
    assert_eq!(found.start, 0);
}

/// A rejection saying "closest was 82% similar at line 3" tells the caller
/// their patch is stale. A bare "not found" sends them re-reading everything.
#[test]
fn the_closest_match_is_reported_even_below_threshold() {
    let haystack = lines("alpha\nbeta\nlet value = compute();");
    let needle = lines("let value = compute_all();");

    let closest = find_closest_sequence(&haystack, &needle).expect("something is always closest");
    assert_eq!(closest.start, 2, "the similar line is at index 2");
    assert!(
        closest.score < 1.0 && closest.score > 0.5,
        "score should reflect partial similarity: {}",
        closest.score
    );
}

#[test]
fn a_context_header_locates_its_line() {
    let haystack = lines("fn a() {}\nfn target() {}\nfn b() {}");
    assert_eq!(find_context_line(&haystack, "fn target() {}", 0), Some(1));
}

/// The header is a hint, matched loosely, so re-indentation does not break it.
#[test]
fn a_context_header_matches_across_whitespace() {
    let haystack = lines("    fn target() {}");
    assert_eq!(find_context_line(&haystack, "fn  target()  {}", 0), Some(0));
}

#[test]
fn a_missing_context_header_is_not_located() {
    let haystack = lines("fn a() {}");
    assert_eq!(find_context_line(&haystack, "fn nowhere() {}", 0), None);
    assert_eq!(find_context_line(&haystack, "   ", 0), None);
}

/// A patch written against a differently indented copy has the right shape and
/// the wrong absolute indentation. Applying it verbatim leaves misaligned code
/// that shows up as a spurious diff on every later edit.
#[test]
fn replacement_lines_are_reindented_to_the_matched_block() {
    let matched = lines("        let x = 1;");
    let replacement = lines("    let x = 2;\n    let y = 3;");

    let adjusted = adjust_indentation(&matched, &replacement);
    assert_eq!(adjusted, lines("        let x = 2;\n        let y = 3;"));
}

#[test]
fn matching_indentation_is_left_untouched() {
    let matched = lines("    let x = 1;");
    let replacement = lines("    let x = 2;");
    assert_eq!(adjust_indentation(&matched, &replacement), replacement);
}

/// Giving a blank line indentation would add trailing whitespace, which many
/// projects reject in CI.
#[test]
fn blank_lines_gain_no_indentation() {
    let matched = lines("        code");
    let replacement = vec!["    a".to_string(), String::new(), "    b".to_string()];

    let adjusted = adjust_indentation(&matched, &replacement);
    assert_eq!(adjusted[1], "", "a blank line must stay blank");
    assert_eq!(adjusted[0], "        a");
    assert_eq!(adjusted[2], "        b");
}

/// A line less indented than the block's own base is unusual; leaving it alone
/// beats guessing.
#[test]
fn lines_outdented_past_the_base_are_left_alone() {
    let matched = lines("        code");
    let replacement = vec!["    normal".to_string(), "outdented".to_string()];

    let adjusted = adjust_indentation(&matched, &replacement);
    assert_eq!(adjusted[1], "outdented");
}

/// omp's threshold. Pinned because it is the whole tradeoff between rejecting
/// valid patches and applying them in the wrong place.
#[test]
fn the_threshold_matches_omps() {
    assert_eq!(DEFAULT_FUZZY_THRESHOLD, 0.95);
}

/// A whitespace-only line is the case the blank-line guard actually exists for.
///
/// Found by mutation testing: removing the guard broke nothing, because a truly
/// empty line falls through to the outdented-line branch and is returned
/// unchanged anyway. A line of exactly the replacement indent does strip
/// successfully, so without the guard it is rewritten to the matched indent and
/// becomes trailing whitespace, which many projects reject in CI.
#[test]
fn a_whitespace_only_line_does_not_become_trailing_whitespace() {
    let matched = lines("        code");
    let replacement = vec!["    a".to_string(), "    ".to_string(), "    b".to_string()];

    let adjusted = adjust_indentation(&matched, &replacement);
    assert_eq!(
        adjusted[1], "    ",
        "a whitespace-only line must not be re-indented into longer trailing whitespace"
    );
    assert_eq!(adjusted[0], "        a");
    assert_eq!(adjusted[2], "        b");
}
