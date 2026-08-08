use super::*;
use std::fs;
use tempfile::TempDir;

fn tree(files: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().expect("temp");
    for (name, body) in files {
        let path = temp.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("dirs");
        }
        fs::write(&path, body).expect("write");
    }
    temp
}

fn targets(temp: &TempDir) -> Vec<Target> {
    jcode_search::resolve_targets(None, temp.path()).expect("targets")
}

fn run(temp: &TempDir, pattern: &str, replacement: &str) -> RewritePlan {
    plan(
        pattern,
        replacement,
        &targets(temp),
        temp.path(),
        &RewriteOptions::default(),
    )
    .expect("plan")
}

#[test]
fn a_rewrite_is_planned_but_not_written() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let found = run(&temp, "one()", "two()");

    assert_eq!(found.files.len(), 1);
    assert_eq!(found.files[0].after, "fn alpha() { two(); }\n");
    assert_eq!(
        fs::read_to_string(temp.path().join("a.rs")).expect("read"),
        "fn alpha() { one(); }\n",
        "planning must not touch the file"
    );
}

/// The point of staging: the model is shown the whole change before any of it
/// lands, so a rewrite that is wrong in file 9 is caught before file 1 is
/// written.
#[test]
fn every_file_is_planned_before_any_is_written() {
    let temp = tree(&[
        ("a.rs", "fn a() { one(); }\n"),
        ("b.rs", "fn b() { one(); }\n"),
    ]);
    let found = run(&temp, "one()", "two()");

    assert_eq!(found.files.len(), 2);
    for name in ["a.rs", "b.rs"] {
        assert!(
            fs::read_to_string(temp.path().join(name))
                .expect("read")
                .contains("one()"),
            "{name} was written during planning"
        );
    }
}

#[test]
fn metavariables_are_carried_into_the_replacement() {
    let temp = tree(&[("a.rs", "fn alpha() { log(x); }\n")]);
    let found = run(&temp, "log($ARG)", "trace($ARG)");

    assert_eq!(found.files[0].after, "fn alpha() { trace(x); }\n");
}

/// Rewriting every match in a file, not just the first. A single-replacement
/// rewrite would silently leave the rest behind and report success.
#[test]
fn every_match_in_a_file_is_rewritten() {
    let temp = tree(&[("a.rs", "fn a() { one(); one(); one(); }\n")]);
    let found = run(&temp, "one()", "two()");

    assert_eq!(found.files[0].count, 3);
    assert_eq!(found.files[0].after, "fn a() { two(); two(); two(); }\n");
}

/// Back-to-front application. Rewriting forward shifts every later offset by
/// the length delta of the edit before it, which corrupts the file when the
/// replacement is a different length than the match.
#[test]
fn replacements_of_a_different_length_do_not_corrupt_later_matches() {
    let temp = tree(&[("a.rs", "fn a() { x(); y(); x(); }\n")]);
    let found = run(&temp, "x()", "much_longer_name()");

    assert_eq!(
        found.files[0].after,
        "fn a() { much_longer_name(); y(); much_longer_name(); }\n"
    );
}

#[test]
fn a_file_with_no_matches_is_not_in_the_plan() {
    let temp = tree(&[
        ("a.rs", "fn a() { one(); }\n"),
        ("b.rs", "fn b() { other(); }\n"),
    ]);
    let found = run(&temp, "one()", "two()");

    assert_eq!(found.files.len(), 1);
    assert_eq!(found.files[0].path, "a.rs");
    assert_eq!(found.files_searched, 2, "both were considered");
}

/// A rewrite whose replacement equals the match changes nothing, and a plan
/// listing it would show the caller an empty diff.
#[test]
fn a_rewrite_that_changes_nothing_is_not_reported_as_a_change() {
    let temp = tree(&[("a.rs", "fn a() { one(); }\n")]);
    let found = run(&temp, "one()", "one()");

    assert!(found.is_empty(), "identical rewrite is not a change");
}

