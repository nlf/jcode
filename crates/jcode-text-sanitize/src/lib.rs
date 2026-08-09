//! Sanitizing text that arrived from outside our control.
//!
//! Captured command output, a language server's diagnostic, a subprocess's stderr:
//! all of it reaches a renderer, and none of it is attached to a terminal that would
//! give escape sequences a meaning. Retaining them cannot produce useful styling and
//! can leak SGR parameters as visible text or let an OSC command through to the
//! terminal.
//!
//! Moved here from `jcode-base` so that crates which need it do not have to depend
//! on a two-minute compile. The behaviour is unchanged and its tests came along.

/// Remove terminal escape sequences before untrusted text is rendered.
///
/// Handles the three shapes that matter: CSI (`ESC [` … final byte), the string
/// families (OSC, DCS, SOS, PM, APC — terminated by BEL or `ESC \`), and the
/// single-character and nF escapes. The C1 8-bit forms are handled too, since a
/// UTF-8 decode can produce them and a filter that only looks for `ESC` misses them.
///
/// Note it strips escapes, not all control characters: `\t` and `\n` survive, since
/// callers that care about layout handle those themselves and deleting a newline
/// changes the meaning of the text.
pub fn strip_ansi_escape_sequences(text: &str) -> String {
    fn consume_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
        for ch in chars.by_ref() {
            if ('@'..='~').contains(&ch) {
                break;
            }
        }
    }

    fn consume_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
        let mut saw_escape = false;
        for ch in chars.by_ref() {
            if ch == '\u{7}' || (saw_escape && ch == '\\') {
                break;
            }
            saw_escape = ch == '\u{1b}';
        }
    }

    fn consume_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                consume_csi(chars);
            }
            Some(']' | 'P' | 'X' | '^' | '_') => {
                chars.next();
                consume_string(chars);
            }
            Some(_) => {
                while chars
                    .peek()
                    .is_some_and(|ch| ('\u{20}'..='\u{2f}').contains(ch))
                {
                    chars.next();
                }
                if chars
                    .peek()
                    .is_some_and(|ch| ('\u{30}'..='\u{7e}').contains(ch))
                {
                    chars.next();
                }
            }
            None => {}
        }
    }

    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => consume_escape(&mut chars),
            '\u{9b}' => consume_csi(&mut chars),
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => consume_string(&mut chars),
            '\u{80}'..='\u{9f}' => {}
            _ => clean.push(ch),
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case that came with the function from `jcode-base`, unchanged.
    #[test]
    fn strip_ansi_escape_sequences_removes_terminal_controls() {
        let input = "\u{1b}[32mgreen\u{1b}[0m \u{1b}]8;;http://example.com\u{7}link\u{1b}]8;;\u{7} \
                     done";
        assert_eq!(strip_ansi_escape_sequences(input), "green link done");
    }

    /// Tabs and newlines are layout, not escapes, and must survive.
    ///
    /// Written when `jcode-lsp` began expanding tabs itself: if this function ate
    /// them, the tab-stop logic downstream would have nothing to do and the bug
    /// would look like it was in the caller.
    #[test]
    fn tabs_and_newlines_survive() {
        assert_eq!(strip_ansi_escape_sequences("a\tb\nc\r\nd"), "a\tb\nc\r\nd");
    }

    /// An unterminated OSC swallows the rest, which is the safe direction.
    ///
    /// The alternative -- emitting the payload as visible text -- means a server can
    /// print whatever it likes into our UI by opening a sequence and never closing
    /// it. Losing trailing text from an already-malformed string is the better
    /// failure.
    #[test]
    fn an_unterminated_string_sequence_consumes_the_remainder() {
        assert_eq!(
            strip_ansi_escape_sequences("before\u{1b}]0;never closed"),
            "before"
        );
    }

    /// The 8-bit C1 forms, which a filter looking only for ESC would miss.
    #[test]
    fn eight_bit_control_forms_are_handled() {
        // CSI as a single 0x9B character.
        assert_eq!(strip_ansi_escape_sequences("a\u{9b}31mb"), "ab");
        // A bare C1 that is not an introducer is dropped.
        assert_eq!(strip_ansi_escape_sequences("a\u{85}b"), "ab");
    }

    /// Text with no escapes is returned as-is.
    #[test]
    fn clean_text_is_unchanged() {
        assert_eq!(strip_ansi_escape_sequences("plain text"), "plain text");
        assert_eq!(strip_ansi_escape_sequences(""), "");
    }
}
