//! Position resolution tests. Group D, plus the cases omp's fixtures imply.

use super::*;

fn column(content: &str, line: usize, symbol: &str) -> usize {
    resolve_column(content, line, Some(symbol)).expect("should resolve")
}

#[test]
fn a_symbol_resolves_to_its_first_occurrence() {
    let content = "fn main() { resolve(); }\n";
    assert_eq!(column(content, 1, "resolve"), 12);
}

/// **omp's `$store` regression, with their exact fixture and expected columns.**
/// Their identifier pattern rejected a leading `$`, so `$store` was treated as a
/// non-identifier, boundary checking was skipped, and it resolved *inside*
/// `bar$store` — handing the server a column in the middle of a different variable.
#[test]
fn a_dollar_identifier_resolves_past_a_compound_match() {
    let content = "let bar$store = $store + 1;\n";

    assert_eq!(
        column(content, 1, "$store"),
        16,
        "must find the standalone $store, not the substring inside bar$store"
    );
    // And the compound is itself a valid `$`-bearing identifier resolving to its
    // own start, not into either fragment.
    assert_eq!(column(content, 1, "bar$store"), 4);
}

/// The plainer version of the same rule: a bare identifier matches only at word
/// boundaries, so `id` does not resolve inside `uuid`.
#[test]
fn a_bare_identifier_does_not_match_inside_a_longer_word() {
    let content = "let uuid = id + 1;\n";
    assert_eq!(
        column(content, 1, "id"),
        11,
        "`id` must skip the `id` inside `uuid`"
    );
}

#[test]
fn boundaries_hold_at_both_ends_of_the_line() {
    // At the very start.
    assert_eq!(column("id = 1;\n", 1, "id"), 0);
    // At the very end, with no trailing character.
    assert_eq!(column("x = id", 1, "id"), 4);
}

#[test]
fn an_underscore_continues_an_identifier_so_it_is_not_a_boundary() {
    let content = "let my_id = id;\n";
    assert_eq!(
        column(content, 1, "id"),
        12,
        "the `id` in `my_id` is not a standalone occurrence"
    );
}

/// **`$` must continue an identifier as well as start one**, and the two are
/// separate rules.
///
/// Found by mutation testing: removing `$` from the *continuation* set left every
/// test passing, because the `$store` case only exercised `$` in the leading
/// position. Without this, `store` would resolve inside `store$1` — a different
/// variable — which is the same class of silent wrong position as omp's original
/// bug, just reached from the other side.
///
/// `$` in identifiers is not exotic: it is legal in JavaScript and TypeScript, and
/// generated code uses it heavily (`store$1`, `_$3`).
#[test]
fn a_dollar_continues_an_identifier_so_it_is_not_a_boundary_either() {
    let content = "let store$1 = store;\n";
    assert_eq!(
        column(content, 1, "store"),
        14,
        "the `store` inside `store$1` is a different variable and must be skipped"
    );

    // The same rule from the other direction: a trailing `$` is part of the word.
    let observable = "let value$ = value;\n";
    assert_eq!(
        column(observable, 1, "value"),
        13,
        "the `value` inside `value$` is not a standalone occurrence"
    );
}

/// A symbol that is not a bare identifier has no meaningful word boundaries, so it
/// is matched as a plain substring. Requiring boundaries would make `foo.bar`
/// unresolvable.
#[test]
fn a_non_identifier_symbol_is_matched_as_a_substring() {
    assert_eq!(column("value = foo.bar();\n", 1, "foo.bar"), 8);
    assert_eq!(column("a -> b\n", 1, "->"), 2);
    assert_eq!(column("x = (a, b);\n", 1, "(a, b)"), 4);
}

// =============================================================================
// Occurrence selectors
// =============================================================================

#[test]
fn an_occurrence_selector_picks_the_nth_match() {
    let content = "foo(foo, foo);\n";
    assert_eq!(column(content, 1, "foo"), 0, "no selector means the first");
    assert_eq!(column(content, 1, "foo#1"), 0);
    assert_eq!(column(content, 1, "foo#2"), 4);
    assert_eq!(column(content, 1, "foo#3"), 9);
}

