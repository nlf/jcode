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

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod syntax_tests;
