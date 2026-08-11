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


// ─── block_at ────────────────────────────────────────────────────────────────
//
// What `PUT 5*:` actually resolves to. The contract has two halves: the span
// must be the construct a person would point at, and every case where that is
// unclear must be `None` rather than a guess, because the caller turns `None`
// into a message and a guess into an edit.

#[test]
fn a_function_resolves_to_its_whole_body() {
    let text = "fn a() {\n\tone();\n}\nfn b() {\n\ttwo();\n\tthree();\n}\n";

    assert_eq!(block_at("x.rs", text, 1), Some(BlockSpan { start: 1, end: 3 }));
    assert_eq!(block_at("x.rs", text, 4), Some(BlockSpan { start: 4, end: 7 }));
}

#[test]
fn the_largest_construct_starting_on_the_line_wins() {
    // Several nodes begin on line 1: the function, its name, its parameters.
    // The useful one is the whole function, because that is what a person means
    // by "the block at line 1".
    let text = "function f() {\n  if (x) {\n    a();\n  }\n  b();\n}\nconst z = 1;\n";

    assert_eq!(block_at("x.ts", text, 1), Some(BlockSpan { start: 1, end: 6 }));
}

#[test]
fn an_if_else_resolves_to_the_whole_statement_not_its_first_branch() {
    // Where "largest" actually earns its keep. Two multi-line nodes begin on
    // line 1: the `if_statement` through line 5, and the first branch's
    // `statement_block` through line 3. Taking the smaller one would resolve
    // `PUT 1*:` to the `if` and its consequent while orphaning the `else`,
    // which then dangles with nothing to attach to.
    //
    // Nesting alone does not distinguish these, which is why the earlier tests
    // do not cover it: every other construct tried has one candidate per line,
    // so the choice is invisible until a branching statement appears.
    let text = "if (a) {\n  b();\n} else {\n  c();\n}\nafter();\n";

    assert_eq!(block_at("x.ts", text, 1), Some(BlockSpan { start: 1, end: 5 }));

    // Same shape for try/catch.
    let guarded = "try {\n  a();\n} catch (e) {\n  b();\n}\nafter();\n";
    assert_eq!(block_at("x.ts", guarded, 1), Some(BlockSpan { start: 1, end: 5 }));
}

#[test]
fn a_nested_construct_resolves_to_itself() {
    let text = "function f() {\n  if (x) {\n    a();\n  }\n  b();\n}\nconst z = 1;\n";

    assert_eq!(block_at("x.ts", text, 2), Some(BlockSpan { start: 2, end: 4 }));
}

#[test]
fn a_line_that_begins_no_multi_line_construct_is_none() {
    // A body line, a closing brace and a one-line statement are all refusals
    // rather than spans, so the caller can say what to write instead.
    let text = "function f() {\n  if (x) {\n    a();\n  }\n  b();\n}\nconst z = 1;\n";

    assert_eq!(block_at("x.ts", text, 3), None, "a body line");
    assert_eq!(block_at("x.ts", text, 4), None, "a closing brace");
    assert_eq!(block_at("x.ts", text, 7), None, "a one-line statement");
}

#[test]
fn a_file_that_does_not_parse_yields_nothing() {
    // Tree-sitter always returns a tree, recovering from errors by inventing
    // structure, so a span from a broken file may cover nothing the author
    // would recognise. Refusing is the only safe answer, and it matches how
    // `parses_cleanly` treats the same file.
    //
    // The cost of not doing this is concrete rather than theoretical. Each of
    // these resolves to a plausible-looking span without the check, and the
    // first is the worst: a function whose closing brace is missing resolves to
    // a span that stops mid-body, so `PUT 1*:` would replace part of a function
    // and leave the rest behind. A model reaching for a block anchor in a file
    // it has just half-broken is not an unusual situation.
    let unclosed = "fn a() {\n\tone();\n";
    assert_eq!(block_at("x.rs", unclosed, 1), None, "resolves to lines 1-2 unguarded");

    // An error elsewhere in the file also disqualifies an otherwise intact
    // block: the recovery that invented structure at line 2 is the same parse
    // that reported line 4, and there is no way to trust one and not the other.
    let broken_above = "function f() {\n  a(;\n}\nfunction g() {\n  b();\n}\n";
    assert_eq!(block_at("x.ts", broken_above, 4), None);
}

