//! Splitting a shell command into its top-level segments.
//!
//! Ported from oh-my-pi's `src/tools/shell-tokenize.ts`, behaviour-first.
//!
//! This is not a shell parser and must not pretend to be one. It splits on
//! `;`, `|`, `&` and newlines while respecting quoting, and **gives up
//! entirely** on anything it cannot read confidently: command substitution,
//! subshells, heredocs, brace groups. Giving up returns no segments, which
//! makes the caller fall back to matching the whole command. Guessing at
//! `$(...)` instead would mean interpreting text that shell semantics say is a
//! different command.

/// One top-level command in a compound line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    /// True when this segment consumes the previous stage's stdout.
    ///
    /// A piped segment reads stdin, which no path-based tool can replace, so
    /// callers exclude it from interception. `grep x file` is redirectable;
    /// `cat file | grep x` is not, on the `grep` side.
    pub piped_stdin: bool,
}

/// Whether a character at this position is part of a redirection operator
/// rather than a segment separator.
///
/// `>|` is clobber, `>&`/`<&` are fd duplication, `&>` is redirect-both. None
/// of them separate commands, and treating them as separators would split a
/// single command in half.
fn is_redirection_operator(bytes: &[u8], index: usize) -> bool {
    let previous = index.checked_sub(1).and_then(|i| bytes.get(i)).copied();
    let next = bytes.get(index + 1).copied();
    match bytes[index] {
        b'|' => previous == Some(b'>'),
        b'&' => previous == Some(b'>') || previous == Some(b'<') || next == Some(b'>'),
        _ => false,
    }
}

/// Split `command` into top-level segments.
///
/// Returns empty when the command contains anything this cannot read
/// confidently. Empty means "no opinion", not "no commands".
pub fn segments(command: &str) -> Vec<Segment> {
    let bytes = command.as_bytes();
    let mut segments: Vec<Segment> = Vec::new();
    let mut segment_start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut at_word_start = true;
    let mut current_piped = false;
    let mut index = 0usize;

    macro_rules! push_segment {
        ($end:expr) => {{
            let text = command[segment_start..$end].trim();
            if text.is_empty() {
                false
            } else {
                segments.push(Segment {
                    text: text.to_string(),
                    piped_stdin: current_piped,
                });
                true
            }
        }};
    }

    while index < bytes.len() {
        let ch = bytes[index];

        if in_single {
            if ch == b'\'' {
                in_single = false;
            }
            index += 1;
            continue;
        }

        if in_double {
            if ch == b'\\' {
                // A trailing backslash means the command is incomplete.
                if index + 1 >= bytes.len() {
                    return Vec::new();
                }
                index += 2;
                continue;
            }
            if ch == b'"' {
                in_double = false;
                index += 1;
                continue;
            }
            // Substitution inside quotes is still substitution.
            if ch == b'`' || (ch == b'$' && bytes.get(index + 1) == Some(&b'(')) {
                return Vec::new();
            }
            index += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single = true;
                at_word_start = false;
                index += 1;
                continue;
            }
            b'"' => {
                in_double = true;
                at_word_start = false;
                index += 1;
                continue;
            }
            b'\\' => {
                if index + 1 >= bytes.len() {
                    return Vec::new();
                }
                index += 2;
                at_word_start = false;
                continue;
            }
            _ => {}
        }

        // Constructs whose contents are a different command, or whose structure
        // this cannot follow. Bailing out is the safe answer.
        let next = bytes.get(index + 1).copied();
        let opens_group = (ch == b'{' || ch == b'}')
            && at_word_start
            && next.is_none_or(|c| matches!(c, b' ' | b'\t' | b'\n' | b';'));
        if ch == b'`'
            || ch == b'('
            || ch == b')'
            || (ch == b'$' && next == Some(b'('))
            || (ch == b'$' && next == Some(b'{'))
            || (ch == b'<' && next == Some(b'<'))
            || opens_group
        {
            return Vec::new();
        }

        // A comment runs to end of line and is not part of any command.
        if ch == b'#' && at_word_start {
            let pushed = push_segment!(index);
            match command[index + 1..].find('\n') {
                None => return segments,
                Some(offset) => {
                    let newline = index + 1 + offset;
                    index = newline + 1;
                    segment_start = index;
                    at_word_start = true;
                    if pushed {
                        current_piped = false;
                    }
                    continue;
                }
            }
        }

        if matches!(ch, b'\n' | b';' | b'|' | b'&') && !is_redirection_operator(bytes, index) {
            let pushed = push_segment!(index);
            let doubled = matches!(ch, b'|' | b'&') && next == Some(ch);
            let pipe_stderr = ch == b'|' && next == Some(b'&');
            if doubled || pipe_stderr {
                index += 1;
            }
            // `|` and `|&` feed the next segment; `||`, `&&`, `;` do not. A
            // blank continuation line preserves whatever was pending.
            if pushed || ch != b'\n' {
                current_piped = ch == b'|' && !doubled;
            }
            index += 1;
            segment_start = index;
            at_word_start = true;
            continue;
        }

        at_word_start = matches!(ch, b' ' | b'\t');
        index += 1;
    }

    // An unterminated quote means the command is incomplete.
    if in_single || in_double {
        return Vec::new();
    }
    push_segment!(bytes.len());
    segments
}

/// Advance past one shell word, respecting quoting.
///
/// Returns `None` when the word is unterminated, which the caller treats as
/// "cannot read this command".
pub fn skip_word(command: &str, start: usize) -> Option<usize> {
    let bytes = command.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut index = start;

    while index < bytes.len() {
        let ch = bytes[index];
        if in_single {
            if ch == b'\'' {
                in_single = false;
            }
            index += 1;
            continue;
        }
        if in_double {
            if ch == b'\\' {
                if index + 1 >= bytes.len() {
                    return None;
                }
                index += 2;
                continue;
            }
            if ch == b'"' {
                in_double = false;
            }
            index += 1;
            continue;
        }
        match ch {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'\\' => {
                if index + 1 >= bytes.len() {
                    return None;
                }
                index += 1;
            }
            b' ' | b'\t' => return Some(index),
            _ => {}
        }
        index += 1;
    }

    if in_single || in_double {
        None
    } else {
        Some(bytes.len())
    }
}

/// Strip leading `NAME=value` assignments.
///
/// `FOO=1 cat x` is a `cat` call, and a rule matching on the leading word would
/// miss it. Returns `None` when there were no assignments, or when what follows
/// them is unreadable.
pub fn without_leading_assignments(command: &str) -> Option<String> {
    let bytes = command.as_bytes();
    let mut index = 0usize;
    let mut found = false;

    while index < bytes.len() {
        while matches!(bytes.get(index), Some(b' ') | Some(b'\t')) {
            index += 1;
        }
        let assignment_start = index;
        match bytes.get(index) {
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' => {}
            _ => break,
        }
        let mut name_end = index + 1;
        while bytes
            .get(name_end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            name_end += 1;
        }
        if bytes.get(name_end) != Some(&b'=') {
            // A word that is not an assignment ends the prefix.
            return found.then(|| command[assignment_start..].trim_start().to_string());
        }
        let word_end = skip_word(command, name_end + 1)?;
        found = true;
        index = word_end;
        // No early return for "ran off the end" here: assignments with nothing
        // after them fall through to the emptiness check below, which already
        // yields None. Mutation testing showed the extra guard was unreachable.
    }

    if !found {
        return None;
    }
    let rest = command[index..].trim_start();
    (!rest.is_empty()).then(|| rest.to_string())
}

#[cfg(test)]
#[path = "tokenize_tests.rs"]
mod tokenize_tests;
