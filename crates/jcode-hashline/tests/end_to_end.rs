//! End-to-end tests: the modules composing as a caller would use them.
//!
//! Every other test in this crate exercises one module. These prove the seams
//! hold, which is a different question — each module can be correct while the
//! handoffs between them are wrong, and the handoffs are where a port usually
//! breaks.
//!
//! The flow modelled here is the real one:
//!
//! 1. `read` shows numbered lines under a `[path#TAG]` header, recording what
//!    it displayed.
//! 2. The model authors a patch quoting that header.
//! 3. The patch splits into sections, parses into ops, validates against the
//!    tag and the seen lines, and applies.
//! 4. The result carries a fresh tag so the next edit needs no re-read.

use jcode_hashline::{
    apply_ops, compute_file_hash, format_hashline_header, format_numbered_lines, parse_ops,
    preflight, prepare, split_sections, RejectReason, SectionInput, SnapshotStore,
};

const SOURCE: &str = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";

/// Simulate a `read`: record the snapshot with provenance, and render what the
/// model would see.
fn simulated_read(store: &SnapshotStore, path: &str, text: &str) -> (String, String) {
    let line_count = text.split('\n').count();
    let seen: Vec<usize> = (1..=line_count).collect();
    let tag = store.record(path, text, Some(&seen));
    let rendered = format!(
        "{}\n{}",
        format_hashline_header(path, &tag),
        format_numbered_lines(text, 1)
    );
    (tag, rendered)
}

/// Drive a whole patch the way a tool would.
fn run_patch(store: &SnapshotStore, current: &str, patch: &str) -> Result<String, RejectReason> {
    let sections = split_sections(patch, None).expect("patch must split");
    assert_eq!(sections.len(), 1, "these fixtures use one section");
    let section = &sections[0];

    let parsed = parse_ops(&section.body).expect("body must parse");
    let prepared = prepare(
        store,
        &section.path,
        current,
        section.file_hash.as_deref(),
        &parsed.ops,
        true,
        None,
    )?;
    Ok(prepared.after)
}

/// The core loop: read, author against what was shown, apply.
#[test]
fn a_patch_authored_from_read_output_applies() {
    let store = SnapshotStore::new();
    let (tag, rendered) = simulated_read(&store, "main.rs", SOURCE);

    // What the model sees. The header it must quote, and numbered lines.
    assert!(rendered.starts_with(&format!("[main.rs#{tag}]")));
    assert!(rendered.contains("2:    let x = 1;"));

    let patch = format!("[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 42;");
    let result = run_patch(&store, SOURCE, &patch).expect("patch must apply");

    assert_eq!(
        result,
        "fn main() {\n    let x = 42;\n    println!(\"{x}\");\n}\n"
    );
}

/// The chaining property: an edit's result tag anchors the next edit, so a
/// multi-step change costs one read rather than one read per step.
#[test]
fn a_second_edit_chains_off_the_first_without_a_re_read() {
    let store = SnapshotStore::new();
    let (tag, _) = simulated_read(&store, "main.rs", SOURCE);

    let first = run_patch(
        &store,
        SOURCE,
        &format!("[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 42;"),
    )
    .expect("first edit");

    // The tool records what it wrote, which is how the chain continues.
    let next_tag = store.record("main.rs", &first, None);

    let second = run_patch(
        &store,
        &first,
        &format!("[main.rs#{next_tag}]\nPUT 3.=3:\n+    dbg!(x);"),
    )
    .expect("second edit must apply against the first edit's tag");

    assert_eq!(
        second,
        "fn main() {\n    let x = 42;\n    dbg!(x);\n}\n"
    );
}

