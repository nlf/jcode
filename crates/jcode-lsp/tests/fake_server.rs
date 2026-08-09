//! End-to-end tests for the fake LSP server.
//!
//! The fake server is the fixture every later test depends on, so it needs its
//! own tests: a broken fixture produces failures that look like client bugs and
//! sends the next person debugging the wrong layer.
//!
//! These deliberately spawn the real binary and speak framed JSON-RPC to it over
//! real pipes, using only `std` process handling. There is no client yet; that
//! is the point. If these pass, the fixture is sound and the client can be built
//! against it.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use jcode_lsp::framing::{encode, MessageFramer};
use serde_json::{json, Value};

/// A spawned fake server plus its pipes.
/// `stdin` is an `Option` so a test can close it and watch the server react to
/// EOF, which is the teardown path when a client dies without saying goodbye.
struct Fake {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    framer: MessageFramer,
}

impl Fake {
    fn spawn(env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fake_lsp_server"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn the fake server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            framer: MessageFramer::new(),
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("serialize");
        let stdin = self.stdin.as_mut().expect("stdin is still open");
        stdin.write_all(&encode(&body)).expect("write");
        stdin.flush().expect("flush");
    }

    /// Close our end of the server's stdin, so it sees EOF.
    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.await_message(|message| message.get("id").and_then(Value::as_i64) == Some(id))
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Read messages until one satisfies `wanted`, or time out.
    ///
    /// Reads one byte at a time rather than filling a buffer. Slow, irrelevant at
    /// this scale, and it means these tests exercise the framer's incremental
    /// path against a real process rather than the easy whole-message case.
    fn await_message(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while let Some(body) = self.framer.next_message().expect("framing") {
                let message: Value = serde_json::from_slice(&body).expect("json");
                if wanted(&message) {
                    return message;
                }
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for a matching message");
            }
            let mut byte = [0u8; 1];
            match self.stdout.read(&mut byte) {
                Ok(0) => panic!("the server closed stdout before the expected message"),
                Ok(_) => self.framer.push(&byte),
                Err(error) => panic!("read failed: {error}"),
            }
        }
    }

    fn state(&mut self, id: i64) -> Value {
        self.request(id, "test/state", json!(null))
            .get("result")
            .cloned()
            .expect("a result")
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        // Kill rather than a polite shutdown: a test that failed mid-way must
        // not leave a process behind, and `exit` may be exactly what is disabled.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn it_answers_initialize_with_capabilities_and_server_info() {
    let mut fake = Fake::spawn(&[]);
    let response = fake.request(1, "initialize", json!({"processId": 4242}));
    let result = response.get("result").expect("a result");

    assert!(
        result["capabilities"]["definitionProvider"].as_bool() == Some(true),
        "capabilities missing: {result}"
    );
    assert_eq!(result["serverInfo"]["name"], "fake-lsp");

    // `processId` round-trips so a test can assert we send our own pid, which
    // servers use to exit when their client dies.
    let state = fake.state(2);
    assert_eq!(state["processId"], 4242);
    assert_eq!(state["initializeCount"], 1);
}

/// The spec's code for an unsupported method. This must be distinguishable from
/// a transport failure: several of omp's regressions exist because treating
/// "method not found" as fatal tears down a healthy server.
#[test]
fn an_unknown_method_returns_minus_32601_rather_than_dying() {
    let mut fake = Fake::spawn(&[]);
    let response = fake.request(1, "textDocument/somethingUnsupported", json!({}));
    assert_eq!(response["error"]["code"], -32601);

    // Still alive and serving afterwards, which is the actual claim.
    let echoed = fake.request(2, "test/echo", json!({"alive": true}));
    assert_eq!(echoed["result"]["alive"], true);
}

#[test]
fn it_echoes_params_so_a_request_can_be_round_tripped() {
    let mut fake = Fake::spawn(&[]);
    let sent = json!({"nested": {"list": [1, 2, 3]}, "text": "héllo → 日本語"});
    let response = fake.request(1, "test/echo", sent.clone());
    assert_eq!(response["result"], sent, "non-ASCII must survive the round trip");
}

