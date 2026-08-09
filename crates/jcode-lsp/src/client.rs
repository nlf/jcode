//! The client: lifecycle, requests, and answering the server's questions.
//!
//! Ties [`crate::transport`] and [`crate::correlation`] together into something
//! that speaks LSP. Everything above this is a tool adapter's problem.
//!
//! # Why the server's questions must be answered promptly
//!
//! A language server that asks the client something **blocks until answered**,
//! and several block *semantic requests* behind the answer. So an unanswered
//! `workspace/configuration` pull does not degrade the session, it wedges it: the
//! server is waiting on us, we are waiting on the server, and the symptom is a
//! `definition` request that times out on a healthy process.
//!
//! omp's handler set exists because of that, and this is a port of it. Two of its
//! choices are worth naming because they look arbitrary and are not:
//!
//! - **Unknown server requests get `-32601`, not silence.** The spec requires an
//!   answer, and a server told "method not found" moves on. A server told nothing
//!   waits.
//! - **`refresh` requests get a `null` answer**, not an error. They are void
//!   acknowledgements; omp's comment ties stalling on them to the same hang as
//!   dynamic registration (their #3029).
//!
//! # What is deliberately not here
//!
//! `workspace/applyEdit` is refused in v1. Honouring it means applying a
//! `WorkspaceEdit`, which is v2's work, and a half-applied edit is worse than a
//! refused one. The refusal is explicit (`applied: false` with a reason) rather
//! than an error, because that is the spec's way of saying no and servers handle
//! it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::freshness::{Observation, equivalent_uris};

use crate::correlation::{Answer, Pendings, RequestFailure, ServerRequest};
use crate::jsonrpc::{self, Incoming, METHOD_NOT_FOUND, RequestId, ResponseError};
use crate::transport::{FromServer, Transport};

/// Default per-request deadline when the caller names none.
///
/// Exists only so a request cannot leak forever. A caller with an opinion should
/// pass one, and the tool layer always will: 30 seconds is far too long to make a
/// user wait and far too short for a cold `rust-analyzer` on a large workspace,
/// which is exactly why it is a fallback and not a policy.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadline for a single write.
///
/// Bounds the wedged-stdin case. Generous because a healthy write completes in
/// microseconds, so anything approaching this is already pathological.
const WRITE_DEADLINE: Duration = Duration::from_secs(5);

/// Deadline for a best-effort `$/cancelRequest`.
///
/// Much shorter than [`WRITE_DEADLINE`] because the caller's own timeout has
/// already expired by the time we send it. A courtesy notification must not add
/// seconds to a failure that has already been decided, and the server it is aimed
/// at is precisely the one likely to be wedged.
const CANCEL_DEADLINE: Duration = Duration::from_millis(250);

/// How long `shutdown` may take before we stop being polite.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the process to actually exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(1);

/// Server capabilities, kept as raw JSON.
///
/// Deliberately not modelled as a struct. Capabilities are open-ended, servers
/// add their own, and the questions we ask of them are all "did you advertise
/// X" — which a `Value` answers without a type that goes stale every LSP release.
pub type Capabilities = Value;

/// A live connection to one language server.
pub struct Client {
    /// The server's name, for messages. Not a protocol value.
    name: String,
    /// Workspace root. Sent in `initialize` and answered to
    /// `workspace/workspaceFolders`.
    root: std::path::PathBuf,
    /// Settings answered to `workspace/configuration`, keyed by section.
    settings: Value,
    transport: Transport,
    pendings: Arc<Pendings>,
    /// What the server said it can do, available after `initialize`.
    capabilities: Arc<Mutex<Capabilities>>,
    /// Dynamic registrations, by registration id.
    ///
    /// Separate from `capabilities` because they arrive later and are removable.
    /// A server may advertise nothing statically and register everything
    /// dynamically, so a client checking only static capabilities concludes it
    /// can do nothing.
    dynamic: Arc<Mutex<HashMap<String, String>>>,
    /// Open documents and their versions, so `didChange` numbering is right and a
    /// second `didOpen` for one URI can be suppressed.
    open: Arc<Mutex<HashMap<String, i64>>>,
    /// Latest published diagnostics per URI, with the version they describe.
    diagnostics: Arc<Mutex<HashMap<String, PublishedDiagnostics>>>,
    /// Bumped on **every** publish, for any URI.
    ///
    /// Load-bearing for [`crate::freshness`]: "the same publish" and "a fresh
    /// publish of identical content" are different events, and only the second
    /// means the server has re-analysed. Comparing the diagnostics themselves
    /// cannot tell them apart, because an unchanged file republishes an identical
    /// list — so without this counter the settle window never restarts and a stale
    /// publish is accepted.
    generation: Arc<AtomicU64>,
}

