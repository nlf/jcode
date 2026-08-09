//! Transport tests, against the real fake server over real pipes.
//!
//! Group A's process failures. Every case here is a way a language server breaks
//! that an in-process mock cannot reproduce, which is the whole argument for the
//! fake-server binary.

use std::time::Duration;

use jcode_lsp::framing::encode;
use jcode_lsp::jsonrpc::{self, RequestId};
use jcode_lsp::transport::{FromServer, Transport, WriteError};
use serde_json::{json, Value};

/// Generous, because these run on a loaded machine and a flaky timeout test is
/// worse than no test. The failures under test are permanent hangs, so any finite
/// bound distinguishes them.
const PATIENT: Duration = Duration::from_secs(10);
const WRITE_DEADLINE: Duration = Duration::from_secs(5);

fn spawn(env: &[(&str, &str)]) -> (Transport, tokio::sync::mpsc::UnboundedReceiver<FromServer>) {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Transport::spawn(
        env!("CARGO_BIN_EXE_fake_lsp_server"),
        &[],
        std::path::Path::new("."),
        &env,
    )
    .expect("spawn the fake server")
}

/// Read from the channel until `wanted` matches, or fail on close/timeout.
async fn await_message(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<FromServer>,
    wanted: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + PATIENT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => panic!("timed out waiting for a matching message"),
            Ok(None) => panic!("the channel closed before the expected message"),
            Ok(Some(FromServer::Message(body))) => {
                let message: Value = serde_json::from_slice(&body).expect("json");
                if wanted(&message) {
                    return message;
                }
            }
            Ok(Some(FromServer::Junk { headers })) => {
                panic!("unexpected junk from the fixture: {headers:?}")
            }
            Ok(Some(FromServer::Closed { reason, stderr })) => {
                panic!("the transport closed: {reason} (stderr: {stderr:?})")
            }
        }
    }
}

/// Wait for the close notice, which is what a failure must always produce.
async fn await_close(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<FromServer>,
) -> (String, String) {
    let deadline = tokio::time::Instant::now() + PATIENT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => panic!("the transport never reported a close"),
            Ok(None) => panic!("the channel dropped without a Closed notice"),
            Ok(Some(FromServer::Closed { reason, stderr })) => return (reason, stderr),
            Ok(Some(_)) => continue,
        }
    }
}

async fn send_request(transport: &Transport, id: i64, method: &str, params: Value) {
    let body = serde_json::to_vec(&jsonrpc::request(&RequestId::Number(id), method, &params))
        .expect("serialize");
    transport
        .send(&body, WRITE_DEADLINE)
        .await
        .expect("the write should succeed");
}

#[tokio::test]
async fn a_request_gets_a_response_back_through_the_channel() {
    let (transport, mut rx) = spawn(&[]);
    send_request(&transport, 1, "initialize", json!({"processId": 1})).await;

    let response = await_message(&mut rx, |message| message["id"] == 1).await;
    assert_eq!(response["result"]["serverInfo"]["name"], "fake-lsp");
}

