//! Behaviour spec for text shape.
//!
//! Rules from oh-my-pi's `packages/hashline/src/normalize.ts`, and the
//! round-trip cases their `apply-patch-adverserial.test.ts` pins:
//! "preserves CRLF endings and trailing newline", "preserves UTF-8 BOM and
//! CRLF endings", "preserves missing trailing newline".

use super::*;

#[test]
fn line_endings_are_detected_by_which_comes_first() {
    assert_eq!(detect_line_ending("a\r\nb\r\n"), LineEnding::Crlf);
    assert_eq!(detect_line_ending("a\nb\n"), LineEnding::Lf);
}

/// A file with no newline at all has no ending to detect, so LF is the safe
/// default: it is what a later appended line will use.
#[test]
fn a_file_without_newlines_defaults_to_lf() {
    assert_eq!(detect_line_ending("single line"), LineEnding::Lf);
    assert_eq!(detect_line_ending(""), LineEnding::Lf);
}

/// Decided by which comes first, not by counting. A file whose first line ends
/// CRLF is a CRLF file even with stray LF lines later, and rewriting it to the
/// majority style would touch every line.
#[test]
fn a_mixed_file_takes_the_style_of_its_first_ending() {
    assert_eq!(detect_line_ending("a\r\nb\nc\n"), LineEnding::Crlf);
    assert_eq!(detect_line_ending("a\nb\r\nc\r\n"), LineEnding::Lf);
}

#[test]
fn normalizing_collapses_crlf_and_lone_cr() {
    assert_eq!(normalize_to_lf("a\r\nb\r\n"), "a\nb\n");
    assert_eq!(normalize_to_lf("a\rb\r"), "a\nb\n");
    assert_eq!(normalize_to_lf("a\nb\n"), "a\nb\n");
}

#[test]
fn restoring_re_encodes_to_the_requested_ending() {
    assert_eq!(restore_line_endings("a\nb\n", LineEnding::Crlf), "a\r\nb\r\n");
    assert_eq!(restore_line_endings("a\nb\n", LineEnding::Lf), "a\nb\n");
}

#[test]
fn a_bom_is_split_off_and_reported() {
    let (had_bom, rest) = strip_bom("\u{feff}content");
    assert!(had_bom);
    assert_eq!(rest, "content");

    let (had_bom, rest) = strip_bom("content");
    assert!(!had_bom);
    assert_eq!(rest, "content");
}

#[test]
fn trailing_newlines_are_detected() {
    assert!(has_trailing_newline("a\n"));
    assert!(!has_trailing_newline("a"));
    assert!(!has_trailing_newline(""));
}

/// The round trip is the point: capture then restore with no edit in between
/// must return the file byte for byte, or every patch rewrites the whole file.
#[test]
fn capture_and_restore_round_trips_exactly() {
    for original in [
        "a\nb\n",
        "a\r\nb\r\n",
        "\u{feff}a\nb\n",
        "\u{feff}a\r\nb\r\n",
        "a\nb",
        "a\r\nb",
        "\u{feff}a\r\nb",
        "single",
        "",
    ] {
        let (shape, normalized) = TextShape::capture(original);
        assert_eq!(
            shape.restore(&normalized),
            original,
            "round trip changed {original:?}"
        );
    }
}

/// Capture normalizes for matching: patches are written in LF and without a
/// BOM, so that is what they are matched against.
#[test]
fn capture_returns_lf_text_without_a_bom() {
    let (shape, normalized) = TextShape::capture("\u{feff}a\r\nb\r\n");

    assert_eq!(normalized, "a\nb\n");
    assert_eq!(shape.line_ending, LineEnding::Crlf);
    assert!(shape.bom);
    assert!(shape.trailing_newline);
}

/// omp's "preserves CRLF endings and trailing newline".
#[test]
fn an_edited_crlf_file_stays_crlf() {
    let (shape, _) = TextShape::capture("alpha\r\nbeta\r\n");
    assert_eq!(shape.restore("alpha\nBETA\n"), "alpha\r\nBETA\r\n");
}

/// omp's "preserves UTF-8 BOM and CRLF endings".
#[test]
fn an_edited_bom_file_keeps_its_bom() {
    let (shape, _) = TextShape::capture("\u{feff}alpha\r\n");
    assert_eq!(shape.restore("ALPHA\n"), "\u{feff}ALPHA\r\n");
}

/// omp's "preserves missing trailing newline". A file with no final newline is
/// a deliberate state in some formats, and adding one is a real change nobody
/// asked for.
#[test]
fn a_file_without_a_trailing_newline_does_not_gain_one() {
    let (shape, _) = TextShape::capture("alpha\nbeta");
    assert_eq!(shape.restore("alpha\nBETA\n"), "alpha\nBETA");
}

#[test]
fn a_file_with_a_trailing_newline_keeps_it() {
    let (shape, _) = TextShape::capture("alpha\n");
    assert_eq!(shape.restore("ALPHA"), "ALPHA\n");
}

/// Restoring must not fabricate a newline for an emptied file: "" and "\n" are
/// different files.
#[test]
fn an_emptied_file_does_not_gain_a_newline() {
    let (shape, _) = TextShape::capture("alpha\n");
    assert_eq!(shape.restore(""), "");
}

/// Several trailing newlines collapse to none when the original had none, so a
/// patch cannot leave a blank tail behind.
#[test]
fn extra_trailing_newlines_are_removed_when_the_original_had_none() {
    let (shape, _) = TextShape::capture("alpha");
    assert_eq!(shape.restore("alpha\n\n\n"), "alpha");
}
