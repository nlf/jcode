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

/// Apply with a syntax veto, the way a caller holding a parser would.
///
/// `parses` stands in for `jcode_ast::parses_cleanly`. The fake used by most
/// tests below counts brackets, which is enough to model "this file is
/// balanced" without pulling a tree-sitter dependency into this crate. Where a
/// test needs the parser to be *wrong* about something (a brace in prose), it
/// passes its own closure.
fn apply_with_veto(
    text: &str,
    patch: &str,
    parses: impl Fn(&str) -> bool,
) -> (String, Vec<String>) {
    let mut ops = parse_ops(patch).expect("fixture patch parses").ops;
    let lines: Vec<&str> = text.split('\n').collect();
    let outcome = repair_with_syntax_veto(&mut ops, &lines, parses, |candidate| {
        apply_ops(text, candidate).ok().map(|result| result.text)
    });
    let applied = apply_ops(text, &ops).expect("fixture applies");
    (applied.text, outcome.warnings)
}

/// A stand-in parser: a file is well-formed when its brackets balance.
fn brackets_balance(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    compute_balance(&lines) == Balance::default()
}

/// Run the spare pass with no veto in the way.
///
/// A few guards inside the spare rule are only observable this way. Reached
/// through `repair_with_syntax_veto`, a candidate the guard would refuse is
/// usually one the parser refuses too, so a test written that way passes
/// whether or not the guard exists.
fn apply_spares_directly(text: &str, patch: &str) -> (String, Vec<String>) {
    let mut ops = parse_ops(patch).expect("fixture patch parses").ops;
    let lines: Vec<&str> = text.split('\n').collect();
    let outcome = repair_boundaries_with(&mut ops, &lines, true);
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

// Closer spares. These are the rules that claim a bracket is syntax rather
// than text, so each is gated on a parse check: the authored edit must break
// the file and the repaired one must fix it. The tests come in pairs, because
// what a spare must *not* do matters more than what it does.

#[test]
fn spares_the_closing_line_the_payload_forgot_to_restate() {
    // The model added a method and dropped the `};` that ends the object. The
    // authored edit leaves the literal unterminated, and keeping the deleted
    // line restores it.
    let file = joined(&[
        "const handlers = {",
        "\ta() {",
        "\t\treturn 1;",
        "\t},",
        "};",
    ]);
    let (text, warnings) = apply_with_veto(
        &file,
        "PUT 5=5:\n+\tb() {\n+\t\treturn 2;\n+\t},",
        brackets_balance,
    );

    assert_eq!(
        text,
        joined(&[
            "const handlers = {",
            "\ta() {",
            "\t\treturn 1;",
            "\t},",
            "\tb() {",
            "\t\treturn 2;",
            "\t},",
            "};",
        ])
    );
    assert!(
        warnings.iter().any(|w| w.contains("dropped closing line")),
        "{warnings:?}"
    );
}

#[test]
fn does_not_spare_a_closing_line_the_payload_already_restates() {
    // The range is internally unbalanced and the payload ends with the same
    // closer. Keeping the deleted one would put a second `}` outside the
    // payload, which is the opposite of the mistake being repaired.
    let file = joined(&["class Foo {", "\tok();", "\t}", "}"]);
    let (text, warnings) =
        apply_with_veto(&file, "PUT 1=4:\n+class Foo {\n+\tok();\n+}", brackets_balance);

    assert_eq!(text, joined(&["class Foo {", "\tok();", "}"]));
    assert_eq!(text.lines().filter(|l| *l == "}").count(), 1);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_closer_with_nothing_to_close_is_not_kept() {
    // The range ends in a `}` but nothing above it is open, so the brace is
    // stray text rather than a terminator. Keeping it would preserve a bracket
    // that closes nothing, which is not the mistake this rule repairs.
    //
    // Reached through the spare pass directly, because the veto would refuse
    // this candidate anyway and would mask which guard did the work.
    let file = joined(&["one();", "}"]);
    let (text, warnings) = apply_spares_directly(&file, "PUT 1=2:\n+two();");

    assert_eq!(text, "two();");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_payload_that_is_not_short_a_bracket_gets_no_spare() {
    // The payload closes everything it opens, so it is not missing anything and
    // there is nothing for a spare to restore. Without this check the rule
    // keeps a closer the model deliberately moved inside its new content, which
    // silently adds a bracket.
    let file = joined(&["fn a() {", "\tif x {", "\t}", "}"]);
    let (text, warnings) = apply_spares_directly(&file, "PUT 2=3:\n+\ttwo();");

    assert_eq!(text, joined(&["fn a() {", "\ttwo();", "}"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_authored_edit_that_still_parses_is_never_second_guessed() {
    // The decisive gate, and the reason the parser is consulted twice rather
    // than once. The arithmetic nominates a spare here, and the spared result
    // would also parse, so checking only the repaired candidate would let the
    // rewrite through. What forbids it is that the file the author actually
    // wrote is already well-formed: a file that parses is not missing a closer,
    // whatever the counting says.
    //
    // The parser accepts everything, which is the strongest possible form of
    // this test: nothing downstream can refuse the spare, so only this gate can.
    let file = joined(&["fn a() {", "\tone();", "}"]);
    let (text, warnings) = apply_with_veto(&file, "PUT 2=3:\n+\tif x {\n+\t}", |_| true);

    assert_eq!(text, joined(&["fn a() {", "\tif x {", "\t}"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_brace_in_prose_is_never_resurrected() {
    // The corruption case this whole mechanism exists to prevent. In a file the
    // parser cannot judge, `parses` is false for everything, so no repaired
    // result can ever be shown to be better and the authored edit stands.
    let file = joined(&["A paragraph.", "closing thought }", "The end."]);
    let (text, warnings) = apply_with_veto(&file, "PUT 2=2:\n+a new thought", |_| false);

    assert_eq!(
        text,
        joined(&["A paragraph.", "a new thought", "The end."]),
        "prose must be edited exactly as written"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_spare_that_does_not_fix_the_file_is_not_applied() {
    // The authored edit breaks the file, a spare is nominated, but the result
    // is still broken. A repair only lands when it is shown to work, so the
    // authored edit is returned rather than a half-measure.
    let file = joined(&["fn a() {", "\tone();", "}"]);
    // The payload opens two more blocks than it closes, so keeping the `}`
    // cannot balance the file.
    let (text, warnings) = apply_with_veto(
        &file,
        "PUT 2=3:\n+\tif x {\n+\t\tif y {",
        brackets_balance,
    );

    assert_eq!(text, joined(&["fn a() {", "\tif x {", "\t\tif y {"]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_closer_whose_opener_another_hunk_deleted_is_not_kept() {
    // Two hunks: one removes the block's opening line, another replaces its
    // body and would otherwise keep the closer. With the opener gone the
    // closer has nothing to close, so keeping it would leave a stray brace.
    let file = joined(&["if (x) {", "\tbody();", "}", "after();"]);
    let (text, warnings) = apply_with_veto(
        &file,
        "PUT 1=1:\n+// opener removed\n\nPUT 2=3:\n+\tfresh();",
        brackets_balance,
    );

    assert_eq!(text, joined(&["// opener removed", "\tfresh();", "after();"]));
    assert!(
        !warnings.iter().any(|w| w.contains("dropped closing line")),
        "{warnings:?}"
    );
}

#[test]
fn a_jsx_style_closer_is_not_treated_as_a_bracket() {
    // `</div>` carries no brackets, so the balance arithmetic that justifies
    // every spare would be satisfied by an empty requirement. Excluding the
    // JSX form from the closer test is what stops the rule keeping arbitrary
    // lines it cannot actually reason about.
    assert!(!is_structural_closer("</div>"));
    assert!(!is_structural_closer("/>"));
    assert!(is_structural_closer("}"));
    assert!(is_structural_closer("\t});"));
    assert!(is_structural_closer("  }],"));
    assert!(!is_structural_closer("} else {"));
    assert!(!is_structural_closer(""));
}

#[test]
fn without_a_veto_no_closer_is_ever_spared() {
    // The plain entry point must stay conservative, since a caller with no
    // parser has no way to check the result.
    let file = joined(&[
        "const handlers = {",
        "\ta() {",
        "\t\treturn 1;",
        "\t},",
        "};",
    ]);
    let (text, warnings) = apply(&file, "PUT 5=5:\n+\tb() {\n+\t\treturn 2;\n+\t},");

    // The `};` is gone, as authored: unbalanced, but exactly what was asked
    // for, and no worse than before this rule existed.
    assert!(!text.contains("};"), "{text}");
    assert!(warnings.is_empty(), "{warnings:?}");
}
