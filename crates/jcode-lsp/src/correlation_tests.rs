//! Correlation tests.
//!
//! The id-collision property is the one that matters most here: it is Group B's
//! sharpest case and the failure it prevents is a wedged handshake rather than a
//! wrong answer.

use super::*;

/// The server's answer, or a panic naming what arrived instead.
///
/// A helper because the channel now yields `Answer`, and unwrapping "delivered", then
/// "answered rather than closed", then "ok rather than error" at every call site buries
/// what each test is actually about.
fn answered(answer: Answer) -> Result<serde_json::Value, ResponseError> {
    match answer {
        Answer::Answered(result) => result,
        Answer::Closed(detail) => panic!("expected an answer, got a closed connection: {detail}"),
    }
}
use crate::jsonrpc::{self, Incoming};
use serde_json::json;

#[tokio::test]
async fn a_registered_request_receives_its_answer() {
    let pendings = Pendings::new();
    let id = pendings.next_id();
    let receive = pendings.register(id.clone(), "textDocument/hover").await;

    pendings
        .complete(&id, Ok(json!({"contents": "docs"})))
        .await;

    let result = receive.await.expect("the channel should deliver");
    assert_eq!(answered(result).expect("a success")["contents"], "docs");
    assert_eq!(
        pendings.outstanding().await,
        0,
        "a completed request must leave the map"
    );
}

#[tokio::test]
async fn an_error_answer_reaches_the_caller_as_an_error() {
    let pendings = Pendings::new();
    let id = pendings.next_id();
    let receive = pendings.register(id.clone(), "textDocument/rename").await;

    pendings
        .complete(
            &id,
            Err(ResponseError {
                code: crate::jsonrpc::METHOD_NOT_FOUND,
                message: "no rename here".into(),
                data: None,
            }),
        )
        .await;

    let error = receive.await.expect("delivered");
    let error = answered(error).expect_err("must be an error");
    assert!(RequestFailure::Server(error).is_method_not_found());
}

/// Ids must never be reused within a connection. If they were, a late answer to
/// an abandoned request could resolve a new one holding the same id — which
/// returns one method's result to another method's caller.
#[tokio::test]
async fn ids_are_monotonic_and_never_reused() {
    let pendings = Pendings::new();
    let first = pendings.next_id();
    let second = pendings.next_id();
    assert_ne!(first, second);

    // Even after the first completes, its id must not come back.
    pendings.register(first.clone(), "a").await;
    pendings.complete(&first, Ok(json!(null))).await;
    let third = pendings.next_id();
    assert_ne!(third, first);
    assert_ne!(third, second);
}

/// A late answer to something nobody awaits is not a fault. It happens whenever a
/// request times out and the server replies afterwards, and treating it as an
/// error would turn a normal race into noise.
#[tokio::test]
async fn an_answer_to_an_unknown_id_is_dropped_quietly() {
    let pendings = Pendings::new();
    // Must not panic, must not deadlock.
    pendings
        .complete(&RequestId::Number(999), Ok(json!({"late": true})))
        .await;
    assert_eq!(pendings.outstanding().await, 0);
}

/// Abandoning a request must remove it, or the map grows for the life of the
/// connection and a later answer resolves a caller that has gone.
#[tokio::test]
async fn forgetting_a_request_removes_it_and_names_it() {
    let pendings = Pendings::new();
    let id = pendings.next_id();
    pendings
        .register(id.clone(), "textDocument/definition")
        .await;

    assert_eq!(
        pendings.forget(&id).await.as_deref(),
        Some("textDocument/definition"),
        "forget should report what was abandoned, for the error message"
    );
    assert_eq!(pendings.outstanding().await, 0);
    assert_eq!(
        pendings.forget(&id).await,
        None,
        "forgetting twice is a no-op"
    );
}

/// **Every outstanding request must fail when the connection dies.** Without
/// this, each in-flight caller waits out its own timeout, so a server that died
/// instantly still costs one full timeout per request and reports the wrong cause.
#[tokio::test]
async fn a_dead_connection_fails_every_outstanding_request_at_once() {
    let pendings = Pendings::new();
    let mut receivers = Vec::new();
    for method in ["hover", "definition", "references"] {
        let id = pendings.next_id();
        receivers.push((method, pendings.register(id, method).await));
    }
    assert_eq!(pendings.outstanding().await, 3);

    pendings
        .fail_all("the language server closed its stdout (stderr: no such file)")
        .await;

    assert_eq!(pendings.outstanding().await, 0, "the map must be emptied");
    for (_method, receive) in receivers {
        // **`Closed`, not `Answered`.** This used to fabricate a `ResponseError` with
        // `code: 0`, which the client turned into `RequestFailure::Server` -- the variant
        // whose documentation promises the server answered and is healthy. For a dead
        // connection that is backwards, and a caller matching on it to decide "do not
        // reconnect, this server just lacks the method" would conclude the opposite of
        // the truth.
        //
        // The old version of this test asserted `code == 0` and called it correct in a
        // comment. The assertion was faithful to the implementation and the implementation
        // was wrong, which is the failure mode of a test written from the code.
        match receive.await.expect("delivered") {
            Answer::Closed(detail) => assert!(
                detail.contains("stdout"),
                "the transport's reason must survive: {detail}"
            ),
            other => panic!("a dead connection must report Closed, got {other:?}"),
        }
    }
}