/// Diagnostics as published, with their document version.
#[derive(Debug, Clone)]
pub struct PublishedDiagnostics {
    pub diagnostics: Vec<Value>,
    /// The document version these describe, when the server said.
    ///
    /// `None` is common and load-bearing: many servers never echo a version, so
    /// freshness cannot be established by matching and has to be settled by
    /// quiescence instead.
    pub version: Option<i64>,
    /// The URI exactly as the server spelled it.
    ///
    /// The map is keyed by the *normalized* URI so that two spellings of one file
    /// cannot coexist as separate entries, but a later request has to echo back the
    /// spelling the server actually used. So the spelling is kept here rather than in
    /// the key, which is how omp splits it too.
    pub uri: String,
}

/// Everything needed to start one server.
///
/// A struct rather than eight positional parameters, which clippy objected to and
/// was right about: `&[String]` for args and `&[(String, String)]` for env are
/// adjacent and swappable at a call site, and two `Value`s for settings and
/// init-options are worse. Named fields make a wrong call site a compile error
/// instead of a server that starts unconfigured.
pub struct ServerSpec {
    /// For messages, not a protocol value.
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    /// Workspace root: `rootUri`, workspace folders, and the process cwd.
    pub root: std::path::PathBuf,
    pub env: Vec<(String, String)>,
    /// Answered to `workspace/configuration` and pushed via
    /// `didChangeConfiguration`.
    pub settings: Value,
    /// Sent as `initializationOptions` in the handshake.
    pub init_options: Value,
}

impl Client {
    /// Start a server and complete the LSP handshake.
    ///
    /// The order is fixed by the spec and by what servers actually tolerate:
    /// `initialize` → store capabilities → `initialized` →
    /// `workspace/didChangeConfiguration`. Sending configuration before
    /// `initialized` is a violation some servers reject; sending a semantic
    /// request before configuration means the first one runs unconfigured, which
    /// omp records as their #5276.
    pub async fn start(spec: ServerSpec, timeout: Duration) -> Result<Self, RequestFailure> {
        let (transport, rx) = Transport::spawn(&spec.program, &spec.args, &spec.root, &spec.env)
            .map_err(|error| RequestFailure::Write {
                method: "spawn".to_string(),
                detail: format!("could not start {}: {error}", spec.program),
            })?;

        let client = Self {
            name: spec.name,
            root: spec.root.clone(),
            settings: spec.settings,
            transport,
            pendings: Arc::new(Pendings::new()),
            capabilities: Arc::new(Mutex::new(json!({}))),
            dynamic: Arc::new(Mutex::new(HashMap::new())),
            open: Arc::new(Mutex::new(HashMap::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
        };

        client.spawn_router(rx);

        let capabilities = client
            .request(
                "initialize",
                json!({
                    // Our pid, so the server can exit if we die without saying so.
                    "processId": std::process::id(),
                    "rootUri": path_to_uri(&spec.root),
                    "workspaceFolders": [workspace_folder(&spec.root)],
                    "capabilities": client_capabilities(),
                    "initializationOptions": spec.init_options,
                }),
                timeout,
            )
            .await?;

        *client.capabilities.lock().await = capabilities
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));

        client.notify("initialized", json!({})).await?;
        // Pushed after `initialized` rather than relied on being pulled: a server
        // that does not pull configuration would otherwise never receive it.
        client
            .notify(
                "workspace/didChangeConfiguration",
                json!({"settings": client.settings.clone()}),
            )
            .await?;

        Ok(client)
    }