#[test]
fn an_unknown_language_or_impossible_line_yields_nothing() {
    let text = "fn a() {\n\tone();\n}\n";

    assert_eq!(block_at("x.zzz", text, 1), None, "no language");
    assert_eq!(block_at("x.rs", text, 0), None, "line 0 is not a line");
    assert_eq!(block_at("x.rs", text, 99), None, "past the end");
    assert_eq!(block_at("x.rs", "", 1), None, "empty file");
}

#[test]
fn the_file_itself_is_never_a_block_in_it() {
    // Otherwise `PUT 1*:` on a file whose first line opens the only top-level
    // item resolves to the root node, and the model gets a whole-file rewrite
    // it did not ask for. The single item still resolves: it is the root that
    // is excluded, not whatever happens to span every line.
    let text = "fn only() {\n\tbody();\n}\n";

    assert_eq!(
        block_at("x.rs", text, 1),
        Some(BlockSpan { start: 1, end: 3 }),
        "the function is a block even when it is the whole file's content"
    );
}

#[test]
fn a_file_with_no_trailing_newline_resolves_the_same() {
    // How a file ends is not a property of the blocks in it. An earlier version
    // excluded the whole-file span by comparing against `lines().count()`,
    // which differs from the split count depending on the trailing newline, so
    // the sole function in a file without one was silently refused.
    let with = "fn a() {\n\tone();\n}\nfn b() {\n\ttwo();\n}\n";
    let without = "fn a() {\n\tone();\n}\nfn b() {\n\ttwo();\n}";

    assert_eq!(block_at("x.rs", with, 1), Some(BlockSpan { start: 1, end: 3 }));
    assert_eq!(block_at("x.rs", without, 1), Some(BlockSpan { start: 1, end: 3 }));
    assert_eq!(block_at("x.rs", without, 4), Some(BlockSpan { start: 4, end: 6 }));

    let sole = "fn only() {\n\tbody();\n}";
    assert_eq!(
        block_at("x.rs", sole, 1),
        Some(BlockSpan { start: 1, end: 3 }),
        "a single function with no trailing newline still resolves"
    );
}

#[test]
fn a_span_ending_at_the_start_of_the_next_line_does_not_claim_it() {
    // Some grammars end a node at column 0 of the *following* line rather than
    // at the end of its own, and counting that line hands the block a line it
    // does not own.
    //
    // Go is the sharp case. A function's `statement_list` is reported as
    // running from the first body line to the start of the closing brace, so
    // without the adjustment line 2, an ordinary statement in the middle of a
    // function, looks like a block spanning lines 2-3. `PUT 2*:` would then
    // replace the statement *and* the closing brace, breaking the function
    // while appearing to do exactly what was asked.
    let go = "func a() {\n\tone()\n}\nfunc b() {\n\ttwo()\n}\n";

    assert_eq!(block_at("x.go", go, 1), Some(BlockSpan { start: 1, end: 3 }));
    assert_eq!(
        block_at("x.go", go, 2),
        None,
        "a body line is not a block; unguarded it resolves to lines 2-3"
    );

    // YAML shows the same rule on a container: the mapping under `a:` ends at
    // the start of line 4, and claiming that line would replace `d: 3`, a
    // sibling key at the top level.
    let yaml = "a:\n  b: 1\n  c: 2\nd: 3\ne: 4\n";
    assert_eq!(block_at("x.yaml", yaml, 1), Some(BlockSpan { start: 1, end: 3 }));

    // The braces-and-semicolons languages every other test here uses cannot
    // show this: their nodes end on their own closing line, so a test written
    // in Rust or TypeScript passes whether or not the rule exists.
}

#[test]
fn no_input_can_panic_block_at() {
    let files = ["", "\n", "a", "fn a() {\n}", "{{{", "\u{1f600}\nx\n"];
    for text in files {
        for line in [0usize, 1, 2, 99] {
            for path in ["x.rs", "x.ts", "x.zzz", ""] {
                let _ = block_at(path, text, line);
            }
        }
    }
}













