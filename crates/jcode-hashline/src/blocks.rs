//! Turning `PUT 5*:` into `PUT 5.=9:`, by asking a parser where the block ends.
//!
//! Ported from oh-my-pi's `block.ts`. The whole point is arithmetic the model
//! should not have to do: naming a function by the line it starts on is
//! reliable, counting to its closing brace is not, and a range that ends one
//! line short silently leaves a stray `}` behind.
//!
//! # Deferred, not parsed
//!
//! The parser cannot resolve a block, because `PUT 5*:` means nothing without
//! the file and its language. So the anchor survives parsing as
//! [`Anchor::Block`] and is resolved here, before anything else runs. After
//! [`resolve`] there are no block anchors left, and every later stage, repair,
//! recovery and the applier, sees only concrete ranges and needs to know
//! nothing about this.
//!
//! # Refusing is the common case, and it has to be useful
//!
//! An anchor that does not name a block is the mistake this feature invites:
//! pointing at a blank line, at a closing brace, or at a bare statement rather
//! than the construct that encloses it. Each refusal therefore looks around the
//! anchor and says what to write instead, because "no block there" leaves a
//! model to guess again, and its second guess is usually the same one.

use crate::parser::{Anchor, Op};

/// The lines a block occupies, 1-indexed and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSpan {
    pub start: usize,
    pub end: usize,
}

/// Where the block beginning on a line ends, as far as the caller can tell.
///
/// `None` means "no block begins there", covering an unknown language, a file
/// that does not parse, a blank line and a bare statement alike. As with
/// [`crate::patcher::SyntaxCheck`], the crate stays parser-free and the host
/// supplies this. See `jcode_ast::block_at`.
pub type BlockResolver<'a> = &'a dyn Fn(&str, &str, usize) -> Option<BlockSpan>;

/// How far to look for a block to suggest, once the anchor has already failed.
///
/// Only ever walked on the error path, so the bound is about not scanning a
/// 50,000-line file to build one message, not about performance in general.
const SUGGESTION_SCAN_LIMIT: usize = 64;

