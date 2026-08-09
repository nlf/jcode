//! A fake language server: a real process, speaking real framed JSON-RPC over
//! stdio.
//!
//! Ported from omp's `test/fixtures/fake-lsp-server.ts`, which is the foundation
//! of their whole LSP regression suite.
//!
//! # Why this is a process and not a mock
//!
//! An in-process fake cannot produce the failures that actually happen: a header
//! split across reads, a body split across reads, a server that stops reading
//! its stdin so our writes block on the pipe, a server that exits mid-request.
//! Those are properties of pipes and processes, not of an interface, so faking
//! at the interface tests the wrong layer and passes when the real thing hangs.
//!
//! # Introspection
//!
//! `test/state` returns what the server observed: how many times each document
//! was opened, the version sequence of each change, which documents were closed,
//! and the notification order. That last one matters because LSP ordering
//! requirements are real — configuration must land after `initialized`, and a
//! semantic request before that is a protocol violation a server is entitled to
//! reject. Asserting on the observed order is the only way to catch it.
//!
//! # Failure knobs
//!
//! Beyond omp's fixture, this one can be told to misbehave, because the
//! misbehaviour is what needs testing. Set via environment variables so a test
//! configures the server at spawn without a handshake:
//!
//! - `FAKE_LSP_SPLIT_WRITES=1` — emit every message as several small writes,
//!   with the header deliberately cut mid-word. Turns the framing tests from a
//!   unit-level claim into an end-to-end one.
//! - `FAKE_LSP_HANG_ON=<method>` — accept the request and never answer, so
//!   timeout and cancellation paths are reachable.
//! - `FAKE_LSP_EXIT_ON=<method>` — exit the process while the request is in
//!   flight, which must reject the pending request rather than hang forever.
//! - `FAKE_LSP_STOP_READING_ON=<method>` — stop draining stdin, so our next
//!   write blocks on a full pipe. This is the wedged-server case.
//! - `FAKE_LSP_NO_CAPABILITIES=1` — return an empty capability set, so
//!   capability-gated paths can be exercised.
//! - `FAKE_LSP_SKIP_EXIT=1` — ignore `exit`, so teardown must fall back to
//!   killing the process.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use jcode_lsp::framing::{encode, Framed, MessageFramer};
use serde_json::{json, Value};

/// What the server has observed, for `test/state`.
#[derive(Default)]
struct Observed {
    initialize_count: u32,
    process_id: Option<i64>,
    /// Per-URI count of `didOpen`. A second open for one URI is a client bug:
    /// the document is already tracked, and re-opening resets the server's
    /// version expectations.
    did_open: HashMap<String, u32>,
    /// Per-URI version sequence from `didChange`, so a test can assert versions
    /// increase monotonically.
    did_change: HashMap<String, Vec<i64>>,
    did_close: Vec<String>,
    /// Every notification method, in arrival order.
    notifications: Vec<String>,
    /// Documents currently open, with their last-seen text.
    documents: HashMap<String, (i64, String)>,
}

struct Server {
    observed: Mutex<Observed>,
    stop_reading: Arc<AtomicBool>,
    split_writes: bool,
    no_capabilities: bool,
    skip_exit: bool,
    hang_on: Option<String>,
    exit_on: Option<String>,
    stop_reading_on: Option<String>,
}

