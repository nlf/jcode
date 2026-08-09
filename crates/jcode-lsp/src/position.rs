//! Turning `file` + `line` + `symbol` into an LSP position.
//!
//! # Why a symbol rather than a column
//!
//! An LSP position is a line and a character offset, and a model cannot reliably
//! count characters. So the tool takes a *symbol* — a substring of the line — and
//! resolves the column itself. That moves the counting to the side that can do it,
//! and it makes the call readable: `symbol: "resolve_column"` says what was meant
//! where `character: 27` does not.
//!
//! # Why a missing symbol is an error rather than a fallback
//!
//! omp *errors* when the named symbol is absent from the line, and does not fall
//! back to the first non-whitespace column. That looks unhelpful and is correct: a
//! rename at a guessed position is a **silent wrong rename**, and the model has no
//! way to detect it. Failing tells it to look again; guessing corrupts the file and
//! reports success.
//!
//! The one place a fallback is allowed is when no symbol was given at all, which is
//! an explicit "anywhere on this line" and is fine for `hover`.
//!
//! # The word-boundary rule, and the `$` bug it came from
//!
//! A bare identifier matches only at word boundaries, so `id` does not resolve
//! inside `uuid`. omp's regression is more specific: their identifier pattern
//! rejected a leading `$`, so `$store` was treated as a non-identifier, boundary
//! checking was skipped, and it resolved **inside `bar$store`** — handing the server
//! a column in the middle of a different variable. Their fixture
//! `let bar$store = $store + 1;` expects column 16 for `$store` and 4 for
//! `bar$store`, and both are asserted here.
//!
//! A symbol that is *not* a bare identifier (`foo.bar`, `->`, `a b`) is matched as a
//! plain substring, because word boundaries are meaningless for it.

/// Where a symbol could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionError {
    /// The line does not exist in the file.
    NoSuchLine { line: usize, lines: usize },
    /// The symbol is not on that line.
    ///
    /// Carries the line's text so the model can see what is actually there rather
    /// than guessing again from the same wrong assumption.
    SymbolNotFound {
        symbol: String,
        line: usize,
        text: String,
    },
    /// Fewer occurrences than the `#N` selector asked for.
    OccurrenceOutOfRange {
        symbol: String,
        wanted: usize,
        found: usize,
        line: usize,
    },
}

impl std::fmt::Display for PositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchLine { line, lines } => {
                write!(f, "line {line} is past the end of the file ({lines} lines)")
            }
            Self::SymbolNotFound { symbol, line, text } => write!(
                f,
                "symbol {symbol:?} is not on line {line}. That line reads: {text:?}"
            ),
            Self::OccurrenceOutOfRange {
                symbol,
                wanted,
                found,
                line,
            } => write!(
                f,
                "asked for occurrence {wanted} of {symbol:?} on line {line}, but there \
                 {} only {found}",
                if *found == 1 { "is" } else { "are" }
            ),
        }
    }
}

impl std::error::Error for PositionError {}

/// A symbol with an optional occurrence selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSpec {
    pub symbol: String,
    /// 1-indexed. `foo#2` means the second `foo` on the line.
    pub occurrence: usize,
}

/// Parse `name` or `name#N`.
///
/// The split is on the **last** `#` followed only by digits, so a symbol that
/// itself starts with `#` survives: TypeScript private fields are named `#count`,
/// and `#count#2` must parse as the second `#count` rather than as a symbol named
/// `` with occurrence... something. omp notes the same case.
pub fn parse_symbol(spec: &str) -> SymbolSpec {
    if let Some(hash) = spec.rfind('#')
        && hash > 0
        && let Some(digits) = spec.get(hash + 1..)
        && !digits.is_empty()
        && digits.chars().all(|character| character.is_ascii_digit())
        && let Ok(occurrence) = digits.parse::<usize>()
        // `#0` is not a valid selector: occurrences are 1-indexed, and treating 0
        // as "the first" would silently accept a nonsense request.
        && occurrence >= 1
    {
        return SymbolSpec {
            symbol: spec[..hash].to_string(),
            occurrence,
        };
    }
    SymbolSpec {
        symbol: spec.to_string(),
        occurrence: 1,
    }
}

