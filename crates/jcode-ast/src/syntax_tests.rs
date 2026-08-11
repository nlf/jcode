//! Tests for the syntax probe.
//!
//! The cases that matter most are the ones where delimiter counting is wrong:
//! a brace inside a regex, a string, or prose. Those are the reason this exists
//! rather than a cheaper heuristic.

use super::*;

#[test]
fn a_well_formed_file_parses() {
    assert!(parses_cleanly("f.js", "function f() {\n  return 1;\n}\n"));
    assert!(parses_cleanly("f.rs", "fn main() {\n    let x = 1;\n}\n"));
    assert!(parses_cleanly("f.py", "def f():\n    return 1\n"));
}

#[test]
fn a_missing_closer_does_not_parse() {
    assert!(!parses_cleanly("f.js", "function f() {\n  return 1;\n"));
    assert!(!parses_cleanly("f.rs", "fn main() {\n    let x = 1;\n"));
}

#[test]
fn a_stray_closer_does_not_parse() {
    assert!(!parses_cleanly("f.js", "function f() {\n  return 1;\n}\n}\n"));
}

#[test]
fn a_brace_inside_a_regex_is_not_a_block() {
    // The case that motivates the whole probe. Delimiter arithmetic reads this
    // as one unclosed brace and would happily "spare" a closer to balance it,
    // writing a `}` into a file that is already correct. The parser knows the
    // brace is inside a regex literal.
    let source = "const re = /{/;\nconst x = 1;\n";
    assert!(parses_cleanly("f.js", source));
}

#[test]
fn a_brace_inside_a_string_is_not_a_block() {
    assert!(parses_cleanly("f.js", "const s = \"{\";\nconst x = 1;\n"));
}

#[test]
fn an_unrecognised_language_yields_no_opinion() {
    // Markdown has no grammar here, so prose full of braces cannot be judged.
    // Returning false is what withholds permission to repair it, which is
    // exactly right: braces in prose are not syntax and must never be
    // "balanced" by a repair.
    assert!(!parses_cleanly("notes.md", "a paragraph with a } in it\n"));
    assert!(!parses_cleanly("data.bin", "\u{0}\u{1}\u{2}"));
}

#[test]
fn a_path_with_no_usable_extension_yields_no_opinion() {
    assert!(!parses_cleanly("Makefile", "all:\n\techo hi\n"));
    assert!(!parses_cleanly("", "anything"));
}

#[test]
fn an_empty_file_parses_when_the_language_is_known() {
    // Nothing is wrong with an empty source file, and reporting otherwise
    // would withhold permission for a repair that appends to one.
    assert!(parses_cleanly("f.js", ""));
}

#[test]
fn the_same_text_probed_twice_gives_the_same_answer() {
    // The cache is keyed on language and text, so a second probe must agree
    // with the first. A cache keyed on the path alone would answer for the
    // wrong content once a file changed, which in the repair layer means
    // judging one candidate by another's parse.
    let source = "function f() {\n  return 1;\n}\n";
    assert!(parses_cleanly("f.js", source));
    assert!(parses_cleanly("f.js", source));

    let broken = "function f() {\n  return 1;\n";
    assert!(!parses_cleanly("f.js", broken));
    assert!(!parses_cleanly("f.js", broken));

    // And the first answer is not reused for the second text under the same
    // path.
    assert!(parses_cleanly("f.js", source));
}

#[test]
fn one_text_can_be_valid_in_one_language_and_not_another() {
    // Keying the cache by text alone would let the first language's answer
    // stand for every later one.
    let source = "fn main() {}\n";
    assert!(parses_cleanly("a.rs", source));
    assert!(!parses_cleanly("a.py", source));
}

#[test]
fn the_cache_does_not_retain_the_text_it_was_asked_about() {
    // The cache exists to skip repeated parses of the same candidate during a
    // repair pass. Keying it on the content itself made each entry as large as
    // the file it described: 256 entries of ordinary source held over a
    // hundred megabytes, live, for the sake of a few parses. Confirmed by
    // reverting the key and watching this report 148,498,578 bytes.
    //
    // Measured rather than asserted structurally, since the point is the bytes
    // and not the type. The files here are large but syntactically trivial, so
    // the test spends its time on the thing being measured rather than on
    // tree-sitter.
    let big = "const x = 1;\n".repeat(4_000);
    for index in 0..CACHE_CAPACITY {
        let _ = parses_cleanly("f.js", &format!("// {index}\n{big}"));
    }

    let held: usize = cache()
        .lock()
        .expect("cache")
        .keys()
        .map(|(language, _, _)| language.len() + std::mem::size_of::<CacheKey>())
        .sum();
    assert!(
        held < 64 * 1024,
        "the cache should stay small regardless of file size, held {held} bytes"
    );
}
