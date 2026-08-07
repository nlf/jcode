//! Behaviour spec for walking and matching.
//!
//! These use real temp directories rather than a mocked filesystem: the
//! interesting behaviour here *is* the filesystem interaction, and a mock would
//! only assert that the mock works.

use super::*;
use std::fs;

/// Build a tree from (relative path, contents) pairs.
fn tree(entries: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    for (path, contents) in entries {
        let full = temp.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&full, contents).expect("write");
    }
    temp
}

fn names(paths: &[PathBuf], root: &Path) -> Vec<String> {
    let mut out: Vec<String> = paths
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    out.sort();
    out
}

#[test]
fn no_path_searches_the_workspace_root() {
    let temp = tree(&[("a.rs", "x")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path, temp.path());
    assert_eq!(targets[0].original, ".");
}

#[test]
fn a_semicolon_list_becomes_several_targets() {
    let temp = tree(&[("src/a.rs", "x"), ("test/b.rs", "y")]);
    let targets = resolve_targets(Some("src; test"), temp.path()).expect("resolve");

    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].path, temp.path().join("src"));
    assert_eq!(targets[1].path, temp.path().join("test"));
}

/// Selectors are per entry, so one call can scope one file and not another.
#[test]
fn each_entry_carries_its_own_selector() {
    let temp = tree(&[("a.rs", "x"), ("b.rs", "y")]);
    let targets = resolve_targets(Some("a.rs:10-20; b.rs"), temp.path()).expect("resolve");

    assert_eq!(targets[0].ranges.len(), 1);
    assert_eq!(targets[0].ranges[0].start, 10);
    assert!(
        targets[1].ranges.is_empty(),
        "the second entry had no selector"
    );
}

/// omp issue #4618: a file that really is named `notes:1-2` must be searched,
/// not silently reinterpreted as `notes` scoped to lines 1-2.
#[test]
fn an_existing_file_wins_over_the_selector_reading_of_its_name() {
    let temp = tree(&[("notes:1-2", "hello")]);
    let targets = resolve_targets(Some("notes:1-2"), temp.path()).expect("resolve");

    assert_eq!(targets[0].path, temp.path().join("notes:1-2"));
    assert!(
        targets[0].ranges.is_empty(),
        "an existing path must not be read as a selector"
    );
}

/// A glob's characters are never selector syntax, and brace alternations can
/// contain colons.
#[test]
fn a_glob_entry_skips_selector_peeling() {
    let temp = tree(&[("a.rs", "x")]);
    let targets = resolve_targets(Some("src/**/*.rs"), temp.path()).expect("resolve");

    assert!(targets[0].is_glob);
    assert!(targets[0].ranges.is_empty());
}

#[test]
fn an_impossible_selector_is_reported_rather_than_ignored() {
    let temp = tree(&[("a.rs", "x")]);
    let error = resolve_targets(Some("a.rs:100-50"), temp.path()).expect_err("bad selector");

    assert!(
        error.message().contains("end must be >= start"),
        "{}",
        error.message()
    );
}

#[test]
fn finding_files_walks_a_directory() {
    let temp = tree(&[("src/a.rs", "x"), ("src/nested/b.rs", "y")]);
    let targets = resolve_targets(Some("src"), temp.path()).expect("resolve");
    let found = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");

    assert_eq!(names(&found, temp.path()), vec!["src/a.rs", "src/nested/b.rs"]);
}

/// A bare `*.rs` should find nested files too: that is what a caller means by
/// it far more often than "top level only".
#[test]
fn a_bare_extension_glob_matches_nested_files() {
    let temp = tree(&[("a.rs", "x"), ("deep/nested/b.rs", "y"), ("c.txt", "z")]);
    let targets = resolve_targets(Some("*.rs"), temp.path()).expect("resolve");
    let found = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");

    assert_eq!(names(&found, temp.path()), vec!["a.rs", "deep/nested/b.rs"]);
}

#[test]
fn a_rooted_glob_matches_only_its_subtree() {
    let temp = tree(&[("src/a.rs", "x"), ("other/b.rs", "y")]);
    let targets = resolve_targets(Some("src/**/*.rs"), temp.path()).expect("resolve");
    let found = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");

    assert_eq!(names(&found, temp.path()), vec!["src/a.rs"]);
}