    /// The routing task: everything the server says goes through here.
    ///
    /// One task, so ordering is preserved and there is exactly one place that
    /// decides what a message is. The alternative — routing at each call site —
    /// is how the two id spaces get confused.
    fn spawn_router(&self, mut rx: mpsc::UnboundedReceiver<FromServer>) {
        let pendings = Arc::clone(&self.pendings);
        let diagnostics = Arc::clone(&self.diagnostics);
        let generation = Arc::clone(&self.generation);
        let dynamic = Arc::clone(&self.dynamic);
        let answers = self.answer_channel();
        let name = self.name.clone();
        let settings = self.settings.clone();
        // The router answers `workspace/workspaceFolders`, so it needs the root.
        // Clippy caught this as an unused field, which it was: without it that
        // request fell through to `-32601`, and a server told "method not found"
        // for folders falls back to guessing a root, then resolves imports
        // against the wrong tree.
        let root = self.root.clone();

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    FromServer::Message(body) => {
                        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
                            // Malformed JSON in a well-framed message is the
                            // peer's problem. Skip it; later messages are still
                            // well-framed.
                            continue;
                        };
                        let Ok(incoming) = jsonrpc::decode(value) else {
                            continue;
                        };
                        match incoming {
                            // Ours. Note this arm can only be reached by a
                            // message with no `method`, which is what keeps the
                            // id spaces apart.
                            Incoming::Response { id, result } => {
                                pendings.complete(&id, result).await;
                            }
                            Incoming::Notification { method, params } => {
                                handle_notification(&method, params, &diagnostics, &generation)
                                    .await;
                            }
                            Incoming::Request { id, method, params } => {
                                let request = ServerRequest { id, method, params };
                                handle_server_request(
                                    request, &dynamic, &settings, &root, &answers,
                                )
                                .await;
                            }
                        }
                    }
                    // Survivable, and worth knowing about: it means something is
                    // printing to stdout.
                    FromServer::Junk { .. } => continue,
                    FromServer::Closed { reason, stderr } => {
                        // Every in-flight request fails now, with the cause.
                        // Without this each waits out its own timeout and reports
                        // a timeout instead of the real reason.
                        let detail = if stderr.trim().is_empty() {
                            format!("{name}: {reason}")
                        } else {
                            format!("{name}: {reason} (stderr: {})", stderr.trim())
                        };
                        pendings.fail_all(&detail).await;
                        return;
                    }
                }
            }
            // The channel closed without a `Closed` event, which means the
            // transport was dropped. Still must not leave callers hanging.
            pendings.fail_all("the LSP transport was dropped").await;
        });
    }

    /// A channel the router uses to send answers back to the server.
    ///
    /// Indirection rather than sharing the transport, because the router task
    /// cannot hold a reference into `self` while `self` is also used by callers.
    /// A dedicated writer task owns the sending half.
    fn answer_channel(&self) -> mpsc::UnboundedSender<Value> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let stdin = self.transport.writer();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let Ok(body) = serde_json::to_vec(&message) else {
                    continue;
                };
                // A failed answer is logged by the caller's absence of progress,
                // not here: this task has nowhere to report. Dropping is right
                // because the alternative is blocking the router.
                let _ = stdin.send(&body, WRITE_DEADLINE).await;
            }
        });
        tx
    }

    /// Send a request and await its answer.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RequestFailure> {
        let id = self.pendings.next_id();
        let receive = self.pendings.register(id.clone(), method).await;

        let body =
            serde_json::to_vec(&jsonrpc::request(&id, method, &params)).map_err(|error| {
                RequestFailure::Write {
                    method: method.to_string(),
                    detail: error.to_string(),
                }
            })?;

        if let Err(error) = self.transport.send(&body, WRITE_DEADLINE).await {
            // Remove the pending entry: nobody will ever answer a request that
            // was not sent, and leaving it would leak until the connection dies.
            self.pendings.forget(&id).await;
            let desynchronised = error.desynchronised();
            let failure = RequestFailure::Write {
                method: method.to_string(),
                detail: error.to_string(),
            };
            if desynchronised {
                // A partial frame reached the server, so the byte stream is
                // unusable: every later message would be misparsed. Fail everything
                // outstanding now, rather than letting each caller wait out its own
                // timeout against a connection that can never answer.
                //
                // The transport itself refuses further writes, so this is about
                // telling the waiters promptly rather than about preventing more
                // corruption.
                self.pendings
                    .fail_all(&format!(
                        "{}: the connection is desynchronised and must be restarted",
                        self.name
                    ))
                    .await;
            }
            return Err(failure);
        }

        match tokio::time::timeout(timeout, receive).await {
            Ok(Ok(Answer::Answered(Ok(result)))) => Ok(result),
            Ok(Ok(Answer::Answered(Err(error)))) => Err(RequestFailure::Server(error)),
            // The connection died. Reported as `Closed` with the transport's own reason,
            // not as `Server`: the latter's contract is that the server answered and is
            // healthy, which is the opposite of what happened.
            Ok(Ok(Answer::Closed(detail))) => Err(RequestFailure::Closed {
                method: method.to_string(),
                detail,
            }),
            // The sender was dropped without a value, which `fail_all` does not
            // do. Treat as a closed connection rather than reporting success.
            Ok(Err(_)) => Err(RequestFailure::Closed {
                method: method.to_string(),
                detail: "the connection ended before an answer arrived".to_string(),
            }),
            Err(_) => {
                self.pendings.forget(&id).await;
                // Tell the server to stop working on it. Advisory, and worth
                // sending: a server still computing an abandoned request is
                // spending our CPU.
                //
                // **Sent with a short deadline of its own, and detached.** The
                // caller's timeout has already expired, so making them wait a
                // further `WRITE_DEADLINE` for a courtesy notification turns a
                // 300ms timeout into a 5.3s one against exactly the wedged server
                // that caused it. The cancel is best-effort by nature, so a
                // background attempt is the honest shape.
                let writer = self.transport.writer();
                if let Ok(body) = serde_json::to_vec(&jsonrpc::notification(
                    "$/cancelRequest",
                    &json!({"id": id}),
                )) {
                    tokio::spawn(async move {
                        let _ = writer.send(&body, CANCEL_DEADLINE).await;
                    });
                }
                Err(RequestFailure::TimedOut {
                    method: method.to_string(),
                    after: timeout,
                })
            }
        }
    }

    /// Send a notification. Nothing answers it.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RequestFailure> {
        let body =
            serde_json::to_vec(&jsonrpc::notification(method, &params)).map_err(|error| {
                RequestFailure::Write {
                    method: method.to_string(),
                    detail: error.to_string(),
                }
            })?;
        self.transport
            .send(&body, WRITE_DEADLINE)
            .await
            .map_err(|error| RequestFailure::Write {
                method: method.to_string(),
                detail: error.to_string(),
            })
    }

    /// What the server advertised, statically.
    pub async fn capabilities(&self) -> Capabilities {
        self.capabilities.lock().await.clone()
    }

    /// Whether a capability is available, statically **or** by dynamic
    /// registration.
    ///
    /// Both must be consulted. A server may advertise nothing in `initialize` and
    /// register everything afterwards, so a client checking only the static set
    /// concludes the server can do nothing and stops asking.
    pub async fn supports(&self, capability: &str, method: &str) -> bool {
        let statically = {
            let capabilities = self.capabilities.lock().await;
            match capabilities.get(capability) {
                // `false` is a real "no", distinct from absent.
                Some(Value::Bool(supported)) => *supported,
                // An options object means yes, with detail we do not need here.
                Some(Value::Object(_)) => true,
                _ => false,
            }
        };
        if statically {
            return true;
        }
        self.dynamic
            .lock()
            .await
            .values()
            .any(|registered| registered == method)
    }

    /// How many requests are still awaiting an answer.
    ///
    /// Exists for tests. The invariant it checks is real but invisible from outside:
    /// a request that ends in a timeout must remove itself from the pending map, or
    /// the map grows for the life of the connection and a late answer resolves a
    /// caller that is already gone.
    ///
    /// Added because a reviewer pointed out three times that deleting the `forget` on
    /// the timeout arm left the whole suite green: the test that claimed to cover it
    /// only checked that a *later* request still worked, which it does either way. A
    /// leak with no symptom needs something that can see the leak.
    pub async fn outstanding(&self) -> usize {
        self.pendings.outstanding().await
    }

    /// Diagnostics last published for a URI.
    ///
    /// Matches by [`equivalent_uris`] rather than by string, because a server is free to
    /// echo a different spelling of the URI it was given: percent-encoded where we sent
    /// raw, a different drive-letter case, redundant path segments. omp keys their whole
    /// diagnostics map through an equivalence function for this reason
    /// (`EquivalentUriMap`), and one of their freshness tests has a server renormalizing
    /// `/renormalized.ts` to `/%72enormalized.ts`.
    ///
    /// A reviewer pointed out that `equivalent_uris` was tested and exported while both
    /// lookups here did exact-string `get`, so the whole C3 group was dead code on the
    /// hot path -- and an earlier commit had *improved* that function without noticing
    /// nothing called it. The same mistake as `freshness` before its generation counter:
    /// a module with passing tests and no caller looks finished.
    ///
    /// # Cost
    ///
    /// The exact hit is tried first and is the overwhelmingly common case, so the scan
    /// only happens when a server renormalized. It is O(open files) with a string
    /// normalization each, on a path that is already waiting on a language server. The
    /// alternative -- normalizing every key on insert -- would lose the URI the server
    /// actually used, which is what has to be echoed back in later requests.
    pub async fn diagnostics_for(&self, uri: &str) -> Option<PublishedDiagnostics> {
        let map = self.diagnostics.lock().await;
        // Keys are normalized on insert, so normalizing the query is the whole lookup
        // for every case the normalizer handles, and there is exactly one entry per
        // file rather than one per spelling.
        if let Some(published) = map.get(&crate::freshness::normalize_uri(uri)) {
            return Some(published.clone());
        }
        // Retained for anything `equivalent_uris` calls equal that `normalize_uri` does
        // not map to the same string. Today there is nothing in that gap -- equivalence
        // *is* normalized comparison -- so this is belt-and-braces rather than
        // load-bearing, and cheap because it only runs on a miss.
        map.iter()
            .find(|(published_uri, _)| equivalent_uris(uri, published_uri))
            .map(|(_, published)| published.clone())
    }

    /// Diagnostics for a URI as a [`crate::freshness::Observation`], which is what
    /// [`crate::freshness::FreshnessWait`] consumes.
    ///
    /// The generation is read **after** the lookup, and deliberately so. Both orders
    /// are wrong in one direction and this is the safe one:
    ///
    /// - read the counter first and a publish landing in between makes the returned
    ///   generation *older* than the diagnostics, so a genuinely new publish looks
    ///   like the same one and the settle window does not restart. That accepts a
    ///   stale result, which is the bug freshness exists to prevent.
    /// - read it after, as here, and the same race makes the generation *newer* than
    ///   the diagnostics. The waiter restarts its window and looks again. It costs a
    ///   settle interval and reaches the right answer.
    ///
    /// A single lock covering both would remove the race, but the counter is shared
    /// across URIs while the map is per-URI, so that means serialising every publish
    /// behind one mutex to save a bounded wait on a rare interleaving. Not worth it.
    ///
    /// Uses the same equivalence matching as [`Self::diagnostics_for`]: an exact hit
    /// first, then a scan. A freshness wait that missed a renormalized publish would
    /// wait out its whole timeout and report no diagnostics for a file the server had
    /// already analysed, which is the exact failure this module exists to prevent.
    pub async fn observation_for(&self, uri: &str) -> Observation {
        let published = self.diagnostics_for(uri).await;
        Observation {
            diagnostics: published.as_ref().map(|p| p.diagnostics.clone()),
            version: published.as_ref().and_then(|p| p.version),
            generation: self.generation.load(Ordering::SeqCst),
        }
    }

    /// Open a document, or do nothing if it is already open.
    ///
    /// A second `didOpen` for one URI is a client bug: the server is already
    /// tracking it, and re-opening resets its version expectations so every later
    /// `didChange` looks stale.
    pub async fn open_document(
        &self,
        uri: &str,
        language_id: &str,
        text: &str,
    ) -> Result<(), RequestFailure> {
        {
            let mut open = self.open.lock().await;
            if open.contains_key(uri) {
                return Ok(());
            }
            open.insert(uri.to_string(), 1);
        }
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }}),
        )
        .await
    }

    /// Whether a document is open.
    pub async fn is_open(&self, uri: &str) -> bool {
        self.open.lock().await.contains_key(uri)
    }

    /// Close a document and forget its version.
    pub async fn close_document(&self, uri: &str) -> Result<(), RequestFailure> {
        if self.open.lock().await.remove(uri).is_none() {
            // Closing something never opened would tell the server about a
            // document it does not have.
            return Ok(());
        }
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        )
        .await
    }

    /// Shut the server down politely, then make sure it is gone.
    ///
    /// Returns `false` when the process outlived the whole sequence. **A caller
    /// reporting a restart must treat `false` as a failed teardown**: a server
    /// that survives its own shutdown is the daemon leak with no symptom, and omp
    /// has a regression test for reporting it as success.
    pub async fn shutdown(&mut self) -> bool {
        // Fail pending requests first. They cannot be answered by a server we are
        // about to stop, and leaving them means each caller waits out a timeout
        // after the connection is already gone.
        self.pendings.fail_all("the client is shutting down").await;

        let polite = self
            .request("shutdown", json!(null), SHUTDOWN_TIMEOUT)
            .await
            .is_ok();
        if polite {
            let _ = self.notify("exit", json!(null)).await;
            if self.transport.wait_for_exit(EXIT_TIMEOUT).await {
                return true;
            }
        }

        // Either it refused to shut down or it ignored `exit`. Closing stdin is
        // the next escalation; a well-behaved server treats EOF as a reason to
        // stop.
        self.transport.close_stdin().await;
        if self.transport.wait_for_exit(EXIT_TIMEOUT).await {
            return true;
        }
        self.transport.kill(EXIT_TIMEOUT).await
    }

    /// The server's pid, for diagnostics.
    pub fn pid(&self) -> Option<u32> {
        self.transport.pid()
    }

    /// The captured stderr tail.
    pub async fn stderr_tail(&self) -> String {
        self.transport.stderr_tail().await
    }
}

