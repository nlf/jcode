//! One language server per project per language, shared and reused.
//!
//! # Why a registry rather than a client per call
//!
//! A cold `rust-analyzer` on a large workspace takes tens of seconds to become useful. Starting
//! one per tool call would make every navigation request pay that, so the first call would time
//! out and the second would too. The whole value of the client depends on outliving a single
//! call.
//!
//! # Lifetime: per project, with an idle timeout
//!
//! Keyed by (project root, server name). Two tool calls in one repository share a client; two
//! repositories do not, because a language server's whole model is scoped to a root and
//! pointing one at two trees is not a supported thing to do.
//!
//! **This inverts omp deliberately.** They default `idleTimeoutMs` to disabled, which is right
//! for a per-invocation CLI: the process exits and takes its servers with it. Our daemon is
//! long-lived, so "never idle out" means a `rust-analyzer` per project touched since boot,
//! resident forever. On a machine where someone has visited a dozen repositories that is a dozen
//! language servers holding their indexes in memory. So the timeout is **on by default** and
//! configurable, which is the one place the port should not follow its source.
//!
//! # What this deliberately does not do
//!
//! No mux, no cross-session sharing beyond the daemon's own process, no warmup on startup.
//! omp's `mux/` is 1,241 lines for sharing one server between separate processes, which we do
//! not need because our sessions already share a daemon.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::client::{Client, ServerSpec};
use crate::config::Available;
use crate::correlation::RequestFailure;

/// How long an unused client is kept before being shut down.
///
/// Two minutes: long enough that a sequence of related navigation calls reuses one server, short
/// enough that an abandoned project does not hold an index all day. A cold start costs seconds,
/// so re-paying it after two idle minutes is a worthwhile trade against the memory.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// A live client and when it was last used.
struct Entry {
    client: Arc<Client>,
    last_used: Instant,
}

/// The key: one server per language per project root.
type Key = (PathBuf, String);

/// Live clients, keyed by project and server.
pub struct Registry {
    entries: Mutex<HashMap<Key, Entry>>,
    idle_timeout: Option<Duration>,
}

impl Registry {
    /// A registry with the default idle timeout.
    pub fn new() -> Self {
        Self::with_idle_timeout(Some(DEFAULT_IDLE_TIMEOUT))
    }

    /// A registry with an explicit timeout, or `None` to keep clients forever.
    ///
    /// `None` is what omp defaults to. Available because a short-lived process genuinely does not
    /// want the reaping, and because a user who has asked for it should get it.
    pub fn with_idle_timeout(idle_timeout: Option<Duration>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            idle_timeout,
        }
    }

    /// Get the client for this server in this project, starting it if needed.
    ///
    /// # Concurrency
    ///
    /// The lock is held across the handshake, so two concurrent callers cannot start two
    /// `rust-analyzer`s for one project — omp has a regression test for exactly that (A2), and it
    /// is the reason this is not a get-then-insert.
    ///
    /// The cost is that a second caller waits out the first's cold start rather than starting its
    /// own. That is the right trade: two cold starts of the same server on the same tree is
    /// strictly worse than one, both in time and in memory, and the second would not finish
    /// sooner.
    pub async fn get_or_start(
        &self,
        root: &Path,
        server: &Available,
        timeout: Duration,
    ) -> Result<Arc<Client>, RequestFailure> {
        let key = (root.to_path_buf(), server.name.clone());
        let mut entries = self.entries.lock().await;

        // Reap first, so an idle client is not handed out and then immediately shut down. Also
        // the only place reaping happens: a background task would need a handle to this registry
        // and a shutdown path of its own, for a job that is only ever needed when someone asks
        // for a client.
        self.reap_idle(&mut entries).await;

        if let Some(entry) = entries.get_mut(&key) {
            // A wedged client is replaced rather than handed out. `unusable` means a partial
            // frame desynchronised the stream, so nothing sent to it will ever be understood --
            // handing it over would produce one failure per call until the timeout reaps it.
            if entry.client.unusable() {
                entries.remove(&key);
            } else {
                entry.last_used = Instant::now();
                return Ok(Arc::clone(&entry.client));
            }
        }

        let client = Client::start(
            ServerSpec {
                name: server.name.clone(),
                program: server.resolved.to_string_lossy().to_string(),
                args: server.config.args.clone(),
                root: root.to_path_buf(),
                env: Vec::new(),
                settings: server
                    .config
                    .settings
                    .clone()
                    .unwrap_or(serde_json::json!({})),
                init_options: server
                    .config
                    .init_options
                    .clone()
                    .unwrap_or(serde_json::json!({})),
            },
            timeout,
        )
        .await?;

        let client = Arc::new(client);
        entries.insert(
            key,
            Entry {
                client: Arc::clone(&client),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    /// Shut down and forget clients idle longer than the timeout.
    async fn reap_idle(&self, entries: &mut HashMap<Key, Entry>) {
        let Some(timeout) = self.idle_timeout else {
            return;
        };
        let now = Instant::now();
        // Collected first because `shutdown` needs `&mut Client`, which means taking the entry
        // out of the map; retaining in place would leave the map borrowed.
        let expired: Vec<Key> = entries
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.last_used) >= timeout)
            .map(|(key, _)| key.clone())
            .collect();

        for key in expired {
            if let Some(entry) = entries.remove(&key) {
                // A client still held by a caller mid-request must not be torn down under it, so
                // shutdown only happens when this registry holds the last reference. If it does
                // not, the entry is dropped from the map and the client dies with its last
                // holder -- which is the same outcome, just later.
                if let Ok(mut client) = Arc::try_unwrap(entry.client) {
                    client.shutdown().await;
                }
            }
        }
    }

    /// How many clients are live. For tests and for `status`.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Whether any client is live.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Shut every client down.
    ///
    /// For daemon shutdown. A language server that outlives its client is the "daemon leak with
    /// no symptom" that `Transport::wait_for_exit` documents, so this is not optional at exit.
    pub async fn shutdown_all(&self) {
        let mut entries = self.entries.lock().await;
        for (_, entry) in entries.drain() {
            if let Ok(mut client) = Arc::try_unwrap(entry.client) {
                client.shutdown().await;
            }
        }
    }

    /// Force the idle sweep, for tests.
    ///
    /// Exposed because the reaping is otherwise only reachable through `get_or_start`, and a test
    /// for "an idle client is reaped" should not have to start a second server to observe the
    /// first being collected.
    pub async fn reap_now(&self) {
        let mut entries = self.entries.lock().await;
        self.reap_idle(&mut entries).await;
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

// Tests live in `tests/registry.rs`: the registry manages real processes, and
// `CARGO_BIN_EXE_fake_lsp_server` is only set for integration targets.
