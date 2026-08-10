#![cfg_attr(test, allow(clippy::items_after_test_module))]

use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use jcode_terminal_image::{ImageDisplayParams, ImageProtocol, display_image};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const DEFAULT_LIMIT: usize = 5000;
const MAX_LINE_LEN: usize = 2000;

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ReadInput {
    file_path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    /// PDF page selection, e.g. "3", "2-5", or "1,4,9-11".
    ///
    /// Advertised on the OAuth path but previously absent here, so serde
    /// silently dropped it and a page request returned the whole document.
    #[serde(default)]
    pages: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadRangeStyle {
    OffsetLimit,
    StartEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalizedReadRange {
    offset: usize,
    limit: usize,
    style: ReadRangeStyle,
}

impl NormalizedReadRange {}

/// Parse a `pages` selection such as "3", "2-5", or "1,4,9-11" into 1-based
/// page numbers.
///
/// Returns an error rather than silently reading the whole document: a
/// selection that is quietly ignored is worse than one that is refused,
/// because the model cannot tell it did not get what it asked for.
fn parse_page_selection(spec: &str) -> Result<Vec<usize>> {
    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((start, end)) => {
                let start: usize = start.trim().parse().map_err(|_| {
                    anyhow::anyhow!("invalid page range '{part}': expected numbers like 2-5")
                })?;
                let end: usize = end.trim().parse().map_err(|_| {
                    anyhow::anyhow!("invalid page range '{part}': expected numbers like 2-5")
                })?;
                if start == 0 || end == 0 {
                    return Err(anyhow::anyhow!("page numbers are 1-based, got '{part}'"));
                }
                if end < start {
                    return Err(anyhow::anyhow!("page range '{part}' ends before it starts"));
                }
                pages.extend(start..=end);
            }
            None => {
                let page: usize = part.trim().parse().map_err(|_| {
                    anyhow::anyhow!("invalid page '{part}': expected a number like 3 or 2-5")
                })?;
                if page == 0 {
                    return Err(anyhow::anyhow!("page numbers are 1-based, got '0'"));
                }
                pages.push(page);
            }
        }
    }
    if pages.is_empty() {
        return Err(anyhow::anyhow!(
            "'pages' was empty; use a selection like 3, 2-5, or 1,4,9-11"
        ));
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

fn normalize_read_range(params: &ReadInput) -> Result<NormalizedReadRange> {
    let has_start_end = params.start_line.is_some() || params.end_line.is_some();
    let has_mixed_offset = match (params.start_line, params.end_line, params.offset) {
        (Some(start_line), _, Some(offset)) => {
            if start_line == 0 {
                true
            } else {
                offset.checked_add(1) != Some(start_line)
            }
        }
        (None, Some(_), Some(offset)) => offset != 0,
        _ => params.offset.is_some(),
    };

    if has_start_end && has_mixed_offset {
        return Err(anyhow::anyhow!(
            "Use either start_line/end_line (1-based) or offset (0-based), not both. `limit` may be used with either style."
        ));
    }

    if has_start_end {
        let start_line = params.start_line.unwrap_or(1);
        if start_line == 0 {
            return Err(anyhow::anyhow!(
                "start_line must be 1 or greater (it is 1-based)."
            ));
        }

        let limit = if let Some(end_line) = params.end_line {
            if end_line == 0 {
                return Err(anyhow::anyhow!(
                    "end_line must be 1 or greater (it is 1-based)."
                ));
            }
            if end_line < start_line {
                return Err(anyhow::anyhow!(
                    "end_line ({}) must be greater than or equal to start_line ({}).",
                    end_line,
                    start_line
                ));
            }
            end_line - start_line + 1
        } else {
            params.limit.unwrap_or(DEFAULT_LIMIT)
        };

        return Ok(NormalizedReadRange {
            offset: start_line - 1,
            limit,
            style: ReadRangeStyle::StartEnd,
        });
    }

    Ok(NormalizedReadRange {
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
        style: ReadRangeStyle::OffsetLimit,
    })
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a text, image, or PDF file with numbered lines. Not `cat` in bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "Path to a file. Scope it like foo.rs:50-100 or :5-16,960-973. Not cat/head/sed in bash."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based line to start from. Alternative spelling of start_line."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based line to start from. Alternative spelling of offset."
                },
                "end_line": {
                    "type": "integer",
                    "description": "1-based last line to read, inclusive. Use with start_line instead of limit."
                },
                "limit": {
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "description": "Max text lines to read. Default 5000."
                },
                "pages": {
                    "type": "string",
                    "description": "For PDFs, which pages to read: \"3\", \"2-5\", or \"1,4,9-11\". Omit for all pages."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let mut params: ReadInput = serde_json::from_value(input)?;

        // An inline selector (`foo.rs:50-100`) is peeled off the path, matching
        // omp, whose read takes one `path` and no range parameters. Ours keeps
        // offset/start_line/end_line/limit because models send them and the
        // curated OAuth Read schema advertises them, so this is an additional
        // spelling rather than a replacement.
        //
        // The peel is skipped when the literal path exists, so a file genuinely
        // named `notes:1-2` is read rather than truncated.
        let mut selector_ranges: Vec<jcode_search::LineRange> = Vec::new();
        if !ctx.resolve_path(Path::new(&params.file_path)).exists() {
            let split = jcode_search::split_path_and_selector(&params.file_path);
            if let Some(selector) = split.selector.as_deref() {
                match jcode_search::selector_line_ranges(Some(selector)) {
                    Ok(Some(ranges)) => {
                        selector_ranges = ranges;
                        params.file_path = split.path;
                    }
                    // A selector-shaped suffix with impossible bounds is a
                    // mistake worth reporting, not a filename.
                    Err(error) => return Err(anyhow::anyhow!(error.message())),
                    Ok(None) => {}
                }
            }
        }

        let range = normalize_read_range(&params)?;

        let path = ctx.resolve_path(Path::new(&params.file_path));

        // Check if file exists
        if !path.exists() {
            return Err(anyhow::anyhow!(file_not_found_message(
                &params.file_path,
                &path,
                ctx.working_dir.as_deref(),
            )));
        }

        // Check for image files and display in terminal if supported
        if is_image_file(&path) {
            return handle_image_file(&path, &params.file_path);
        }

        // Check for PDF files and extract text
        if is_pdf_file(&path) {
            let selection = params
                .pages
                .as_deref()
                .map(parse_page_selection)
                .transpose()?;
            return handle_pdf_file(&path, &params.file_path, selection.as_deref());
        }

        // Check for binary files
        if is_binary_file(&path) {
            return Ok(ToolOutput::new(format!(
                "Binary file detected: {}\nUse appropriate tools to handle binary files.",
                params.file_path
            )));
        }

        // Read file
        let content = tokio::fs::read_to_string(&path).await?;

        // Which lines this call actually put in front of the model. `edit`'s
        // seen-line guard is checked against these, so a line clipped at
        // MAX_LINE_LEN still counts: the model saw the line and its number,
        // which is what anchoring an edit to it requires.
        let mut seen_lines: Vec<usize> = Vec::new();
        let mut truncated_line_count = 0usize;
        let file_lines: Vec<String> = content.lines().map(str::to_string).collect();
        let total_lines = file_lines.len();

        // A selector resolves through the ported window logic, which brings
        // omp's asymmetric context padding and window merging. An explicit
        // offset/limit keeps the older single-window path so those parameters
        // mean exactly what they say.
        let windows = if selector_ranges.is_empty() {
            let start = range.offset + 1;
            let end = (range.offset + range.limit).min(total_lines).max(start);
            vec![jcode_read::Window { start, end }]
        } else {
            jcode_read::resolve(
                &jcode_read::Request {
                    ranges: selector_ranges.clone(),
                    limit: params.limit,
                },
                total_lines,
            )
        };

        let mut output = String::new();
        {
            use std::fmt::Write;
            let mut previous_end: Option<usize> = None;
            for window in &windows {
                if previous_end.is_some() {
                    let _ = writeln!(output, "{}", jcode_read::ELISION);
                }
                for number in window.start..=window.end.min(total_lines) {
                    let Some(line) = file_lines.get(number - 1) else {
                        continue;
                    };
                    seen_lines.push(number);
                    if line.len() > MAX_LINE_LEN {
                        truncated_line_count += 1;
                        let _ = writeln!(
                            output,
                            "{:>5}\t{}...",
                            number,
                            crate::util::truncate_str(line, MAX_LINE_LEN)
                        );
                    } else {
                        let _ = writeln!(output, "{:>5}\t{}", number, line);
                    }
                }
                previous_end = Some(window.end);
            }
        }

        let end = windows.last().map(|w| w.end.min(total_lines)).unwrap_or(0);

        // Publish file touch event for swarm coordination
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: path.to_path_buf(),
            op: FileOp::Read,
            intent: None,
            summary: Some(format!(
                "read lines {}-{} of {}",
                range.offset + 1,
                end,
                total_lines
            )),
            detail: None,
        }));

        if truncated_line_count > 0 || end < total_lines {
            crate::logging::warn(&format!(
                "[tool:read] returned truncated output for {} in session {} (tool_call={} range={}..{} total_lines={} truncated_lines={})",
                params.file_path,
                ctx.session_id,
                ctx.tool_call_id,
                range.offset + 1,
                end,
                total_lines,
                truncated_line_count
            ));
        }

        // Add metadata
        if end < total_lines {
            // The hint is derived from where reading actually stopped, not from
            // the request. A selector read populates no offset/limit, so
            // deriving it from `range` produced nonsense like `offset=5000` on
            // a 200-line file - found by running a real agent, which quoted it
            // back verbatim.
            //
            // A selector read is continued with a selector, since that is the
            // spelling the caller used and mixing the two is what produced the
            // confusion.
            let continuation_hint = if !selector_ranges.is_empty() {
                format!("{}:{}- to continue", params.file_path, end + 1)
            } else {
                match range.style {
                    ReadRangeStyle::OffsetLimit => format!("offset={} to continue", end),
                    ReadRangeStyle::StartEnd => {
                        format!("start_line={} to continue", end + 1)
                    }
                }
            };
            output.push_str(&format!(
                "\n... {} more lines (use {})\n",
                total_lines - end,
                continuation_hint
            ));
        }

        if output.is_empty() {
            Ok(ToolOutput::new("(empty file)"))
        } else {
            // Record the file and stamp the output with `[path#TAG]`.
            //
            // The store is keyed by the *normalized* path, the same form
            // `split_sections` produces when it parses a header, so a tag
            // minted here is found again on lookup. Keying by the raw
            // `file_path` instead meant an absolute-path read and its own
            // absolute-path header landed under different keys, and a genuinely
            // stale tag was reported as "not from this session" - which sends
            // the model looking for a session mixup that never happened.
            //
            // The tag hashes the whole file, not the displayed range, so two
            // reads of one unchanged file agree and `edit` does not report a
            // spurious concurrent modification for a partial read.
            let cwd = ctx.working_dir.as_deref().and_then(|dir| dir.to_str());
            let key = jcode_hashline::normalize_path(&params.file_path, cwd);
            let store = super::hashline_store::for_session(&ctx.session_id);
            let tag = store.record(&key, &content, Some(&seen_lines));
            Ok(ToolOutput::new(format!(
                "{}\n{}",
                jcode_hashline::format_hashline_header(&params.file_path, &tag),
                output
            )))
        }
    }
}