/// A TypeScript private field is named `#count`, so the split must be on the *last*
/// `#` followed by digits. Splitting on the first would make this unaddressable.
#[test]
fn a_symbol_starting_with_a_hash_still_parses() {
    let spec = parse_symbol("#count");
    assert_eq!(spec.symbol, "#count");
    assert_eq!(spec.occurrence, 1);

    let with_selector = parse_symbol("#count#2");
    assert_eq!(with_selector.symbol, "#count");
    assert_eq!(with_selector.occurrence, 2);
}

#[test]
fn a_trailing_hash_without_digits_is_part_of_the_symbol() {
    let spec = parse_symbol("weird#");
    assert_eq!(spec.symbol, "weird#");
    assert_eq!(spec.occurrence, 1);

    let non_numeric = parse_symbol("weird#abc");
    assert_eq!(non_numeric.symbol, "weird#abc");
    assert_eq!(non_numeric.occurrence, 1);
}

/// Occurrences are 1-indexed, so `#0` is nonsense. Treating it as "the first"
/// would silently accept a request the caller got wrong.
#[test]
fn a_zero_occurrence_selector_is_not_treated_as_a_selector() {
    let spec = parse_symbol("foo#0");
    assert_eq!(spec.symbol, "foo#0", "#0 is not a valid selector");
    assert_eq!(spec.occurrence, 1);
}

/// Matches do not overlap, so `#2` means something predictable. With overlapping
/// matches, `--` in `---` would be two occurrences and the caller could not reason
/// about which one it asked for.
///
/// Uses a non-identifier symbol deliberately. My first attempt used `aa` in `aaa`,
/// which fails for a *different* reason — `aa` is a bare identifier, so word
/// boundaries reject it inside `aaa` before overlap is ever considered. The test
/// passed for the wrong reason until the error message said so, which is a good
/// argument for asserting the error *variant* rather than just that it failed.
#[test]
fn matches_do_not_overlap() {
    // Three dashes contain `--` once by non-overlapping counting, twice by
    // overlapping. `--` is not an identifier, so boundaries do not apply and
    // overlap is the only thing under test.
    let error = resolve_column("x = ---;\n", 1, Some("--#2")).expect_err("only one match");
    match error {
        PositionError::OccurrenceOutOfRange { found, .. } => assert_eq!(
            found, 1,
            "`--` in `---` is one non-overlapping match, not two"
        ),
        other => panic!("expected out-of-range, got {other}"),
    }

    // And the single match resolves where expected.
    assert_eq!(column("x = ---;\n", 1, "--"), 4);
}

// =============================================================================
// Errors, and why they are errors
// =============================================================================

/// **A missing symbol must error, not fall back.** A rename at a guessed position
/// is a silent wrong rename that the model cannot detect; failing tells it to look
/// again.
#[test]
fn a_symbol_absent_from_the_line_is_an_error_naming_what_is_there() {
    let error = resolve_column("fn main() {}\n", 1, Some("missing"))
        .expect_err("must not guess a position");

    match &error {
        PositionError::SymbolNotFound { symbol, line, text } => {
            assert_eq!(symbol, "missing");
            assert_eq!(*line, 1);
            // The line's text is included so the model can see what is actually
            // there rather than guessing again from the same wrong assumption.
            assert_eq!(text, "fn main() {}");
        }
        other => panic!("expected symbol-not-found, got {other}"),
    }
    assert!(error.to_string().contains("fn main() {}"), "{error}");
}

#[test]
fn an_out_of_range_occurrence_is_an_error_saying_how_many_there_are() {
    let error = resolve_column("foo(foo);\n", 1, Some("foo#5")).expect_err("only two occurrences");
    match &error {
        PositionError::OccurrenceOutOfRange { wanted, found, .. } => {
            assert_eq!(*wanted, 5);
            assert_eq!(*found, 2);
        }
        other => panic!("expected out-of-range, got {other}"),
    }
    let text = error.to_string();
    assert!(text.contains('5') && text.contains('2'), "{text}");
}

