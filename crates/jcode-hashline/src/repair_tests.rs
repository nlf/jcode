//! Boundary-repair tests, ported from oh-my-pi's `boundary-repair.test.ts`.
//!
//! Their cases are the specification. Several carry the incident that produced
//! them, and those comments are kept: a test named after the file it corrupted
//! is far easier to reason about than one named after a rule.
//!
//! Cases depending on their tree-sitter probe are not ported, because the
//! closer-spare rules that need it are not built. See the module docs.

use super::*;
use crate::apply::apply_ops;
use crate::parser::parse_ops;

/// Apply a patch the way the patcher would, with repair in between.
fn apply(text: &str, patch: &str) -> (String, Vec<String>) {
    let mut ops = parse_ops(patch).expect("fixture patch parses").ops;
    let lines: Vec<&str> = text.split('\n').collect();
    let outcome = repair_boundaries(&mut ops, &lines);
    let applied = apply_ops(text, &ops).expect("fixture applies");
    (applied.text, outcome.warnings)
}

fn joined(rows: &[&str]) -> String {
    rows.join("\n")
}

#[test]
fn restores_a_uniformly_omitted_base_indent_from_unchanged_structural_rows() {
    let file = joined(&[
        "    if (value > 90) {",
        "      result = error;",
        "    } else if (value > 70) {",
        "      result = plain;",
        "    } else {",
        "      result = warning;",
        "    }",
    ]);
    let patch = joined(&[
        "PUT 2=6:",
        "+  result = error;",
        "+} else if (value > 70) {",
        "+  result = warning;",
        "+} else {",
        "+  result = plain;",
    ]);

    let (text, warnings) = apply(&file, &patch);
    assert_eq!(
        text,
        joined(&[
            "    if (value > 90) {",
            "      result = error;",
            "    } else if (value > 70) {",
            "      result = warning;",
            "    } else {",
            "      result = plain;",
            "    }",
        ])
    );
    assert!(
        warnings.iter().any(|w| w.contains("Auto-indented")),
        "{warnings:?}"
    );
}