/// Pushed notifications must arrive without anyone waiting on a response.
/// Diagnostics are the tool's main product and they are pushed, so a client that
/// only reads while awaiting a reply loses them.
#[tokio::test]
async fn pushed_notifications_arrive_with_no_request_outstanding() {
    let (transport, mut rx) = spawn(&[]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    let body = serde_json::to_vec(&jsonrpc::notification(
        "textDocument/didOpen",
        &json!({"textDocument": {
            "uri": "file:///tmp/a.rs", "languageId": "rust", "version": 1, "text": ""
        }}),
    ))
    .expect("serialize");
    transport.send(&body, WRITE_DEADLINE).await.expect("write");

    let published = await_message(&mut rx, |message| {
        message["method"] == "textDocument/publishDiagnostics"
    })
    .await;
    assert_eq!(published["params"]["uri"], "file:///tmp/a.rs");
}

/// Split writes end to end. A client that reads a line, or assumes one read per
/// message, cannot complete this.
#[tokio::test]
async fn messages_split_across_writes_are_reassembled() {
    let (transport, mut rx) = spawn(&[("FAKE_LSP_SPLIT_WRITES", "1")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    // A second one, because the first could pass by luck if the tail happened to
    // land in one read.
    send_request(&transport, 2, "test/echo", json!({"n": 2})).await;
    let echoed = await_message(&mut rx, |message| message["id"] == 2).await;
    assert_eq!(echoed["result"]["n"], 2);
}

/// **A server that exits must announce it**, or every pending request waits out
/// its timeout with no explanation. Group A11.
#[tokio::test]
async fn an_exiting_server_produces_a_close_notice() {
    let (transport, mut rx) = spawn(&[("FAKE_LSP_EXIT_ON", "test/echo")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    send_request(&transport, 2, "test/echo", json!({})).await;
    let (reason, _) = await_close(&mut rx).await;
    assert!(
        reason.contains("stdout"),
        "the reason should name what was observed, got {reason:?}"
    );
}

/// The stderr tail must reach the close notice. **This is the difference between
/// "the server timed out" and "the server said it could not find libclang".**
/// A server that dies at startup explains itself on stderr and nowhere else.
#[tokio::test]
async fn stderr_is_captured_and_reported_when_the_server_dies() {
    // A program that prints to stderr and exits non-zero: the shape of a server
    // with bad arguments or a missing library.
    let (mut transport, mut rx) = Transport::spawn(
        "sh",
        &[
            "-c".to_string(),
            "echo 'error while loading shared libraries: libclang.so' >&2; exit 1".to_string(),
        ],
        std::path::Path::new("."),
        &[],
    )
    .expect("spawn");

    let (_, stderr) = await_close(&mut rx).await;
    assert!(
        stderr.contains("libclang"),
        "the stderr tail must reach the caller, got {stderr:?}"
    );
    // And it is readable directly, for a caller that wants it without waiting on
    // the channel.
    assert!(transport.stderr_tail().await.contains("libclang"));
    assert!(
        transport.wait_for_exit(PATIENT).await,
        "the process should be gone"
    );
}

/// **The wedged-server case, and the one a naive client cannot survive.** A
/// server that stops reading its stdin fills the pipe, and an unbounded write
/// then blocks in the kernel forever. The deadline is what turns that into a
/// reportable failure.
#[tokio::test]
async fn a_server_that_stops_reading_stdin_fails_the_write_rather_than_hanging() {
    let (transport, mut rx) = spawn(&[("FAKE_LSP_STOP_READING_ON", "test/echo")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    // Trip the knob. This request is read, so it succeeds.
    send_request(&transport, 2, "test/echo", json!({})).await;

    // Now fill the pipe. A short deadline keeps the test quick; the failure being
    // tested is permanent, so any bound demonstrates it. The payload is large
    // because a pipe buffer is typically 64 KiB and a few small writes would sit
    // in it happily.
    let filler = json!({"jsonrpc": "2.0", "method": "test/noise", "params": {"pad": "x".repeat(256 * 1024)}});
    let body = serde_json::to_vec(&filler).expect("serialize");

    let mut outcome = None;
    // Several attempts: the first may still fit in the buffer.
    for _ in 0..8 {
        match transport.send(&body, Duration::from_millis(300)).await {
            Ok(()) => continue,
            Err(error) => {
                outcome = Some(error);
                break;
            }
        }
    }

    match outcome {
        Some(WriteError::Blocked { .. }) => {}
        Some(other) => panic!("expected a blocked write, got {other}"),
        None => panic!(
            "a server that stopped reading stdin never blocked a write — \
             the deadline is not doing anything"
        ),
    }
}

/// **A cancelled write that already put bytes on the pipe must poison the
/// transport.**
///
/// Found by probing after the test above was already green, which is the point: the
/// old test asserted only that the write *failed*, and that was true while the
/// stream was being silently corrupted.
///
/// `AsyncWriteExt::write_all` is cancel-unsafe in the way that matters. Dropped at a
/// timeout, the bytes it already handed the kernel stay on the pipe — measured at
/// **65,537 bytes** for a cancelled 1 MiB write against a non-reading child. The
/// server then holds half a frame, the next frame is appended to it, and
/// `Content-Length` measures the wrong bytes, so *every* later message is misframed.
/// A corrupted stream, not a lost message, and it surfaces far from its cause.
#[tokio::test]
async fn a_partial_write_poisons_the_transport_rather_than_corrupting_the_stream() {
    let (transport, mut rx) = spawn(&[("FAKE_LSP_STOP_READING_ON", "test/echo")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;
    assert!(
        !transport.desynchronised(),
        "a healthy transport must not start poisoned"
    );

    // Trip the knob, then fill the pipe until a write is cut off mid-frame.
    send_request(&transport, 2, "test/echo", json!({})).await;
    let filler = json!({
        "jsonrpc": "2.0",
        "method": "test/noise",
        "params": {"pad": "x".repeat(512 * 1024)}
    });
    let body = serde_json::to_vec(&filler).expect("serialize");

    let mut blocked = None;
    for _ in 0..8 {
        match transport.send(&body, Duration::from_millis(300)).await {
            Ok(()) => continue,
            Err(error) => {
                blocked = Some(error);
                break;
            }
        }
    }

    let error = blocked.expect("a wedged server must eventually block a write");
    assert!(
        error.desynchronised(),
        "a write cut off mid-frame leaves the stream unusable and must say so, got {error}"
    );
    // The message has to name the consequence, or a caller logs it and retries into
    // a corrupted stream.
    assert!(
        error.to_string().contains("desynchronised"),
        "the error must name the consequence, got {error}"
    );
    assert!(
        transport.desynchronised(),
        "the transport itself must be marked unusable"
    );

    // And it must refuse further writes rather than appending to the half-frame.
    // Refusing costs a restart; appending corrupts every later message.
    let refused = transport
        .send(b"{}", Duration::from_secs(1))
        .await
        .expect_err("a poisoned transport must refuse to write");
    assert!(
        refused.desynchronised(),
        "the refusal must carry the same reason, got {refused}"
    );
}

/// Closing stdin must end a healthy server. The teardown path when a client goes
/// away without a polite shutdown; a server that ignores EOF becomes an orphan.
#[tokio::test]
async fn closing_stdin_ends_the_server() {
    let (mut transport, mut rx) = spawn(&[]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    transport.close_stdin().await;
    assert!(
        transport.wait_for_exit(PATIENT).await,
        "the server should exit when its stdin closes"
    );
    let (reason, _) = await_close(&mut rx).await;
    assert!(!reason.is_empty(), "a close must carry a reason");
}

/// A server ignoring `exit` must be killable, and the kill **confirmed**. omp has
/// a regression for this: reporting a restart when the process survived is the
/// leak with no symptom.
#[tokio::test]
async fn a_server_that_ignores_exit_is_killed_and_confirmed_gone() {
    let (mut transport, mut rx) = spawn(&[("FAKE_LSP_SKIP_EXIT", "1")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    let body = serde_json::to_vec(&jsonrpc::notification("exit", &json!(null))).expect("serialize");
    transport.send(&body, WRITE_DEADLINE).await.expect("write");

    // It must still be alive: that is what the knob is for, and without this
    // assertion the kill below would pass against a server that had already gone.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        transport.exited().expect("try_wait").is_none(),
        "the fixture should have ignored `exit`"
    );

    assert!(
        transport.kill(PATIENT).await,
        "kill must confirm the process is gone, not merely request it"
    );
}

/// Unframed junk is reported, not fatal. A wrapper script printing a banner must
/// not end the session, and the caller should still learn about it.
#[tokio::test]
async fn junk_on_stdout_is_reported_and_survivable() {
    // A shell that prints a bogus header block, then a real message, then waits.
    let real = String::from_utf8(encode(br#"{"id":1,"result":{"ok":true}}"#)).expect("utf-8");
    let script = format!("printf 'Some banner\\r\\n\\r\\n'; printf '%s' '{real}'; sleep 30");
    let (_transport, mut rx) = Transport::spawn(
        "sh",
        &["-c".to_string(), script],
        std::path::Path::new("."),
        &[],
    )
    .expect("spawn");

    // The junk arrives as junk...
    let deadline = tokio::time::Instant::now() + PATIENT;
    let mut saw_junk = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => panic!("nothing arrived"),
            Ok(None) => panic!("the channel closed early"),
            Ok(Some(FromServer::Junk { headers })) => {
                assert!(headers.contains("banner"), "{headers:?}");
                saw_junk = true;
            }
            // ...and the real message behind it still arrives, which is the point.
            Ok(Some(FromServer::Message(body))) => {
                let message: Value = serde_json::from_slice(&body).expect("json");
                assert_eq!(message["result"]["ok"], true);
                assert!(saw_junk, "the junk should have been reported first");
                return;
            }
            Ok(Some(FromServer::Closed { reason, stderr })) => {
                panic!("junk must not close the transport: {reason} {stderr:?}")
            }
        }
    }
}

/// A server that never answers must not also block the writer. The hang belongs
/// to the request, and the transport must stay usable.
#[tokio::test]
async fn a_hanging_request_does_not_block_later_writes() {
    let (transport, mut rx) = spawn(&[("FAKE_LSP_HANG_ON", "test/echo")]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    // Accepted and never answered.
    send_request(&transport, 2, "test/echo", json!({})).await;

    // A different request still works, so the transport is not wedged by the
    // unanswered one.
    send_request(&transport, 3, "test/state", json!(null)).await;
    let state = await_message(&mut rx, |message| message["id"] == 3).await;
    assert_eq!(state["result"]["initializeCount"], 1);
}

/// Concurrent sends must not interleave halves of two frames, which would
/// desynchronise the server permanently.
///
/// # What mutation testing found here, and it is not what I expected
///
/// This test could not be made to fail. Three attempts at an interleaving
/// mutation all still serialised, and the reason is structural rather than
/// accidental: `AsyncWriteExt::write_all` takes `&mut self`, so **Rust will not
/// let two tasks hold the pipe at once**. Taking the pipe out of the mutex,
/// writing in halves with a yield between, and putting it back still serialises,
/// because the second sender blocks waiting for the pipe to come back.
///
/// So the interleave this test describes is unrepresentable in safe Rust, and the
/// mutex is preventing contention rather than corruption. That mirrors the
/// hashline port's finding, where the borrow checker refused a lost-update race
/// before it could be written.
///
/// Kept anyway, for two reasons: it is a real end-to-end check that ten
/// concurrent senders each get their own payload back, and if the write path is
/// ever rewritten to something the type system does not protect (a raw fd, a
/// buffer shared between tasks) this is the test that starts failing. Recorded as
/// a structural guard, not as evidence of a caught bug.
#[tokio::test]
async fn concurrent_sends_do_not_interleave_frames() {
    let (transport, mut rx) = spawn(&[]);
    send_request(&transport, 1, "initialize", json!({})).await;
    await_message(&mut rx, |message| message["id"] == 1).await;

    let transport = std::sync::Arc::new(transport);
    let mut handles = Vec::new();
    for id in 10..20 {
        let transport = std::sync::Arc::clone(&transport);
        handles.push(tokio::spawn(async move {
            // A sizeable payload per message, so an interleave would be likely
            // rather than theoretical.
            let params = json!({"pad": "y".repeat(8 * 1024), "id": id});
            let body =
                serde_json::to_vec(&jsonrpc::request(&RequestId::Number(id), "test/echo", &params))
                    .expect("serialize");
            transport.send(&body, WRITE_DEADLINE).await.expect("write");
        }));
    }
    for handle in handles {
        handle.await.expect("send task");
    }

    // Every one must come back, correctly framed and with its own payload. A
    // single interleave would corrupt the stream and lose the rest.
    let mut seen = std::collections::BTreeSet::new();
    while seen.len() < 10 {
        let message = await_message(&mut rx, |message| message.get("id").is_some()).await;
        let id = message["id"].as_i64().expect("an id");
        assert_eq!(
            message["result"]["id"], id,
            "each response must carry its own payload back"
        );
        seen.insert(id);
    }
    assert_eq!(seen.len(), 10, "all ten must round-trip");
}
