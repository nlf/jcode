//! Behaviour spec for structural file search.
//!
//! These use real temp directories: the behaviour under test is the
//! interaction between walking, per-file language inference and the caps, and a
//! mock would only assert the mock works.

use super::*;
use std::fs;

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

fn run(temp: &tempfile::TempDir, pattern: &str) -> SearchResult {
    let targets = targets_for(None, temp.path()).expect("targets");
    search(pattern, &targets, temp.path(), &SearchOptions::default()).expect("search")
}

#[test]
fn a_pattern_matches_across_files() {
    let temp = tree(&[
        ("a.rs", "fn alpha() { one(); }\n"),
        ("b.rs", "fn beta() { two(); }\n"),
    ]);

    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(found.total_files, 2);
    assert_eq!(found.files.iter().map(|f| f.total).sum::<usize>(), 2);
}

/// Language is inferred per file, which is what makes a whole-tree search
/// possible without naming a language.
#[test]
fn each_file_is_parsed_as_its_own_language() {
    let temp = tree(&[
        ("a.rs", "fn alpha() { one(); }\n"),
        ("b.py", "def alpha():\n    pass\n"),
    ]);

    let rust_hits = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(rust_hits.total_files, 1);
    assert_eq!(rust_hits.files[0].path, "a.rs");
}

/// A file whose language has no grammar cannot be parsed, which is not the
/// same as not matching. The caller is told, so "no results" is not mistaken
/// for "no such code".
#[test]
fn files_without_a_grammar_are_counted_not_silently_dropped() {
    let temp = tree(&[
        ("a.rs", "fn alpha() { one(); }\n"),
        ("notes.txt", "fn alpha() { one(); }\n"),
        ("data.bin", "fn alpha() { one(); }\n"),
    ]);

    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(found.total_files, 1, "only the Rust file can be parsed");
    assert_eq!(
        found.unsupported_files, 2,
        "the caller must know two files could not be parsed"
    );
}

/// An explicit language overrides inference, for a file with an unusual
/// extension.
#[test]
fn an_explicit_language_overrides_inference() {
    let temp = tree(&[("script.txt", "fn alpha() { one(); }\n")]);
    let targets = targets_for(None, temp.path()).expect("targets");

    let options = SearchOptions {
        language: Some(crate::resolve_language("rust").expect("rust")),
        ..SearchOptions::default()
    };
    let found = search("fn $NAME() { $$$BODY }", &targets, temp.path(), &options).expect("search");

    assert_eq!(found.total_files, 1);
    assert_eq!(found.unsupported_files, 0);
}

#[test]
fn a_pattern_matching_nothing_returns_no_files() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let found = run(&temp, "struct $NAME { $$$FIELDS }");

    assert!(found.files.is_empty());
    assert_eq!(found.total_files, 0);
}

/// Gitignored files are skipped, inherited from the shared walker rather than
/// reimplemented.
#[test]
fn the_shared_walkers_ignore_rules_apply() {
    let temp = tree(&[
        (".gitignore", "vendor/\n"),
        ("a.rs", "fn alpha() { one(); }\n"),
        ("vendor/b.rs", "fn beta() { two(); }\n"),
    ]);

    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(found.total_files, 1);
    assert_eq!(found.files[0].path, "a.rs");
}

#[test]
fn the_per_file_cap_bounds_a_hot_file() {
    let body: String = (0..50).map(|i| format!("fn f{i}() {{ x(); }}\n")).collect();
    let temp = tree(&[("a.rs", body.as_str())]);
    let targets = targets_for(None, temp.path()).expect("targets");

    let options = SearchOptions {
        per_file_limit: 5,
        ..SearchOptions::default()
    };
    let found = search("fn $NAME() { $$$BODY }", &targets, temp.path(), &options).expect("search");

    assert_eq!(found.files[0].matches.len(), 5);
    assert_eq!(
        found.files[0].total, 50,
        "the true count survives the cap so the caller can be told"
    );
}

#[test]
fn the_file_cap_bounds_a_broad_search() {
    let entries: Vec<(String, String)> = (0..30)
        .map(|i| (format!("f{i}.rs"), format!("fn f{i}() {{ x(); }}\n")))
        .collect();
    let refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, body)| (path.as_str(), body.as_str()))
        .collect();
    let temp = tree(&refs);
    let targets = targets_for(None, temp.path()).expect("targets");

    let options = SearchOptions {
        file_limit: 10,
        ..SearchOptions::default()
    };
    let found = search("fn $NAME() { $$$BODY }", &targets, temp.path(), &options).expect("search");

    assert_eq!(found.files.len(), 10);
    assert_eq!(found.total_files, 30, "the true total is reported");
    assert!(found.file_limit_reached);
}

/// A window that exactly fits is not truncation, or the caller is told to
/// narrow a search that already showed everything.
#[test]
fn a_search_within_the_file_cap_is_not_marked_truncated() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert!(!found.file_limit_reached);
}

/// One unreadable file must not lose every other file's results.
#[test]
fn a_binary_file_does_not_fail_the_search() {
    let temp = tree(&[("a.rs", "fn alpha() { one(); }\n")]);
    fs::write(temp.path().join("blob.rs"), [0xff, 0xfe, 0x00, 0x01]).expect("binary");

    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(found.total_files, 1);
    assert_eq!(found.files[0].path, "a.rs");
}