impl Server {
    fn from_env() -> Self {
        let flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
        let value = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            observed: Mutex::new(Observed::default()),
            stop_reading: Arc::new(AtomicBool::new(false)),
            split_writes: flag("FAKE_LSP_SPLIT_WRITES"),
            no_capabilities: flag("FAKE_LSP_NO_CAPABILITIES"),
            skip_exit: flag("FAKE_LSP_SKIP_EXIT"),
            hang_on: value("FAKE_LSP_HANG_ON"),
            exit_on: value("FAKE_LSP_EXIT_ON"),
            stop_reading_on: value("FAKE_LSP_STOP_READING_ON"),
        }
    }

    /// Write one framed message to stdout.
    ///
    /// With `split_writes` the frame is emitted in three pieces, the first
    /// deliberately cutting the header mid-word, then flushed between each. A
    /// client that reads a line or assumes one read per message fails here, and
    /// that is the point: it makes the framing property observable end to end
    /// rather than only in a unit test.
    fn send(&self, message: &Value) {
        let body = serde_json::to_vec(message).expect("serialize");
        let framed = encode(&body);
        let mut stdout = std::io::stdout().lock();

        if self.split_writes && framed.len() > 12 {
            // 8 lands inside "Content-Length", which is the cut that breaks a
            // line-oriented reader.
            let first = 8;
            let second = framed.len() / 2;
            for piece in [&framed[..first], &framed[first..second], &framed[second..]] {
                let _ = stdout.write_all(piece);
                let _ = stdout.flush();
            }
        } else {
            let _ = stdout.write_all(&framed);
            let _ = stdout.flush();
        }
    }

    fn respond(&self, id: &Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn respond_error(&self, id: &Value, code: i64, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        }));
    }

    fn publish_diagnostics(&self, uri: &str, version: i64) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": version,
                "diagnostics": [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 1}
                    },
                    "message": "fake",
                    "severity": 2
                }]
            }
        }));
    }

    /// Apply the configured misbehaviour for a method, if any.
    ///
    /// Returns true when the caller should not answer the request.
    fn misbehave(&self, method: &str) -> bool {
        if self.exit_on.as_deref() == Some(method) {
            // Exit without answering. The client must reject the pending
            // request on process exit; a client that only ever completes
            // requests on a reply hangs here forever.
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        if self.stop_reading_on.as_deref() == Some(method) {
            // Stop draining stdin. Our next write eventually blocks on a full
            // pipe, which is the wedged-server case: nothing is broken, nothing
            // responds, and a client without a write deadline never returns.
            self.stop_reading.store(true, Ordering::SeqCst);
            return false;
        }
        if self.hang_on.as_deref() == Some(method) {
            return true;
        }
        false
    }

    fn capabilities(&self) -> Value {
        if self.no_capabilities {
            return json!({});
        }
        json!({
            "textDocumentSync": 1,
            "hoverProvider": true,
            "definitionProvider": true,
            "typeDefinitionProvider": true,
            "implementationProvider": true,
            "referencesProvider": true,
            "documentSymbolProvider": true,
            "workspaceSymbolProvider": true,
            "renameProvider": {"prepareProvider": true},
            "codeActionProvider": {"resolveProvider": true}
        })
    }

    fn handle_request(&self, id: &Value, method: &str, params: &Value) {
        if self.misbehave(method) {
            return;
        }

        match method {
            "initialize" => {
                let mut observed = self.lock();
                observed.initialize_count += 1;
                observed.process_id = params.get("processId").and_then(Value::as_i64);
                drop(observed);
                self.respond(
                    id,
                    json!({
                        "capabilities": self.capabilities(),
                        "serverInfo": {"name": "fake-lsp", "version": std::process::id().to_string()}
                    }),
                );
            }
            "test/state" => {
                let observed = self.lock();
                self.respond(
                    id,
                    json!({
                        "initializeCount": observed.initialize_count,
                        "processId": observed.process_id,
                        "didOpen": observed.did_open,
                        "didChange": observed.did_change,
                        "didClose": observed.did_close,
                        "notifications": observed.notifications,
                        "openDocuments": observed.documents.len(),
                    }),
                );
            }
            "test/echo" => self.respond(id, params.clone()),
            // Ask the client something, so the server→client direction is
            // drivable from a test. The client's answer is discarded here; what
            // matters is that it answers at all, since a server blocked on an
            // unanswered request stops serving.
            "test/serverRequest" => {
                let inner_method = params
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("workspace/configuration");
                let inner_params = params.get("params").cloned().unwrap_or(json!(null));
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "id": format!("fake-{}", std::process::id()),
                    "method": inner_method,
                    "params": inner_params
                }));
                self.respond(id, json!({"sent": inner_method}));
            }
            "shutdown" => self.respond(id, Value::Null),
            // Everything else is genuinely unsupported, and must say so with the
            // spec's code. A client that treats "method not found" as a
            // transport failure tears down a healthy server; several of omp's
            // regressions are about exactly that distinction.
            _ => self.respond_error(id, -32601, &format!("Method not found: {method}")),
        }
    }

    fn handle_notification(&self, method: &str, params: &Value) {
        self.lock().notifications.push(method.to_string());

        match method {
            "textDocument/didOpen" => {
                let document = params.get("textDocument").cloned().unwrap_or(json!({}));
                let uri = document
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let version = document.get("version").and_then(Value::as_i64).unwrap_or(0);
                let text = document
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                {
                    let mut observed = self.lock();
                    *observed.did_open.entry(uri.clone()).or_insert(0) += 1;
                    observed.documents.insert(uri.clone(), (version, text));
                }
                self.publish_diagnostics(&uri, version);
            }
            "textDocument/didChange" => {
                let document = params.get("textDocument").cloned().unwrap_or(json!({}));
                let uri = document
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let version = document.get("version").and_then(Value::as_i64).unwrap_or(0);
                {
                    let mut observed = self.lock();
                    observed
                        .did_change
                        .entry(uri.clone())
                        .or_default()
                        .push(version);
                    let previous = observed
                        .documents
                        .get(&uri)
                        .map(|(_, text)| text.clone())
                        .unwrap_or_default();
                    observed.documents.insert(uri.clone(), (version, previous));
                }
                self.publish_diagnostics(&uri, version);
            }
            "textDocument/didClose" => {
                let uri = params
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut observed = self.lock();
                observed.did_close.push(uri.clone());
                observed.documents.remove(&uri);
            }
            "exit" if !self.skip_exit => {
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
            _ => {}
        }
    }

    fn handle(&self, message: &Value) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id");
        let params = message.get("params").cloned().unwrap_or(json!(null));

        match (method, id) {
            // A request: has both a method and an id.
            (Some(method), Some(id)) => self.handle_request(id, method, &params),
            // A notification: a method and no id.
            (Some(method), None) => self.handle_notification(method, &params),
            // A response to something we asked. Nothing to do, but it must not
            // be mistaken for a request with a missing method.
            (None, _) => {}
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Observed> {
        self.observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn main() {
    let server = Server::from_env();
    let mut framer = MessageFramer::new();
    let mut stdin = std::io::stdin().lock();
    let mut chunk = [0u8; 8192];

    loop {
        if server.stop_reading.load(Ordering::SeqCst) {
            // Deliberately sleep rather than exit: an exited process is a
            // *different* failure from a live one that has stopped reading, and
            // only the second exercises a blocked write.
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }

        match stdin.read(&mut chunk) {
            // Clean EOF: the client closed our stdin, so there is nothing more
            // to serve.
            Ok(0) => return,
            Ok(read) => framer.push(&chunk[..read]),
            Err(_) => return,
        }

        loop {
            match framer.next_message() {
                Ok(Framed::Message(body)) => {
                    // Malformed JSON inside a well-framed message is the peer's
                    // problem, not a reason to die: a real server answers with a
                    // parse error and keeps going.
                    if let Ok(message) = serde_json::from_slice::<Value>(&body) {
                        server.handle(&message);
                    }
                }
                // Junk from the client. Skip it, exactly as a real server would
                // rather than dying on whatever printed it.
                Ok(Framed::Resync { .. }) => continue,
                Ok(Framed::Incomplete) => break,
                // A body over the cap is the only unrecoverable case: we cannot
                // skip a body whose length we do not trust. Exiting is honest.
                Err(_) => return,
            }
        }
    }
}
