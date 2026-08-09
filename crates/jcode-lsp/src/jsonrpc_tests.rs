//! JSON-RPC classification tests.
//!
//! Group B's territory. The recurring theme is that misclassification is not a
//! parse error but a deadlock: an unanswered server request stops the server.

use super::*;
use serde_json::json;

fn decoded(value: Value) -> Incoming {
    decode(value).expect("should classify")
}

#[test]
fn a_method_with_an_id_is_a_request_that_must_be_answered() {
    let message = decoded(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "workspace/configuration",
        "params": {"items": [{"section": "rust-analyzer"}]}
    }));
    match message {
        Incoming::Request { id, method, params } => {
            assert_eq!(id, RequestId::Number(7));
            assert_eq!(method, "workspace/configuration");
            assert_eq!(params["items"][0]["section"], "rust-analyzer");
        }
        other => panic!("a method plus an id is a request, got {other:?}"),
    }
}

#[test]
fn a_method_without_an_id_is_a_notification() {
    let message = decoded(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": "file:///a.rs", "diagnostics": []}
    }));
    match message {
        Incoming::Notification { method, .. } => {
            assert_eq!(method, "textDocument/publishDiagnostics");
        }
        other => panic!("a method with no id is a notification, got {other:?}"),
    }
}

#[test]
fn an_id_without_a_method_is_a_response() {
    let message = decoded(json!({"jsonrpc": "2.0", "id": 3, "result": {"ok": true}}));
    match message {
        Incoming::Response { id, result } => {
            assert_eq!(id, RequestId::Number(3));
            assert_eq!(result.expect("a success")["ok"], true);
        }
        other => panic!("an id with no method is a response, got {other:?}"),
    }
}

/// A notification carrying `"id": null` must not be treated as a request. If it
/// were, we would answer it, and a server may reject an unsolicited response.
#[test]
fn an_explicit_null_id_is_not_an_id() {
    let message = decoded(json!({"jsonrpc": "2.0", "id": null, "method": "initialized"}));
    match message {
        Incoming::Notification { method, .. } => assert_eq!(method, "initialized"),
        other => panic!("a null id is absent, not present, got {other:?}"),
    }
}