/// A caller that gave up must not make delivery fail loudly. The reader task
/// completes requests and must not care whether anyone is still listening.
#[tokio::test]
async fn completing_a_request_whose_caller_dropped_is_harmless() {
    let pendings = Pendings::new();
    let id = pendings.next_id();
    let receive = pendings.register(id.clone(), "hover").await;
    drop(receive);

    pendings.complete(&id, Ok(json!(null))).await;
    assert_eq!(pendings.outstanding().await, 0);
}

/// **The collision case.** A server request carrying an id we have outstanding
/// must not be matched against our pending map. `decode` classifies on `method`
/// first, which is what keeps the two apart; this test pins the consequence at
/// the level where the bug would actually bite.
///
/// The failure it prevents: our request resolves with a `method` field it cannot
/// use, *and* the server's configuration pull is dropped — so the server blocks
/// forever waiting for an answer it will never get. omp's comment records this as
/// a wedged lazy cold start (their #3001).
#[tokio::test]
async fn a_server_request_sharing_our_id_does_not_resolve_our_request() {
    let pendings = Pendings::new();

    // Ours, outstanding, with id 1.
    let ours = pendings.next_id();
    assert_eq!(ours, RequestId::Number(1));
    let receive = pendings
        .register(ours.clone(), "textDocument/documentSymbol")
        .await;

    // The server asks us something, also with id 1. Legal: separate id spaces.
    let inbound = jsonrpc::decode(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "workspace/configuration",
        "params": {"items": [{"section": "rust-analyzer"}]}
    }))
    .expect("classify");

    // The routing rule: a message with a method is server-originated, full stop.
    let server_request = match inbound {
        Incoming::Request { id, method, params } => ServerRequest { id, method, params },
        other => panic!("a method plus an id must be a request, got {other:?}"),
    };
    assert_eq!(server_request.method, "workspace/configuration");
    assert_eq!(
        server_request.id, ours,
        "the collision is real, not contrived"
    );

    // Ours is untouched: still outstanding, still unresolved.
    assert_eq!(
        pendings.outstanding().await,
        1,
        "the server's request must not have consumed our pending entry"
    );

    // And it still completes correctly when its real answer arrives.
    pendings
        .complete(&ours, Ok(json!([{"name": "main"}])))
        .await;
    let result = answered(receive.await.expect("delivered")).expect("a success");
    assert_eq!(result[0]["name"], "main");
}

/// The converse: a genuine response with no `method` must reach the pending map.
/// A rule that sent everything to the server-request path would be equally broken
/// and would hang every request instead.
#[tokio::test]
async fn a_response_without_a_method_is_routed_to_the_pending_map() {
    let pendings = Pendings::new();
    let id = pendings.next_id();
    let receive = pendings.register(id.clone(), "test/echo").await;

    let inbound = jsonrpc::decode(json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}))
        .expect("classify");
    match inbound {
        Incoming::Response { id, result } => pendings.complete(&id, result).await,
        other => panic!("expected a response, got {other:?}"),
    }

    assert_eq!(
        answered(receive.await.expect("delivered")).expect("success")["ok"],
        true
    );
}

/// Concurrent registration must not lose entries. `batch` can drive several tool
/// calls at once, and each may have a request in flight against one server.
#[tokio::test]
async fn concurrent_registrations_all_survive() {
    let pendings = Arc::new(Pendings::new());
    let mut handles = Vec::new();
    for _ in 0..32 {
        let pendings = Arc::clone(&pendings);
        handles.push(tokio::spawn(async move {
            let id = pendings.next_id();
            let receive = pendings.register(id.clone(), "hover").await;
            (id, receive)
        }));
    }

    let mut registered = Vec::new();
    for handle in handles {
        registered.push(handle.await.expect("task"));
    }
    assert_eq!(
        pendings.outstanding().await,
        32,
        "every concurrent registration must be recorded"
    );

    // Every id distinct, so none overwrote another.
    let ids: std::collections::BTreeSet<String> =
        registered.iter().map(|(id, _)| id.to_string()).collect();
    assert_eq!(ids.len(), 32, "ids must be unique under concurrency");

    for (id, receive) in registered {
        pendings.complete(&id, Ok(json!(null))).await;
        answered(receive.await.expect("delivered")).expect("success");
    }
}