#[test]
fn opening_a_document_publishes_diagnostics_and_is_recorded() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    fake.notify("initialized", json!({}));
    fake.notify(
        "textDocument/didOpen",
        json!({"textDocument": {
            "uri": "file:///tmp/a.rs",
            "languageId": "rust",
            "version": 1,
            "text": "fn main() {}\n"
        }}),
    );

    let published = fake.await_message(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
    });
    assert_eq!(published["params"]["uri"], "file:///tmp/a.rs");
    assert_eq!(published["params"]["version"], 1);
    assert_eq!(published["params"]["diagnostics"][0]["severity"], 2);

    let state = fake.state(2);
    assert_eq!(state["didOpen"]["file:///tmp/a.rs"], 1);
    assert_eq!(state["openDocuments"], 1);
}

/// Version sequences are what make stale diagnostics detectable, so the fixture
/// has to record them faithfully or Group C's freshness tests are meaningless.
#[test]
fn changes_are_recorded_in_version_order_and_closing_forgets_the_document() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    let uri = "file:///tmp/b.rs";
    fake.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": ""}}),
    );
    for version in [2, 3, 4] {
        fake.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": format!("v{version}")}]
            }),
        );
    }

    let state = fake.state(2);
    assert_eq!(
        state["didChange"][uri],
        json!([2, 3, 4]),
        "versions must be recorded in arrival order"
    );

    fake.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    let after = fake.state(3);
    assert_eq!(after["didClose"], json!([uri]));
    assert_eq!(after["openDocuments"], 0, "a closed document must be dropped");
}

/// Notification order is asserted because LSP ordering rules are real:
/// configuration must follow `initialized`, and a semantic request before that is
/// something a server may reject. Only the observed order can catch it.
#[test]
fn notifications_are_recorded_in_arrival_order() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    fake.notify("initialized", json!({}));
    fake.notify("workspace/didChangeConfiguration", json!({"settings": {}}));

    let state = fake.state(2);
    let notifications: Vec<&str> = state["notifications"]
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        notifications,
        vec!["initialized", "workspace/didChangeConfiguration"]
    );
}

/// The server→client direction. A server blocked on an unanswered request stops
/// serving, so this is the shape of the deadlock the client must avoid.
#[test]
fn it_can_be_driven_to_send_the_client_a_request() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    fake.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "test/serverRequest",
        "params": {"method": "workspace/configuration", "params": {"items": [{"section": "rust-analyzer"}]}}
    }));

    let inbound = fake.await_message(|message| {
        message.get("method").and_then(Value::as_str) == Some("workspace/configuration")
    });
    assert!(
        inbound.get("id").is_some(),
        "a server request must carry an id, or it cannot be answered: {inbound}"
    );
    assert_eq!(inbound["params"]["items"][0]["section"], "rust-analyzer");
}

/// The framing knob, end to end. With writes split mid-header, a client that
/// reads a line or assumes one read per message cannot get this far.
#[test]
fn split_writes_still_produce_readable_messages() {
    let mut fake = Fake::spawn(&[("FAKE_LSP_SPLIT_WRITES", "1")]);
    let response = fake.request(1, "initialize", json!({}));
    assert_eq!(response["result"]["serverInfo"]["name"], "fake-lsp");

    let echoed = fake.request(2, "test/echo", json!({"n": 1}));
    assert_eq!(echoed["result"]["n"], 1, "a second split message must also arrive");
}

