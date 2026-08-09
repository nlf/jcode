//! Request correlation: matching responses to requests, and answering the
//! server's own questions.
//!
//! # The two id spaces, and why they need two maps
//!
//! This is the single most important structural decision in the crate, and it is
//! the one a plausible implementation gets wrong.
//!
//! JSON-RPC ids are allocated **independently by each peer**. We number our
//! requests from 1; the server numbers its own from wherever it likes. So a
//! server's `workspace/configuration` request can carry `id: 1` while our
//! `documentSymbol` request with `id: 1` is still in flight. Both are legal.
//!
//! A client with one pending map keyed on id will match the server's *request*
//! against our pending entry, resolve our request with a `method` field it cannot
//! use, and drop the configuration pull the server is blocked on. omp's comment
//! on this names the resulting symptom — a wedged lazy cold start — and their
//! regression test for it is the sharpest in their suite.
//!
//! The fix is not to key more carefully. It is to **classify on `method` first**:
//! anything with a `method` is server-originated, and only a message without one
//! can be an answer to us. [`crate::jsonrpc::decode`] does that, and this module
//! keeps the two paths physically separate so they cannot be confused later.
//!
//! # Why the pending map holds oneshot senders
//!
//! A request completes on the reader task and is awaited by a caller task, so the
//! two need a rendezvous. `oneshot` is exactly that, and dropping the sender
//! (which happens if the map is cleared) makes the receiver fail rather than hang
//! — the behaviour we want when a server dies with requests outstanding.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};

use crate::jsonrpc::{RequestId, ResponseError};

/// Why a request did not produce a result.
#[derive(Debug)]
pub enum RequestFailure {
    /// The server answered with an error.
    ///
    /// A *successful* exchange with a negative answer, not a transport failure.
    /// `-32601` in particular means the server is healthy and simply does not
    /// implement this, so the caller should stop asking rather than reconnect.
    Server(ResponseError),
    /// No answer within the deadline.
    ///
    /// Carries the method and the duration because "LSP request timed out" with
    /// neither is unactionable: a slow `rust-analyzer` cold start and a wedged
    /// server look identical without them.
    TimedOut { method: String, after: Duration },
    /// The connection went away before an answer arrived.
    ///
    /// `detail` carries the transport's reason and its stderr tail, which is
    /// usually the only explanation of a server that died at startup.
    Closed { method: String, detail: String },
    /// The request could not be written.
    Write { method: String, detail: String },
}

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(error) => write!(f, "LSP error: {error}"),
            Self::TimedOut { method, after } => {
                write!(f, "LSP request {method} timed out after {after:?}")
            }
            Self::Closed { method, detail } => {
                write!(f, "LSP request {method} failed: {detail}")
            }
            Self::Write { method, detail } => {
                write!(f, "LSP request {method} could not be sent: {detail}")
            }
        }
    }
}

impl std::error::Error for RequestFailure {}

impl RequestFailure {
    /// Whether this is the server saying "I do not implement that".
    ///
    /// Recognised by **code**, never by message wording: servers phrase it
    /// differently and matching prose silently stops working. Callers use this to
    /// fall back to another approach rather than to tear the client down.
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Server(error) if error.is_method_not_found())
    }
}

/// One outstanding request.
struct Pending {
    /// Kept so a failure can name the method. A bare id tells the caller nothing
    /// and tells a log reader less.
    method: String,
    respond: oneshot::Sender<Result<serde_json::Value, ResponseError>>,
}

/// The set of requests we are waiting on.
///
/// Only ever holds **our** requests. Server-originated requests never enter here;
/// see the module comment for why that separation is structural rather than
/// stylistic.
#[derive(Default)]
pub struct Pendings {
    /// `std::sync::Mutex` would be tempting and wrong: this is held across an
    /// `await` in `register`, and a std mutex guard is not `Send` across one.
    inner: Mutex<HashMap<RequestId, Pending>>,
    /// Monotonic, never reused within a connection. Reuse would let a late answer
    /// to an abandoned request resolve a new one holding the same id.
    next_id: AtomicI64,
}

impl Pendings {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            // From 1 rather than 0: `id: 0` is legal but reads as "unset" in logs
            // and in some servers' own diagnostics.
            next_id: AtomicI64::new(1),
        }
    }

    /// Allocate the next request id.
    pub fn next_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Record a request as outstanding, returning the channel to await.
    pub async fn register(
        &self,
        id: RequestId,
        method: &str,
    ) -> oneshot::Receiver<Result<serde_json::Value, ResponseError>> {
        let (respond, receive) = oneshot::channel();
        self.inner.lock().await.insert(
            id,
            Pending {
                method: method.to_string(),
                respond,
            },
        );
        receive
    }

    /// Deliver an answer, if the request is still outstanding.
    ///
    /// An unknown id is dropped rather than treated as an error. It means the
    /// request already timed out or was cancelled, and a late answer to something
    /// nobody is waiting for is not a fault worth reporting.
    pub async fn complete(&self, id: &RequestId, result: Result<serde_json::Value, ResponseError>) {
        if let Some(pending) = self.inner.lock().await.remove(id) {
            // A closed receiver means the caller gave up first. Nothing to do.
            let _ = pending.respond.send(result);
        }
    }

    /// Give up on a request, and report what it was.
    pub async fn forget(&self, id: &RequestId) -> Option<String> {
        self.inner
            .lock()
            .await
            .remove(id)
            .map(|pending| pending.method)
    }

    /// Fail every outstanding request with the same reason.
    ///
    /// Called when the connection dies. **Not optional**: without it every
    /// in-flight caller waits out its own timeout, so a server that died
    /// instantly still costs one timeout per request and reports the wrong cause.
    pub async fn fail_all(&self, detail: &str) {
        let drained: Vec<(RequestId, Pending)> = self.inner.lock().await.drain().collect();
        for (_, pending) in drained {
            let _ = pending.respond.send(Err(ResponseError {
                // 0 rather than a real JSON-RPC code: no server said this, we
                // did, and claiming a protocol code would be a lie a caller
                // could match on.
                code: 0,
                message: format!("{} ({detail})", pending.method),
                data: None,
            }));
        }
    }

    /// How many requests are outstanding. For tests and diagnostics.
    pub async fn outstanding(&self) -> usize {
        self.inner.lock().await.len()
    }
}

/// A server-originated request awaiting our answer.
///
/// Deliberately a distinct type from anything in [`Pendings`]. The two never mix,
/// and giving them different types means a future refactor cannot accidentally
/// route one into the other's map — which is the bug this module exists to
/// prevent.
#[derive(Debug)]
pub struct ServerRequest {
    pub id: RequestId,
    pub method: String,
    pub params: serde_json::Value,
}

/// Shared handle to the pending set.
pub type SharedPendings = Arc<Pendings>;

#[cfg(test)]
#[path = "correlation_tests.rs"]
mod correlation_tests;