/// Handle a notification from the server.
async fn handle_notification(
    method: &str,
    params: Value,
    diagnostics: &Arc<Mutex<HashMap<String, PublishedDiagnostics>>>,
    generation: &Arc<AtomicU64>,
) {
    if method != "textDocument/publishDiagnostics" {
        // `window/logMessage`, `$/progress` and friends. Dropped for now;
        // progress tracking arrives with the readiness work.
        return;
    }
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return;
    };
    let published = PublishedDiagnostics {
        diagnostics: params
            .get("diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        // Absent and `null` both mean "no version", which is different from a
        // version of 0 and must not be conflated with it.
        version: params.get("version").and_then(Value::as_i64),
        uri: uri.to_string(),
    };
    // **Keyed by the normalized URI, not the spelling the server used.**
    //
    // Keying by the spelling let two equivalent spellings coexist as separate entries,
    // and then an exact-match lookup preferred whichever the caller happened to ask
    // with -- which is the *older* one when a server renormalizes mid-session.
    //
    // Measured before the fix: publish `/renorm.rs` at v1, then `/%72enorm.rs` at v7,
    // and `observation_for("/renorm.rs")` returned generation 2 with version 1. A
    // freshness waiter sees the generation move, re-reads, exact-hits the stale entry,
    // and settles on pre-edit content. That is a wrong answer, which is worse than the
    // missed publish the equivalence scan was added to fix.
    //
    // omp cannot reach this state: `EquivalentUriMap` normalizes the key on set, so the
    // second publish overwrites the first and there is only ever one entry. Following
    // them. The server's own spelling is kept in the value, since that is what has to
    // be echoed back in later requests -- which was the real reason not to normalize,
    // and it is satisfied by storing it rather than by keying on it.
    //
    // Found by an adversarial reviewer on the fourth pass, in code the third pass had
    // just approved.
    diagnostics
        .lock()
        .await
        .insert(crate::freshness::normalize_uri(uri), published);
    // After the insert, so an observer that sees a new generation is guaranteed to
    // see the publish that caused it. Bumping first would let a waiter read the new
    // counter with the old diagnostics and restart its settle window against content
    // it has already considered.
    generation.fetch_add(1, Ordering::SeqCst);
}

