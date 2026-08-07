//! Behaviour ported from omp's `prefixes.ts`, which has no dedicated test file
//! of its own — its behaviour is exercised indirectly through `leniency` and
//! `format-v2`. These tests make it explicit, because the module is the one
//! omp's own doc calls out as load-bearing: without it "every content line
//! turns into a (malformed) op".
//!
//! The stakes are asymmetric and the tests reflect it. Failing to strip a
//! prefix writes `12:` into the file. Stripping one that was real content
//! deletes part of a line. The second is worse, so the negative cases here
//! outnumber the positive ones.

use super::*;

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_string).collect()
}

// ─── the core case ───────────────────────────────────────────────────────────

/// The failure this module exists to prevent: a model echoes `read` output back
/// as a body row, and the line number becomes file content.
#[test]
fn hashline_prefixes_are_stripped_when_every_content_line_has_one() {
    let input = lines("1:fn main() {\n2:    let x = 1;\n3:}");

    assert_eq!(
        strip_new_line_prefixes(&input),
        vec!["fn main() {", "    let x = 1;", "}"]
    );
}

/// Indentation must survive. It is the content most likely to be silently
/// mangled, and a diff that loses it is unreadable.
#[test]
fn stripping_preserves_leading_whitespace_of_the_content() {
    let input = lines("10:        deeply_indented();");
    assert_eq!(strip_new_line_prefixes(&input), vec!["        deeply_indented();"]);
}

/// Search output marks matched lines with `>>` and may carry a sigil.
#[test]
fn search_markers_and_sigils_are_recognized_as_prefixes() {
    for (input, expected) in [
        (">>12:matched", "matched"),
        (">>>12:matched", "matched"),
        ("+ 12:added", "added"),
        ("- 12:removed", "removed"),
        ("* 12:changed", "changed"),
    ] {
        assert_eq!(
            strip_new_line_prefixes(&lines(input)),
            vec![expected],
            "failed on {input:?}"
        );
    }
}

#[test]
fn diff_style_plus_prefixes_are_stripped_when_at_least_half_carry_one() {
    let input = lines("+added one\n+added two\n+added three");
    assert_eq!(
        strip_new_line_prefixes(&input),
        vec!["added one", "added two", "added three"]
    );
}

/// `++` is an escaped literal `+`, not a diff marker. Stripping it would eat a
/// real character from a Markdown list or a C++ increment.
#[test]
fn a_doubled_plus_is_literal_content_not_a_diff_marker() {
    let input = lines("++literal\n++also literal");
    assert_eq!(strip_new_line_prefixes(&input), vec!["++literal", "++also literal"]);
}

// ─── the dangerous direction: content that merely looks prefixed ─────────────

/// The reason hashline stripping demands *every* content line match. One line
/// that happens to start with `digits:` is far more likely to be a timestamp,
/// a YAML key, or a dict literal than an echoed prefix.
#[test]
fn a_partial_match_is_left_alone_because_it_is_probably_real_content() {
    let input = lines("12:00 is the meeting time\nplain prose line\nanother plain line");
    assert_eq!(
        strip_new_line_prefixes(&input),
        input,
        "one prefix-looking line among prose must not trigger stripping"
    );
}

/// Content that is genuinely `digits:` on every line is ambiguous, and omp
/// resolves it toward stripping. Pinned so the choice is visible: this input
/// loses its leading numbers.
#[test]
fn uniformly_digit_colon_content_is_treated_as_prefixed_which_is_the_known_tradeoff() {
    let input = lines("12:00\n13:30\n14:45");

    assert_eq!(
        strip_new_line_prefixes(&input),
        vec!["00", "30", "45"],
        "ambiguous by construction; omp strips, and callers that cannot \
         tolerate this use strip_hashline_prefixes with content they trust"
    );
}

/// Unprefixed input must pass through untouched. This is the common case for a
/// model that authored a payload from scratch rather than by echoing.
#[test]
fn unprefixed_content_passes_through_unchanged() {
    let input = lines("fn main() {\n    let x = 1;\n}");
    assert_eq!(strip_new_line_prefixes(&input), input);
}

#[test]
fn an_empty_payload_passes_through() {
    assert_eq!(strip_new_line_prefixes(&[]), Vec::<String>::new());
    assert_eq!(strip_new_line_prefixes(&lines("")), vec![""]);
}

/// Stacked prefixes happen when output is echoed through more than one tool.
#[test]
fn stacked_prefixes_are_all_stripped() {
    let input = lines("1:2:actual content");
    assert_eq!(strip_new_line_prefixes(&input), vec!["actual content"]);
}

