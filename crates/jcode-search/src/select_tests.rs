//! Behaviour spec for result selection.
//!
//! The values and rules come from oh-my-pi's `src/tools/grep.ts`. What is
//! tested here is the part that decides what a caller actually sees, which is
//! where a search tool is useful or useless.

use super::*;

fn hit(path: &str, line: usize) -> Match {
    Match {
        path: path.to_string(),
        line,
        text: format!("line {line}"),
    }
}

fn file(path: &str, lines: &[usize]) -> FileMatches {
    let matches: Vec<Match> = lines.iter().map(|line| hit(path, *line)).collect();
    FileMatches {
        path: path.to_string(),
        total: matches.len(),
        matches,
    }
}

#[test]
fn matches_group_by_file_in_first_seen_order() {
    let grouped = group_by_file(vec![hit("b.rs", 1), hit("a.rs", 2), hit("b.rs", 3)]);

    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].path, "b.rs", "first seen file comes first");
    assert_eq!(grouped[0].matches.len(), 2);
    assert_eq!(grouped[1].path, "a.rs");
}

/// First-seen order is what makes `skip` paginate correctly. If grouping
/// reordered files, page two would repeat or drop files from page one.
#[test]
fn grouping_is_stable_so_pagination_does_not_repeat_files() {
    let matches = vec![hit("c.rs", 1), hit("a.rs", 1), hit("b.rs", 1)];
    let first = group_by_file(matches.clone());
    let second = group_by_file(matches);

    let order = |files: &[FileMatches]| -> Vec<String> {
        files.iter().map(|file| file.path.clone()).collect()
    };
    assert_eq!(order(&first), order(&second));
}

#[test]
fn a_multi_file_search_caps_matches_per_file() {
    let lines: Vec<usize> = (1..=50).collect();
    let selection = select(vec![file("a.rs", &lines), file("b.rs", &lines)], 0, 20, false);

    assert_eq!(selection.files[0].matches.len(), MULTI_FILE_PER_FILE_MATCHES);
    assert_eq!(
        selection.files[0].total, 50,
        "the true count survives the cap so the caller can be told"
    );
}

/// A caller who named one file has no diversity to protect, so the cap is
/// higher. Applying the multi-file cap here would hide 180 of 200 matches in
/// exactly the case where the caller wants them all.
#[test]
fn a_single_file_search_gets_the_higher_cap() {
    let lines: Vec<usize> = (1..=300).collect();
    let selection = select(vec![file("a.rs", &lines)], 0, 20, true);

    assert_eq!(selection.files[0].matches.len(), SINGLE_FILE_MATCHES);
}

#[test]
fn the_file_window_limits_how_many_files_come_back() {
    let files: Vec<FileMatches> = (0..30).map(|i| file(&format!("f{i}.rs"), &[1])).collect();
    let selection = select(files, 0, DEFAULT_FILE_LIMIT, false);

    assert_eq!(selection.files.len(), DEFAULT_FILE_LIMIT);
    assert_eq!(selection.total_files, 30, "the true total is still reported");
    assert!(selection.file_limit_reached);
    assert_eq!(selection.next_skip, DEFAULT_FILE_LIMIT);
}

/// Skipping must resume exactly where the previous page stopped. An off-by-one
/// here silently hides a file on every page boundary.
#[test]
fn skip_resumes_where_the_previous_page_ended() {
    let files: Vec<FileMatches> = (0..30).map(|i| file(&format!("f{i}.rs"), &[1])).collect();
    let first = select(files.clone(), 0, 10, false);
    let second = select(files, first.next_skip, 10, false);

    assert_eq!(first.files.last().unwrap().path, "f9.rs");
    assert_eq!(second.files.first().unwrap().path, "f10.rs");
}

/// A full window is not the same as a truncated one. Reporting the limit when
/// the results happen to fill it exactly offers a next page that returns
/// nothing.
#[test]
fn a_window_that_exactly_fits_is_not_reported_as_truncated() {
    let files: Vec<FileMatches> = (0..10).map(|i| file(&format!("f{i}.rs"), &[1])).collect();
    let selection = select(files, 0, 10, false);

    assert!(
        !selection.file_limit_reached,
        "exactly filling the window is not truncation"
    );
    assert_eq!(pagination_message(&selection, 0), "");
}

#[test]
fn the_pagination_message_names_the_next_skip() {
    let files: Vec<FileMatches> = (0..30).map(|i| file(&format!("f{i}.rs"), &[1])).collect();
    let selection = select(files, 0, 20, false);
    let message = pagination_message(&selection, 0);

    assert!(message.contains("files 1-20 of 30"), "{message}");
    assert!(message.contains("skip=20"), "{message}");
}

/// Round-robin is the whole point: one match from each file before a second
/// from any. Concatenating instead would return 20 hits from the first file
/// and none from the rest.
#[test]
fn interleaving_takes_one_match_per_file_in_rotation() {
    let files = vec![file("a.rs", &[1, 2, 3]), file("b.rs", &[10, 20])];
    let selected = interleave(&files, None);

    let order: Vec<(&str, usize)> = selected
        .iter()
        .map(|item| (item.path.as_str(), item.line))
        .collect();
    assert_eq!(
        order,
        vec![("a.rs", 1), ("b.rs", 10), ("a.rs", 2), ("b.rs", 20), ("a.rs", 3)]
    );
}

/// The cap applies after the rotation. Trimming mid-rotation would favour
/// whichever files sort first, which is the bias the rotation removes.
#[test]
fn the_cap_applies_after_rotation_so_every_file_is_represented() {
    let many: Vec<usize> = (1..=100).collect();
    let files = vec![file("hot.rs", &many), file("cold.rs", &[7])];
    let selected = interleave(&files, Some(2));

    assert_eq!(selected.len(), 2);
    assert!(
        selected.iter().any(|item| item.path == "cold.rs"),
        "the file with one match must survive a tight cap: {selected:?}"
    );
}

#[test]
fn a_selector_filters_matches_to_its_ranges() {
    let matches = vec![hit("a.rs", 10), hit("a.rs", 60), hit("a.rs", 120)];
    let ranges = vec![LineRange {
        start: 50,
        end: Some(100),
    }];

    let filtered = filter_to_ranges(matches, &ranges);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].line, 60);
}

/// No selector means no filtering, not "filter to nothing".
#[test]
fn no_ranges_leaves_matches_untouched() {
    let matches = vec![hit("a.rs", 1), hit("a.rs", 2)];
    assert_eq!(filter_to_ranges(matches.clone(), &[]), matches);
}

#[test]
fn an_open_ended_range_keeps_everything_after_its_start() {
    let matches = vec![hit("a.rs", 1), hit("a.rs", 500)];
    let ranges = vec![LineRange {
        start: 100,
        end: None,
    }];

    let filtered = filter_to_ranges(matches, &ranges);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].line, 500);
}

/// omp's constants. Pinned because they are a considered tradeoff between
/// output size and usefulness, and drifting from them silently changes what
/// every search returns.
#[test]
fn the_limits_match_omps() {
    assert_eq!(DEFAULT_FILE_LIMIT, 20);
    assert_eq!(MULTI_FILE_PER_FILE_MATCHES, 20);
    assert_eq!(SINGLE_FILE_MATCHES, 200);
    assert_eq!(INTERNAL_TOTAL_CAP, 2000);
    assert!(
        INTERNAL_TOTAL_CAP >= DEFAULT_FILE_LIMIT * MULTI_FILE_PER_FILE_MATCHES,
        "the internal cap must cover a full window or the window cannot be filled"
    );
}
