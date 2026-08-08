//! Behaviour spec for structural matching.
//!
//! The point of these is the distinction from text search: a pattern matches
//! by *shape*, so it finds code a grep would miss and skips text a grep would
//! wrongly hit.

use super::*;

fn rust() -> SupportLang {
    resolve_language("rust").expect("rust is supported")
}

fn find_all(source: &str, pattern: &str) -> Matches {
    find(source, pattern, rust(), DEFAULT_MAX_MATCHES).expect("should match")
}

#[test]
fn a_pattern_matches_by_structure() {
    let source = "fn alpha() { one(); }\nfn beta() { two(); }\n";
    let found = find_all(source, "fn $NAME() { $$$BODY }");

    assert_eq!(found.matches.len(), 2);
    assert_eq!(found.matches[0].line, 1);
    assert_eq!(found.matches[1].line, 2);
}

/// The whole reason this exists: formatting does not change structure, so a
/// reformatted function still matches. A text search would miss it.
#[test]
fn formatting_does_not_defeat_a_pattern() {
    let source = "fn alpha(\n) {\n    one();\n}\n";
    let found = find_all(source, "fn $NAME() { $$$BODY }");

    assert_eq!(
        found.matches.len(),
        1,
        "a function split across lines is still that function"
    );
}

/// The converse: a pattern does not match text that merely looks similar,
/// which is where a regex produces false hits.
#[test]
fn a_pattern_does_not_match_a_comment_or_a_string() {
    let source = "// fn ghost() { nothing(); }\nlet s = \"fn ghost() { nothing(); }\";\n";
    let found = find_all(source, "fn $NAME() { $$$BODY }");

    assert!(
        found.matches.is_empty(),
        "a comment and a string literal are not functions: {:?}",
        found.matches
    );
}

#[test]
fn metavariables_are_bound_and_reported() {
    let source = "fn alpha() { one(); }\n";
    let found = find_all(source, "fn $NAME() { $$$BODY }");

    let bindings = &found.matches[0].bindings;
    let name = bindings
        .iter()
        .find(|(key, _)| key == "NAME")
        .expect("NAME should be bound");
    assert_eq!(name.1, "alpha");
}

/// Deterministic ordering matters for caching and for diffing one search
/// against another.
#[test]
fn bindings_come_back_sorted() {
    let source = "fn alpha(x: u8) -> u8 { x }\n";
    let found = find_all(source, "fn $NAME($ARG: $TYPE) -> $RET { $$$BODY }");

    let names: Vec<&str> = found.matches[0]
        .bindings
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "bindings should be sorted: {names:?}");
}

#[test]
fn a_match_reports_where_it_starts() {
    let source = "\n\nfn alpha() { one(); }\n";
    let found = find_all(source, "fn $NAME() { $$$BODY }");

    assert_eq!(found.matches[0].line, 3, "1-based line");
    assert_eq!(found.matches[0].column, 1, "1-based column");
}

#[test]
fn a_pattern_with_no_matches_returns_nothing() {
    let found = find_all("fn alpha() { one(); }\n", "struct $NAME { $$$FIELDS }");
    assert!(found.matches.is_empty());
    assert!(!found.truncated);
}

/// A pattern like `$X` matches every node, so an unbounded search costs the
/// whole output budget.
#[test]
fn collection_stops_at_the_cap() {
    let source: String = (0..500).map(|i| format!("fn f{i}() {{ x(); }}\n")).collect();
    let found = find(&source, "fn $NAME() { $$$BODY }", rust(), 10).expect("match");

    assert_eq!(found.matches.len(), 10);
    assert!(found.truncated, "the caller must know to narrow");
}

#[test]
fn a_search_within_the_cap_is_not_marked_truncated() {
    let source = "fn alpha() { one(); }\n";
    let found = find(source, "fn $NAME() { $$$BODY }", rust(), 10).expect("match");
    assert!(!found.truncated);
}

/// An empty pattern matches every node and means nothing.
#[test]
fn an_empty_pattern_is_refused() {
    assert_eq!(
        find("code", "", rust(), 10),
        Err(MatchError::EmptyPattern)
    );
    assert_eq!(
        find("code", "   ", rust(), 10),
        Err(MatchError::EmptyPattern)
    );
}

/// Models send the names they know, not the ones upstream happens to take.
#[test]
fn language_aliases_resolve() {
    for (alias, canonical) in [
        ("rs", "rust"),
        ("py", "python"),
        ("js", "javascript"),
        ("ts", "typescript"),
        ("c++", "cpp"),
        ("c#", "csharp"),
        ("sh", "bash"),
        ("yml", "yaml"),
    ] {
        assert_eq!(
            resolve_language(alias).expect("alias should resolve"),
            resolve_language(canonical).expect("canonical should resolve"),
            "{alias} should resolve like {canonical}"
        );
    }
}

#[test]
fn language_resolution_ignores_case_and_padding() {
    assert_eq!(
        resolve_language("  RUST  ").expect("should resolve"),
        rust()
    );
}

