//! Behaviour spec for write-target guards.
//!
//! Both guards come from real incidents in oh-my-pi: a scout emitting a
//! multi-file read expression as a write target (their issue #6809), and the
//! single-selector case their `readSelectorForEmptyWrite` exists for.

use super::*;

#[test]
fn a_selector_shaped_target_with_no_content_is_refused() {
    let misfire = check("notes.md:50-100", "", false).expect("should be refused");

    assert_eq!(
        misfire,
        Misfire::Selector {
            target: "notes.md:50-100".to_string(),
            selector: "50-100".to_string(),
        }
    );
}

/// The refusal has to name the call the model meant, or it retries the same
/// mistake.
#[test]
fn the_refusal_points_at_read() {
    let message = check("notes.md:50-100", "", false)
        .expect("refused")
        .message();

    assert!(message.contains("use read"), "{message}");
    assert!(message.contains("notes.md:50-100"), "{message}");
    assert!(
        message.contains("non-empty write is never blocked"),
        "the escape hatch must be stated: {message}"
    );
}

/// A model that sent contents meant to write a file, whatever it called it.
#[test]
fn content_makes_a_selector_shaped_target_legitimate() {
    assert_eq!(check("notes.md:50-100", "real content", false), None);
}

/// A file that genuinely has that name stays writable.
#[test]
fn an_existing_file_is_never_blocked() {
    assert_eq!(check("notes.md:50-100", "", true), None);
    assert_eq!(check("a:1-2;b:3-4", "", true), None);
}

#[test]
fn an_ordinary_path_is_not_refused() {
    for target in ["notes.md", "src/lib.rs", "a-b_c.txt", "deep/nested/file.rs"] {
        assert_eq!(check(target, "", false), None, "{target:?}");
    }
}

/// omp's #6809: a scout emitted a multi-file read expression as one write
/// target. Honouring it creates a nested directory tree in the workspace.
#[test]
fn a_semicolon_joined_selector_list_is_refused() {
    let misfire = check("a.txt:1-2;b/c.txt:3-4", "", false).expect("should be refused");

    assert_eq!(
        misfire,
        Misfire::SelectorList {
            target: "a.txt:1-2;b/c.txt:3-4".to_string(),
            count: 2,
        }
    );
}

/// The list guard fires even with content, because the non-empty escape exists
/// for a lone selector-shaped filename and never for a list: honouring it there
/// silently creates `a.txt:1-2;b/` as a directory.
#[test]
fn a_selector_list_is_refused_even_with_content() {
    assert!(check("a.txt:1-2;b/c.txt:3-4", "some content", false).is_some());
}

#[test]
fn the_list_refusal_says_to_issue_one_read_per_path() {
    let message = check("a.txt:1-2;b.txt:3-4", "", false)
        .expect("refused")
        .message();

    assert!(message.contains("one read per path"), "{message}");
    assert!(message.contains("2 read selectors"), "{message}");
}

/// A semicolon in a path is legal. Only a list where EVERY segment carries a
/// selector is a misdispatched read.
#[test]
fn a_path_containing_a_semicolon_is_not_a_list() {
    assert_eq!(check("weird;name.txt", "", false), None);
    assert_eq!(check("a.txt:1-2;plain.txt", "", false), None);
}

/// A target that is not a list can still end in a selector, and then the
/// single-selector guard is the one that applies. Not a list is not the same
/// as not a misfire.
#[test]
fn a_non_list_ending_in_a_selector_falls_to_the_single_guard() {
    assert!(matches!(
        check("plain.txt;b.txt:3-4", "", false),
        Some(Misfire::Selector { .. })
    ));
    assert!(matches!(
        check(";a.txt:1-2", "", false),
        Some(Misfire::Selector { .. })
    ));
}

#[test]
fn an_empty_segment_disqualifies_the_list() {
    // Trailing `;`: the last segment is empty, so this is not a list. It also
    // does not end in a selector, so nothing fires.
    assert_eq!(check("a.txt:1-2;", "", false), None);
}

/// A display-mode selector is still a read selector.
#[test]
fn a_raw_selector_is_also_caught() {
    assert!(check("notes.md:raw", "", false).is_some());
}

/// A Windows drive path is not a selector, or writes on Windows break.
#[test]
fn a_windows_drive_path_is_not_a_misfire() {
    assert_eq!(check("C:/src/main.rs", "", false), None);
}

/// Three or more segments still count, and the message reports the real number.
#[test]
fn a_longer_list_reports_its_length() {
    let misfire = check("a:1-2;b:3-4;c:5-6", "", false).expect("refused");
    assert_eq!(
        misfire,
        Misfire::SelectorList {
            target: "a:1-2;b:3-4;c:5-6".to_string(),
            count: 3,
        }
    );
}

/// A lone selector-shaped target is a Selector misfire, not a one-element
/// list. The distinction matters because the messages differ: one says "use
/// read", the other says "issue one read per path", which is nonsense advice
/// for a single path.
///
/// Found by mutation testing: removing the length check left the earlier tests
/// passing, because they only asserted that *something* was refused.
#[test]
fn a_single_selector_is_not_reported_as_a_list() {
    let misfire = check("notes.md:50-100", "", false).expect("refused");

    assert!(
        matches!(misfire, Misfire::Selector { .. }),
        "expected a Selector misfire, got {misfire:?}"
    );
    assert!(
        !misfire.message().contains("one read per path"),
        "list advice is wrong for a single path: {}",
        misfire.message()
    );
}