/// Answer a request from the server.
///
/// **Every branch answers.** That is the whole contract: a server blocked on an
/// unanswered request stops serving, and several block semantic requests
/// specifically.
async fn handle_server_request(
    request: ServerRequest,
    dynamic: &Arc<Mutex<HashMap<String, String>>>,
    settings: &Value,
    root: &std::path::Path,
    answers: &mpsc::UnboundedSender<Value>,
) {
    let ServerRequest { id, method, params } = request;

    let answer = match method.as_str() {
        // The folders we advertised in `initialize`. A server that asks and is
        // refused falls back to guessing a root, and then resolves imports against
        // the wrong tree — which presents as "definition not found" for code that
        // plainly exists. A wrong answer rather than an error, so the worst shape.
        "workspace/workspaceFolders" => jsonrpc::response(&id, &json!([workspace_folder(root)])),
        // The settings the server asked for, in the order it asked, with `null`
        // for sections we do not have. **Order matters**: the server matches the
        // result array positionally, so a reordered answer gives it another
        // section's settings.
        "workspace/configuration" => {
            let items = params
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let answers: Vec<Value> = items
                .iter()
                .map(|item| match item.get("section").and_then(Value::as_str) {
                    Some(section) => lookup_section(settings, section),
                    // An item with no section answers `null`, matching omp
                    // exactly (`settings?.[item.section ?? ""] ?? null`, so a
                    // lookup of the empty key). Returning the whole settings
                    // object instead would be a guess, and their regression test
                    // pins `[null, true, null]` for precisely this shape.
                    None => Value::Null,
                })
                .collect();
            jsonrpc::response(&id, &Value::Array(answers))
        }
        // Dynamic registration. Recorded, then acknowledged: some servers block
        // semantic requests until this succeeds.
        "client/registerCapability" => {
            if let Some(registrations) = params.get("registrations").and_then(Value::as_array) {
                let mut dynamic = dynamic.lock().await;
                for registration in registrations {
                    let id = registration.get("id").and_then(Value::as_str);
                    let method = registration.get("method").and_then(Value::as_str);
                    if let (Some(id), Some(method)) = (id, method) {
                        dynamic.insert(id.to_string(), method.to_string());
                    }
                }
            }
            jsonrpc::response(&id, &Value::Null)
        }
        "client/unregisterCapability" => {
            // Both spellings, because the spec's own field name is misspelled
            // (`unregisterations`) and servers use either.
            let unregistrations = params
                .get("unregisterations")
                .or_else(|| params.get("unregistrations"))
                .and_then(Value::as_array);
            if let Some(unregistrations) = unregistrations {
                let mut dynamic = dynamic.lock().await;
                for registration in unregistrations {
                    if let Some(id) = registration.get("id").and_then(Value::as_str) {
                        dynamic.remove(id);
                    }
                }
            }
            jsonrpc::response(&id, &Value::Null)
        }
        // v1 refuses to apply edits, but refuses *in the spec's terms* so the
        // server can react. An error would read as a client fault; `applied:
        // false` is a legitimate answer.
        "workspace/applyEdit" => jsonrpc::response(
            &id,
            &json!({
                "applied": false,
                "failureReason": "this client does not apply server-initiated edits",
            }),
        ),
        // Headless: no UI. The spec's "nothing selected" is `null`.
        "window/showMessageRequest" => jsonrpc::response(&id, &Value::Null),
        // Nothing to display, and `success: false` is the spec's shape.
        "window/showDocument" => jsonrpc::response(&id, &json!({"success": false})),
        // Void acknowledgements. omp ties stalling on these to the same hang as
        // dynamic registration.
        "window/workDoneProgress/create"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/codeAction/refresh"
        | "workspace/inlineValue/refresh"
        | "workspace/foldingRange/refresh"
        | "workspace/diagnostic/refresh" => jsonrpc::response(&id, &Value::Null),
        // Anything else: the spec's "method not found", which is an answer. A
        // server told this moves on; a server told nothing waits.
        other => {
            jsonrpc::error_response(&id, METHOD_NOT_FOUND, &format!("Method not found: {other}"))
        }
    };

    let _ = answers.send(answer);
}

