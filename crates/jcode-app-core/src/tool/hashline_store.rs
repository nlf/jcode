//! Per-session hashline snapshot stores.
//!
//! `read` records what it showed the model; `edit` looks that up to resolve a
//! `[path#TAG]` header. The two are separate tool invocations with no shared
//! value between them, so the provenance has to live somewhere both can reach.
//!
//! # Why a global registry rather than a field on `ToolContext`
//!
//! `ToolContext` is constructed in 31 places across the workspace, most of them
//! tests. Threading a store through every one of them is a large diff whose
//! every line is mechanical, and it would force each test to decide what to
//! pass. The `Bus::global()` precedent in `jcode-base` solves the same problem
//! the same way: tools reach a process-wide singleton and key by session.
//!
//! Keying by `session_id` is what keeps concurrent sessions from seeing each
//! other's snapshots. Within a session, `SnapshotStore` is already
//! concurrency-safe (`Arc<Mutex>`), which matters because `batch` runs tools
//! through `FuturesUnordered` under one `session_id`.

use jcode_hashline::SnapshotStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Sessions whose stores we retain. A store is bounded internally (30 paths, 4
/// versions each), but the map itself would otherwise grow once per session for
/// the life of the process: a long-lived daemon serves many sessions.
///
/// Eviction is oldest-inserted-first rather than LRU. The cost of evicting a
/// live session is one stale-tag error that tells the model to re-read, so the
/// simpler policy is worth more than the precision.
const MAX_SESSIONS: usize = 64;

struct Registry {
    stores: HashMap<String, Arc<SnapshotStore>>,
    /// Insertion order, for eviction. Parallel to `stores`' keys.
    order: Vec<String>,
}

fn registry() -> &'static Mutex<Registry> {
    static INSTANCE: OnceLock<Mutex<Registry>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        Mutex::new(Registry {
            stores: HashMap::new(),
            order: Vec::new(),
        })
    })
}

/// The snapshot store for a session, creating it on first use.
///
/// Returns an `Arc` rather than a guard so callers hold no lock across the file
/// I/O that follows: `read` mints a tag and then writes a large string.
pub fn for_session(session_id: &str) -> Arc<SnapshotStore> {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(store) = registry.stores.get(session_id) {
        return Arc::clone(store);
    }

    while registry.order.len() >= MAX_SESSIONS {
        let evicted = registry.order.remove(0);
        registry.stores.remove(&evicted);
    }

    let store = Arc::new(SnapshotStore::new());
    registry
        .stores
        .insert(session_id.to_string(), Arc::clone(&store));
    registry.order.push(session_id.to_string());
    store
}

/// Drop a session's snapshots. Called when a session ends.
pub fn forget_session(session_id: &str) {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.stores.remove(session_id);
    registry.order.retain(|id| id != session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessions are the isolation boundary: one session must never resolve a
    /// tag another session minted, or an edit lands in a file this session
    /// never read.
    #[test]
    fn separate_sessions_get_separate_stores() {
        let a = for_session("registry-iso-a");
        let b = for_session("registry-iso-b");

        let tag = a.record("f.txt", "one\n", None);

        assert!(a.by_hash("f.txt", &tag).is_some());
        assert!(
            b.by_hash("f.txt", &tag).is_none(),
            "a tag minted in one session resolved in another"
        );
    }

    /// Two calls in one session must share, or `edit` cannot see what `read`
    /// recorded, which is the entire point of the registry.
    #[test]
    fn the_same_session_gets_the_same_store() {
        let tag = for_session("registry-same").record("f.txt", "one\n", None);

        assert!(
            for_session("registry-same")
                .by_hash("f.txt", &tag)
                .is_some(),
            "a second lookup in one session missed the first call's snapshot"
        );
    }

    #[test]
    fn forgetting_a_session_drops_its_snapshots() {
        let store = for_session("registry-forget");
        let tag = store.record("f.txt", "one\n", None);
        forget_session("registry-forget");

        assert!(
            for_session("registry-forget")
                .by_hash("f.txt", &tag)
                .is_none(),
            "snapshots survived the session being forgotten"
        );
    }

    /// Without eviction the map grows once per session forever in a daemon.
    #[test]
    fn the_registry_evicts_rather_than_growing_without_bound() {
        for i in 0..(MAX_SESSIONS * 2) {
            for_session(&format!("registry-evict-{i}"));
        }

        let held = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stores
            .len();
        assert!(
            held <= MAX_SESSIONS,
            "registry held {held} sessions, above the {MAX_SESSIONS} cap"
        );
    }

    /// Eviction must drop the map entry and the order entry together. If only
    /// one is removed they drift, and the cap silently stops holding.
    #[test]
    fn eviction_keeps_the_order_list_and_the_map_in_step() {
        for i in 0..(MAX_SESSIONS + 10) {
            for_session(&format!("registry-step-{i}"));
        }

        let registry = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            registry.stores.len(),
            registry.order.len(),
            "map and order list disagree, so eviction is dropping one but not the other"
        );
    }
}