/// The single-strip variant exists precisely so content beginning with
/// `digits:` survives one layer of prefixing.
#[test]
fn the_single_strip_variant_removes_exactly_one_layer() {
    assert_eq!(strip_one_leading_hashline_prefix("1:2:content"), "2:content");
    assert_eq!(strip_one_leading_hashline_prefix("no prefix here"), "no prefix here");
}

// ─── read metadata: rows that are not source ─────────────────────────────────

/// An elision marker stands for content the model never saw. Treating it as a
/// body row would write the marker into the file and delete what it elided —
/// the worst available outcome, and the reason these are filtered rather than
/// stripped.
#[test]
fn elision_markers_are_recognized_as_metadata() {
    for line in [
        "…",
        "...",
        "  …  ",
        "12-40:    … elided …",
        "[…120ln elided; re-read needed ranges with foo.rs:5-16]",
        "[Showing lines 1-50 of 900. Use :50- to continue]",
        "[850 more lines in file. Use :51- to continue]",
    ] {
        assert!(is_read_metadata_line(line), "should be metadata: {line:?}");
    }
}

/// Real content must never be mistaken for metadata.
#[test]
fn ordinary_content_is_not_metadata() {
    for line in [
        "fn main() {",
        "let range = 12-40;",
        "// ... and so on",
        "[dependencies]",
        "[package#name]",
        "",
    ] {
        assert!(!is_read_metadata_line(line), "should not be metadata: {line:?}");
    }
}

#[test]
fn metadata_rows_are_dropped_from_a_prefixed_payload() {
    let input = lines("1:first\n…\n3:third");

    assert_eq!(
        strip_new_line_prefixes(&input),
        vec!["first", "third"],
        "the elision marker must not survive as a body row"
    );
}

/// Section headers are addressing, not content, so they are dropped when the
/// payload is recognized as echoed file output.
#[test]
fn section_headers_are_dropped_from_a_prefixed_payload() {
    let input = lines("[src/foo.rs#1A2B]\n1:first\n2:second");

    assert_eq!(strip_new_line_prefixes(&input), vec!["first", "second"]);
}

/// A bracketed line that is not a valid header is content. `[dependencies]` in
/// a Cargo.toml is the obvious case.
#[test]
fn a_bracketed_line_that_is_not_a_valid_header_is_content() {
    let input = lines("1:[dependencies]\n2:serde = \"1\"");
    assert_eq!(strip_new_line_prefixes(&input), vec!["[dependencies]", "serde = \"1\""]);
}

// ─── the strict variant ──────────────────────────────────────────────────────

#[test]
fn the_strict_variant_strips_only_when_every_content_line_is_prefixed() {
    let all = lines("1:a\n2:b");
    assert_eq!(strip_hashline_prefixes(&all), vec!["a", "b"]);

    let partial = lines("1:a\nplain");
    assert_eq!(
        strip_hashline_prefixes(&partial),
        partial,
        "a single unprefixed line must veto the whole strip"
    );
}

/// The strict variant must not accept a diff `+` block, which the lenient one
/// does. That difference is the entire reason both exist.
#[test]
fn the_strict_variant_ignores_diff_plus_payloads() {
    let input = lines("+added one\n+added two");
    assert_eq!(strip_hashline_prefixes(&input), input);
    assert_eq!(strip_new_line_prefixes(&input), vec!["added one", "added two"]);
}

// ─── payload normalization ───────────────────────────────────────────────────

/// A trailing newline must not produce a phantom empty final row, which would
/// append a blank line to the file on every edit.
#[test]
fn a_trailing_newline_does_not_produce_a_phantom_row() {
    assert_eq!(parse_payload_text("a\nb\n"), vec!["a", "b"]);
    assert_eq!(parse_payload_text("a\nb"), vec!["a", "b"]);
}

#[test]
fn carriage_returns_are_removed_so_crlf_behaves_like_lf() {
    assert_eq!(parse_payload_text("a\r\nb\r\n"), vec!["a", "b"]);
}

#[test]
fn payload_normalization_also_strips_prefixes() {
    assert_eq!(parse_payload_text("1:a\n2:b\n"), vec!["a", "b"]);
}

/// A blank line inside a payload is intentional content and must survive, or
/// every edit silently collapses the blank lines in its body.
#[test]
fn blank_lines_inside_a_payload_survive() {
    assert_eq!(parse_payload_text("a\n\nb"), vec!["a", "", "b"]);
}
