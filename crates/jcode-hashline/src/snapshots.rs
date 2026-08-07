//! Per-session store binding a section tag to the exact file content that
//! minted it, plus the lines a producer actually displayed.
//!
//! Ported from oh-my-pi's `snapshots.ts`, with one deliberate divergence: this
//! store is safe under concurrent access. omp's tool calls are sequential, so
//! theirs is an unsynchronised map that mutates `seenLines` in place. jcode's
//! `batch` tool drives sub-calls on a `FuturesUnordered` that share one
//! `session_id`, so several `read`s of one file can land at once. An
//! unsynchronised port would silently lose one read's provenance and then
//! reject an edit the model was entitled to make.
//!
//! # Why `seenLines` exists
//!
//! A tag proves the file has not changed. It does not prove the model ever saw
//! the lines it is editing: a read of lines 1-50 mints a tag for the whole
//! file, and an anchor at line 900 would validate against it. Recording what
//! each producer displayed lets the patcher refuse edits to unseen lines, which
//! is the failure mode where a model edits from memory and mangles a file.
//!
//! Absent provenance means "not recorded", not "nothing seen", so a producer
//! that does not yet record degrades to the old behaviour instead of blocking.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::format::compute_file_hash;

/// Default number of distinct paths tracked before the coldest is evicted.
pub const DEFAULT_MAX_PATHS: usize = 30;
/// Default number of full-file versions retained per path.
pub const DEFAULT_MAX_VERSIONS_PER_PATH: usize = 4;

/// One observed full-file version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Canonical path this version belongs to.
    pub path: String,
    /// Full normalized text as observed.
    pub text: String,
    /// Content tag for `text`.
    pub hash: String,
    /// 1-indexed lines a producer displayed under this tag.
    ///
    /// `None` means no provenance was recorded, and the seen-line guard is
    /// skipped. An empty set means a producer recorded that it displayed
    /// nothing, which is different and does block.
    pub seen_lines: Option<BTreeSet<usize>>,
}

/// Bounds on retention.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotStoreOptions {
    pub max_paths: usize,
    pub max_versions_per_path: usize,
}

impl Default for SnapshotStoreOptions {
    fn default() -> Self {
        Self {
            max_paths: DEFAULT_MAX_PATHS,
            max_versions_per_path: DEFAULT_MAX_VERSIONS_PER_PATH,
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    /// Per-path version history, newest first.
    versions: HashMap<String, Vec<Snapshot>>,
    /// Paths in least-recently-used order, coldest first.
    recency: Vec<String>,
}

/// A bounded, concurrency-safe snapshot store.
///
/// Cloning shares one store, so a `batch` sub-call and its parent see the same
/// provenance. Scope one per session: keying by session is what makes a
/// subagent's reads grant its parent nothing, structurally rather than by
/// convention.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    inner: Arc<Mutex<Inner>>,
    options: SnapshotStoreOptions,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::with_options(SnapshotStoreOptions::default())
    }