/// Resolve a `workspace/configuration` section against our settings.
///
/// Section names are **dotted paths**, not flat keys: `rust-analyzer.checkOnSave`
/// addresses `settings["rust-analyzer"]["checkOnSave"]`. Treating the whole string
/// as one key answers `null` for every nested request, which is how a server ends
/// up running with none of the configuration we carefully supplied.
///
/// Ambiguity is real and resolved the useful way. A settings map could plausibly
/// contain the literal key `"rust-analyzer.checkOnSave"`, so that is tried
/// **first** and the nested walk is the fallback. A config written either way then
/// works, and only a config written both ways is ambiguous — in which case the
/// exact match is the more specific intent.
fn lookup_section(settings: &Value, section: &str) -> Value {
    if let Some(exact) = settings.get(section) {
        return exact.clone();
    }
    let mut cursor = settings;
    for part in section.split('.') {
        match cursor.get(part) {
            Some(next) => cursor = next,
            // `null` rather than an omitted entry: the server matches the answer
            // array positionally, so the slot has to be filled.
            None => return Value::Null,
        }
    }
    cursor.clone()
}

/// A `file://` URI for a path.
///
/// Minimal on purpose: enough for `rootUri` and workspace folders. Full
/// percent-encoding arrives with the document work, where a path with a `#` in it
/// actually matters.
fn path_to_uri(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

fn workspace_folder(root: &std::path::Path) -> Value {
    json!({
        "uri": path_to_uri(root),
        "name": root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "workspace".to_string()),
    })
}