/// Resolve a zero-based character offset on a one-based line.
///
/// `symbol` of `None` means "anywhere on this line", which resolves to the first
/// non-whitespace character. Callers that must not guess — `rename`, `references`,
/// `definition` — should refuse to call this without a symbol rather than relying on
/// that fallback.
pub fn resolve_column(
    content: &str,
    line: usize,
    symbol: Option<&str>,
) -> Result<usize, PositionError> {
    // `split('\n')` rather than `lines()`: `lines()` swallows a trailing newline, so
    // the last line of a file ending in `\n` would be unaddressable.
    let all: Vec<&str> = content.split('\n').collect();
    let index = line.max(1) - 1;
    let text = all.get(index).ok_or(PositionError::NoSuchLine {
        line,
        lines: all.len(),
    })?;

    let Some(spec) = symbol.map(parse_symbol) else {
        return Ok(first_non_whitespace(text));
    };

    let matches = find_matches(text, &spec.symbol);
    if matches.is_empty() {
        return Err(PositionError::SymbolNotFound {
            symbol: spec.symbol,
            line,
            text: text.to_string(),
        });
    }
    matches
        .get(spec.occurrence - 1)
        .copied()
        .ok_or(PositionError::OccurrenceOutOfRange {
            symbol: spec.symbol,
            wanted: spec.occurrence,
            found: matches.len(),
            line,
        })
}

/// Character offsets of every match of `symbol` in `text`.
///
/// Offsets are in **characters, not bytes**, because that is what LSP means by a
/// position on a UTF-8 document with the default `utf-16` position encoding... and
/// even under `utf-8` encoding a byte offset into a multi-byte character is not a
/// valid position. Returning byte offsets would put the cursor mid-character on any
/// line containing non-ASCII, which is common in comments and strings.
fn find_matches(text: &str, symbol: &str) -> Vec<usize> {
    if symbol.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let bare = is_bare_identifier(symbol);

    // Exact first. Case-insensitive is a fallback rather than an equal
    // alternative: a case-insensitive hit when an exact one exists would resolve
    // `Foo` onto `foo`, which are different symbols in every language we care
    // about.
    collect(text, symbol, bare, false, &mut found);
    if found.is_empty() {
        collect(text, symbol, bare, true, &mut found);
    }
    found
}

fn collect(text: &str, symbol: &str, bare: bool, fold_case: bool, out: &mut Vec<usize>) {
    let haystack: Vec<char> = if fold_case {
        text.to_lowercase().chars().collect()
    } else {
        text.chars().collect()
    };
    let needle: Vec<char> = if fold_case {
        symbol.to_lowercase().chars().collect()
    } else {
        symbol.chars().collect()
    };
    if needle.is_empty() || haystack.len() < needle.len() {
        return;
    }

    let mut at = 0usize;
    while at + needle.len() <= haystack.len() {
        if haystack[at..at + needle.len()] == needle[..] {
            let boundaries_ok = !bare
                || (!is_identifier_char(at.checked_sub(1).map(|before| haystack[before]))
                    && !is_identifier_char(haystack.get(at + needle.len()).copied()));
            if boundaries_ok {
                out.push(at);
                // Non-overlapping: the next search starts after this match, so
                // `aa` in `aaa` is one occurrence rather than two. Overlapping
                // matches would make `#2` mean something the caller cannot predict.
                at += needle.len();
                continue;
            }
        }
        at += 1;
    }
}

/// Whether a symbol is a bare identifier, and so needs word boundaries.
///
/// `$` is a leading-position identifier character, which is the whole point of
/// omp's regression: excluding it made `$store` a non-identifier, so boundary
/// checking was skipped and it matched inside `bar$store`.
fn is_bare_identifier(symbol: &str) -> bool {
    let mut characters = symbol.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    characters.all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

/// Whether a character continues an identifier.
///
/// `None` (start or end of line) is not an identifier character, so a symbol at
/// either edge of the line is at a boundary.
fn is_identifier_char(character: Option<char>) -> bool {
    character.is_some_and(|character| {
        character.is_alphanumeric() || character == '_' || character == '$'
    })
}

fn first_non_whitespace(text: &str) -> usize {
    text.chars()
        .position(|character| !character.is_whitespace())
        // An all-whitespace or empty line resolves to 0 rather than failing: it is
        // a legitimate position, and `hover` on a blank line is a reasonable if
        // useless request.
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "position_tests.rs"]
mod position_tests;
