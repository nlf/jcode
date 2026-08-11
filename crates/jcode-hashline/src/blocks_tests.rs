//! Block-resolution tests, ported from oh-my-pi's `block.test.ts`.
//!
//! The resolver is stubbed rather than real, exactly as omp stubs theirs. What
//! is under test here is the transform and its refusals: whether a span becomes
//! the right range, and whether a failure says something a model can act on.
//! Whether tree-sitter finds the right span is `jcode-ast`'s business and is
//! tested there.

use super::*;
use crate::parser::parse_ops;

fn ops(patch: &str) -> Vec<Op> {
    parse_ops(patch).expect("fixture patch parses").ops
}

/// omp's stub: the block beginning on line N spans [N, N+1].
fn stub(_path: &str, _text: &str, line: usize) -> Option<BlockSpan> {
    Some(BlockSpan {
        start: line,
        end: line + 1,
    })
}

fn resolve_with(
    patch: &str,
    text: &str,
    resolver: Option<BlockResolver<'_>>,
) -> Result<Vec<Op>, String> {
    let mut ops = ops(patch);
    resolve(&mut ops, "x.ts", text, resolver)?;
    Ok(ops)
}

#[test]
fn a_block_becomes_the_range_it_covers() {
    // The whole point: `PUT 2*:` is `PUT 2.=3:` once someone can see line 2,
    // and the model never had to count to the closing brace.
    let resolved = resolve_with("PUT 2*:\n+A\n+B", "ignored", Some(&stub)).expect("resolves");

    assert_eq!(resolved, ops("PUT 2.=3:\n+A\n+B"));
}

#[test]
fn a_block_with_no_payload_becomes_a_cut() {
    // An empty body means deletion, as it does for a range. The range form is
    // rewritten to `CUT` while parsing; a block cannot be, because its extent
    // is not known until here.
    let resolved = resolve_with("CUT 2*", "ignored", Some(&stub)).expect("resolves");

    assert_eq!(resolved, vec![Op::Cut { start: 2, end: 3 }]);
}

#[test]
fn ops_without_a_block_anchor_are_untouched() {
    let resolved = resolve_with("PUT 1.=1:\n+X", "ignored", Some(&stub)).expect("resolves");

    assert_eq!(resolved, ops("PUT 1.=1:\n+X"));
}

#[test]
fn a_block_anchor_with_no_resolver_is_refused_by_name() {
    // A caller with no parser must be told the feature is unavailable rather
    // than have the anchor silently ignored, which would drop the edit.
    let error = resolve_with("PUT 2*:\n+X", "ignored", None).expect_err("must refuse");

    assert!(error.contains("no parser configured"), "{error}");
    assert!(error.contains("5.=9"), "must suggest the concrete form: {error}");
}

// ─── refusals that have to teach ─────────────────────────────────────────────
//
// Every one of these is a mis-anchor the feature invites, and a bare "no block
// there" would leave the model to make the same guess again. What is asserted
// is the suggestion, not just the failure.

#[test]
fn an_unresolvable_anchor_names_the_line_and_shows_its_surroundings() {
    let text = "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot";
    let error = resolve_with("PUT 3*:\n+X", text, Some(&|_, _, _| None)).expect_err("must refuse");

    assert!(
        error.contains("could not resolve a syntactic block beginning on line 3"),
        "{error}"
    );
    // Two lines either side, with the anchor marked, because the useful
    // realisation is usually "that is not the line I meant".
    assert!(error.contains(" 1:alpha"), "{error}");
    assert!(error.contains("*3:charlie"), "{error}");
    assert!(error.contains(" 5:echo"), "{error}");
    assert!(!error.contains("foxtrot"), "context must stay bounded: {error}");
}

#[test]
fn a_blank_anchor_points_at_the_next_real_block() {
    let text = "alpha\n\nfunction x() {\n  return 1;\n}";
    let resolver = |_: &str, _: &str, line: usize| {
        (line == 3).then_some(BlockSpan { start: 3, end: 5 })
    };
    let error = resolve_with("PUT 2*:\n+function y() {}", text, Some(&resolver))
        .expect_err("must refuse");

    assert!(
        error.contains("Line 2 is blank; no syntactic block can begin there"),
        "{error}"
    );
    assert!(
        error.contains("next multi-line block begins at line 3 and ends at line 5"),
        "{error}"
    );
    assert!(error.contains("Retry `PUT 3*:`"), "{error}");
}

