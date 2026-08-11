//! Asking whether a file parses, so a repair cannot claim syntax that is not
//! there.
//!
//! Ported from oh-my-pi's `syntax.ts`. It exists for one caller: the hashline
//! boundary-repair rules that want to spare a deleted `}` on the grounds that
//! it closes a block. Counting delimiters cannot tell that `}` apart from one
//! inside a regex literal, a string, or a sentence of prose, and a repair that
//! gets it wrong writes a brace into a file that did not want one.
//!
//! # `false` withholds permission, it never accuses
//!
//! The single most important property, and the one easiest to lose in a
//! refactor: `false` means "this probe has nothing to prove with". It covers a
//! file that genuinely does not parse, a language nothing here recognises, and
//! a path with no useful extension, all deliberately conflated. A caller may
//! use `true` as evidence that an edit kept a file intact. A caller must never
//! read `false` as evidence *about* the edit, because most of the time it says
//! only that we cannot tell.
//!
//! That asymmetry is what makes the probe safe to consult for a rewrite. It can
//! withhold permission to repair; it can never demand one.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_core::{AstGrep, Node};
use ast_grep_language::SupportLang;

use crate::matching::language_for_path;

/// Parses retained before the coldest is dropped.
///
/// The repair layer probes the same text repeatedly: once for the file as the
/// author wrote it, then once per candidate repair. Parsing is the whole cost
/// of this call, so a small cache turns a quadratic-feeling loop back into a
/// handful of parses.
const CACHE_CAPACITY: usize = 256;

/// A cache key: the language, plus a hash and length of the text.
///
/// **Not the text itself.** Keying on the content made every entry as large as
/// the file it described, and 256 entries of ordinary source came to well over
/// a hundred megabytes held live for the sake of skipping a few parses. A
/// 64-bit hash beside the length is enough to tell candidates apart, and the
/// cost of the rare collision is bounded: two different texts would have to
/// agree on both, and the worst outcome is one wrong verdict on a repair that
/// is checked again by the applier.
type CacheKey = (&'static str, u64, usize);

type Cache = HashMap<CacheKey, bool>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn text_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// True when `text` parses without a syntax error, under the language `path`
/// implies.
///
/// See the module note before using the `false` case for anything.
pub fn parses_cleanly(path: &str, text: &str) -> bool {
    let Some(language) = language_for_path(path) else {
        // Nothing recognises this extension, so there is no opinion to offer.
        return false;
    };

    let key = (language_key(language), text_hash(text), text.len());
    if let Ok(cache) = cache().lock()
        && let Some(cached) = cache.get(&key)
    {
        return *cached;
    }

    let parsed = parse_is_clean(text, language);

    if let Ok(mut cache) = cache().lock() {
        // Evicting an arbitrary entry rather than the oldest. Tracking recency
        // would cost more than it saves here: every entry is one parse, they
        // are interchangeable, and the working set of a repair pass is a few
        // versions of one file.
        if cache.len() >= CACHE_CAPACITY
            && let Some(victim) = cache.keys().next().copied()
        {
            cache.remove(&victim);
        }
        cache.insert(key, parsed);
    }

    parsed
}

/// A stable name for a language, usable as a `'static` key.
fn language_key(language: SupportLang) -> &'static str {
    // `SupportLang` is a fixed enum, so leaking one string per variant is
    // bounded by the number of languages and happens at most once each.
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let names = NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let rendered = format!("{language:?}");
    let mut names = names.lock().expect("language name table poisoned");
    if let Some(name) = names.get(&rendered) {
        return name;
    }
    let leaked: &'static str = Box::leak(rendered.clone().into_boxed_str());
    names.insert(rendered, leaked);
    leaked
}

/// Walk the tree looking for a node tree-sitter flagged as broken.
///
/// Tree-sitter always returns a tree, so there is no parse failure to catch. It
/// records trouble in the tree instead: an `ERROR` node where it could not make
/// sense of the input, and a `MISSING` node where it inserted something absent
/// in order to keep going. Either means the text is not valid in this language.
fn parse_is_clean(text: &str, language: SupportLang) -> bool {
    let doc = AstGrep::new(text, language);
    !subtree_has_error(doc.root())
}

fn subtree_has_error(node: Node<'_, StrDoc<SupportLang>>) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    node.children().any(subtree_has_error)
}

/// The lines a syntactic construct occupies, 1-indexed and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSpan {
    pub start: usize,
    pub end: usize,
}

/// The syntactic construct beginning on line `line`, if one does.
///
/// This is what makes `PUT 5*:` mean "replace the function starting at line 5"
/// without the model having to count to its closing brace. The counting is the
/// error-prone part, and it is the part a parser can simply be asked.
///
/// # Which node, when several begin on the same line
///
/// Every ancestor of a construct tends to start where it starts: at line 1 of a
/// file whose first line is `fn a() {`, the `source_file`, the `function_item`
/// and its name all begin there. The one that is useful is the **largest**
/// construct starting on that line and ending on a later one, because that is
/// what a person means by "the block at line 5". Taking the smallest would
/// resolve an `if/else` to its first branch and orphan the `else`.
///
/// With one exception, and it is the whole reason this is not a one-liner: a
/// node covering the entire file is a wrapper, not a block within it. Braces
/// languages have exactly one such node, the root, but YAML nests several
/// (`document`, `block_node`, `block_mapping`) that each span everything, and
/// taking the largest there resolves `PUT 1*:` to the file rather than to the
/// key the model named. So the largest node *smaller than the whole file* wins,
/// and only when there is none at all does a file-spanning node get used, which
/// is what still lets a file containing one function resolve that function.
///
/// `None` when the language is unknown, the line is blank or out of range, the
/// file does not parse, or nothing multi-line begins there. Every one of those
/// is a refusal to guess, and the caller reports it rather than editing.
pub fn block_at(path: &str, text: &str, line: usize) -> Option<BlockSpan> {
    let language = language_for_path(path)?;
    if line == 0 {
        return None;
    }
    // A block anchored inside a file that does not parse is not trustworthy:
    // tree-sitter recovers from errors by inventing structure, and the span it
    // reports may cover nothing the author would recognise.
    let doc = AstGrep::new(text, language);
    let root = doc.root();
    if subtree_has_error(root.clone()) {
        return None;
    }
    let last_line = text.lines().count();

    // Largest that is not the whole file, and separately largest of any, so a
    // file-spanning node can still be the answer when it is the only one.
    let mut best_within: Option<BlockSpan> = None;
    let mut best_any: Option<BlockSpan> = None;
    let mut stack: Vec<Node<'_, StrDoc<SupportLang>>> = root.children().collect();
    while let Some(node) = stack.pop() {
        let start = node.start_pos().line() + 1;
        let end = node.end_pos().line() + 1;
        // Some grammars end a node at column 0 of the *following* line rather
        // than at the end of its own. Counting that line would make a YAML
        // mapping claim the sibling key below it.
        let end = if node.end_pos().column(&node) == 0 && end > start {
            end - 1
        } else {
            end
        };
        if start == line && end > start {
            let span = BlockSpan { start, end };
            if best_any.is_none_or(|current| end > current.end) {
                best_any = Some(span);
            }
            let whole_file = start == 1 && end >= last_line;
            if !whole_file && best_within.is_none_or(|current| end > current.end) {
                best_within = Some(span);
            }
        }
        // Only descend where the answer can still be: a node that ends before
        // the target line cannot contain one starting on it.
        for child in node.children() {
            if child.end_pos().line() + 1 >= line {
                stack.push(child);
            }
        }
    }
    best_within.or(best_any)
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod syntax_tests;