#[cfg(test)]
mod tests;

fn is_binary_file(path: &Path) -> bool {
    // Check by extension first (no I/O needed)
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        let binary_exts = [
            "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "zip", "tar", "gz", "bz2", "xz",
            "7z", "rar", "exe", "dll", "so", "dylib", "o", "a", "class", "pyc", "wasm", "mp3",
            "mp4", "avi", "mov", "mkv", "flac", "ogg", "wav",
        ];
        if binary_exts.contains(&ext.as_str()) {
            return true;
        }
    }

    // Read only the first 8KB to check for binary content (not the entire file)
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 8192];
        if let Ok(n) = file.read(&mut buf)
            && n > 0
        {
            let null_count = buf[..n].iter().filter(|&&b| b == 0).count();
            return null_count > n / 10;
        }
    }

    false
}

/// Build the not-found error, adding whatever context is actually useful.
///
/// The bare "File not found: <path>" this replaced stated the one thing the
/// model already knew and none of what it needed. Three additions, each only
/// when it applies, because an unconditional note is noise that costs context on
/// every failed read:
///
/// 1. **The working directory, only for a relative path.** That is the hidden
///    half of the resolution, and it is invisible to the model. For an absolute
///    path it played no part, so saying it would mislead.
/// 2. **A parent-directory suggestion**, when the file exists one level up or
///    down from where it was looked for. This is the common mistake of dropping
///    or duplicating a directory level, and it is a higher-confidence guess than
///    a name match, so it is offered first.
/// 3. **Fuzzy filename matches** in the target directory, as before.
///
/// Shared with `edit` and `ls`, which resolve paths the same way
/// and had the same bare message.
pub(crate) fn file_not_found_message(
    requested: &str,
    resolved: &Path,
    working_dir: Option<&Path>,
) -> String {
    path_not_found_message("File", requested, resolved, working_dir)
}

