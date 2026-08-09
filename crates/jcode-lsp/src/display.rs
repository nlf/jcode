//! Making server-supplied text safe to render.
//!
//! Group G of the port. Not cosmetic: every string here comes from a language
//! server, and a diagnostic message, a symbol name and a hover body all reach our
//! TUI. Servers embed source text in messages — rustc quotes the offending line,
//! and Go's compiler errors carry the expression — so tabs and control characters
//! arrive as a matter of course rather than as an attack.
//!
//! omp learned this from an incident ([their #7041], ported below as
//! [`tests::an_expanded_error_is_sanitized_and_truncated`]) where an unsanitized
//! expanded error broke rendering.
//!
//! # What this does not do
//!
//! ANSI stripping is [`jcode_text_sanitize::strip_ansi_escape_sequences`], which
//! already exists and is more thorough than the regex omp uses (it handles CSI, OSC
//! and the C1 range, where omp's `CONTROL_RE` deletes C1 bytes but cannot terminate
//! an OSC string). Reimplementing it here would have produced a second, worse
//! stripper: exactly the duplication the review pass flagged elsewhere.
//!
//! # Divergence: tab width
//!
//! omp expands a tab to **3** spaces (`DEFAULT_TAB_WIDTH` in
//! `packages/utils/src/tab-spacing.ts`). We use **4**, matching
//! `jcode-app-core/src/tool/tool_diff.rs`, which measures tab stops at 4 for edit
//! diffs. Being internally consistent matters more than matching omp's number: a
//! diagnostic rendered beside a diff should not disagree about how wide a tab is.
//! omp's 3 is not wrong, just theirs.
//!
//! Their tests assert only the *absence* of tabs and that the words survive, so
//! this divergence does not fail any ported case — which is why it is written down
//! here rather than discovered later.
//!
//! [their #7041]: https://github.com/can1357/oh-my-pi/issues/7041

/// Tab stop width, matching `tool_diff`'s. See the module note on divergence.
const TAB_WIDTH: usize = 4;

/// Expand tabs to spaces.
///
/// Aligned to the next tab stop rather than a blind replace of one tab with N
/// spaces. omp replaces blindly, which is fine for their purpose (they only assert
/// no tab survives) but wrong for anything that has to line up in a column — and
/// diagnostics are rendered in columns.
pub fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + TAB_WIDTH);
    let mut column = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let advance = TAB_WIDTH - (column % TAB_WIDTH);
                out.extend(std::iter::repeat_n(' ', advance));
                column += advance;
            }
            // A newline restarts the column count, or every line after the first
            // gets its tab stops measured from the start of the *string*.
            '\n' => {
                out.push('\n');
                column = 0;
            }
            _ => {
                out.push(ch);
                column += 1;
            }
        }
    }
    out
}

/// Make text safe for a single line of output.
///
/// For values rendered inline in a tool call header — a symbol name, a query. A
/// newline here would break the line and let a server's string forge what looks
/// like a second row of our UI, so newlines become spaces rather than being
/// stripped: `foo\nbar` must not read as `foobar`.
pub fn inline(text: &str) -> String {
    let stripped = jcode_text_sanitize::strip_ansi_escape_sequences(text);
    let expanded = expand_tabs(&stripped);
    let mut out = String::with_capacity(expanded.len());
    let mut chars = expanded.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                // Collapse CRLF to one space, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' => out.push(' '),
            // Remaining C0 controls carry no meaning once ANSI is gone.
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Make text safe for multi-line output, keeping the line structure.
///
/// For diagnostic bodies and hover text, where the newlines are the content.
pub fn block(text: &str) -> String {
    let stripped = jcode_text_sanitize::strip_ansi_escape_sequences(text);
    let expanded = expand_tabs(&stripped);
    expanded
        .replace("\r\n", "\n")
        .chars()
        .filter(|ch| *ch == '\n' || !ch.is_control())
        .collect()
}

/// Shorten to a display width, marking that it was shortened.
///
/// A server can return a 200 KiB hover or an error containing an entire file. omp's
/// #7041 was this: an expanded error that was merely long. Truncation is part of
/// sanitization rather than a separate concern, because unbounded text is itself
/// the failure.
pub fn truncate(text: &str, limit: usize) -> String {
    // Counted in chars, not bytes, so a multi-byte character is never split and the
    // limit means the same thing for any language. Not grapheme clusters: that needs
    // a segmenter, and the consequence here is a cut inside a combining sequence in
    // text already marked as elided.
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