#[test]
fn a_line_past_the_end_of_the_file_is_an_error() {
    let error = resolve_column("one\ntwo\n", 99, Some("one")).expect_err("no line 99");
    assert!(
        matches!(error, PositionError::NoSuchLine { line: 99, .. }),
        "got {error}"
    );
}

// =============================================================================
// The no-symbol fallback
// =============================================================================

/// With no symbol, resolve to the first non-whitespace character. An explicit
/// "anywhere on this line", which is fine for `hover` and must not be relied on by
/// `rename`.
#[test]
fn no_symbol_resolves_to_the_first_non_whitespace_character() {
    assert_eq!(
        resolve_column("    indented();\n", 1, None).expect("resolves"),
        4
    );
    assert_eq!(resolve_column("flush();\n", 1, None).expect("resolves"), 0);
}

#[test]
fn a_blank_line_with_no_symbol_resolves_to_zero_rather_than_failing() {
    assert_eq!(resolve_column("\n", 1, None).expect("resolves"), 0);
    assert_eq!(resolve_column("   \n", 1, None).expect("resolves"), 0);
}

// =============================================================================
// Line addressing and encoding
// =============================================================================

/// The last line of a file ending in a newline must be addressable. `lines()`
/// swallows the trailing newline, so a naive implementation makes it unreachable.
#[test]
fn the_line_after_a_trailing_newline_is_addressable() {
    let content = "one\ntwo\n";
    assert_eq!(column(content, 2, "two"), 0);
    // And the phantom line after the terminator exists, resolving to 0.
    assert_eq!(resolve_column(content, 3, None).expect("phantom line"), 0);
}

#[test]
fn lines_are_one_indexed() {
    let content = "alpha\nbeta\ngamma\n";
    assert_eq!(column(content, 1, "alpha"), 0);
    assert_eq!(column(content, 2, "beta"), 0);
    assert_eq!(column(content, 3, "gamma"), 0);
    // Line 0 is not a thing; clamped to the first line rather than failing, since
    // an off-by-one from a model is more likely than a deliberate 0.
    assert_eq!(column(content, 0, "alpha"), 0);
}

/// **Offsets are in characters, not bytes.** A byte offset into a line containing
/// non-ASCII puts the cursor mid-character, and comments and strings routinely
/// contain non-ASCII. This is the test that would catch a `find`-based
/// implementation returning byte indices.
#[test]
fn columns_are_character_offsets_not_byte_offsets() {
    // Each of these is multi-byte in UTF-8, so a byte offset would be larger.
    let content = "let café = héllo(wörld);\n";

    let at = column(content, 1, "wörld");
    assert_eq!(
        at,
        content.chars().take_while(|c| *c != 'w').count(),
        "the column must count characters"
    );
    // Proof the distinction is real for this fixture.
    assert_ne!(
        at,
        content.find("wörld").expect("byte index"),
        "the fixture must actually distinguish bytes from characters"
    );
}

#[test]
fn a_symbol_that_is_itself_non_ascii_resolves() {
    let content = "let 日本語 = 1;\n";
    assert_eq!(column(content, 1, "日本語"), 4);
}

/// Case-insensitivity is a **fallback**, not an equal alternative: an exact match
/// must win, or `Foo` resolves onto `foo` and they are different symbols.
#[test]
fn an_exact_match_beats_a_case_insensitive_one() {
    let content = "let foo = Foo::new();\n";
    assert_eq!(column(content, 1, "Foo"), 10, "the exact `Foo` wins");
    assert_eq!(column(content, 1, "foo"), 4, "the exact `foo` wins");
}

#[test]
fn a_case_insensitive_match_is_used_when_there_is_no_exact_one() {
    let content = "let value = Widget::new();\n";
    assert_eq!(
        column(content, 1, "widget"),
        12,
        "with no exact match, fold case rather than failing"
    );
}

#[test]
fn an_empty_symbol_finds_nothing_rather_than_matching_everywhere() {
    let error = resolve_column("anything\n", 1, Some("")).expect_err("empty matches nothing");
    assert!(
        matches!(error, PositionError::SymbolNotFound { .. }),
        "{error}"
    );
}