/// The guarantee that distinguishes hashline from a line-number editor: an edit
/// authored against a stale view never lands at the wrong place. This is the
/// multi-agent case that motivated the whole port.
///
/// Note what "never lands wrong" turned out to mean once recovery arrived. It
/// is not "refuse whenever the file moved": here the line the model targeted
/// still exists, merely one row lower, so the edit is placed on it and the
/// other agent's insertion survives. Refusal is the fallback for when that
/// cannot be proven, not the goal in itself.
#[test]
fn an_edit_authored_against_a_stale_view_lands_on_its_real_target() {
    let store = SnapshotStore::new();
    let (tag, _) = simulated_read(&store, "main.rs", SOURCE);

    // Something else inserts a line above: another agent, a formatter, the
    // user. The model's target moves from line 2 to line 3.
    let drifted = "fn main() {\n    let y = 2;\n    let x = 1;\n    println!(\"{x}\");\n}\n";

    let recovered = run_patch(
        &store,
        drifted,
        &format!("[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 42;"),
    )
    .expect("the target line is unchanged, so the anchor can be relocated");

    // The model's edit landed on the line it meant, and the concurrent
    // insertion is still there.
    assert_eq!(
        recovered,
        "fn main() {\n    let y = 2;\n    let x = 42;\n    println!(\"{x}\");\n}\n"
    );
}

/// The other half: when the targeted line is the one that changed, there is no
/// safe place to put the edit and it is refused.
///
/// This is the case that must never silently succeed. The model authored a
/// replacement for content that no longer exists, so applying it anywhere would
/// discard whatever replaced it.
#[test]
fn an_edit_whose_target_line_itself_changed_is_refused() {
    let store = SnapshotStore::new();
    let (tag, _) = simulated_read(&store, "main.rs", SOURCE);

    // The very line the model is replacing was rewritten underneath it.
    let drifted = "fn main() {\n    let x = 99;\n    println!(\"{x}\");\n}\n";

    let error = run_patch(
        &store,
        drifted,
        &format!("[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 42;"),
    )
    .expect_err("the target moved under the model");

    assert!(matches!(error, RejectReason::StaleTag { .. }), "{error:?}");
}

/// A patch echoing `read` output verbatim must still work. This is the
/// composition `prefixes` exists for: without it every quoted line would carry
/// its number into the file.
#[test]
fn a_patch_whose_body_echoes_read_output_still_lands_clean_content() {
    let store = SnapshotStore::new();
    let (tag, _) = simulated_read(&store, "main.rs", SOURCE);

    // The model pastes the line back with its `2:` prefix and no `+` sigil,
    // which is two near-misses at once.
    let patch = format!("[main.rs#{tag}]\nPUT 2.=2:\n2:    let x = 99;");
    let result = run_patch(&store, SOURCE, &patch).expect("both recoveries must fire");

    assert_eq!(
        result,
        "fn main() {\n    let x = 99;\n    println!(\"{x}\");\n}\n",
        "the line number must not become file content"
    );
}

/// A partial read grants partial provenance, and the guard holds across the
/// whole pipeline rather than only inside the patcher's unit tests.
#[test]
fn a_partial_read_only_authorizes_the_lines_it_displayed() {
    let store = SnapshotStore::new();
    // Only lines 1-2 were shown.
    let tag = store.record("main.rs", SOURCE, Some(&[1, 2]));

    let allowed = run_patch(
        &store,
        SOURCE,
        &format!("[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 42;"),
    );
    assert!(allowed.is_ok(), "line 2 was displayed");

    let refused = run_patch(
        &store,
        SOURCE,
        &format!("[main.rs#{tag}]\nPUT 4.=4:\n+}}"),
    )
    .expect_err("line 4 was never displayed");
    assert!(matches!(refused, RejectReason::UnseenLines { .. }), "{refused:?}");
}

/// Several hunks in one section anchor against the original file, so a model
/// can author them all from a single read without simulating its own patch.
#[test]
fn multiple_hunks_in_one_section_anchor_against_the_original() {
    let store = SnapshotStore::new();
    let (tag, _) = simulated_read(&store, "main.rs", SOURCE);

    let patch = format!(
        "[main.rs#{tag}]\nPUT 2.=2:\n+    let x = 7;\nPUT >3:\n+    dbg!(x);"
    );
    let result = run_patch(&store, SOURCE, &patch).expect("both hunks apply");

    assert_eq!(
        result,
        "fn main() {\n    let x = 7;\n    println!(\"{x}\");\n    dbg!(x);\n}\n"
    );
}