/// What we tell the server we can do.
///
/// Trimmed to what this client actually implements, which is the point: claiming
/// a capability we do not honour invites requests we then fail to answer, and an
/// unanswered request wedges the server. Notably absent: `willSaveWaitUntil`,
/// snippet edits, and anything we cannot act on.
fn client_capabilities() -> Value {
    json!({
        "textDocument": {
            "synchronization": {"didSave": true, "dynamicRegistration": false},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            // `linkSupport` because `LocationLink` carries a selection range,
            // which is what makes a jump land on the name rather than the whole
            // definition.
            "definition": {"linkSupport": true},
            "typeDefinition": {"linkSupport": true},
            "implementation": {"linkSupport": true},
            "references": {},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "rename": {"prepareSupport": true},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": [
                            "quickfix", "refactor", "refactor.extract",
                            "refactor.inline", "refactor.rewrite", "source",
                            "source.organizeImports", "source.fixAll"
                        ]
                    }
                },
                "resolveSupport": {"properties": ["edit"]}
            },
            "publishDiagnostics": {
                "relatedInformation": true,
                // So a publish can be matched to the version it describes, which
                // is the only reliable way to tell fresh from stale.
                "versionSupport": true,
                "codeDescriptionSupport": true,
                "dataSupport": true
            },
            "diagnostic": {"dynamicRegistration": true}
        },
        "window": {"workDoneProgress": true},
        "workspace": {
            "applyEdit": false,
            "workspaceEdit": {
                "documentChanges": true,
                "resourceOperations": ["create", "rename", "delete"],
                "failureHandling": "textOnlyTransactional"
            },
            "configuration": true,
            "workspaceFolders": true,
            "symbol": {},
            "fileOperations": {"willRename": true, "didRename": true}
        }
    })
}

/// Convenience: a `ResponseError` for tests and callers that need one.
pub fn method_not_found(message: &str) -> ResponseError {
    ResponseError {
        code: METHOD_NOT_FOUND,
        message: message.to_string(),
        data: None,
    }
}

/// Not a real id; used where a caller needs a placeholder.
pub fn placeholder_id() -> RequestId {
    RequestId::Number(0)
}