#[test]
fn preserves_an_intentional_indentation_only_replacement() {
    // The payload is a deliberate dedent, so re-indenting it would undo the
    // edit. Nothing above the range opens a block, so the rule stands down.
    let file = joined(&["    first();", "    second();"]);
    let (text, warnings) = apply(&file, "PUT 1=2:\n+first();\n+second();");
    assert_eq!(text, "first();\nsecond();");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_indent_shift_needs_a_majority_of_rows_to_vote_for_it() {
    // Only one payload row is an unchanged copy of its source row, so the
    // single row claiming a four-space shift is not evidence that the model
    // dropped indentation. It is at least as likely to have dedented on
    // purpose, and one row cannot settle it.
    let file = joined(&["if (x) {", "    keep();", "    aaa();", "    bbb();", "}"]);
    let (text, warnings) = apply(&file, "PUT 2=4:\n+keep();\n+ccc();\n+ddd();");

    assert_eq!(
        text,
        joined(&["if (x) {", "keep();", "ccc();", "ddd();", "}"])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_indent_shift_needs_the_payload_to_actually_escape_the_open_block() {
    // Two thirds of the rows agree on a four-space shift, so the vote passes,
    // but the payload is already indented inside the brace above it. It is a
    // deliberate partial dedent, not a body that fell out of its block, and
    // re-indenting would overrule the model on something it plainly meant.
    let file = joined(&[
        "if (x) {",
        "        keep();",
        "        aaa();",
        "        bbb();",
        "}",
    ]);
    let (text, warnings) = apply(&file, "PUT 2=4:\n+    keep();\n+    aaa();\n+    zzz();");

    assert_eq!(
        text,
        joined(&["if (x) {", "    keep();", "    aaa();", "    zzz();", "}"])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn drops_a_duplicated_multi_line_closing_block_the_root_tsx_incident() {
    // The canonical incident: the payload restates the fragment close and the
    // paren close that still live below the range, doubling `</>` and `);`.
    let file = joined(&[
        "import type React from \"react\";",
        "import { Composition } from \"remotion\";",
        "import { Sizzle, type SizzleProps } from \"./compositions/Sizzle\";",
        "import { FPS, totalDurationInFrames } from \"./lib/scenes\";",
        "",
        "export const RemotionRoot: React.FC = () => {",
        "\tconst durationInFrames = totalDurationInFrames();",
        "\treturn (",
        "\t\t<>",
        "\t\t\t<Composition",
        "\t\t\t\tid=\"Sizzle\"",
        "\t\t\t\tcomponent={Sizzle}",
        "\t\t\t\tdurationInFrames={durationInFrames}",
        "\t\t\t\twidth={1920}",
        "\t\t\t\tdefaultProps={{ layout: \"landscape\" }}",
        "\t\t\t/>",
        "\t\t</>",
        "\t);",
        "};",
    ]);
    let patch = joined(&[
        "PUT 7=16:",
        "+\treturn (",
        "+\t\t<>",
        "+\t\t\t<Composition",
        "+\t\t\t\tid=\"Sizzle\"",
        "+\t\t\t\tcomponent={Sizzle}",
        "+\t\t\t\tdurationInFrames={durationInFrames}",
        "+\t\t\t\twidth={1920}",
        "+\t\t\t\tdefaultProps={{ layout: \"landscape\" } satisfies SizzleProps}",
        "+\t\t\t/>",
        "+\t\t</>",
        "+\t);",
    ]);

    let (text, warnings) = apply(&file, &patch);
    assert_eq!(
        text.lines().filter(|l| l.trim() == "</>").count(),
        1,
        "the fragment close must not double: {text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.trim() == ");").count(),
        1,
        "the paren close must not double: {text}"
    );
    assert!(text.ends_with("\t\t</>\n\t);\n};"), "{text}");
    assert!(
        warnings.iter().any(|w| w.contains("delimiter-balance")),
        "{warnings:?}"
    );
}

#[test]
fn drops_a_single_duplicated_structural_closer() {
    // The range ends one line short and the payload restates the `});` that
    // survives just below it.
    let file = joined(&["it('a', () => {", "\tsetup();", "\trun();", "});", "after();"]);
    let (text, warnings) = apply(&file, "PUT 2=3:\n+\tsetup2();\n+\trun2();\n+});");

    assert_eq!(
        text,
        joined(&["it('a', () => {", "\tsetup2();", "\trun2();", "});", "after();"])
    );
    assert!(
        warnings.iter().any(|w| w.contains("delimiter-balance")),
        "{warnings:?}"
    );
}

#[test]
fn drops_a_single_duplicated_structural_opener() {
    // The mirror case, from the tui.ts `planRender(` incident: the range starts
    // one line late and the payload restates the signature opener above it.
    let file = joined(&[
        "class Foo {",
        "\t/** doc */",
        "\tplanRender(",
        "\t\ta: string[],",
        "\t\tb: boolean,",
        "\t): Intent {",
        "\t\treturn x;",
        "\t}",
        "}",
    ]);
    let patch = joined(&[
        "PUT 4=6:",
        "+\tplanRender(",
        "+\t\ta: string[],",
        "+\t\tb: boolean,",
        "+\t\tc: number,",
        "+\t): Intent {",
    ]);

    let (text, warnings) = apply(&file, &patch);
    assert_eq!(
        text,
        joined(&[
            "class Foo {",
            "\t/** doc */",
            "\tplanRender(",
            "\t\ta: string[],",
            "\t\tb: boolean,",
            "\t\tc: number,",
            "\t): Intent {",
            "\t\treturn x;",
            "\t}",
            "}",
        ])
    );
    assert_eq!(
        text.lines().filter(|l| *l == "\tplanRender(").count(),
        1,
        "the opener must not double"
    );
    assert!(
        warnings.iter().any(|w| w.contains("delimiter-balance")),
        "{warnings:?}"
    );
}

#[test]
fn preserves_a_duplicated_opener_that_does_not_account_for_the_imbalance() {
    // The payload duplicates `if (a) {` but is net two braces open. Dropping
    // the one opener cannot zero the delta, so the duplication is evidently
    // deliberate and nothing fires. This is the exact-equality guard: a
    // `covers` check here would drop a line that only partly explains the
    // imbalance, silently.
    let file = joined(&["if (a) {", "\tfoo();", "}", "bar();"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+if (a) {\n+\tif (b) {\n+\t\tfoo();");

    assert_eq!(
        text,
        joined(&["if (a) {", "if (a) {", "\tif (b) {", "\t\tfoo();", "}", "bar();"])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn drops_duplicated_leading_and_trailing_lines_around_a_range_replacement() {
    let file = joined(&[
        "func _cmd_travel_homeworld():",
        "\tvar destination = get_homeworld()",
        "\ttravel_to(destination)",
        "\tprint_status()",
    ]);
    let patch = joined(&[
        "PUT 2=3:",
        "+func _cmd_travel_homeworld():",
        "+\tvar destination = find_homeworld()",
        "+\ttravel_to(destination)",
        "+\tprint_status()",
    ]);

    let (text, warnings) = apply(&file, &patch);
    assert_eq!(
        text,
        joined(&[
            "func _cmd_travel_homeworld():",
            "\tvar destination = find_homeworld()",
            "\ttravel_to(destination)",
            "\tprint_status()",
        ])
    );
    assert_eq!(
        text.lines()
            .filter(|l| *l == "func _cmd_travel_homeworld():")
            .count(),
        1
    );
    assert!(
        warnings.iter().any(|w| w.contains("boundary echo")),
        "{warnings:?}"
    );
}

#[test]
fn preserves_a_payload_whose_echoes_would_cover_every_line() {
    // Both edges match, but between them there is nothing left. A payload that
    // is entirely echo is not a boundary mistake, and repairing it would turn a
    // replacement into a deletion.
    let file = joined(&["A", "B", "old", "C", "D"]);
    let (text, warnings) = apply(&file, "PUT 3=3:\n+A\n+B\n+C\n+D");

    assert_eq!(text, joined(&["A", "B", "A", "B", "C", "D", "C", "D"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn preserves_a_payload_made_only_of_lines_matching_both_neighbours() {
    let file = joined(&["a", "old", "c"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+a\n+c");

    assert_eq!(text, joined(&["a", "a", "c", "c"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn preserves_balance_shifting_echoes_that_do_not_explain_the_delta() {
    // The payload opens with the same bare `}` sitting above the range and
    // closes with the one below it, but it is internally balanced while those
    // edges sum to minus two braces. The echo therefore explains nothing, and
    // stripping it would corrupt the brace structure.
    let file = joined(&["}", "old();", "}"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+}\n+if (a) {\n+if (b) {\n+x();\n+}");

    assert_eq!(
        text,
        joined(&["}", "}", "if (a) {", "if (b) {", "x();", "}", "}"])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn still_drops_a_balance_neutral_wrapper_echo() {
    // The common case the rule exists for: the model restated the function
    // signature and its closing brace to show where the body goes. The edges
    // are balance-neutral, so they are dropped without needing to explain any
    // delta.
    let file = joined(&["function f() {", "old();", "}"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+function f() {\n+fresh();\n+}");

    assert_eq!(text, joined(&["function f() {", "fresh();", "}"]));
    assert!(
        warnings.iter().any(|w| w.contains("boundary echo")),
        "{warnings:?}"
    );
}

#[test]
fn a_balance_preserving_replacement_is_left_alone() {
    // The payload's last line coincidentally equals the line below the range,
    // but the payload is balanced, so there is no imbalance to explain and the
    // coincidence is not evidence of anything.
    let file = joined(&["start();", "old();", "end();"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+fresh();\n+end();");

    assert_eq!(text, joined(&["start();", "fresh();", "end();", "end();"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

// Delimiter counting. These pin the parts of the scanner that decide whether a
// bracket counts, which is what every balance-based rule rests on.

#[test]
fn brackets_inside_strings_and_comments_do_not_count() {
    assert!(compute_balance(&["let s = \"{\";"]).is_zero());
    assert!(compute_balance(&["let s = '{';"]).is_zero());
    assert!(compute_balance(&["// {"]).is_zero());
    assert!(compute_balance(&["/* { */"]).is_zero());
    assert!(!compute_balance(&["if (x) {"]).is_zero());
}

#[test]
fn a_block_comment_and_a_backtick_string_span_lines_but_a_quote_does_not() {
    // A block comment and a template literal genuinely continue across a
    // newline, so their state carries. An unterminated single quote is far more
    // likely to be an apostrophe in prose, so it is reset at the line end
    // rather than swallowing the rest of the file.
    assert!(compute_balance(&["/*", "{", "*/"]).is_zero());
    assert!(compute_balance(&["let s = `", "{", "`;"]).is_zero());
    assert!(!compute_balance(&["// it's fine", "if (x) {"]).is_zero());
}

#[test]
fn an_escaped_quote_does_not_end_a_string() {
    assert!(compute_balance(&["let s = \"\\\"{\";"]).is_zero());
}

#[test]
fn a_miscount_can_only_suppress_a_repair_never_cause_one() {
    // The scanner does not understand regex literals, so it reads the brace in
    // `/{/` as real. That is deliberate naivety, and it is safe because every
    // repair needs an exact balance match: a wrong count fails the check and
    // the repair stands down. This test pins the naivety so that anyone
    // "fixing" it knows it was a decision.
    assert!(!compute_balance(&["const re = /{/;"]).is_zero());

    // And the consequence at the level that matters: a payload whose count is
    // thrown off by a regex is left exactly as authored.
    let file = joined(&["start();", "old();", "end();"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+const re = /{/;\n+end();");
    assert_eq!(
        text,
        joined(&["start();", "const re = /{/;", "end();", "end();"])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_blank_line_above_the_range_is_not_an_echo_worth_dropping() {
    // Whitespace matching whitespace is a coincidence, not a restatement, so at
    // least one echoed line has to carry content. Each side is checked
    // separately: here the leading echo is a blank line and the trailing echo
    // is real, so only the leading side's content rule can refuse the repair.
    let file = joined(&["", "old();", "tail();"]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+\n+fresh();\n+tail();");

    assert_eq!(text, joined(&["", "", "fresh();", "tail();", "tail();"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_blank_line_below_the_range_is_not_an_echo_worth_dropping() {
    // The mirror, pinning the trailing side's own content rule.
    let file = joined(&["head();", "old();", ""]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+head();\n+fresh();\n+");

    assert_eq!(text, joined(&["head();", "head();", "fresh();", "", ""]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_echo_of_blank_lines_alone_is_not_evidence() {
    // Both sides blank at once. Without the content rule this payload loses its
    // deliberate blank lines, because they match the blanks bracketing the
    // range and the two-sided echo fires before any balance-based check could
    // stand it down.
    let file = joined(&["", "old();", ""]);
    let (text, warnings) = apply(&file, "PUT 2=2:\n+\n+fresh();\n+");

    assert_eq!(text, joined(&["", "", "fresh();", "", ""]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn repairs_are_confined_to_the_replacement_that_earned_them() {
    // Two hunks in one patch, only one of which restates a neighbour. The
    // other must be applied exactly as authored.
    let file = joined(&[
        "function f() {",
        "old();",
        "}",
        "const x = 1;",
        "const y = 2;",
    ]);
    let patch = joined(&[
        "PUT 2=2:",
        "+function f() {",
        "+fresh();",
        "+}",
        "",
        "PUT 5=5:",
        "+const y = 3;",
    ]);

    let (text, warnings) = apply(&file, &patch);
    assert_eq!(
        text,
        joined(&["function f() {", "fresh();", "}", "const x = 1;", "const y = 3;"])
    );
    assert_eq!(warnings.len(), 1, "only one hunk was repaired: {warnings:?}");
}

#[test]
fn an_insertion_is_never_boundary_repaired() {
    // `PUT >N:` has no range for a payload to overflow, so a line matching its
    // neighbour is an ordinary duplicate the model asked for.
    let file = joined(&["a", "b", "c"]);
    let (text, warnings) = apply(&file, "PUT >2:\n+b");

    assert_eq!(text, joined(&["a", "b", "b", "c"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}
