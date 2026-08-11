//! Landing-shift tests, ported from oh-my-pi's `landing-shift.test.ts`.
//!
//! The outward half only. The inward correction is for `insert_after_block N:`
//! lowerings, and `>N*` is not implemented, so there is nothing that could
//! reach it: a test would be asserting on unreachable code.

use super::*;
use crate::apply::apply_ops;
use crate::parser::parse_ops;

fn apply(text: &str, patch: &str) -> (String, Vec<String>) {
    let mut ops = parse_ops(patch).expect("fixture patch parses").ops;
    let lines: Vec<&str> = text.split('\n').collect();
    let warnings = repair_landings(&mut ops, &lines);
    let applied = apply_ops(text, &ops).expect("fixture applies");
    (applied.text, warnings)
}

fn joined(rows: &[&str]) -> String {
    rows.join("\n")
}

/// omp's fixture.
const FILE: &[&str] = &[
    "function f() {", // 1
    "    if (x) {",   // 2
    "        a();",   // 3
    "    }",          // 4
    "    b();",       // 5
    "}",              // 6
    "",
];

#[test]
fn slides_a_shallower_body_past_the_closing_line_and_warns() {
    // The whole point. `c();` is written at the depth of `if`, so it is a
    // sibling of that block, but line 3 is inside it. Applied literally the
    // statement lands in a scope its author did not write it for.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+    c();");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "    }",
            "    c();",
            "    b();",
            "}",
            "",
        ])
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("past 1 closing line(s) to after line 4"),
        "{}",
        warnings[0]
    );
}

#[test]
fn crosses_several_levels_and_stops_where_the_depth_matches() {
    let nested = joined(&[
        "function f() {",     // 1
        "    if (x) {",       // 2
        "        for (y) {",  // 3
        "            a();",   // 4
        "        }",          // 5
        "    }",              // 6
        "    b();",           // 7
        "}",                  // 8
        "",
    ]);

    // Depth 4 escapes both the `for` and the `if`.
    let (outer, outer_warnings) = apply(&nested, "PUT >4:\n+    c();");
    assert_eq!(outer.split('\n').nth(6), Some("    c();"));
    assert!(
        outer_warnings[0].contains("past 2 closing line(s) to after line 6"),
        "{}",
        outer_warnings[0]
    );

    // Depth 8 escapes only the `for`, staying inside the `if`. The stop
    // condition is what separates these: crossing continues while the closers
    // are deeper than the body and ends the moment one sits at its level.
    let (inner, inner_warnings) = apply(&nested, "PUT >4:\n+        c();");
    assert_eq!(inner.split('\n').nth(5), Some("        c();"));
    assert!(
        inner_warnings[0].contains("past 1 closing line(s) to after line 5"),
        "{}",
        inner_warnings[0]
    );
}