/// Ignoring gitignored files by default is the difference between a tool that
/// is trusted and one whose output is mostly `target/` and `node_modules/`.
#[test]
fn gitignored_files_are_skipped_by_default() {
    let temp = tree(&[
        (".gitignore", "ignored/\n"),
        ("kept.rs", "needle"),
        ("ignored/hidden.rs", "needle"),
    ]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let found = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");

    let listed = names(&found, temp.path());
    assert!(listed.contains(&"kept.rs".to_string()));
    assert!(
        !listed.iter().any(|name| name.contains("ignored/")),
        "gitignored files leaked into results: {listed:?}"
    );
}

#[test]
fn gitignore_can_be_turned_off() {
    let temp = tree(&[
        (".gitignore", "ignored/\n"),
        ("ignored/hidden.rs", "needle"),
    ]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let options = WalkOptions {
        hidden: false,
        respect_gitignore: false,
    };
    let found = find_files(&targets, temp.path(), &options).expect("find");

    assert!(
        names(&found, temp.path())
            .iter()
            .any(|name| name.contains("ignored/")),
        "opting out of gitignore should reveal ignored files"
    );
}

#[test]
fn hidden_files_are_skipped_by_default_and_can_be_included() {
    let temp = tree(&[(".secret.rs", "needle"), ("visible.rs", "needle")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");

    let default = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");
    assert!(!names(&default, temp.path()).contains(&".secret.rs".to_string()));

    let with_hidden = find_files(
        &targets,
        temp.path(),
        &WalkOptions {
            hidden: true,
            respect_gitignore: true,
        },
    )
    .expect("find");
    assert!(names(&with_hidden, temp.path()).contains(&".secret.rs".to_string()));
}

#[test]
fn content_search_reports_one_indexed_line_numbers() {
    let temp = tree(&[("a.rs", "first\nsecond needle\nthird\n")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line, 2, "line numbers are 1-indexed");
    assert_eq!(matches[0].path, "a.rs", "paths are relative to the root");
    assert_eq!(matches[0].text, "second needle");
}

/// omp defaults to case-sensitive (`grep.ts:1015`), which is the opposite of
/// what their parameter name suggests. Pinned so it cannot drift back.
#[test]
fn search_is_case_sensitive_by_default_matching_omp() {
    let temp = tree(&[("a.rs", "NEEDLE\nneedle\n")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");

    let sensitive = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");
    assert_eq!(sensitive.len(), 1, "case-sensitive should match one line");

    let insensitive = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        false,
    )
    .expect("search");
    assert_eq!(insensitive.len(), 2, "opting out should match both");
}

#[test]
fn the_pattern_is_a_regex() {
    let temp = tree(&[("a.rs", "fn alpha()\nfn beta()\n")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let matches = search_contents(
        r"fn \w+\(\)",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    assert_eq!(matches.len(), 2);
}

#[test]
fn an_invalid_regex_is_reported_rather_than_matching_nothing() {
    let temp = tree(&[("a.rs", "x")]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let error = search_contents("(unclosed", &targets, temp.path(), &WalkOptions::default(), true)
        .expect_err("invalid regex");

    assert!(error.message().contains("Invalid regex"), "{}", error.message());
}

/// A selector must actually restrict the search, or scoping is decorative.
#[test]
fn a_selector_restricts_which_lines_match() {
    let body = (1..=100)
        .map(|i| format!("line {i} needle\n"))
        .collect::<String>();
    let temp = tree(&[("a.rs", body.as_str())]);
    let targets = resolve_targets(Some("a.rs:10-12"), temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    let lines: Vec<usize> = matches.iter().map(|item| item.line).collect();
    assert_eq!(lines, vec![10, 11, 12]);
}

/// Binary files have no useful lines and one "line" can be megabytes.
#[test]
fn unreadable_files_are_skipped_rather_than_failing_the_search() {
    let temp = tree(&[("text.rs", "needle")]);
    fs::write(temp.path().join("binary.bin"), [0xff, 0xfe, 0x00, 0x01]).expect("write binary");

    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("a binary file must not fail the whole search");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, "text.rs");
}

/// A missing path returns nothing rather than erroring, so one bad entry in a
/// semicolon list does not lose the results from the good ones.
#[test]
fn a_missing_path_yields_no_matches_without_failing() {
    let temp = tree(&[("a.rs", "needle")]);
    let targets = resolve_targets(Some("nope; a.rs"), temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    assert_eq!(matches.len(), 1, "the good entry still returns its match");
}

/// Collection stops at the internal cap so a pathological file cannot exhaust
/// memory before selection has a chance to trim.
#[test]
fn collection_stops_at_the_internal_cap() {
    let body = (0..(INTERNAL_TOTAL_CAP + 500))
        .map(|_| "needle\n")
        .collect::<String>();
    let temp = tree(&[("huge.rs", body.as_str())]);
    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    assert_eq!(matches.len(), INTERNAL_TOTAL_CAP);
}

/// A checked-in dataset or minified bundle costs the whole output budget.
#[test]
fn files_above_the_size_ceiling_are_not_searched() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("small.rs"), "needle").expect("write");
    let big = format!("{}\nneedle\n", "x".repeat(MAX_FILE_BYTES as usize + 1));
    fs::write(temp.path().join("big.rs"), big).expect("write");

    let targets = resolve_targets(None, temp.path()).expect("resolve");
    let matches = search_contents(
        "needle",
        &targets,
        temp.path(),
        &WalkOptions::default(),
        true,
    )
    .expect("search");

    let paths: Vec<&str> = matches.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(paths, vec!["small.rs"]);
}

/// Two entries naming the same file must not return it twice.
#[test]
fn overlapping_targets_do_not_duplicate_files() {
    let temp = tree(&[("src/a.rs", "x")]);
    let targets = resolve_targets(Some("src; src/a.rs"), temp.path()).expect("resolve");
    let found = find_files(&targets, temp.path(), &WalkOptions::default()).expect("find");

    assert_eq!(found.len(), 1, "the same file was returned twice: {found:?}");
}
