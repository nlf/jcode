//! Result-normalisation tests.
//!
//! The five legal answer shapes for one navigation request are the point of this module, so most of
//! these are about a server answering in a form a naive caller would read as "nothing found".

use super::*;
use serde_json::json;

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from("/project")
}

fn location(uri: &str, line: i64, character: i64) -> Value {
    json!({
        "uri": uri,
        "range": {
            "start": {"line": line, "character": character},
            "end": {"line": line, "character": character + 4}
        }
    })
}

/// **All five legal shapes for a definition answer produce the same thing.**
///
/// `null`, a single `Location`, an array of them, a `LocationLink`, an array of links. A caller
/// handling only the array case works against one server and reports "no definition found" against
/// another — which is worse than an error, because it looks like an answer.
#[test]
fn every_legal_answer_shape_is_normalised() {
    // Nothing found.
    assert!(Locations::from_result(&Value::Null).is_empty());

    // A single Location, not wrapped in an array.
    let single = Locations::from_result(&location("file:///project/a.rs", 10, 4));
    assert_eq!(single.len(), 1);
    assert_eq!(single.0[0].line, 10);

    // An array of them.
    let many = Locations::from_result(&json!([
        location("file:///project/a.rs", 1, 0),
        location("file:///project/b.rs", 2, 0),
    ]));
    assert_eq!(many.len(), 2);

    // A single LocationLink.
    let link = Locations::from_result(&json!({
        "targetUri": "file:///project/c.rs",
        "targetRange": {"start": {"line": 100, "character": 0}, "end": {"line": 200, "character": 0}},
        "targetSelectionRange": {"start": {"line": 100, "character": 3}, "end": {"line": 100, "character": 9}}
    }));
    assert_eq!(link.len(), 1);
    assert_eq!(link.0[0].uri, "file:///project/c.rs");

    // An array of links.
    let links = Locations::from_result(&json!([
        {
            "targetUri": "file:///project/d.rs",
            "targetRange": {"start": {"line": 1, "character": 0}, "end": {"line": 9, "character": 0}}
        },
        {
            "targetUri": "file:///project/e.rs",
            "targetRange": {"start": {"line": 2, "character": 0}, "end": {"line": 8, "character": 0}}
        },
    ]));
    assert_eq!(links.len(), 2);
}

/// **A `LocationLink` resolves to the name, not the whole symbol.**
///
/// `targetRange` covers the entire declaration — for a 200-line function, all 200 lines.
/// `targetSelectionRange` is just the identifier. Preferring the wrong one means go-to-definition
/// lands on the opening brace of a long function instead of on its name, which looks like the
/// feature working badly rather than a client bug.
#[test]
fn a_location_link_prefers_the_selection_range() {
    let link = json!({
        "targetUri": "file:///project/a.rs",
        "targetRange": {"start": {"line": 100, "character": 0}, "end": {"line": 300, "character": 1}},
        "targetSelectionRange": {"start": {"line": 100, "character": 7}, "end": {"line": 100, "character": 15}}
    });
    let resolved = Locations::from_result(&link);
    assert_eq!(
        resolved.0[0].character, 7,
        "landed on the range, not the name"
    );

    // The selection range is optional, so the target range is the fallback rather than an error.
    let without = json!({
        "targetUri": "file:///project/a.rs",
        "targetRange": {"start": {"line": 42, "character": 2}, "end": {"line": 90, "character": 1}}
    });
    let resolved = Locations::from_result(&without);
    assert_eq!(resolved.0[0].line, 42);
    assert_eq!(resolved.0[0].character, 2);
}

/// An unrecognised entry is skipped, not fatal.
///
/// A server sending one odd element among five good ones should produce four answers. omp's
/// `flatMap` returns `[]` for anything it does not recognise, which is the same decision.
#[test]
fn an_unrecognised_entry_does_not_lose_the_others() {
    let mixed = Locations::from_result(&json!([
        location("file:///project/a.rs", 1, 0),
        {"something": "unexpected"},
        42,
        location("file:///project/b.rs", 2, 0),
    ]));
    assert_eq!(mixed.len(), 2, "a stray element discarded the good ones");
}