#[test]
fn a_bare_statement_is_refused_with_both_the_exact_and_enclosing_forms() {
    // The mis-anchor that would otherwise be silently destructive: line 2 is
    // one statement, and the model that wrote `*` was reaching for the function
    // around it. Both readings are offered rather than either being assumed.
    let text = "function x() {\n  run();\n}";
    let resolver = |_: &str, _: &str, line: usize| match line {
        2 => Some(BlockSpan { start: 2, end: 2 }),
        1 => Some(BlockSpan { start: 1, end: 3 }),
        _ => None,
    };
    let error =
        resolve_with("PUT 2*:\n+  stop();", text, Some(&resolver)).expect_err("must refuse");

    assert!(error.contains("resolved a single-line block"), "{error}");
    assert!(
        error.contains("For only this statement use `PUT 2:`"),
        "{error}"
    );
    assert!(
        error.contains("begins at line 1 and ends at line 3; use `PUT 1*:` to target it"),
        "{error}"
    );
}

#[test]
fn a_deleting_block_is_offered_cut_forms_not_put_forms() {
    // The suggestion has to be a form of the op the model actually used.
    // Offering `PUT` in answer to a deletion is worse than no suggestion: it is
    // an instruction to do something else entirely.
    //
    // What distinguishes the two here is the payload, not the keyword. `CUT 2*`
    // and `PUT 2*:` with no body parse to the same op, because an empty
    // replacement *is* a deletion, so the message is chosen by whether a
    // payload exists. Asserting on the keyword would be asserting on something
    // this layer cannot see.
    let text = "function x() {\n  run();\n}";
    let resolver = |_: &str, _: &str, line: usize| match line {
        2 => Some(BlockSpan { start: 2, end: 2 }),
        1 => Some(BlockSpan { start: 1, end: 3 }),
        _ => None,
    };
    let error = resolve_with("CUT 2*", text, Some(&resolver)).expect_err("must refuse");

    assert!(error.contains("For only this statement use `CUT 2`"), "{error}");
    assert!(error.contains("use `CUT 1*` to target it"), "{error}");
    assert!(!error.contains("PUT"), "must not suggest a PUT: {error}");

    // And the mirror, so the branch is shown to be selected rather than
    // constant: the same anchor carrying a payload is offered `PUT` forms.
    let replacing =
        resolve_with("PUT 2*:\n+  stop();", text, Some(&resolver)).expect_err("must refuse");
    assert!(replacing.contains("use `PUT 2:`"), "{replacing}");
    assert!(!replacing.contains("CUT"), "must not suggest a CUT: {replacing}");
}

#[test]
fn an_anchor_past_the_end_of_the_file_omits_the_context_preview() {
    // There is nothing to show, and a preview of blank lines would suggest the
    // file has content there.
    let error =
        resolve_with("PUT 9*:\n+X", "only\ntwo", Some(&|_, _, _| None)).expect_err("must refuse");

    assert!(
        error.contains("could not resolve a syntactic block beginning on line 9"),
        "{error}"
    );
    assert!(!error.contains("\n\n"), "no preview expected: {error}");
}

#[test]
fn a_refusal_leaves_every_op_unapplied() {
    // Refusing whole rather than per-op. A patch with a good hunk and a bad one
    // must not half-apply: the model would then be reasoning about a file in a
    // state neither it nor the error describes.
    let mut ops = ops("PUT 1.=1:\n+kept\nPUT 9*:\n+X");
    let before = ops.clone();

    resolve(&mut ops, "x.ts", "only\ntwo", Some(&|_, _, _| None)).expect_err("must refuse");

    assert_eq!(ops, before, "a refused resolve must not mutate the ops");
}

#[test]
fn no_anchor_can_panic_the_resolver() {
    // Resolution runs before anything validates a model's arithmetic, so it has
    // to be total over whatever the parser accepted.
    let files = ["", "\n", "a", "a\nb\n", "{\n}\n"];
    let mut checked = 0;

    std::panic::set_hook(Box::new(|_| {}));
    for text in files {
        for line in [1usize, 2, 3, 99] {
            for patch in [format!("PUT {line}*:\n+Z"), format!("CUT {line}*")] {
                let Ok(parsed) = parse_ops(&patch) else {
                    continue;
                };
                for resolver in [
                    Some(&stub as BlockResolver<'_>),
                    Some(&|_: &str, _: &str, _: usize| None as Option<BlockSpan>),
                    None,
                ] {
                    checked += 1;
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut ops = parsed.ops.clone();
                        let _ = resolve(&mut ops, "x.ts", text, resolver);
                    }));
                    assert!(
                        outcome.is_ok(),
                        "resolve panicked on text={text:?} patch={patch:?}"
                    );
                }
            }
        }
    }
    let _ = std::panic::take_hook();
    assert!(checked > 100, "the sweep should be broad, ran {checked}");
}