    pub fn with_options(options: SnapshotStoreOptions) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            options,
        }
    }

    /// Record `text` for `path` and return its tag.
    ///
    /// Recording byte-identical content again refreshes recency, promotes that
    /// version to head, and unions any newly displayed lines, so two partial
    /// reads of one file state widen a single snapshot instead of creating two.
    ///
    /// Deduplication requires **full-text equality, not tag equality**. Two
    /// texts can collide on 16 bits; fusing them would attribute one's seen
    /// lines to the other and let the patcher resolve a tag to the wrong text.
    pub fn record(
        &self,
        path: &str,
        text: &str,
        seen_lines: Option<&[usize]>,
    ) -> String {
        let hash = compute_file_hash(text);
        let mut inner = self.inner.lock().expect("snapshot store poisoned");

        Self::touch_recency(&mut inner, path);

        let history = inner.versions.entry(path.to_string()).or_default();
        if let Some(index) = history
            .iter()
            .position(|version| version.hash == hash && version.text == text)
        {
            let mut existing = history.remove(index);
            merge_seen_lines(&mut existing.seen_lines, seen_lines);
            history.insert(0, existing);
        } else {
            let mut snapshot = Snapshot {
                path: path.to_string(),
                text: text.to_string(),
                hash: hash.clone(),
                seen_lines: None,
            };
            merge_seen_lines(&mut snapshot.seen_lines, seen_lines);
            history.insert(0, snapshot);
            history.truncate(self.options.max_versions_per_path);
        }

        Self::evict_cold_paths(&mut inner, self.options.max_paths);
        hash
    }

    /// Merge displayed lines into the version with this tag, if retained.
    ///
    /// Lets a producer attach provenance after the tag was minted, which is the
    /// normal case when the body is formatted after hashing.
    pub fn record_seen_lines(&self, path: &str, hash: &str, lines: &[usize]) {
        let mut inner = self.inner.lock().expect("snapshot store poisoned");
        if let Some(history) = inner.versions.get_mut(path)
            && let Some(version) = history.iter_mut().find(|version| version.hash == hash)
        {
            merge_seen_lines(&mut version.seen_lines, Some(lines));
        }
    }

    /// Most recently recorded version for `path`.
    pub fn head(&self, path: &str) -> Option<Snapshot> {
        let inner = self.inner.lock().expect("snapshot store poisoned");
        inner.versions.get(path)?.first().cloned()
    }

    /// Retained version for `path` whose tag equals `hash`.
    ///
    /// On a collision this returns the most recently recorded of the colliding
    /// versions, because the tag alone cannot distinguish them. Callers that
    /// need certainty use [`by_content`](Self::by_content).
    pub fn by_hash(&self, path: &str, hash: &str) -> Option<Snapshot> {
        let inner = self.inner.lock().expect("snapshot store poisoned");
        inner
            .versions
            .get(path)?
            .iter()
            .find(|version| version.hash == hash)
            .cloned()
    }

    /// Retained version for `path` whose text equals `text` exactly.
    pub fn by_content(&self, path: &str, text: &str) -> Option<Snapshot> {
        let inner = self.inner.lock().expect("snapshot store poisoned");
        inner
            .versions
            .get(path)?
            .iter()
            .find(|version| version.text == text)
            .cloned()
    }

    /// Every retained version carrying `hash`, across all paths.
    ///
    /// Used to recover the intended file when a section names a path that does
    /// not exist but carries a tag this store minted, i.e. the model mistyped
    /// the path of a file it read.
    pub fn find_by_hash(&self, hash: &str) -> Vec<Snapshot> {
        let inner = self.inner.lock().expect("snapshot store poisoned");
        let mut matches: Vec<Snapshot> = inner
            .versions
            .values()
            .flatten()
            .filter(|version| version.hash == hash)
            .cloned()
            .collect();
        matches.sort_by(|a, b| a.path.cmp(&b.path));
        matches
    }

    /// Drop the history for one path.
    pub fn invalidate(&self, path: &str) {
        let mut inner = self.inner.lock().expect("snapshot store poisoned");
        inner.versions.remove(path);
        inner.recency.retain(|entry| entry != path);
    }

    /// Move history and provenance from `from` to `to`, so tags minted against
    /// a source path stay valid after a rename.
    pub fn relocate(&self, from: &str, to: &str) {
        let mut inner = self.inner.lock().expect("snapshot store poisoned");
        let Some(source) = inner.versions.remove(from) else {
            return;
        };
        inner.recency.retain(|entry| entry != from);
        if source.is_empty() {
            return;
        }

        let relocated: Vec<Snapshot> = source
            .into_iter()
            .map(|version| Snapshot {
                path: to.to_string(),
                ..version
            })
            .collect();

        let max_versions = self.options.max_versions_per_path;
        let merged = match inner.versions.remove(to) {
            None => relocated,
            Some(destination) => {
                let mut seen: Vec<String> = Vec::new();
                let mut merged = Vec::new();
                for version in relocated.into_iter().chain(destination) {
                    if seen.contains(&version.hash) {
                        continue;
                    }
                    seen.push(version.hash.clone());
                    merged.push(version);
                }
                merged.truncate(max_versions);
                merged
            }
        };

        inner.versions.insert(to.to_string(), merged);
        Self::touch_recency(&mut inner, to);
        Self::evict_cold_paths(&mut inner, max_versions.max(self.options.max_paths));
    }

    /// Drop every history.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().expect("snapshot store poisoned");
        inner.versions.clear();
        inner.recency.clear();
    }

    fn touch_recency(inner: &mut Inner, path: &str) {
        inner.recency.retain(|entry| entry != path);
        inner.recency.push(path.to_string());
    }

    fn evict_cold_paths(inner: &mut Inner, max_paths: usize) {
        while inner.recency.len() > max_paths {
            let coldest = inner.recency.remove(0);
            inner.versions.remove(&coldest);
        }
    }
}

/// Union `lines` into a snapshot's provenance, creating the set on first use.
///
/// `None` in, no change: a producer that records nothing must not turn absent
/// provenance into an empty set, because the two mean different things to the
/// seen-line guard.
fn merge_seen_lines(target: &mut Option<BTreeSet<usize>>, lines: Option<&[usize]>) {
    let Some(lines) = lines else {
        return;
    };
    let set = target.get_or_insert_with(BTreeSet::new);
    set.extend(lines.iter().copied());
}

#[cfg(test)]
#[path = "snapshots_tests.rs"]
mod snapshots_tests;
