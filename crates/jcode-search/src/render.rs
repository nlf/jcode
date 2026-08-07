//! Rendering search results for the model.
//!
//! Ported from oh-my-pi's `grouped-file-output.ts` and `match-line-format.ts`.
//!
//! The format is a prefix-folded directory tree, one `#` per level, with match
//! lines under each file. Folding the shared prefix into one header is what
//! keeps a search across a deep tree from spending most of its output budget
//! repeating `packages/coding-agent/src/` on every line.

use crate::select::Selection;

/// Format one match or context line.
///
/// Matched lines carry `*`, context lines a space, so line numbers stay in
/// column. In hashline mode the body is `LINE:content`, which is the editable
/// shape `edit` accepts, so a search result can be patched without a re-read.
/// Plain mode uses `LINE|content`, which is display-only.
///
/// Line numbers are never padded: padding would change the byte offsets a
/// hashline anchor refers to.
pub fn format_match_line(line: usize, text: &str, is_match: bool, hashline: bool) -> String {
    let marker = if is_match { '*' } else { ' ' };
    let separator = if hashline { ':' } else { '|' };
    format!("{marker}{line}{separator}{text}")
}

/// The directory prefix shared by every path, as a path prefix.
///
/// Returns an empty string when the paths share nothing, or when only one
/// component is shared and it buys nothing. Compares whole components rather
/// than characters, so `src/foo` and `src/foobar` share `src/` and not
/// `src/foo`.
pub fn common_prefix(paths: &[String]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    if paths.len() == 1 {
        // One file has no shared prefix to fold: everything before the file
        // name is its directory, and folding it would leave a bare name.
        return match first.rfind('/') {
            Some(index) => first[..=index].to_string(),
            None => String::new(),
        };
    }

    let split = |path: &String| -> Vec<String> {
        path.split('/').map(str::to_string).collect::<Vec<_>>()
    };
    let mut shared = split(first);

    for path in paths.iter().skip(1) {
        let mut parts = split(path);
        // Drop the file name: only directories can be a shared prefix. Popping
        // `shared` too would be redundant, since `zip` below stops at the
        // shorter list, so the first path's file name can never be matched
        // against anything and is truncated away on the first iteration.
        // Mutation testing found the extra pop was dead code.
        parts.pop();
        shared.truncate(
            shared
                .iter()
                .zip(parts.iter())
                .take_while(|(a, b)| a == b)
                .count(),
        );
        if shared.is_empty() {
            return String::new();
        }
    }

    if shared.is_empty() {
        String::new()
    } else {
        format!("{}/", shared.join("/"))
    }
}

/// Render a selection as grouped, prefix-folded output.
///
/// `tags` supplies a hashline tag per path when the caller recorded snapshots;
/// a file with a tag gets a `path#TAG` header and `LINE:content` bodies, so the
/// model can patch what the search showed it without reading the file again.
pub fn render(selection: &Selection, tags: &dyn Fn(&str) -> Option<String>) -> String {
    if selection.files.is_empty() {
        return "No matches found.".to_string();
    }

    let paths: Vec<String> = selection
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let prefix = common_prefix(&paths);

    let mut out = String::new();
    if !prefix.is_empty() {
        out.push_str(&format!("# {prefix}\n"));
    }

    for file in &selection.files {
        let shown = file
            .path
            .strip_prefix(prefix.as_str())
            .unwrap_or(&file.path);
        let tag = tags(&file.path);
        let header = match &tag {
            Some(tag) => format!("{shown}#{tag}"),
            None => shown.to_string(),
        };
        // One more `#` than the folded prefix header, so the nesting is visible
        // even when the prefix is empty.
        let depth = if prefix.is_empty() { "#" } else { "##" };
        out.push_str(&format!("{depth} {header}"));

        // The true count, not the shown count: a caller who sees 20 of 400
        // matches needs to know to narrow rather than assume they saw it all.
        if file.total > file.matches.len() {
            out.push_str(&format!(
                " ({} of {} matches)",
                file.matches.len(),
                file.total
            ));
        }
        out.push('\n');

        let mut previous: Option<usize> = None;
        for item in &file.matches {
            // A gap marker, so consecutive line numbers are not misread as a
            // contiguous block of the file.
            if let Some(previous) = previous
                && item.line > previous + 1
            {
                out.push_str("...\n");
            }
            out.push_str(&format_match_line(
                item.line,
                &item.text,
                true,
                tag.is_some(),
            ));
            out.push('\n');
            previous = Some(item.line);
        }
    }

    // pagination_message() already returns empty when nothing was withheld, so
    // the emptiness is checked once, here, rather than duplicated as a second
    // condition that can drift from it.
    let pagination =
        crate::select::pagination_message(selection, selection.next_skip - selection.files.len());
    if !pagination.is_empty() {
        out.push_str(&format!("\n{pagination}\n"));
    }

    out
}

/// Render a file list, for `glob`.
pub fn render_paths(paths: &[String], total: usize) -> String {
    if paths.is_empty() {
        return "No files found.".to_string();
    }

    let prefix = common_prefix(paths);
    let mut out = String::new();
    if !prefix.is_empty() {
        out.push_str(&format!("# {prefix}\n"));
    }
    for path in paths {
        let shown = path.strip_prefix(prefix.as_str()).unwrap_or(path);
        out.push_str(shown);
        out.push('\n');
    }
    if total > paths.len() {
        out.push_str(&format!(
            "\nShowing {} of {} files. Narrow the pattern to see others.\n",
            paths.len(),
            total
        ));
    }
    out
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