/// The error names what is available, so the caller does not have to guess.
#[test]
fn an_unknown_language_lists_the_supported_ones() {
    let error = resolve_language("cobol").expect_err("not supported");
    let message = error.message();

    assert!(message.contains("cobol"), "{message}");
    assert!(message.contains("rust"), "{message}");
    assert!(message.contains("python"), "{message}");
}

#[test]
fn a_path_implies_its_language() {
    assert_eq!(language_for_path("src/lib.rs"), Some(rust()));
    assert_eq!(
        language_for_path("app.py"),
        Some(resolve_language("python").expect("python"))
    );
    assert_eq!(language_for_path("README"), None);
    assert_eq!(language_for_path("data.bin"), None);
}

/// Every language the list advertises must actually resolve, or the error
/// message sends callers to a language that does not work.
#[test]
fn every_advertised_language_resolves() {
    for language in supported_languages() {
        assert!(
            resolve_language(language).is_ok(),
            "{language} is advertised but does not resolve"
        );
    }
}

/// Structural search across languages, since the point is that it is not
/// Rust-specific.
#[test]
fn patterns_work_in_other_languages() {
    let python = resolve_language("python").expect("python");
    let found = find("def alpha():\n    pass\n", "def $NAME():\n    $$$BODY", python, 10)
        .expect("should match");
    assert_eq!(found.matches.len(), 1);
    assert_eq!(
        found.matches[0]
            .bindings
            .iter()
            .find(|(key, _)| key == "NAME")
            .expect("NAME bound")
            .1,
        "alpha"
    );
}

/// The message has to teach the format, since a model that sent a regex will
/// otherwise send another one.
#[test]
fn a_bad_pattern_error_explains_what_a_pattern_is() {
    let error = MatchError::BadPattern {
        pattern: r"fn \w+".to_string(),
        language: "rust".to_string(),
    };
    let message = error.message();

    assert!(message.contains("metavariable"), "{message}");
    assert!(
        message.contains("not a regular expression"),
        "{message}"
    );
}

/// Upstream PANICS when a pattern is compiled against a language it is not
/// valid for: `find_all(&str)` builds the pattern internally and
/// `pattern.rs:252` unwraps the error.
///
/// A whole-tree search infers the language per file, so a Rust pattern is
/// necessarily compiled against Python somewhere, and that would take the
/// process down. Found by a test that searched a mixed tree.
#[test]
fn a_pattern_invalid_for_the_language_errors_rather_than_panicking() {
    let python = resolve_language("python").expect("python");
    let error = find("def alpha():\n    pass\n", "fn $NAME() { $$$BODY }", python, 10)
        .expect_err("a Rust pattern is not valid Python");

    assert!(
        matches!(error, MatchError::BadPattern { .. }),
        "expected BadPattern, got {error:?}"
    );
}

// --- adversarial recheck ---

/// The match cap must actually bound the work, or a pattern matching every node
/// in a large file returns an unbounded result and costs the whole budget.
#[test]
fn the_match_cap_bounds_what_is_returned() {
    let rust = resolve_language("rust").expect("rust");
    let source = format!("fn a() {{\n{}}}\n", "    one();\n".repeat(500));

    let found = find(&source, "one()", rust, 10).expect("match");
    assert_eq!(found.matches.len(), 10);
    assert!(
        found.truncated,
        "a capped result that does not say so reads as the whole answer"
    );
}

/// Line and column are 1-indexed, which is what every editor and every other
/// tool in this codebase uses. Off-by-one here sends edits to the wrong line.
#[test]
fn positions_are_one_indexed() {
    let rust = resolve_language("rust").expect("rust");
    let found = find("fn a() {\n    one();\n}\n", "one()", rust, 10).expect("match");

    assert_eq!(found.matches[0].line, 2);
    assert_eq!(found.matches[0].column, 5);
}

/// Multibyte text ON THE SAME LINE, before the match. Each of these characters
/// is 3 bytes, so a column counted in bytes reports 21 where a column counted
/// in characters reports 15. Putting the text on an earlier line would not
/// exercise this at all.
#[test]
fn multibyte_text_on_the_same_line_does_not_shift_the_reported_column() {
    let rust = resolve_language("rust").expect("rust");
    // `let s = "日本語";` is 12 characters before `one()` starts... counted
    // precisely below.
    let source = "fn a() {\n    let 日本語 = 1; one();\n}\n";
    let found = find(source, "one()", rust, 10).expect("match");

    let line = source.lines().nth(1).expect("line 2");
    let expected = line.chars().count() - "one();".chars().count() + 1;
    assert_eq!(found.matches[0].line, 2);
    assert_eq!(
        found.matches[0].column, expected,
        "column looks byte-counted rather than character-counted"
    );
}

/// Source that does not parse yields no matches rather than an error or a
/// panic. Half-written code is normal in a live repo.
#[test]
fn source_that_does_not_parse_yields_no_matches() {
    let rust = resolve_language("rust").expect("rust");
    let found = find("fn a( { unclosed", "one()", rust, 10).expect("no error");

    assert!(found.matches.is_empty());
}

/// An empty source is a no-op, not a panic.
#[test]
fn empty_source_yields_no_matches() {
    let rust = resolve_language("rust").expect("rust");
    assert!(find("", "one()", rust, 10).expect("no error").matches.is_empty());
}