#[test]
fn an_empty_pattern_is_refused() {
    let temp = tree(&[("a.rs", "fn alpha() {}\n")]);
    let targets = targets_for(None, temp.path()).expect("targets");
    let error = search("", &targets, temp.path(), &SearchOptions::default())
        .expect_err("an empty pattern matches everything");

    assert!(error.message().contains("pattern is required"), "{}", error.message());
}

/// Path resolution is shared, so a structural search takes the same scoping a
/// text search does.
#[test]
fn searching_can_be_scoped_to_a_subtree() {
    let temp = tree(&[
        ("src/a.rs", "fn alpha() { one(); }\n"),
        ("other/b.rs", "fn beta() { two(); }\n"),
    ]);

    let targets = targets_for(Some("src"), temp.path()).expect("targets");
    let found = search(
        "fn $NAME() { $$$BODY }",
        &targets,
        temp.path(),
        &SearchOptions::default(),
    )
    .expect("search");

    assert_eq!(found.total_files, 1);
    assert_eq!(found.files[0].path, "src/a.rs");
}

/// A pattern that no searched file can use is a pattern problem, not an empty
/// result. Reporting "no matches" would send the caller narrowing a search that
/// could never have worked.
#[test]
fn a_pattern_no_file_can_use_is_reported_as_a_pattern_error() {
    let temp = tree(&[("a.py", "def alpha():\n    pass\n")]);
    let targets = targets_for(None, temp.path()).expect("targets");

    let error = search(
        "fn $NAME() { $$$BODY }",
        &targets,
        temp.path(),
        &SearchOptions::default(),
    )
    .expect_err("a Rust pattern cannot be used on a Python-only tree");

    assert!(error.message().contains("metavariable"), "{}", error.message());
}

/// But a mixed tree is fine: the files that cannot use the pattern are counted
/// and the ones that can still return matches.
#[test]
fn files_the_pattern_cannot_be_used_on_are_counted_not_fatal() {
    let temp = tree(&[
        ("a.rs", "fn alpha() { one(); }\n"),
        ("b.py", "def beta():\n    pass\n"),
    ]);

    let found = run(&temp, "fn $NAME() { $$$BODY }");
    assert_eq!(found.total_files, 1, "the Rust file matched");
    assert_eq!(
        found.incompatible_files, 1,
        "the Python file could not use the pattern, and that is expected"
    );
}

// --- adversarial recheck ---

/// A symlink loop must not hang the search or blow the stack. A structural
/// search walks a tree and repos do contain self-referential links.
#[test]
fn a_symlink_loop_does_not_hang_the_search() {
    let temp = tree(&[("a.rs", "fn a() { one(); }\n")]);
    let link = temp.path().join("loop");
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.path(), &link).expect("symlink");
    #[cfg(not(unix))]
    return;

    let found = run(&temp, "one()");
    assert!(found.total_files >= 1, "the real file was still found");
}

/// A file past the walker's size ceiling is skipped rather than parsed. The
/// ceiling belongs to jcode-search, so this pins that the ast search inherits
/// it rather than walking its own way and parsing multi-megabyte files.
#[test]
fn a_file_over_the_size_ceiling_is_not_parsed() {
    let temp = tempfile::TempDir::new().expect("temp");
    let padding = "// pad\n".repeat(jcode_search::MAX_FILE_BYTES as usize / 7 + 1);
    std::fs::write(
        temp.path().join("big.rs"),
        format!("fn a() {{ one(); }}\n{padding}"),
    )
    .expect("write");
    std::fs::write(temp.path().join("small.rs"), "fn b() { one(); }\n").expect("write");
    let targets = targets_for(None, temp.path()).expect("targets");

    let found = search("one()", &targets, temp.path(), &SearchOptions::default())
        .expect("search should skip the big file, not fail");

    assert_eq!(
        found.files.len(),
        1,
        "expected only the small file, got {:?}",
        found.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert_eq!(found.files[0].path, "small.rs");
}

/// Deeply nested source must not blow the parser stack. Generated code and
/// minified files routinely nest far deeper than hand-written code.
#[test]
fn deeply_nested_source_does_not_blow_the_stack() {
    let depth = 2_000;
    let body = format!("{}one(){}", "f(".repeat(depth), ")".repeat(depth));
    let temp = tree(&[("a.rs", &format!("fn a() {{ {body}; }}\n"))]);

    let found = run(&temp, "one()");
    assert_eq!(found.total_files, 1);
}

/// A file whose bytes are not UTF-8 is skipped, not a failure. A binary with a
/// source extension is rare but not impossible, and one bad file must not lose
/// every other file's results.
#[test]
fn a_non_utf8_file_is_skipped_rather_than_failing_the_search() {
    let temp = tree(&[("good.rs", "fn a() { one(); }\n")]);
    std::fs::write(temp.path().join("bad.rs"), [0xff, 0xfe, 0x00, 0x01]).expect("write");

    let found = run(&temp, "one()");
    assert_eq!(found.total_files, 1, "the readable file was still returned");
}
