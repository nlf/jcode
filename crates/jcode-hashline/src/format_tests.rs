//! Behaviour ported from oh-my-pi's `packages/hashline`, whose tests are the
//! specification for this crate.
//!
//! The interop fixtures are the point. omp's own tests mostly compare
//! `computeFileHash` against itself, which would pass for any hash function and
//! prove nothing about compatibility. These pin the tag to values observed from
//! omp's source and its documented collision, so a divergence in the hash, the
//! seed, the bit width, or the normalization fails here rather than in
//! production against a patch we cannot apply.

use super::*;

/// omp `test/snapshots.test.ts:119-124` documents these two texts as colliding
/// on `1D84`, as a regression for their issue #4075. It is the only literal tag
/// value in their suite, which makes it the one true interop fixture available:
/// reproducing it pins the algorithm (XXH32), the seed (0), the width (low 16
/// bits), the case (upper), and the normalization all at once.
#[test]
fn the_documented_collision_from_omp_issue_4075_reproduces_exactly() {
    let a = "line one 263\nline two 4471\n";
    let b = "line one 410\nline two 6970\n";

    assert_eq!(compute_file_hash(a), "1D84", "omp records this text as 1D84");
    assert_eq!(compute_file_hash(b), "1D84", "omp records this text as 1D84");
}

/// A tag is four uppercase hex digits, zero-padded. The header regex on the
/// other side is `[0-9A-F]{4}`, so lowercase or an unpadded short value is not
/// merely cosmetic: it fails to parse.
#[test]
fn a_tag_is_always_four_uppercase_hex_digits() {
    for text in [
        "",
        "a",
        "hello world\n",
        "line one 263\nline two 4471\n",
        &"x".repeat(10_000),
    ] {
        let tag = compute_file_hash(text);
        assert_eq!(tag.len(), FILE_HASH_LENGTH, "wrong width for {text:?}: {tag}");
        assert!(
            tag.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)),
            "not uppercase hex for {text:?}: {tag}"
        );
    }
}

/// The reason normalization exists: a file read back through a renderer that
/// trimmed trailing whitespace, or over a transport that rewrote line endings,
/// must still hash to the tag the model was handed. Otherwise every CRLF file
/// rejects every edit.
#[test]
fn trailing_whitespace_and_line_endings_do_not_change_the_tag() {
    let expected = compute_file_hash("a\nb\n");

    for (label, text) in [
        ("crlf", "a\r\nb\r\n"),
        ("trailing spaces", "a   \nb\t\n"),
        ("mixed tabs, spaces and crlf", "a \r\nb\t \r\n"),
    ] {
        assert_eq!(
            compute_file_hash(text),
            expected,
            "{label} must not change the tag"
        );
    }
}

/// The other half of that rule. Leading whitespace is indentation, which is
/// content: collapsing it would let a reindented file keep a stale tag and
/// accept an edit anchored against the old shape.
#[test]
fn leading_whitespace_is_content_and_does_change_the_tag() {
    assert_ne!(
        compute_file_hash("a\n"),
        compute_file_hash("  a\n"),
        "indentation must be part of the hashed content"
    );
}

/// Interior whitespace is content too; only the trailing run is normalized.
#[test]
fn interior_whitespace_is_content_and_does_change_the_tag() {
    assert_ne!(compute_file_hash("a b\n"), compute_file_hash("a  b\n"));
}

/// Any read of byte-identical content mints the same tag. This is what lets
/// repeated reads of one file state fuse onto a single anchor, so a partial
/// read followed by another partial read widens one snapshot rather than
/// creating two.
#[test]
fn identical_content_always_mints_the_same_tag() {
    let text = "fn main() {\n    println!(\"hi\");\n}\n";
    assert_eq!(compute_file_hash(text), compute_file_hash(text));

    // Built at runtime rather than a literal, so this compares independently
    // constructed buffers. The previous form passed `&text.to_string()`, which
    // derefs straight back to the same `&str` and asserted `f(x) == f(x)`.
    let rebuilt: String = text.chars().collect();
    assert_eq!(compute_file_hash(text), compute_file_hash(&rebuilt));
}

/// Different content generally differs. Sixteen bits collide by birthday at
/// roughly 256 distinct texts, so this is a sanity check on the common case,
/// not a uniqueness guarantee: the collision test above is the honest statement
/// of the contract.
#[test]
fn different_content_generally_mints_a_different_tag() {
    let tags: std::collections::HashSet<String> = (0..64)
        .map(|i| compute_file_hash(&format!("line {i}\n")))
        .collect();
    assert!(
        tags.len() >= 60,
        "64 distinct texts collapsed to {} tags, which suggests a broken hash",
        tags.len()
    );
}

/// An empty file still has a tag. It has to: a model may edit a file into
/// existence and then anchor against it.
#[test]
fn an_empty_file_has_a_tag() {
    let tag = compute_file_hash("");
    assert_eq!(tag.len(), FILE_HASH_LENGTH);
}

/// A file with no trailing newline is a different state from one with it, and
/// must not fuse: the trailing newline is a real difference an edit can make.
#[test]
fn a_missing_trailing_newline_is_a_different_state() {
    assert_ne!(compute_file_hash("a\nb"), compute_file_hash("a\nb\n"));
}

/// Non-ASCII content hashes over its bytes without panicking. Rust indexes
/// strings by byte and TypeScript by UTF-16 code unit, which is exactly the
/// kind of difference that silently diverges a port.
#[test]
fn non_ascii_content_hashes_without_panicking() {
    for text in ["héllo\n", "日本語\n", "emoji 🎉\n", "combining é\n"] {
        let tag = compute_file_hash(text);
        assert_eq!(tag.len(), FILE_HASH_LENGTH, "failed on {text:?}");
    }
}

/// Trailing-whitespace normalization applies to the final line as well as to
/// newline-terminated ones. omp's regex is `[ \t\r]+(?=\n|$)`, where the `$`
/// alternative is the final-line case.
#[test]
fn the_final_line_is_normalized_even_without_a_trailing_newline() {
    assert_eq!(compute_file_hash("a\nb   "), compute_file_hash("a\nb"));
}

#[test]
fn a_section_header_renders_as_path_hash_tag() {
    assert_eq!(format_hashline_header("src/foo.rs", "1A2B"), "[src/foo.rs#1A2B]");
}

/// Numbered output is `N:TEXT`, the shape the model reads line numbers from and
/// then cites back in an anchor.
#[test]
fn numbered_lines_render_as_number_colon_text() {
    assert_eq!(format_numbered_line(1, "fn main() {"), "1:fn main() {");
    assert_eq!(format_numbered_line(42, ""), "42:");
}

#[test]
fn numbered_lines_start_at_the_requested_offset() {
    assert_eq!(format_numbered_lines("a\nb\nc", 10), "10:a\n11:b\n12:c");
}

/// Numbering must not collapse blank lines, or every line after one is
/// misnumbered and every subsequent anchor points at the wrong place.
#[test]
fn numbering_preserves_blank_lines() {
    assert_eq!(format_numbered_lines("a\n\nc", 1), "1:a\n2:\n3:c");
}