/// A patch spanning two files splits, then preflights as a unit.
#[test]
fn a_two_file_patch_validates_every_section_before_any_would_be_written() {
    let store = SnapshotStore::new();
    let a = "alpha\n";
    let b = "beta\n";
    let tag_a = store.record("a.txt", a, Some(&[1, 2]));
    let tag_b = store.record("b.txt", b, Some(&[1, 2]));

    let patch = format!(
        "[a.txt#{tag_a}]\nPUT 1.=1:\n+ALPHA\n[b.txt#{tag_b}]\nPUT 1.=1:\n+BETA"
    );
    let sections = split_sections(&patch, None).expect("must split");
    assert_eq!(sections.len(), 2);

    let parsed: Vec<_> = sections
        .iter()
        .map(|section| parse_ops(&section.body).expect("body parses"))
        .collect();
    let current = [a, b];
    let inputs: Vec<SectionInput<'_>> = sections
        .iter()
        .zip(&parsed)
        .zip(current)
        .map(|((section, parsed), text)| SectionInput {
            path: &section.path,
            current_text: text,
            expected_tag: section.file_hash.as_deref(),
            ops: &parsed.ops,
        })
        .collect();

    let prepared = preflight(&store, &inputs, true, None).expect("both sections validate");
    assert_eq!(prepared[0].after, "ALPHA\n");
    assert_eq!(prepared[1].after, "BETA\n");
}

/// The guarantee that matters for a multi-file patch: a bad section anywhere
/// means no section is prepared, so a partial application cannot happen. Before
/// `preflight` existed a caller had to loop and would have written the first
/// file before discovering the second was stale.
#[test]
fn a_bad_section_anywhere_prevents_the_whole_patch() {
    let store = SnapshotStore::new();
    let a = "alpha\n";
    let b = "beta\n";
    let tag_a = store.record("a.txt", a, Some(&[1, 2]));

    // b.txt carries a tag nothing minted.
    let patch = format!(
        "[a.txt#{tag_a}]\nPUT 1.=1:\n+ALPHA\n[b.txt#FFFF]\nPUT 1.=1:\n+BETA"
    );
    let sections = split_sections(&patch, None).expect("must split");
    let parsed: Vec<_> = sections
        .iter()
        .map(|section| parse_ops(&section.body).expect("body parses"))
        .collect();
    let current = [a, b];
    let inputs: Vec<SectionInput<'_>> = sections
        .iter()
        .zip(&parsed)
        .zip(current)
        .map(|((section, parsed), text)| SectionInput {
            path: &section.path,
            current_text: text,
            expected_tag: section.file_hash.as_deref(),
            ops: &parsed.ops,
        })
        .collect();

    let error = preflight(&store, &inputs, true, None).expect_err("b.txt is unvalidatable");
    assert!(error.message().contains("b.txt"), "{}", error.message());
}

/// The tag a `read` renders must be the tag the patcher accepts. A mismatch
/// anywhere in that chain makes every edit fail, so it is worth asserting
/// directly rather than inferring from the tests above.
#[test]
fn the_tag_rendered_by_read_is_the_tag_the_patcher_validates() {
    let store = SnapshotStore::new();
    let (tag, rendered) = simulated_read(&store, "main.rs", SOURCE);

    assert_eq!(tag, compute_file_hash(SOURCE));
    let header_tag = rendered
        .lines()
        .next()
        .and_then(|line| line.rsplit_once('#'))
        .map(|(_, tail)| tail.trim_end_matches(']'))
        .expect("header must carry a tag");
    assert_eq!(header_tag, tag);
}

/// Applying with no operations must leave the text untouched. Trivial, but it
/// pins that the pipeline has no implicit normalization: a patch that does
/// nothing must not rewrite line endings or the trailing newline.
#[test]
fn an_empty_patch_leaves_the_text_byte_identical() {
    let result = apply_ops(SOURCE, &[]).expect("empty patch applies");
    assert_eq!(result.text, SOURCE);
    assert_eq!(result.first_changed_line, None);
}
