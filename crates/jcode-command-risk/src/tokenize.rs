//! Minimal shell tokenizer, tuned for risk analysis rather than execution.
//!
//! This deliberately does not implement POSIX shell. It implements just enough
//! to answer "which words are targets of a destructive command", and it is
//! written to **fail loud rather than quiet**: when it cannot understand
//! something it marks the segment as opaque so the caller escalates.

/// One shell word plus the classification bits the assessor needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    /// True when this segment's stdin comes from a pipe, so its operands are
    /// partly supplied at runtime by the previous command.
    pub receives_pipe: bool,
    /// True for `>` / `>|` redirect destinations, which are truncated on open.
    pub is_truncating_redirect_target: bool,
    /// True for control operators like `&&`, which are never path targets.
    pub is_operator: bool,
}

impl Token {
    fn word(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            receives_pipe: false,
            is_truncating_redirect_target: false,
            is_operator: false,
        }
    }

    /// The command name without its directory, so `/bin/rm` matches `rm`.
    pub fn basename(&self) -> String {
        self.text
            .rsplit('/')
            .next()
            .unwrap_or(&self.text)
            .to_string()
    }

    pub fn is_flag(&self) -> bool {
        self.text.starts_with('-') && self.text.len() > 1
    }

    /// Whether this flag requests recursion, including bundles like `-rf`.
    pub fn is_recursive_flag(&self) -> bool {
        if !self.is_flag() {
            return false;
        }
        if self.text.starts_with("--") {
            return self.text == "--recursive";
        }
        self.text.contains('r') || self.text.contains('R')
    }
}

/// Characters that separate one command from the next.
const SEGMENT_SEPARATORS: &[&str] = &["&&", "||", ";", "|", "\n", "(", ")"];

/// Split a command line into individual command segments, each tokenized.
///
/// `rm -rf a && rm -rf b` yields two segments so both are assessed. Without
/// this, chaining would be a trivial bypass.
pub fn split_segments(command: &str) -> Vec<Vec<Token>> {
    let tokens = tokenize(&strip_heredoc_bodies(command));
    let mut segments = Vec::new();
    let mut current = Vec::new();

    let mut next_receives_pipe = false;
    for token in tokens {
        if token.is_operator && SEGMENT_SEPARATORS.contains(&token.text.as_str()) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            // The command after `|` consumes the previous one's output as
            // operands, which this parser cannot see (#604 review).
            next_receives_pipe = token.text == "|";
            continue;
        }
        let mut token = token;
        token.receives_pipe = next_receives_pipe;
        current.push(token);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Remove heredoc bodies, which are data written to a file rather than
/// commands to be run.
///
/// Without this, `cat > t.rs <<'EOF'` followed by a body that merely *mentions*
/// `/dev/null` or `rm -rf` is assessed as if the body were executed, and
/// writing a test fixture about the gate trips the gate. The body is kept when
/// it is piped onward (`cat <<EOF | bash`), where it really does become
/// commands.
fn strip_heredoc_bodies(command: &str) -> String {
    if !command.contains("<<") {
        return command.to_string();
    }

    let mut out: Vec<String> = Vec::new();
    let mut lines = command.lines();

    while let Some(line) = lines.next() {
        let delimiters = heredoc_delimiters(line);
        out.push(strip_heredoc_operators(line));
        if delimiters.is_empty() || line.contains('|') {
            continue;
        }
        // Consume the body of each heredoc opened on this line, in order.
        for delimiter in delimiters {
            for body_line in lines.by_ref() {
                if body_line.trim() == delimiter {
                    break;
                }
            }
        }
    }

    out.join("\n")
}

/// The delimiter words for every heredoc opened on one line, in order.
fn heredoc_delimiters(line: &str) -> Vec<String> {
    let bytes: Vec<char> = line.chars().collect();
    let mut delimiters = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != '<' || bytes[i + 1] != '<' {
            i += 1;
            continue;
        }
        // `<<<` is a herestring: one line of data, no body to consume.
        if bytes.get(i + 2) == Some(&'<') {
            i += 3;
            continue;
        }
        let mut j = i + 2;
        if bytes.get(j) == Some(&'-') {
            j += 1;
        }
        while bytes.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
            j += 1;
        }
        let mut word = String::new();
        while let Some(&c) = bytes.get(j) {
            match c {
                '\'' | '"' | '\\' => {}
                c if c.is_whitespace() || c == '|' || c == ';' || c == '&' || c == '>' => break,
                c => word.push(c),
            }
            j += 1;
        }
        if !word.is_empty() {
            delimiters.push(word);
        }
        i = j;
    }
    delimiters
}