/// Servers do send string ids. Modelling ids as numbers works until one does not,
/// and the failure is a request we cannot answer.
#[test]
fn a_string_id_is_preserved_verbatim() {
    let message = decoded(json!({"id": "fake-1", "method": "workspace/applyEdit"}));
    match message {
        Incoming::Request { id, .. } => {
            assert_eq!(id, RequestId::String("fake-1".to_string()));
            // Round-trips to the same JSON, or our answer names an id the server
            // never sent and it waits forever.
            assert_eq!(
                serde_json::to_value(&id).expect("serialize"),
                json!("fake-1")
            );
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

/// A numeric id must serialize back as a number, not as a string. A server
/// matching `7` against `"7"` finds nothing, and the request never completes.
#[test]
fn a_numeric_id_round_trips_as_a_number() {
    let id = RequestId::Number(7);
    assert_eq!(serde_json::to_value(&id).expect("serialize"), json!(7));
}

/// **An id of an unmodelled shape must still be an id.** Found by probing the
/// type rather than by a mutation: an `untagged` enum of number-or-string yields
/// `None` for a float, a bool or an object, and a `None` id makes `decode`
/// classify a server *request* as a notification. We then never answer it and the
/// server waits forever — the deadlock this module's header is about, reached
/// through a type conversion instead of through the logic.
///
/// The spec says an id is a String, Number, or Null. A float is a Number, so
/// `1.5` is legal, and JSON has no integer type at all: any peer written against
/// a language where numbers are doubles can emit one.
///
/// # What this test does and does not prove
///
/// Honest note, because the distinction matters. Adding the `Other` variant made
/// the enum **total**, so `from_value` can no longer fail and the
/// `.ok()`-versus-`unwrap_or` line in `decode` became behaviourally dead. Reverting
/// only that line therefore does **not** fail this test. What does fail it is
/// dropping an `Other` id in `decode`, which was verified by mutation.
///
/// So this is a regression guard on a property that is currently structural, not
/// a live bug-catcher for the exact line that was changed. Recorded as such rather
/// than counted as stronger evidence than it is.
#[test]
fn an_unparseable_id_is_still_an_id_and_still_a_request() {
    for raw in [json!(1.5), json!(true), json!({"weird": true}), json!([1])] {
        let message = decoded(json!({"id": raw.clone(), "method": "workspace/configuration"}));
        match message {
            Incoming::Request { id, .. } => {
                // Echoed verbatim: we need not understand an id, only return
                // exactly what we were given, or the server cannot match it.
                assert_eq!(
                    serde_json::to_value(&id).expect("serialize"),
                    raw,
                    "the id must round-trip unchanged"
                );
            }
            other => panic!(
                "an id of {raw} must classify as a request, not {other:?} — \
                 a demoted request is never answered and stalls the server"
            ),
        }
    }
}

/// The same shape as a *response* must not be lost either, or the request that
/// produced it never completes and the caller waits out its timeout.
#[test]
fn an_unparseable_id_on_a_response_is_still_a_response() {
    let message = decoded(json!({"id": 1.5, "result": {"ok": true}}));
    match message {
        Incoming::Response { result, .. } => assert_eq!(result.expect("success")["ok"], true),
        other => panic!("expected a response, got {other:?}"),
    }
}

/// Ids are pending-map keys, so equality and hashing must agree. If they
/// disagree, a lookup misses a key the map is holding and the request hangs
/// despite its answer having arrived.
#[test]
fn ids_hash_consistently_with_equality_and_do_not_collide_across_kinds() {
    use std::collections::HashMap;

    let mut pending: HashMap<RequestId, &str> = HashMap::new();
    pending.insert(RequestId::Number(1), "number one");
    pending.insert(RequestId::String("1".into()), "string one");
    pending.insert(RequestId::Other(json!(1.5)), "float");

    // A numeric 1 and a string "1" are different ids. Collapsing them would
    // resolve one request with another's answer.
    assert_eq!(pending.len(), 3, "the three must not collide");
    assert_eq!(pending.get(&RequestId::Number(1)), Some(&"number one"));
    assert_eq!(
        pending.get(&RequestId::String("1".into())),
        Some(&"string one")
    );
    assert_eq!(pending.get(&RequestId::Other(json!(1.5))), Some(&"float"));

    assert_ne!(RequestId::Number(1), RequestId::String("1".into()));
    assert_eq!(RequestId::Number(1), RequestId::Number(1));
}

/// **The collision case**, which is the sharpest of omp's protocol tests. Client
/// and server ids are independent spaces, so a server request may reuse an id we
/// have in flight. One shared pending map answers the wrong one.
#[test]
fn a_server_request_and_our_pending_response_can_share_an_id() {
    let ours = decoded(json!({"id": 1, "result": {"mine": true}}));
    let theirs = decoded(json!({"id": 1, "method": "workspace/configuration"}));

    assert!(
        matches!(ours, Incoming::Response { .. }),
        "our answer must classify as a response"
    );
    assert!(
        matches!(theirs, Incoming::Request { .. }),
        "their call must classify as a request even with the same id"
    );
    // The id alone cannot disambiguate; only the presence of `method` can. A
    // client keying solely on id must therefore keep two separate maps.
}

#[test]
fn an_error_response_is_a_failure_not_a_null_success() {
    let message = decoded(json!({
        "id": 2,
        "error": {"code": -32601, "message": "Method not found: foo/bar"}
    }));
    match message {
        Incoming::Response { result, .. } => {
            let error = result.expect_err("must be an error");
            assert_eq!(error.code, METHOD_NOT_FOUND);
            assert!(error.is_method_not_found());
        }
        other => panic!("expected a response, got {other:?}"),
    }
}

/// `-32601` must be recognised by **code**, never by message text: servers word
/// it differently, and matching prose silently stops working. omp has a
/// regression for exactly this.
#[test]
fn method_not_found_is_recognised_by_code_regardless_of_wording() {
    for wording in [
        "Method not found",
        "Unhandled method foo/bar",
        "no such request",
        "",
    ] {
        let error = ResponseError {
            code: METHOD_NOT_FOUND,
            message: wording.to_string(),
            data: None,
        };
        assert!(
            error.is_method_not_found(),
            "wording must not matter: {wording:?}"
        );
    }

    // And the converse: a different code with a familiar message is *not* it.
    let misleading = ResponseError {
        code: -32603,
        message: "Method not found".to_string(),
        data: None,
    };
    assert!(
        !misleading.is_method_not_found(),
        "an internal error wearing the same words is a different failure"
    );
}

/// A response with neither `result` nor `error` is what a server means by "done,
/// nothing to say". `shutdown` answers this way.
#[test]
fn a_response_with_no_result_and_no_error_is_a_null_success() {
    let message = decoded(json!({"id": 4}));
    match message {
        Incoming::Response { result, .. } => {
            assert_eq!(result.expect("a success"), Value::Null);
        }
        other => panic!("expected a response, got {other:?}"),
    }
}

/// `"error": null` alongside a result is something servers emit. Treating a null
/// error as a failure would turn every such response into a spurious error.
#[test]
fn an_explicit_null_error_is_not_a_failure() {
    let message = decoded(json!({"id": 5, "result": {"ok": 1}, "error": null}));
    match message {
        Incoming::Response { result, .. } => {
            assert_eq!(result.expect("a success")["ok"], 1);
        }
        other => panic!("expected a response, got {other:?}"),
    }
}

/// A malformed error object must stay a failure. Falling back to a null success
/// would report a failed request as having succeeded, which the caller cannot
/// detect.
#[test]
fn a_malformed_error_object_is_still_a_failure() {
    let message = decoded(json!({"id": 6, "error": "just a string"}));
    match message {
        Incoming::Response { result, .. } => {
            let error = result.expect_err("must remain a failure");
            assert!(error.message.contains("malformed"), "{}", error.message);
        }
        other => panic!("expected a response, got {other:?}"),
    }
}

/// `jsonrpc: "2.0"` missing is technically a spec violation and practically
/// common. Refusing the message would be correct and useless.
#[test]
fn a_missing_jsonrpc_version_is_tolerated() {
    let message = decoded(json!({"id": 1, "method": "test/echo"}));
    assert!(matches!(message, Incoming::Request { .. }));
}

/// Servers add their own fields. Ignoring them is required; erroring is not.
#[test]
fn unknown_extra_fields_are_ignored() {
    let message = decoded(json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
        "somethingServerSpecific": {"nested": true}
    }));
    assert!(matches!(message, Incoming::Notification { .. }));
}

#[test]
fn absent_params_become_null_rather_than_an_error() {
    let message = decoded(json!({"method": "exit"}));
    match message {
        Incoming::Notification { params, .. } => assert_eq!(params, Value::Null),
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn a_message_with_neither_a_method_nor_an_id_is_refused() {
    let error = decode(json!({"jsonrpc": "2.0"})).expect_err("must be refused");
    assert_eq!(error, DecodeError::NeitherCallNorAnswer);
}

#[test]
fn a_non_object_message_is_refused() {
    assert_eq!(
        decode(json!([1, 2, 3])).expect_err("must be refused"),
        DecodeError::NotAnObject
    );
    assert_eq!(
        decode(json!("a string")).expect_err("must be refused"),
        DecodeError::NotAnObject
    );
}

#[test]
fn outgoing_messages_carry_the_protocol_version_and_their_fields() {
    let id = RequestId::Number(1);

    let request = request(&id, "textDocument/hover", &json!({"line": 3}));
    assert_eq!(request["jsonrpc"], "2.0");
    assert_eq!(request["id"], 1);
    assert_eq!(request["method"], "textDocument/hover");
    assert_eq!(request["params"]["line"], 3);

    // A notification must have no id at all. An `"id": null` would make some
    // servers answer it, and an unexpected response can desynchronise a strict
    // peer.
    let notification = notification("initialized", &json!({}));
    assert!(
        notification.get("id").is_none(),
        "a notification must not carry an id: {notification}"
    );

    let response = response(&id, &json!([null]));
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"], json!([null]));
    assert!(response.get("error").is_none());

    let failure = error_response(&id, METHOD_NOT_FOUND, "nope");
    assert_eq!(failure["error"]["code"], METHOD_NOT_FOUND);
    assert!(failure.get("result").is_none());
}

/// Our own messages must survive our own decoder. A round trip catches a
/// serialize/classify mismatch, which would otherwise only show up against a
/// live server.
#[test]
fn our_outgoing_messages_decode_as_the_kind_we_meant() {
    let id = RequestId::String("abc".into());

    assert!(matches!(
        decoded(request(&id, "m", &json!(null))),
        Incoming::Request { .. }
    ));
    assert!(matches!(
        decoded(notification("m", &json!(null))),
        Incoming::Notification { .. }
    ));
    assert!(matches!(
        decoded(response(&id, &json!(null))),
        Incoming::Response { result: Ok(_), .. }
    ));
    assert!(matches!(
        decoded(error_response(&id, -1, "x")),
        Incoming::Response { result: Err(_), .. }
    ));
}