/// As above, but for a caller looking for a directory rather than a file, so the
/// message does not tell the model a directory is a "File".
pub(crate) fn directory_not_found_message(
    requested: &str,
    resolved: &Path,
    working_dir: Option<&Path>,
) -> String {
    path_not_found_message("Directory", requested, resolved, working_dir)
}

fn path_not_found_message(
    noun: &str,
    requested: &str,
    resolved: &Path,
    working_dir: Option<&Path>,
) -> String {
    let want_dir = noun == "Directory";
    let mut message = format!("{noun} not found: {requested}");

    // Only meaningful when the working directory actually took part. Two ways
    // it did not: an absolute request, and a `~` request, which `resolve_path`
    // expands to an absolute path before the working directory is ever
    // consulted. Testing `requested.is_relative()` alone gets the tilde case
    // wrong and claims a resolution that did not happen.
    let requested_path = Path::new(requested);
    let used_working_dir = requested_path.is_relative()
        && !requested.starts_with('~')
        && working_dir.is_some_and(|cwd| resolved.starts_with(cwd));
    if used_working_dir && let Some(cwd) = working_dir {
        message.push_str(&format!(
            "\nResolved to {} against the working directory {}.",
            resolved.display(),
            cwd.display()
        ));
    }

    if let Some(hit) = find_path_near_working_dir(resolved, working_dir, want_dir) {
        message.push_str(&format!("\nDid you mean: {hit}"));
        return message;
    }

    let suggestions = find_similar_paths(resolved, want_dir);
    if !suggestions.is_empty() {
        message.push_str(&format!("\nDid you mean: {}", suggestions.join(", ")));
    }

    message
}