/// A server that exits mid-request. The client must reject the pending request
/// on process exit rather than waiting for a reply that cannot come.
#[test]
fn it_can_exit_while_a_request_is_in_flight() {
    let mut fake = Fake::spawn(&[("FAKE_LSP_EXIT_ON", "test/echo")]);
    fake.request(1, "initialize", json!({}));
    fake.send(json!({"jsonrpc": "2.0", "id": 2, "method": "test/echo", "params": {}}));

    // stdout reaching EOF is how the client learns the server is gone.
    let mut buffer = Vec::new();
    fake.stdout
        .read_to_end(&mut buffer)
        .expect("read to EOF");
    let status = fake.child.wait().expect("wait");
    assert!(status.success(), "the fixture should exit cleanly: {status:?}");
}

/// A server that accepts a request and never answers, so timeout and
/// cancellation paths are reachable.
#[test]
fn it_can_hang_on_a_named_method() {
    let mut fake = Fake::spawn(&[("FAKE_LSP_HANG_ON", "test/echo")]);
    fake.request(1, "initialize", json!({}));
    fake.send(json!({"jsonrpc": "2.0", "id": 2, "method": "test/echo", "params": {}}));

    // Nothing arrives for the hung request, but the process stays alive: a hang
    // and an exit are different failures and the client must tell them apart.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.child.try_wait().expect("try_wait").is_none(),
        "a hanging server must stay alive, not exit"
    );
}

/// `exit` ignored, so teardown has to fall back to killing the process. Without
/// this knob the "shutdown left the child alive" leak is untestable, and that
/// leak is the one with no symptom.
#[test]
fn it_can_ignore_exit_so_teardown_must_kill_it() {
    let mut fake = Fake::spawn(&[("FAKE_LSP_SKIP_EXIT", "1")]);
    fake.request(1, "initialize", json!({}));
    fake.request(2, "shutdown", json!(null));
    fake.notify("exit", json!(null));

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.child.try_wait().expect("try_wait").is_none(),
        "with SKIP_EXIT the server must survive `exit`"
    );
}

/// A normal `shutdown` then `exit` must actually end the process. This is the
/// control for the test above: without it, "ignores exit" proves nothing,
/// because a fixture that never exits would pass both.
#[test]
fn shutdown_then_exit_ends_the_process() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    let response = fake.request(2, "shutdown", json!(null));
    assert!(response["result"].is_null(), "shutdown returns null");
    fake.notify("exit", json!(null));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = fake.child.try_wait().expect("try_wait") {
            assert!(status.success(), "clean exit expected, got {status:?}");
            return;
        }
        assert!(Instant::now() < deadline, "the server did not exit after `exit`");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Closing our stdin must end the server. This is the teardown path when the
/// client dies without a polite shutdown, and a server that ignores EOF becomes
/// an orphan.
#[test]
fn closing_stdin_ends_the_process() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    fake.close_stdin();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = fake.child.try_wait().expect("try_wait") {
            assert!(status.success(), "EOF should end it cleanly, got {status:?}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server did not exit when its stdin closed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An empty capability set, so capability-gated paths are reachable. A client
/// that assumes every server supports everything sends requests that come back
/// as errors, and the useful behaviour is to not send them at all.
#[test]
fn it_can_report_no_capabilities() {
    let mut fake = Fake::spawn(&[("FAKE_LSP_NO_CAPABILITIES", "1")]);
    let response = fake.request(1, "initialize", json!({}));
    assert_eq!(
        response["result"]["capabilities"],
        json!({}),
        "the knob must produce a genuinely empty set"
    );
}

/// Malformed JSON inside a well-framed message must not kill the server. A real
/// server answers with a parse error and carries on; dying would turn one bad
/// message into a lost session.
#[test]
fn malformed_json_in_a_valid_frame_does_not_kill_it() {
    let mut fake = Fake::spawn(&[]);
    fake.request(1, "initialize", json!({}));
    let stdin = fake.stdin.as_mut().expect("stdin");
    stdin.write_all(&encode(b"{not json at all")).expect("write");
    stdin.flush().expect("flush");

    let echoed = fake.request(2, "test/echo", json!({"still": "alive"}));
    assert_eq!(echoed["result"]["still"], "alive");
}
