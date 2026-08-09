//! Group G tests: sanitizing server-supplied text.
//!
//! omp's three cases are `lsp-regressions.test.ts:1296`, `:1334` and `:1358`. Theirs
//! go through a full renderer and a theme, then assert on the rendered string. We
//! have no render layer yet, so these test the sanitizers directly — which is where
//! the property lives. Their assertions transfer exactly:
//!
//! - no tab survives, and
//! - the words are still there and still separated.
//!
//! The theme and the widths are theirs; the sanitization is the part that is ours to
//! get right.

use super::*;

/// **G1: symbol metadata is safe to render inline.**
///
/// omp: "sanitizes symbol metadata in renderer output", with the symbol
/// `foo\tbar\nbaz`. A symbol name reaches a one-line header, so both the tab and the
/// newline have to go, and the three words must remain three words.
#[test]
fn a_symbol_containing_a_tab_and_a_newline_renders_on_one_line() {
    let rendered = inline("foo\tbar\nbaz");

    assert!(!rendered.contains('\t'), "a tab survived: {rendered:?}");
    assert!(!rendered.contains('\n'), "a newline survived: {rendered:?}");
    // omp normalizes runs of whitespace before asserting "foo bar baz"; do the same,
    // because the tab legitimately becomes several spaces.
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(normalized, "foo bar baz");
}

/// **G2: tabs inside a diagnostic message.**
///
/// omp: "sanitizes tabs in rendered diagnostic output", with Go's
/// `too many\targuments in call`. This is the everyday case rather than an exotic
/// one: the compiler put a tab in the message because it was quoting source.
#[test]
fn a_diagnostic_message_containing_a_tab_loses_it_but_keeps_the_words() {
    let message = "src/example.go:183:41 [error] [compiler] too many\targuments in call \
                   (WrongArgCount)";
    let rendered = block(message);

    assert!(!rendered.contains('\t'), "a tab survived: {rendered:?}");
    assert!(
        rendered
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("too many arguments in call"),
        "the message lost its words: {rendered:?}"
    );
}

/// **G3: an expanded error is sanitized and truncated (omp's #7041).**
///
/// Their case is `Error:\nserver\tstderr ` followed by 200 `x`s, and it asserts both
/// that no tab survives *and* that 100 consecutive `x`s do not. So the incident was
/// about length as well as tabs: sanitizing without bounding would still have broken
/// their renderer.
#[test]
fn an_expanded_error_is_sanitized_and_truncated() {
    let raw = format!("Error:\nserver\tstderr {}", "x".repeat(200));
    let rendered = truncate(&block(&raw), 80);

    assert!(!rendered.contains('\t'), "a tab survived: {rendered:?}");
    assert!(
        !rendered.contains(&"x".repeat(100)),
        "an unbounded run survived truncation: {} chars",
        rendered.chars().count()
    );
    // Their case keeps the newline (it is a block), so the structure is preserved
    // while the length is not.
    assert!(
        rendered.starts_with("Error:\nserver"),
        "truncation ate the beginning: {rendered:?}"
    );
}

/// Tab stops are columns, not a fixed substitution.
///
/// Nothing in omp asserts this, because their `replaceTabs` cannot do it: they
/// replace one tab with three spaces wherever it falls. Ours aligns, so it is our
/// job to test. A diagnostic is rendered next to a diff that measures tabs at 4, and
/// two different answers about tab width in one screen is a visible defect.
#[test]
fn a_tab_advances_to_the_next_stop_rather_than_a_fixed_width() {
    // Column 0: a full 4-wide stop.
    assert_eq!(expand_tabs("\tx"), "    x");
    // Column 3: one space reaches stop 4.
    assert_eq!(expand_tabs("abc\tx"), "abc x");
    // Column 4: already on a stop, so a full width follows.
    assert_eq!(expand_tabs("abcd\tx"), "abcd    x");
}

/// Each line's tab stops are measured from its own start.
///
/// The bug this catches is a single running column counter across newlines, which is
/// the obvious way to write it and makes every line after the first misalign. Found
/// by writing the loop and then asking what `column` meant after a `\n`.
#[test]
fn tab_stops_restart_on_each_line() {
    assert_eq!(expand_tabs("ab\n\tx"), "ab\n    x");
    assert_eq!(expand_tabs("abcdef\nab\tx"), "abcdef\nab  x");
}

/// A newline becomes a space inline, and is not merely deleted.
///
/// Deleting it turns `foo\nbar` into `foobar`, silently inventing an identifier that
/// the server never sent. In a *symbol* name that is actively misleading, since the
/// reader has no way to tell it was two lines.
#[test]
fn an_inline_newline_becomes_a_space_rather_than_vanishing() {
    assert_eq!(inline("foo\nbar"), "foo bar");
    // And CRLF is one space, not two.
    assert_eq!(inline("foo\r\nbar"), "foo bar");
    // A lone CR too, which some servers on Windows emit.
    assert_eq!(inline("foo\rbar"), "foo bar");
}

/// ANSI is stripped, not passed through or escaped into visible junk.
///
/// A server that colours its own diagnostics (some do, when they think they are on a
/// terminal) would otherwise print `[31m` as text, or worse, leave an unterminated
/// OSC sequence that swallows what follows.
#[test]
fn ansi_escapes_are_removed_from_both_forms() {
    assert_eq!(inline("\u{1b}[31merror\u{1b}[0m"), "error");
    assert_eq!(block("\u{1b}[1mbold\u{1b}[0m\nplain"), "bold\nplain");
    // An OSC string: the payload must go with it, not be left as text. This is the
    // case a naive control-character filter gets wrong.
    assert_eq!(inline("\u{1b}]0;title\u{7}after"), "after");
}

/// Block sanitization keeps newlines and drops everything else that is control.
#[test]
fn block_keeps_line_structure_and_drops_other_controls() {
    let sanitized = block("one\ntwo\u{0}three\u{7}\r\nfour");
    assert_eq!(sanitized, "one\ntwothree\nfour");
}

/// Truncation counts characters, so a multi-byte string is not split mid-character
/// and the limit means the same for any language.
///
/// A byte-based `[..limit]` would panic on a char boundary here, which is the actual
/// failure mode being ruled out.
#[test]
fn truncation_counts_characters_not_bytes() {
    // 10 CJK characters: 30 bytes, 10 chars.
    let text = "日本語のテキスト です";
    assert_eq!(text.chars().count(), 11);

    let cut = truncate(text, 5);
    assert_eq!(cut.chars().count(), 5, "got {cut:?}");
    assert!(cut.ends_with('…'));

    // At the limit exactly: untouched, and no ellipsis.
    let exact = truncate(text, 11);
    assert_eq!(exact, text);
}

/// Text at or under the limit is returned unchanged, including empty.
#[test]
fn truncation_leaves_short_text_alone() {
    assert_eq!(truncate("", 10), "");
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("exactlyten", 10), "exactlyten");
}

/// A limit of zero or one cannot fit content plus a marker, and must not panic.
///
/// `limit - 1` underflows at zero, which is why `saturating_sub` is there; this is
/// the test that says so.
#[test]
fn truncation_survives_a_degenerate_limit() {
    assert_eq!(truncate("abc", 1), "…");
    assert_eq!(truncate("abc", 0), "…");
}

/// Text with nothing to fix comes back unchanged, and the fast path is not a
/// different code path with different behaviour.
#[test]
fn clean_text_passes_through_untouched() {
    assert_eq!(inline("plain symbol"), "plain symbol");
    assert_eq!(block("line one\nline two"), "line one\nline two");
    assert_eq!(expand_tabs("no tabs here"), "no tabs here");
}