/// A `Location` wins over a `LocationLink` when both keys are present.
///
/// Order of checks, matching omp. Not a shape any sane server sends, but the two branches are
/// mutually exclusive only by convention and a test is cheaper than assuming.
#[test]
fn a_location_key_takes_precedence_over_a_target_key() {
    let both = json!({
        "uri": "file:///project/plain.rs",
        "range": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 1}},
        "targetUri": "file:///project/link.rs",
        "targetRange": {"start": {"line": 99, "character": 0}, "end": {"line": 99, "character": 1}}
    });
    assert_eq!(
        Locations::from_result(&both).0[0].uri,
        "file:///project/plain.rs"
    );
}

/// Positions are displayed 1-based and relative to the project.
#[test]
fn a_location_renders_relative_and_one_based() {
    let location = Location {
        uri: "file:///project/src/main.rs".to_string(),
        line: 11,
        character: 4,
    };
    assert_eq!(format_location(&location, &root()), "src/main.rs:12:5");
}

/// A path outside the project keeps its absolute form.
///
/// Jumping into a dependency in the registry is normal, and a relative path from the project root
/// would be a long chain of `..` that tells the reader nothing.
#[test]
fn a_location_outside_the_project_stays_absolute() {
    let location = Location {
        uri: "file:///elsewhere/dep/lib.rs".to_string(),
        line: 0,
        character: 0,
    };
    assert_eq!(
        format_location(&location, &root()),
        "/elsewhere/dep/lib.rs:1:1"
    );
}

/// **A percent-encoded URI is decoded before display.**
///
/// We encode when sending, so a server echoing our URI back returns it encoded. Without decoding,
/// a project containing a space shows `my%20project/src/main.rs` on every result line.
#[test]
fn an_encoded_uri_is_decoded_for_display() {
    let location = Location {
        uri: "file:///project/my%20dir/a%23b.rs".to_string(),
        line: 0,
        character: 0,
    };
    assert_eq!(format_location(&location, &root()), "my dir/a#b.rs:1:1");
}

/// A malformed escape is left alone rather than failing.
///
/// A stray `%` is a server bug, and a path that keeps it still tells the reader which file was
/// meant, where an error tells them nothing.
#[test]
fn a_malformed_escape_survives_decoding() {
    assert_eq!(
        uri_to_path("file:///project/100%_done.rs"),
        std::path::PathBuf::from("/project/100%_done.rs")
    );
    // A truncated escape at the very end.
    assert_eq!(
        uri_to_path("file:///project/a%2"),
        std::path::PathBuf::from("/project/a%2")
    );
}

/// A lax server sending a bare path is tolerated.
#[test]
fn a_bare_path_is_accepted_as_a_location() {
    assert_eq!(
        uri_to_path("/project/a.rs"),
        std::path::PathBuf::from("/project/a.rs")
    );
}

/// Nothing found renders as `None`, so the caller can say so explicitly.
///
/// omp prints "No definition found" rather than an empty list. A model shown an empty result cannot
/// tell it from a failure, and will often retry the same call.
#[test]
fn an_empty_result_renders_as_nothing_rather_than_an_empty_list() {
    assert_eq!(
        render_locations(&Locations::default(), "definition", &root()),
        None
    );

    let found = Locations(vec![Location {
        uri: "file:///project/a.rs".to_string(),
        line: 0,
        character: 0,
    }]);
    assert_eq!(
        render_locations(&found, "definition", &root()).expect("some"),
        "Found 1 definition(s):\n  a.rs:1:1"
    );
}

/// **All three hover shapes flatten, including the deprecated array.**
///
/// `contents` may be a string, a `MarkupContent`, or an array of either. The array form is
/// deprecated and still emitted, notably by older `gopls`, so handling only the object form reports
/// "no hover information" for a server that answered.
#[test]
fn every_hover_shape_is_flattened() {
    assert_eq!(
        hover_text(&json!({"contents": "fn main()"})).expect("string form"),
        "fn main()"
    );

    assert_eq!(
        hover_text(&json!({"contents": {"kind": "markdown", "value": "`fn main()`"}}))
            .expect("markup form"),
        "`fn main()`"
    );

    // The deprecated `MarkedString` object, which carries its text in the same field.
    assert_eq!(
        hover_text(&json!({"contents": {"language": "rust", "value": "fn main()"}}))
            .expect("marked string"),
        "fn main()"
    );

    assert_eq!(
        hover_text(&json!({"contents": [
            {"language": "rust", "value": "fn main()"},
            "The entry point."
        ]}))
        .expect("array form"),
        "fn main()\nThe entry point."
    );
}