#[test]
fn the_file_cap_stops_the_rewrite_and_says_so() {
    let temp = tree(&[
        ("a.rs", "fn a() { one(); }\n"),
        ("b.rs", "fn b() { one(); }\n"),
        ("c.rs", "fn c() { one(); }\n"),
    ]);
    let found = plan(
        "one()",
        "two()",
        &targets(&temp),
        temp.path(),
        &RewriteOptions {
            max_files: 2,
            ..Default::default()
        },
    )
    .expect("plan");

    assert_eq!(found.files.len(), 2);
    assert!(
        found.limit_reached,
        "a partial plan must not look like the whole change"
    );
}

#[test]
fn an_empty_pattern_is_refused() {
    let temp = tree(&[("a.rs", "fn a() {}\n")]);
    let error = plan(
        "   ",
        "x",
        &targets(&temp),
        temp.path(),
        &RewriteOptions::default(),
    )
    .expect_err("empty pattern");

    assert!(error.message().contains("empty"), "{}", error.message());
}

/// Language inference across a mixed tree: the Python file cannot use a Rust
/// pattern, and that is expected rather than fatal.
#[test]
fn files_the_pattern_cannot_be_used_on_are_counted_not_fatal() {
    let temp = tree(&[
        ("a.rs", "fn alpha() { one(); }\n"),
        ("b.py", "def beta():\n    pass\n"),
    ]);
    let found = run(&temp, "fn $N() { $$$B }", "fn $N() { changed(); }");

    assert_eq!(found.files.len(), 1);
    assert_eq!(found.incompatible_files, 1);
}

#[test]
fn a_file_without_a_grammar_is_counted_as_unsupported() {
    let temp = tree(&[("notes.xyz", "one()\n")]);
    let found = run(&temp, "one()", "two()");

    assert!(found.is_empty());
    assert_eq!(found.unsupported_files, 1);
}

/// Structural, not textual: the pattern matches a call, so the same characters
/// inside a string or comment are left alone. This is the whole reason to use
/// ast_edit over a regex replace.
#[test]
fn text_that_is_not_code_is_not_rewritten() {
    let temp = tree(&[(
        "a.rs",
        "fn a() {\n    // calls one()\n    let s = \"one()\";\n    one();\n}\n",
    )]);
    let found = run(&temp, "one()", "two()");

    let after = &found.files[0].after;
    assert!(after.contains("// calls one()"), "comment was rewritten");
    assert!(after.contains("\"one()\""), "string was rewritten");
    assert!(after.contains("    two();"), "the call was not rewritten");
    assert_eq!(found.files[0].count, 1);
}

#[test]
fn the_plan_carries_both_sides_so_a_diff_can_be_rendered() {
    let temp = tree(&[("a.rs", "fn a() { one(); }\n")]);
    let found = run(&temp, "one()", "two()");

    assert_eq!(found.files[0].before, "fn a() { one(); }\n");
    assert_eq!(found.files[0].after, "fn a() { two(); }\n");
    assert_eq!(found.total_replacements, 1);
}

/// The per-file cap has to cut the rewrite, not just the number reported.
/// Counting alone would tell the caller 2 replacements while writing 3.
#[test]
fn the_replacement_cap_limits_what_is_written_not_just_the_count() {
    let temp = tree(&[("a.rs", "fn a() { one(); one(); one(); }\n")]);
    let found = plan(
        "one()",
        "two()",
        &targets(&temp),
        temp.path(),
        &RewriteOptions {
            max_replacements: 2,
            ..Default::default()
        },
    )
    .expect("plan");

    assert_eq!(found.files[0].count, 2);
    assert_eq!(
        found.files[0].after, "fn a() { two(); two(); one(); }\n",
        "the third match must be left alone, matching the reported count"
    );
    assert!(found.limit_reached);
}