/// Look for the same name one directory level away, which catches a path that
/// dropped or duplicated a level.
///
/// Two directions, both real mistakes. Given a working directory `/w/repo` and a
/// miss at `/w/repo/src/main.rs`: the file may sit at `/w/repo/main.rs` (a level
/// too many), or the request may have been rooted a level too high, so
/// `/w/main.rs` is checked as well.
///
/// `want_dir` decides what counts as a hit. Suggesting a *file* to a caller that
/// asked for a directory sends it somewhere it cannot use, so the kinds must
/// match.
///
/// Deliberately narrow. It only ever returns a path that exists, so a suggestion
/// is never invented, and it does not walk the tree.
fn find_path_near_working_dir(
    resolved: &Path,
    working_dir: Option<&Path>,
    want_dir: bool,
) -> Option<String> {
    let file_name = resolved.file_name()?;
    let parent = resolved.parent()?;

    let mut candidates = Vec::new();
    if let Some(grandparent) = parent.parent() {
        candidates.push(grandparent.join(file_name));
    }
    if let Some(cwd) = working_dir {
        candidates.push(cwd.join(file_name));
        if let Some(cwd_parent) = cwd.parent() {
            candidates.push(cwd_parent.join(file_name));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| {
            candidate != resolved
                && if want_dir {
                    candidate.is_dir()
                } else {
                    candidate.is_file()
                }
        })
        .map(|candidate| candidate.display().to_string())
}

/// Fuzzy name matches in the target directory.
///
/// `want_dir` filters by kind for the same reason as above: a caller that asked
/// for a directory cannot use a file, and vice versa.
fn find_similar_paths(path: &Path, want_dir: bool) -> Vec<String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_name().map(|s| s.to_string_lossy().to_lowercase());

    let mut suggestions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.filter_map(|e| e.ok()) {
            let is_dir = entry.path().is_dir();
            if is_dir != want_dir {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if let Some(ref target) = filename {
                // Simple similarity check
                let target_str: &str = target.as_ref();
                if name.contains(target_str) || target_str.contains(&name as &str) {
                    suggestions.push(entry.path().display().to_string());
                    if suggestions.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }

    suggestions
}

/// Check if a file is an image based on extension
fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
        )
    } else {
        false
    }
}