/// Blank out `<<DELIM` operators so the delimiter word is not tokenized as a
/// path operand of the command that opened the heredoc.
fn strip_heredoc_operators(line: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' && chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') {
            let mut j = i + 2;
            if chars.get(j) == Some(&'-') {
                j += 1;
            }
            while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                j += 1;
            }
            while chars
                .get(j)
                .is_some_and(|c| !c.is_whitespace() && !matches!(c, '|' | ';' | '&' | '>'))
            {
                j += 1;
            }
            out.push(' ');
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Tokenize a command line, resolving quotes so `rm "$HOME"` and `rm $HOME`
/// produce the same target text.
///
/// Note the asymmetry with a real shell: we intentionally keep `$VAR` intact
/// rather than expanding it, and the path layer treats an unexpanded variable
/// as unknown-and-therefore-risky.
pub fn tokenize(command: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut current = String::new();
    let mut has_content = false;
    let mut chars = command.chars().peekable();
    let mut pending_redirect = false;

    macro_rules! flush {
        () => {
            if has_content {
                let mut token = Token::word(std::mem::take(&mut current));
                #[allow(unused_assignments)]
                {
                    if pending_redirect {
                        token.is_truncating_redirect_target = true;
                        pending_redirect = false;
                    }
                    has_content = false;
                }
                tokens.push(token);
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_content = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    current.push(q);
                }
            }
            '"' => {
                has_content = true;
                while let Some(q) = chars.next() {
                    if q == '"' {
                        break;
                    }
                    if q == '\\'
                        && let Some(&next) = chars.peek()
                        && matches!(next, '"' | '\\' | '$' | '`')
                    {
                        current.push(next);
                        chars.next();
                        continue;
                    }
                    current.push(q);
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    has_content = true;
                    current.push(next);
                }
            }
            ' ' | '\t' => flush!(),
            '\n' | ';' => {
                flush!();
                let mut op = Token::word(c.to_string());
                op.is_operator = true;
                tokens.push(op);
            }
            '&' | '|' => {
                flush!();
                let mut text = c.to_string();
                if chars.peek() == Some(&c) {
                    text.push(c);
                    chars.next();
                }
                let mut op = Token::word(text);
                op.is_operator = true;
                tokens.push(op);
            }
            '(' | ')' => {
                flush!();
                let mut op = Token::word(c.to_string());
                op.is_operator = true;
                tokens.push(op);
            }
            '>' => {
                // `>>` appends and does not truncate, so it is far less
                // destructive; only a single `>` clobbers.
                if chars.peek() == Some(&'>') {
                    flush!();
                    chars.next();
                } else if chars.peek() == Some(&'&') {
                    // `2>&1` duplicates a file descriptor. Nothing is opened
                    // and nothing is truncated, so neither the fd designator
                    // before it nor the digits after it are paths.
                    drop_fd_designator(&mut current, &mut has_content);
                    flush!();
                    chars.next();
                    while chars.peek().is_some_and(|c| c.is_ascii_digit() || *c == '-') {
                        chars.next();
                    }
                } else {
                    // A leading `2` in `2>file` names a file descriptor, not a
                    // path to be destroyed.
                    drop_fd_designator(&mut current, &mut has_content);
                    flush!();
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    pending_redirect = true;
                }
            }
            '<' => flush!(),
            _ => {
                has_content = true;
                current.push(c);
            }
        }
    }
    flush!();

    tokens
}

/// Discard a pending word when it is the file-descriptor number attached to a
/// redirect (`2>`), so it is never mistaken for a target path.
fn drop_fd_designator(current: &mut String, has_content: &mut bool) {
    if *has_content && !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
        current.clear();
        *has_content = false;
    }
}

#[cfg(test)]
#[path = "tokenize_tests.rs"]
mod tokenize_tests;