/// Resolve every block anchor in `ops` against `text`, in place.
///
/// Returns the lines that were only ever named *by a block*, which the caller
/// needs for the seen-line guard: a model naming a function by its opening line
/// has not read to the closing brace, and must not be required to. See
/// [`crate::patcher::prepare`].
///
/// Returns an error naming the offending anchor when one cannot be resolved.
/// Nothing is written in that case: a block edit that cannot find its block is
/// refused whole, rather than applied to the line the model happened to name.
pub fn resolve(
    ops: &mut [Op],
    path: &str,
    text: &str,
    resolver: Option<BlockResolver<'_>>,
) -> Result<Vec<usize>, String> {
    if !ops.iter().any(is_block_op) {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut expanded = Vec::new();

    for op in ops.iter_mut() {
        let Op::Put {
            anchor: Anchor::Block(line),
            body,
        } = op
        else {
            continue;
        };
        let line = *line;
        let deletes = body.is_empty();

        let Some(resolver) = resolver else {
            return Err(
                "Block anchors (`N*`) are not available here (no parser configured). \
                 Use an explicit line range such as `PUT 5.=9:`."
                    .to_string(),
            );
        };

        match resolver(path, text, line) {
            Some(span) if span.end > span.start => {
                // Every line past the anchor is one the resolver named, not the
                // model. Recorded so the seen-line guard can exempt them: the
                // point of `PUT 5*:` is that the closing brace need not be
                // counted, or therefore read.
                expanded.extend((span.start + 1)..=span.end);
                *op = if deletes {
                    Op::Cut {
                        start: span.start,
                        end: span.end,
                    }
                } else {
                    Op::Put {
                        anchor: Anchor::Range {
                            start: span.start,
                            end: span.end,
                        },
                        body: std::mem::take(body),
                    }
                };
            }
            // A single-line resolution means the anchor named a bare statement
            // rather than the opening line of a construct. The plain form is
            // exact for one line, so say so rather than silently doing the same
            // thing: the model that wrote `*` was reaching for the enclosing
            // block, and that is what the suggestion offers.
            Some(_) => {
                return Err(single_line_message(
                    line,
                    deletes,
                    enclosing_block(line, &lines, path, text, resolver),
                ));
            }
            None => {
                return Err(unresolved_message(
                    line,
                    deletes,
                    &lines,
                    next_block(line, &lines, path, text, resolver),
                    enclosing_block(line, &lines, path, text, resolver),
                ));
            }
        }
    }
    Ok(expanded)
}

fn is_block_op(op: &Op) -> bool {
    matches!(
        op,
        Op::Put {
            anchor: Anchor::Block(_),
            ..
        }
    )
}

/// The first block starting below `line`, for a blank-line anchor.
fn next_block(
    line: usize,
    lines: &[&str],
    path: &str,
    text: &str,
    resolver: BlockResolver<'_>,
) -> Option<BlockSpan> {
    let last = lines.len().min(line + SUGGESTION_SCAN_LIMIT);
    for candidate in (line + 1)..=last {
        if lines.get(candidate - 1)?.trim().is_empty() {
            continue;
        }
        let span = resolver(path, text, candidate)?;
        if span.start == candidate && span.end > candidate {
            return Some(span);
        }
    }
    None
}

/// The nearest block that begins above `line` and encloses it.
fn enclosing_block(
    line: usize,
    lines: &[&str],
    path: &str,
    text: &str,
    resolver: BlockResolver<'_>,
) -> Option<BlockSpan> {
    let first = line.saturating_sub(SUGGESTION_SCAN_LIMIT).max(1);
    for candidate in (first..line).rev() {
        if lines
            .get(candidate - 1)
            .is_some_and(|l| l.trim().is_empty())
        {
            continue;
        }
        if let Some(span) = resolver(path, text, candidate)
            && span.start == candidate
            && span.end >= line
            && span.end > candidate
        {
            return Some(span);
        }
    }
    None
}

fn form(line: usize, deletes: bool) -> String {
    if deletes {
        format!("CUT {line}*")
    } else {
        format!("PUT {line}*:")
    }
}

fn plain_form(line: usize, deletes: bool) -> String {
    if deletes {
        format!("CUT {line}")
    } else {
        format!("PUT {line}:")
    }
}

fn single_line_message(line: usize, deletes: bool, enclosing: Option<BlockSpan>) -> String {
    let mut message = format!(
        "`{}` resolved a single-line block: line {line} is a bare statement, not the \
         opening line of a multi-line construct. For only this statement use `{}`.",
        form(line, deletes),
        plain_form(line, deletes)
    );
    if let Some(block) = enclosing {
        message.push_str(&format!(
            " The nearest enclosing multi-line block begins at line {} and ends at line \
             {}; use `{}` to target it.",
            block.start,
            block.end,
            form(block.start, deletes)
        ));
    }
    message
}

fn unresolved_message(
    line: usize,
    deletes: bool,
    lines: &[&str],
    next: Option<BlockSpan>,
    enclosing: Option<BlockSpan>,
) -> String {
    let anchor_text = lines.get(line - 1);
    let mut message = match (anchor_text, next) {
        (Some(text), Some(block)) if text.trim().is_empty() => format!(
            "Line {line} is blank; no syntactic block can begin there. The next \
             multi-line block begins at line {} and ends at line {}. Retry `{}`.",
            block.start,
            block.end,
            form(block.start, deletes)
        ),
        _ => format!(
            "`{}` could not resolve a syntactic block beginning on line {line} \
             (unsupported language, blank or closing line, or a parse error). Use \
             `{}` with explicit lines.",
            form(line, deletes),
            if deletes {
                format!("CUT {line}.=M")
            } else {
                format!("PUT {line}.=M:")
            }
        ),
    };
    if let Some(block) = enclosing {
        message.push_str(&format!(
            " The nearest enclosing multi-line block begins at line {} and ends at line \
             {}; use `{}` to target it.",
            block.start,
            block.end,
            form(block.start, deletes)
        ));
    }
    if let Some(context) = anchored_context(line, lines) {
        message.push_str("\n\n");
        message.push_str(&context);
    }
    message
}

/// A few lines either side of the anchor, so the model can see what it hit.
///
/// The anchor is marked with `*`, because the useful realisation is usually
/// "that is not the line I meant" rather than anything about blocks.
fn anchored_context(line: usize, lines: &[&str]) -> Option<String> {
    if line == 0 || line > lines.len() {
        return None;
    }
    let first = line.saturating_sub(2).max(1);
    let last = (line + 2).min(lines.len());
    let rendered: Vec<String> = (first..=last)
        .map(|candidate| {
            let marker = if candidate == line { '*' } else { ' ' };
            format!("{marker}{candidate}:{}", lines[candidate - 1])
        })
        .collect();
    Some(rendered.join("\n"))
}

#[cfg(test)]
#[path = "blocks_tests.rs"]
mod blocks_tests;