/// An empty or whitespace-only hover is `None`, not an empty string.
///
/// A server that answers with `{"contents": ""}` has said nothing, and reporting an empty hover as
/// success wastes a turn: the caller shows blank output and the model tries again.
#[test]
fn an_empty_hover_is_reported_as_absent() {
    assert_eq!(hover_text(&json!({"contents": ""})), None);
    assert_eq!(hover_text(&json!({"contents": "   \n  "})), None);
    assert_eq!(hover_text(&json!({"contents": []})), None);
    assert_eq!(hover_text(&json!({})), None);
    assert_eq!(hover_text(&Value::Null), None);
}

/// Empty parts of a hover array are dropped rather than producing blank lines.
#[test]
fn blank_hover_parts_are_dropped() {
    assert_eq!(
        hover_text(&json!({"contents": ["first", "", "  ", "second"]})).expect("some"),
        "first\nsecond"
    );
}

/// **One place reported twice under two path spellings is listed once.**
///
/// Found by running against real clangd, not by a test I wrote. On macOS `/tmp` is a symlink to
/// `/private/tmp`, so a project rooted at `/tmp/x` gets answers under both spellings — and a
/// `references` query about a symbol used exactly once printed "Found 2 reference(s)".
///
/// The count is what makes it more than cosmetic: a reader told there are two call sites goes
/// looking for the second one. Deduplicating on the rendered line means the count and the list are
/// derived from the same thing and cannot disagree.
#[test]
fn one_place_under_two_spellings_is_listed_once() {
    let locations = Locations(vec![
        Location {
            uri: "file:///project/a.rs".to_string(),
            line: 5,
            character: 17,
        },
        // The same position, spelled with a redundant segment. `equivalent_uris` handles this one;
        // the symlink case cannot be reproduced portably, and both arrive here as the same rendered
        // line, which is the property being tested.
        Location {
            uri: "file:///project/./a.rs".to_string(),
            line: 5,
            character: 17,
        },
    ]);

    let rendered = render_locations(&locations, "reference", &root()).expect("some");
    assert_eq!(
        rendered, "Found 1 reference(s):\n  a.rs:6:18",
        "the same place was listed twice, and the count followed it"
    );
}

/// Genuinely distinct places are still listed separately.
///
/// The dedupe must not collapse two call sites on the same line at different columns, which is
/// ordinary in `foo(foo(x))`.
#[test]
fn distinct_places_are_all_listed() {
    let locations = Locations(vec![
        Location {
            uri: "file:///project/a.rs".to_string(),
            line: 5,
            character: 4,
        },
        Location {
            uri: "file:///project/a.rs".to_string(),
            line: 5,
            character: 12,
        },
        Location {
            uri: "file:///project/b.rs".to_string(),
            line: 5,
            character: 4,
        },
    ]);
    let rendered = render_locations(&locations, "reference", &root()).expect("some");
    assert!(rendered.starts_with("Found 3 reference(s):"), "{rendered}");
}

/// **A symlinked project root still yields relative paths.**
///
/// Also found against real clangd. A server answers with *its* idea of the path, which may differ
/// from ours by a symlink — so a file inside the project rendered as a full absolute path on every
/// line, which is mostly noise and obscures the part that matters.
///
/// Only the *root* is canonicalized, and only for display. Resolving it when building URIs would
/// make a server report diagnostics against paths the caller never mentioned.
#[test]
#[cfg(unix)]
fn a_symlinked_root_still_yields_relative_paths() {
    let real = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(real.path().join("src")).expect("mkdir");
    std::fs::write(real.path().join("src/main.rs"), "fn main() {}\n").expect("write");

    // A symlink pointing at the real directory, used as the root the caller knows.
    let link_parent = tempfile::tempdir().expect("tempdir");
    let link = link_parent.path().join("project-link");
    std::os::unix::fs::symlink(real.path(), &link).expect("symlink");

    // The server answers with the resolved path, as clangd does.
    let answered = Location {
        uri: format!("file://{}/src/main.rs", real.path().display()),
        line: 0,
        character: 0,
    };

    let shown = format_location(&answered, &link);
    assert_eq!(
        shown, "src/main.rs:1:1",
        "a file inside the project rendered as an absolute path because the root is a symlink"
    );
}