#[test]
fn a_body_at_the_anchors_own_depth_stays_put() {
    // Nothing to correct: the body already claims the depth it was anchored at.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+        c();");

    assert_eq!(text.split('\n').nth(3), Some("        c();"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_body_at_depth_zero_anchored_at_depth_zero_stays_put() {
    // The same rule where it actually bites. In the fixture above every anchor
    // is indented, so "is the anchor deeper than the body" is comfortably true
    // or false; here both sit at column 0, and only the strictness of *deeper*
    // (rather than "not shallower") keeps the insertion still. Without it the
    // body slides across the `}` below and out of the block it names.
    let flat = joined(&["x();", "", "}", "y();", ""]);
    let (text, warnings) = apply(&flat, "PUT >1:\n+c();");

    assert_eq!(text, joined(&["x();", "c();", "", "}", "y();", ""]));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn content_lines_are_never_crossed() {
    // Which is what makes an indentation-only language immune: with no closing
    // lines to cross, a Python body always lands exactly where it was anchored,
    // however its indentation compares to the anchor.
    let python = joined(&["def f():", "    if x:", "        a()", "    b()", ""]);
    let (text, warnings) = apply(&python, "PUT >3:\n+    c()");

    assert_eq!(
        text,
        joined(&["def f():", "    if x:", "        a()", "    c()", "    b()", ""])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_body_of_pure_closers_claims_no_depth() {
    // Such a body is rebalancing delimiters rather than living somewhere, so
    // its indentation is not a claim about scope and must not move it.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+    }");

    assert_eq!(text.split('\n').nth(3), Some("    }"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn incomparable_indentation_styles_are_left_alone() {
    // The file indents with tabs and the body with spaces, so "shallower" has
    // no meaning between them. Guessing a conversion would be inventing an
    // opinion about the file's style.
    let tabs = joined(&[
        "function f() {",
        "\tif (x) {",
        "\t\ta();",
        "\t}",
        "\tb();",
        "}",
        "",
    ]);
    let (text, warnings) = apply(&tabs, "PUT >3:\n+    c();");

    assert_eq!(text.split('\n').nth(3), Some("    c();"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_body_mixing_tabs_and_spaces_makes_no_claim_at_all() {
    // The mirror, inside a single body. One row indents with a tab and another
    // with spaces, so the rows cannot be ordered against each other and the
    // hunk has no coherent depth to act on. Falling back to whichever row came
    // first would move the insertion on the strength of a comparison that was
    // never valid: here it slides into the `if` the body sits below.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+\tc();\n+    d();");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "\tc();",
            "    d();",
            "    }",
            "    b();",
            "}",
            "",
        ]),
        "an unreadable claim leaves the insertion exactly where it was anchored"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_line_another_hunk_targets_is_never_crossed() {
    // The closer on line 4 is owned by the cut, so what it means after the
    // patch is not what the file shows. Sliding past it would be reasoning
    // about a line on its way out.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+    c();\nCUT 4");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "    c();",
            "    b();",
            "}",
            "",
        ])
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn blank_lines_are_looked_past_but_never_landed_after() {
    // A trailing blank belongs to whatever follows it rather than to the block
    // being left, so the landing is the closer, not the gap above it.
    let gapped = joined(&[
        "function f() {",
        "    if (x) {",
        "        a();",
        "",
        "    }",
        "    b();",
        "}",
        "",
    ]);
    let (text, warnings) = apply(&gapped, "PUT >3:\n+    c();");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "",
            "    }",
            "    c();",
            "    b();",
            "}",
            "",
        ])
    );
    assert!(warnings[0].contains("after line 5"), "{}", warnings[0]);
}

#[test]
fn an_insertion_before_a_line_is_left_alone() {
    // `PUT <N:` names the gap above a line, where there is no run of closers
    // below the anchor to cross and no claim to correct.
    let (text, warnings) = apply(&joined(FILE), "PUT <4:\n+    c();");

    assert_eq!(text.split('\n').nth(3), Some("    c();"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_replacement_is_left_alone() {
    // Only insertions carry a landing to correct. A replacement's range says
    // exactly which lines it covers, and moving it would be overruling that.
    let (text, warnings) = apply(&joined(FILE), "PUT 3=3:\n+    c();");

    assert_eq!(text.split('\n').nth(2), Some("    c();"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_anchor_on_a_blank_line_is_left_alone() {
    // A blank anchor makes no depth claim to be shallower than.
    let gapped = joined(&["function f() {", "    if (x) {", "", "    }", "}", ""]);
    let (text, warnings) = apply(&gapped, "PUT >3:\n+    c();");

    assert_eq!(text.split('\n').nth(3), Some("    c();"));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn an_anchor_of_only_whitespace_is_left_alone() {
    // The case the empty-line test above cannot reach. An empty anchor has no
    // indentation, so it is never "deeper" than a body and the shift declines
    // on the arithmetic alone; a whitespace-only line has real indentation and
    // would compare as deeper, moving an insertion on the strength of trailing
    // spaces the author cannot see and did not choose.
    //
    // Such lines are common: an editor leaving indentation on a line the author
    // blanked, and one at the end of a block is exactly where an insertion gets
    // anchored.
    let padded = joined(&[
        "f() {",
        "    if (x) {",
        "        ", // whitespace only
        "    }",
        "    b();",
        "}",
        "",
    ]);
    let (text, warnings) = apply(&padded, "PUT >3:\n+  c();");

    assert_eq!(
        text,
        joined(&[
            "f() {",
            "    if (x) {",
            "        ",
            "  c();",
            "    }",
            "    b();",
            "}",
            "",
        ]),
        "invisible whitespace must not decide where a body lands"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_closer_shallower_than_the_body_stops_the_crossing() {
    // The body sits at depth 2, between the file's depth-0 and depth-4 levels,
    // so no closer here is at its level and nothing bounds the crossing except
    // this rule. Without it the body escapes the function entirely, past the
    // `}` on line 6, which is further out than its indentation ever asked for.
    //
    // Crossing stops *before* such a closer rather than refusing the shift, so
    // an insertion that has already crossed a legitimate closer keeps that
    // progress.
    let (text, warnings) = apply(&joined(FILE), "PUT >5:\n+  c();");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "    }",
            "    b();",
            "  c();",
            "}",
            "",
        ]),
        "the body must stay inside the function it was anchored in"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn the_body_depth_is_its_shallowest_row_not_its_deepest() {
    // A body whose rows sit at different depths claims the shallowest of them,
    // because that is the scope the whole hunk has to live in: the deeper rows
    // are nested within it. Reading the deepest row instead would place this
    // body inside the `if` its first row was written to close over.
    let (text, warnings) = apply(&joined(FILE), "PUT >3:\n+        c();\n+    d();");

    assert_eq!(
        text,
        joined(&[
            "function f() {",
            "    if (x) {",
            "        a();",
            "    }",
            "        c();",
            "    d();",
            "    b();",
            "}",
            "",
        ])
    );
    assert!(
        warnings[0].contains("to after line 4"),
        "{}",
        warnings[0]
    );
}

#[test]
fn a_blank_line_below_the_anchor_is_not_a_landing() {
    // Looking past a blank is not the same as landing after one. In a language
    // with no closers to cross, the run below the anchor is blank and then
    // content, so there is no shift at all; treating the blank as a landing
    // would move the body across it for no reason and separate it from the
    // line it was anchored to.
    let python = joined(&["def f():", "    if x:", "        a()", "", "    b()", ""]);
    let (text, warnings) = apply(&python, "PUT >4:\n+  c()");

    assert_eq!(
        text,
        joined(&["def f():", "    if x:", "        a()", "", "  c()", "    b()", ""]),
        "the body stays where it was anchored"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn crossing_stops_at_the_first_closer_at_the_bodys_depth() {
    // Two closers sit at the body's own depth, one after the other. The first
    // ends the callback the anchor was inside, which is exactly as far as a
    // depth-4 body asked to go; the second ends the call around it. Continuing
    // past both would put the insertion outside a construct its indentation
    // never mentioned.
    //
    // The fixtures elsewhere in this file cannot show this: their closers step
    // outward one level at a time, so the run ends naturally and the stop
    // condition never has to fire. It takes two closers at the *same* depth,
    // the shape a nested call or callback produces, for the rule to matter.
    let nested_calls = joined(&[
        "f() {",             // 1
        "    g(() => {",     // 2
        "        x();",      // 3
        "    })",            // 4
        "    })",            // 5
        "    y();",          // 6
        "}",                 // 7
        "",
    ]);
    let (text, warnings) = apply(&nested_calls, "PUT >3:\n+    z();");

    assert_eq!(
        text,
        joined(&[
            "f() {",
            "    g(() => {",
            "        x();",
            "    })",
            "    z();",
            "    })",
            "    y();",
            "}",
            "",
        ]),
        "the insertion stops after the first closer at its depth"
    );
    assert!(
        warnings[0].contains("past 1 closing line(s) to after line 4"),
        "{}",
        warnings[0]
    );
}

#[test]
fn no_insertion_shape_can_panic_the_landing_pass() {
    // This runs before anything validates a model's arithmetic, so it has to be
    // total over whatever the parser accepted.
    let files = ["", "\n", "a", "a\n", "{\n}\n", "f() {\n\tx();\n}\n"];
    let mut checked = 0;

    std::panic::set_hook(Box::new(|_| {}));
    for text in files {
        let lines: Vec<&str> = text.split('\n').collect();
        for anchor in [0usize, 1, 2, 99] {
            for body in ["+x", "+    x", "+\tx", "+}", "+", "+    }\n+    y"] {
                let patch = format!("PUT >{anchor}:\n{body}");
                let Ok(parsed) = parse_ops(&patch) else {
                    continue;
                };
                checked += 1;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ops = parsed.ops.clone();
                    repair_landings(&mut ops, &lines);
                }));
                assert!(
                    outcome.is_ok(),
                    "landing panicked on text={text:?} patch={patch:?}"
                );
            }
        }
    }
    let _ = std::panic::take_hook();
    assert!(checked > 50, "the sweep should be broad, ran {checked}");
}