/// Handle reading an image file - display in terminal if supported AND return base64 for model vision
fn handle_image_file(path: &Path, file_path: &str) -> Result<ToolOutput> {
    let protocol = ImageProtocol::detect();

    let data = std::fs::read(path)?;
    let file_size = data.len() as u64;

    let dimensions = get_image_dimensions_from_data(&data);

    let dim_str = dimensions
        .map(|(w, h)| format!("{}x{}", w, h))
        .unwrap_or_else(|| "unknown".to_string());

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    let mut terminal_displayed = false;
    if protocol.is_supported() {
        let params = ImageDisplayParams::from_terminal();
        match display_image(path, &params) {
            Ok(true) => {
                terminal_displayed = true;
            }
            Ok(false) => {}
            Err(e) => {
                crate::logging::info(&format!("Warning: Failed to display image: {}", e));
            }
        }
    }

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "image/png",
    };

    const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;
    let mut output = if file_size <= MAX_IMAGE_SIZE {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
        let display_note = if terminal_displayed {
            "Displayed in terminal. "
        } else {
            ""
        };
        ToolOutput::new(format!(
            "Image: {} ({})\nDimensions: {}\n{}Image sent to model for vision analysis.",
            file_path, size_str, dim_str, display_note
        ))
        .with_labeled_image(media_type, b64, file_path.to_string())
    } else {
        let display_note = if terminal_displayed {
            "\nDisplayed in terminal."
        } else {
            ""
        };
        ToolOutput::new(format!(
            "Image: {} ({})\nDimensions: {}\nImage too large for vision (max 20MB).{}",
            file_path, size_str, dim_str, display_note
        ))
    };

    output = output.with_title(format!("📷 {}", file_path));
    Ok(output)
}

/// Get image dimensions from raw data (duplicated from tui::image for convenience)
fn get_image_dimensions_from_data(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: check signature and parse IHDR chunk
    if data.len() > 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((width, height));
    }

    // JPEG: look for SOF0/SOF2 markers
    if data.len() > 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            // SOF0 (baseline) or SOF2 (progressive)
            if marker == 0xC0 || marker == 0xC2 {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            // Skip to next marker
            if i + 3 < data.len() {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    // GIF: parse header
    if data.len() > 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }

    None
}

/// Check if a file is a PDF based on extension
fn is_pdf_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        ext.to_string_lossy().to_lowercase() == "pdf"
    } else {
        false
    }
}

/// Handle reading a PDF file - extract text content
#[cfg(feature = "pdf")]
fn handle_pdf_file(
    path: &Path,
    file_path: &str,
    selection: Option<&[usize]>,
) -> Result<ToolOutput> {
    // Get file metadata
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    // Extract text from PDF
    match jcode_pdf::extract_text_by_page(path) {
        Ok(pages) => {
            let mut output = String::new();
            output.push_str(&format!("PDF: {} ({})\n", file_path, size_str));
            output.push_str(&format!("{}\n", "=".repeat(60)));

            // Real page boundaries from the extractor. Splitting the combined
            // text on `\x0c` used to be the approach, but nothing emits that
            // separator, so every document looked like a single page.
            let page_count = pages.len();

            match selection {
                Some(selected) => {
                    let out_of_range: Vec<String> = selected
                        .iter()
                        .filter(|p| **p > page_count)
                        .map(|p| p.to_string())
                        .collect();
                    if !out_of_range.is_empty() {
                        output.push_str(&format!(
                            "Pages: {page_count} (requested page(s) {} do not exist)\n\n",
                            out_of_range.join(", ")
                        ));
                    } else {
                        output.push_str(&format!("Pages: {page_count} (showing selection)\n\n"));
                    }
                }
                None => output.push_str(&format!("Pages: {}\n\n", page_count)),
            }

            for (i, page) in pages.iter().enumerate() {
                // `selection` is 1-based, matching how pages are labelled.
                if selection.is_some_and(|selected| !selected.contains(&(i + 1))) {
                    continue;
                }
                let page_text = page.trim();
                if !page_text.is_empty() {
                    output.push_str(&format!("--- Page {} ---\n", i + 1));
                    // Limit each page to reasonable length
                    if page_text.len() > 10000 {
                        output.push_str(crate::util::truncate_str(page_text, 10000));
                        output.push_str("\n... (page truncated)\n");
                    } else {
                        output.push_str(page_text);
                    }
                    output.push_str("\n\n");
                }
            }

            Ok(ToolOutput::new(output))
        }
        Err(e) => {
            // Fall back to metadata only if text extraction fails
            Ok(ToolOutput::new(format!(
                "PDF: {} ({})\nCould not extract text: {}\nThis may be a scanned/image-based PDF.",
                file_path, size_str, e
            )))
        }
    }
}

/// Handle reading a PDF file when PDF support is not compiled in.
#[cfg(not(feature = "pdf"))]
fn handle_pdf_file(
    path: &Path,
    file_path: &str,
    _selection: Option<&[usize]>,
) -> Result<ToolOutput> {
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    Ok(ToolOutput::new(format!(
        "PDF: {} ({})\nPDF text extraction is not available in this build. Rebuild with the `pdf` feature enabled to extract text.",
        file_path, size_str
    )))
}
