//! Client tests: the handshake, requests, and answering the server.
//!
//! Groups A and B, end to end against the real fake server. The recurring theme
//! is that the failures are hangs rather than wrong answers, so most of these
//! would time out rather than assert-fail if the behaviour regressed — which is
//! why each has a bounded deadline and a message saying what was expected.

use std::time::Duration;

use jcode_lsp::client::{Client, ServerSpec};
use jcode_lsp::correlation::RequestFailure;
use serde_json::{Value, json};

const PATIENT: Duration = Duration::from_secs(10);

async fn start(env: &[(&str, &str)], settings: Value) -> Client {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Client::start(
        ServerSpec {
            name: "fake".to_string(),
            program: env!("CARGO_BIN_EXE_fake_lsp_server").to_string(),
            args: Vec::new(),
            root: std::path::PathBuf::from("."),
            env,
            settings,
            init_options: json!({}),
        },
        PATIENT,
    )
    .await
    .expect("the handshake should complete")
}

/// The `test/state` view of what the server observed.
async fn state(client: &Client) -> Value {
    client
        .request("test/state", json!(null), PATIENT)
        .await
        .expect("test/state should answer")
}

#[tokio::test]
async fn the_handshake_completes_and_stores_capabilities() {
    let client = start(&[], json!({})).await;

    let capabilities = client.capabilities().await;
    assert_eq!(
        capabilities["definitionProvider"], true,
        "capabilities must be stored from initialize, got {capabilities}"
    );
    assert!(client.pid().is_some(), "a spawned server has a pid");
}

/// **The handshake order is fixed and load-bearing.** Configuration before
/// `initialized` is a violation some servers reject; a semantic request before
/// configuration runs unconfigured, which omp records as their #5276.
#[tokio::test]
async fn configuration_is_pushed_after_initialized_not_before() {
    let client = start(&[], json!({"rust-analyzer": {"checkOnSave": false}})).await;

    let observed = state(&client).await;
    let notifications: Vec<&str> = observed["notifications"]
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    let initialized = notifications
        .iter()
        .position(|method| *method == "initialized")
        .expect("initialized must be sent");
    let configured = notifications
        .iter()
        .position(|method| *method == "workspace/didChangeConfiguration")
        .expect("configuration must be pushed");
    assert!(
        initialized < configured,
        "configuration must follow initialized, got {notifications:?}"
    );
}

#[tokio::test]
async fn a_request_returns_the_servers_result() {
    let client = start(&[], json!({})).await;
    let echoed = client
        .request("test/echo", json!({"n": 7}), PATIENT)
        .await
        .expect("echo should answer");
    assert_eq!(echoed["n"], 7);
}

/// An unimplemented method is a *successful exchange with a negative answer*, and
/// must be distinguishable from a transport failure. A client that tears down on
/// `-32601` kills healthy servers.
#[tokio::test]
async fn an_unsupported_method_is_a_server_error_not_a_transport_failure() {
    let client = start(&[], json!({})).await;
    let failure = client
        .request("textDocument/nonsense", json!({}), PATIENT)
        .await
        .expect_err("must fail");

    assert!(
        failure.is_method_not_found(),
        "expected method-not-found, got {failure}"
    );
    // Still usable afterwards, which is the actual claim.
    let echoed = client
        .request("test/echo", json!({"alive": true}), PATIENT)
        .await
        .expect("the client must survive a -32601");
    assert_eq!(echoed["alive"], true);
}

/// A timeout must name the method and the duration. "LSP request timed out" with
/// neither is unactionable: a slow cold start and a wedged server read the same.
#[tokio::test]
async fn a_timeout_names_the_method_and_the_duration() {
    let client = start(&[("FAKE_LSP_HANG_ON", "test/echo")], json!({})).await;

    let failure = client
        .request("test/echo", json!({}), Duration::from_millis(300))
        .await
        .expect_err("a hanging request must time out");

    match &failure {
        RequestFailure::TimedOut { method, after } => {
            assert_eq!(method, "test/echo");
            assert_eq!(*after, Duration::from_millis(300));
        }
        other => panic!("expected a timeout, got {other}"),
    }
    let text = failure.to_string();
    assert!(text.contains("test/echo"), "{text}");
    assert!(text.contains("300"), "{text}");
}

/// A timed-out request must not leave anything behind. If it did, the map grows
/// for the life of the connection and a late answer resolves a caller that is
/// gone.
///
/// # This test used to be unable to fail
///
/// It asserted only that a *later* request still worked -- which it does whether or not
/// the timed-out one was forgotten, because a leaked entry inconveniences nobody
/// immediately. A reviewer reported three times that deleting the `forget` on the
/// timeout arm left the entire suite green, and they were right each time.
///
/// The leak has no symptom, so the test needs to look at the thing that leaks. Hence
/// `Client::outstanding`, which exists for this.
#[tokio::test]
async fn a_timed_out_request_does_not_wedge_later_ones() {
    let client = start(&[("FAKE_LSP_HANG_ON", "test/echo")], json!({})).await;

    let _ = client
        .request("test/echo", json!({}), Duration::from_millis(200))
        .await
        .expect_err("must time out");

    assert_eq!(
        client.outstanding().await,
        0,
        "a timed-out request stayed in the pending map; it grows for the life of the \
         connection and a late answer would resolve a caller that is gone"
    );

    // A different method still works.
    let observed = state(&client).await;
    assert_eq!(observed["initializeCount"], 1);

    // And the successful one did not leak either.
    assert_eq!(client.outstanding().await, 0);
}

/// **A timeout must return at its deadline, not at the deadline plus a courtesy
/// write.**
///
/// The `$/cancelRequest` sent after giving up is best-effort, and the server it is
/// aimed at is precisely the one likely to have stopped reading. Sending it inline
/// with the full 5-second write deadline turns a 300ms timeout into a 5.3s one.
///
/// # Why this asserts the deadline rather than reproducing the wedge
///
/// Two attempts to reproduce it end-to-end both passed against the buggy version,
/// for two different reasons, and the second is the interesting one:
///
/// 1. The first never filled the pipe, so no write ever blocked.
/// 2. The second did, but the *partial-write poison* from the fix in the previous
///    commit then made the request fail in **38µs** — before reaching the cancel
///    path at all. Measured, not assumed.
///
/// So on this transport the two protections overlap: a wedge severe enough to make
/// an inline cancel slow also poisons the connection, and a poisoned connection
/// fails instantly. The exposure is the narrow window where a write blocks *without*
/// landing a partial frame — real, since a full pipe rejects a small write outright,
/// but not reachable through the public API without racing.
///
/// What is asserted instead: a hanging (not wedged) server's timeout is bounded by
/// its own deadline plus a small margin. That covers the common case and would catch
/// a regression that made the cancel synchronous *and* slow for any reason. The
/// narrow window is documented rather than tested, which is the honest state.
#[tokio::test]
async fn a_timeout_is_bounded_by_its_own_deadline() {
    let client = start(&[("FAKE_LSP_HANG_ON", "test/echo")], json!({})).await;

    let started = std::time::Instant::now();
    let failure = client
        .request("test/echo", json!({}), Duration::from_millis(300))
        .await
        .expect_err("a hanging request must time out");
    let took = started.elapsed();

    assert!(
        matches!(failure, RequestFailure::TimedOut { .. }),
        "expected a timeout, got {failure}"
    );
    // A generous margin: the point is that it does not pay a further 5s write
    // deadline, not that it returns in exactly 300ms on a loaded machine.
    assert!(
        took < Duration::from_secs(2),
        "a 300ms timeout must return near its deadline; took {took:?}"
    );
}

/// **When the connection dies, every in-flight request fails with the cause.**
/// Without this each waits out its own timeout and reports a timeout instead of
/// the real reason, so one dead server costs one timeout per request and explains
/// none of them.
#[tokio::test]
async fn a_dying_server_fails_in_flight_requests_with_the_reason() {
    let client = start(&[("FAKE_LSP_EXIT_ON", "test/echo")], json!({})).await;

    // A generous timeout, so a timeout would be the *wrong* answer here: the
    // failure must arrive from the close, not from the clock.
    let failure = client
        .request("test/echo", json!({}), Duration::from_secs(30))
        .await
        .expect_err("the request cannot be answered by an exiting server");

    match &failure {
        RequestFailure::Server(error) => {
            // fail_all reports the method and the transport's reason.
            assert!(error.message.contains("test/echo"), "{}", error.message);
            assert!(
                error.message.contains("stdout") || error.message.contains("closed"),
                "the cause must be named, got {}",
                error.message
            );
        }
        RequestFailure::Closed { detail, .. } => {
            assert!(!detail.is_empty(), "a close must carry a detail");
        }
        other => panic!("expected a close-driven failure, not {other}"),
    }
}

/// A server dying at startup explains itself on stderr and nowhere else. That
/// tail must reach the caller, or every such failure reads as an unexplained
/// timeout.
#[tokio::test]
async fn a_server_that_fails_to_start_reports_its_stderr() {
    let failure = match Client::start(
        ServerSpec {
            name: "broken".to_string(),
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo 'cannot open shared object file: libclang.so' >&2; exit 1".to_string(),
            ],
            root: std::path::PathBuf::from("."),
            env: Vec::new(),
            settings: json!({}),
            init_options: json!({}),
        },
        Duration::from_secs(5),
    )
    .await
    {
        Err(failure) => failure,
        Ok(_) => panic!("a server that exits immediately cannot complete a handshake"),
    };

    let text = failure.to_string();
    assert!(
        text.contains("libclang") || text.contains("initialize"),
        "the failure should explain itself, got {text}"
    );
}

/// **Group B: the server's questions must be answered, in order, with `null` for
/// sections we do not have.** The server matches the array positionally, so a
/// reordered answer hands it another section's settings.
///
/// This is omp's `answers missing workspace configuration sections with null in
/// request order`, whose assertion is the literal array `[null, true, null]`.
/// Asserting the array rather than "the server survived" is the difference between
/// checking the behaviour and checking that nothing exploded.
#[tokio::test]
async fn a_configuration_pull_is_answered_in_request_order_with_null_for_gaps() {
    let client = start(&[], json!({"html.auto_closing_tags": true})).await;

    let answered = client
        .request(
            "test/serverRequest",
            json!({
                "method": "workspace/configuration",
                "params": {"items": [
                    {"section": "razor.format.attribute_indent_style"},
                    {"section": "html.auto_closing_tags"},
                    {}
                ]}
            }),
            PATIENT,
        )
        .await
        .expect("the server request should round-trip");

    assert_eq!(
        answered["result"],
        json!([null, true, null]),
        "positional order and null gaps both matter, got {}",
        answered["result"]
    );
    assert_eq!(answered["error"], Value::Null, "this must not be an error");
}

/// A dotted section addresses a nested path when the settings are nested that
/// way. Answering `null` for every nested request is how a server ends up running
/// with none of its configuration, and it is silent.
///
/// Note the exact-key case is checked above (`html.auto_closing_tags` is a
/// literal key), so between them both spellings are pinned.
#[tokio::test]
async fn dotted_configuration_sections_resolve_nested_paths() {
    let client = start(
        &[],
        json!({"rust-analyzer": {"cargo": {"features": "all"}}}),
    )
    .await;

    let answered = client
        .request(
            "test/serverRequest",
            json!({
                "method": "workspace/configuration",
                "params": {"items": [
                    {"section": "rust-analyzer.cargo"},
                    {"section": "rust-analyzer.cargo.features"},
                    {"section": "rust-analyzer.missing"}
                ]}
            }),
            PATIENT,
        )
        .await
        .expect("round-trip");

    assert_eq!(
        answered["result"],
        json!([{"features": "all"}, "all", null]),
        "a dotted section must walk the nested path, got {}",
        answered["result"]
    );
}

/// An unknown server request gets `-32601`, which is an **answer**. A server told
/// "method not found" moves on; a server told nothing waits forever.
#[tokio::test]
async fn an_unknown_server_request_is_answered_with_method_not_found() {
    let client = start(&[], json!({})).await;

    let answered = client
        .request(
            "test/serverRequest",
            json!({"method": "window/somethingWeDoNotHandle", "params": {}}),
            PATIENT,
        )
        .await
        .expect("round-trip");

    // An *answer*, and specifically an error with the spec's code. Silence would
    // leave the server blocked; a null success would tell it we handled something
    // we did not.
    assert_eq!(
        answered["error"]["code"], -32601,
        "an unhandled server request must be refused by code, got {}",
        answered["error"]
    );

    // And the server keeps serving, which is the consequence that matters.
    let observed = state(&client).await;
    assert_eq!(observed["initializeCount"], 1);
}

/// `workspace/applyEdit` is refused in v1, but refused **in the spec's terms** so
/// the server can react. An error would read as a client fault; `applied: false`
/// is a legitimate answer that servers handle.
#[tokio::test]
async fn a_server_initiated_edit_is_refused_in_the_specs_terms() {
    let client = start(&[], json!({})).await;

    let answered = client
        .request(
            "test/serverRequest",
            json!({
                "method": "workspace/applyEdit",
                "params": {"edit": {"changes": {}}}
            }),
            PATIENT,
        )
        .await
        .expect("round-trip");

    assert_eq!(
        answered["result"]["applied"], false,
        "v1 must refuse, got {}",
        answered["result"]
    );
    assert!(
        answered["result"]["failureReason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "a refusal must say why, got {}",
        answered["result"]
    );
    assert_eq!(
        answered["error"],
        Value::Null,
        "refusing is not an error: an error reads as a client fault"
    );
}

/// Dynamic registration must be recorded *and* acknowledged. Some servers block
/// semantic requests until it succeeds, and a client that only checks static
/// capabilities concludes such a server can do nothing.
#[tokio::test]
async fn dynamic_registration_is_recorded_and_acknowledged() {
    let client = start(&[("FAKE_LSP_NO_CAPABILITIES", "1")], json!({})).await;

    // Statically, nothing.
    assert!(
        !client.supports("hoverProvider", "textDocument/hover").await,
        "the fixture advertised no capabilities"
    );

    client
        .request(
            "test/serverRequest",
            json!({
                "method": "client/registerCapability",
                "params": {"registrations": [
                    {"id": "hover-1", "method": "textDocument/hover"}
                ]}
            }),
            PATIENT,
        )
        .await
        .expect("round-trip");

    // The registration is asynchronous relative to our request, so allow it to
    // land. A poll rather than a sleep, so a slow machine does not flake.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if client.supports("hoverProvider", "textDocument/hover").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("a dynamically registered capability must be visible to `supports`");
}

/// Diagnostics are pushed, not requested. A client that only reads while awaiting
/// a response loses them, and diagnostics are the tool's main product.
#[tokio::test]
async fn pushed_diagnostics_are_cached_by_uri() {
    let client = start(&[], json!({})).await;
    let uri = "file:///tmp/a.rs";

    client
        .open_document(uri, "rust", "fn main() {}\n")
        .await
        .expect("didOpen");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(published) = client.diagnostics_for(uri).await {
            assert_eq!(published.diagnostics.len(), 1);
            assert_eq!(published.diagnostics[0]["message"], "fake");
            // The fixture echoes the version, which is what makes freshness
            // decidable.
            assert_eq!(published.version, Some(1));
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pushed diagnostics never arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A second `didOpen` for one URI is a client bug: the server already tracks the
/// document, and re-opening resets its version expectations so every later
/// `didChange` looks stale.
#[tokio::test]
async fn opening_a_document_twice_sends_only_one_did_open() {
    let client = start(&[], json!({})).await;
    let uri = "file:///tmp/b.rs";

    client.open_document(uri, "rust", "").await.expect("first");
    assert!(client.is_open(uri).await);
    client.open_document(uri, "rust", "").await.expect("second");

    let observed = state(&client).await;
    assert_eq!(
        observed["didOpen"][uri], 1,
        "the second open must be suppressed, got {}",
        observed["didOpen"]
    );
}

/// Closing must forget the version, and closing something never opened must not
/// tell the server about a document it does not have.
#[tokio::test]
async fn closing_a_document_forgets_it_and_closing_twice_is_a_no_op() {
    let client = start(&[], json!({})).await;
    let uri = "file:///tmp/c.rs";

    client.open_document(uri, "rust", "").await.expect("open");
    client.close_document(uri).await.expect("close");
    assert!(!client.is_open(uri).await);

    client.close_document(uri).await.expect("close again");
    let observed = state(&client).await;
    let closed = observed["didClose"].as_array().expect("an array").len();
    assert_eq!(closed, 1, "only one didClose should have been sent");
}

/// `workspace/workspaceFolders` must be answered with the real root.
///
/// **This test exists because clippy found the gap, not because I planned it.**
/// `Client.root` was flagged as never read, which was true: the request fell
/// through to `-32601`, and a server told "method not found" for folders falls
/// back to guessing a root. It then resolves imports against the wrong tree, which
/// presents as "definition not found" for code that plainly exists — a wrong
/// answer rather than an error, which is the worst shape.
#[tokio::test]
async fn a_workspace_folders_request_is_answered_with_the_root() {
    let client = start(&[], json!({})).await;

    let answered = client
        .request(
            "test/serverRequest",
            json!({"method": "workspace/workspaceFolders", "params": null}),
            PATIENT,
        )
        .await
        .expect("round-trip");

    let folders = answered["result"]
        .as_array()
        .expect("an array of folders, got a non-array");
    assert_eq!(folders.len(), 1, "one root was configured");
    assert!(
        folders[0]["uri"]
            .as_str()
            .is_some_and(|uri| uri.starts_with("file://")),
        "a folder needs a file URI, got {}",
        folders[0]
    );
    assert!(
        folders[0]["name"].is_string(),
        "a folder needs a name, got {}",
        folders[0]
    );
    assert_eq!(
        answered["error"],
        Value::Null,
        "answering with -32601 would send the server guessing at a root"
    );
}

/// A clean shutdown must actually end the process, and say so truthfully.
#[tokio::test]
async fn shutdown_ends_the_server_and_reports_success() {
    let mut client = start(&[], json!({})).await;
    assert!(
        client.shutdown().await,
        "a healthy server should shut down cleanly"
    );
}

/// **A server that ignores `exit` must still be gone afterwards, and the report
/// must be truthful.** Reporting a restart when the process survived is the
/// daemon leak with no symptom, and omp has a regression for exactly that.
#[tokio::test]
async fn shutdown_escalates_to_a_kill_when_exit_is_ignored() {
    let mut client = start(&[("FAKE_LSP_SKIP_EXIT", "1")], json!({})).await;
    assert!(
        client.shutdown().await,
        "shutdown must escalate until the process is confirmed gone"
    );
}

/// Requests after shutdown must fail rather than hang. A caller that does not
/// know the client is dead should learn immediately.
///
/// Also covers the *other* `forget`: a request whose write fails was never sent, so
/// nobody will ever answer it and leaving it pending leaks until the connection dies.
/// A mutation deleting that one survived the suite even after the timeout arm was
/// covered, because "the request failed" is true either way.
#[tokio::test]
async fn requests_after_shutdown_fail_promptly() {
    let mut client = start(&[], json!({})).await;
    client.shutdown().await;

    let failure = client
        .request("test/echo", json!({}), Duration::from_secs(2))
        .await
        .expect_err("a shut-down client cannot answer");
    // Any failure is acceptable; hanging is not. The deadline above is the test.
    assert!(!failure.to_string().is_empty());

    assert_eq!(
        client.outstanding().await,
        0,
        "a request that was never written stayed in the pending map"
    );
}

/// **An identical republish still advances the generation counter.**
///
/// This is the case the counter exists for, and the one a naive implementation gets
/// wrong. When a server re-analyses a file and finds the same problems, it publishes
/// the same diagnostics at the same version. Nothing in the payload distinguishes
/// that from no publish at all — so a freshness wait that compares only content and
/// version concludes "nothing new" and accepts a result computed *before* the edit it
/// is supposed to be reporting on.
///
/// Asserted through the public API rather than by reading the field: what callers get
/// from `observation_for` is what has to change, since that is what `FreshnessWait`
/// consumes.
#[tokio::test]
async fn an_identical_republish_advances_the_generation() {
    let client = start(&[], json!({})).await;
    let uri = "file:///tmp/republish.rs";

    client
        .open_document(uri, "rust", "fn main() {}")
        .await
        .expect("open");
    let first = settled_observation(&client, uri, 0).await;
    assert!(
        first.diagnostics.is_some(),
        "didOpen must publish before this test means anything"
    );

    // Same URI, same version, byte-identical diagnostics.
    client
        .notify("test/republish", json!({"uri": uri, "version": 1}))
        .await
        .expect("republish");
    let second = settled_observation(&client, uri, first.generation).await;

    assert_eq!(
        second.diagnostics, first.diagnostics,
        "the fixture must republish identical content, or this tests the easy case"
    );
    assert_eq!(second.version, first.version, "and at the same version");
    assert!(
        second.generation > first.generation,
        "an identical republish must still count as a new publish; \
         generation stayed at {}",
        first.generation
    );
}

/// Wait for an observation whose generation has moved past `after`.
///
/// Publishes are asynchronous notifications, so there is no reply to await. Polling
/// with a deadline is the honest way to observe one: a fixed sleep is either flaky or
/// slow, and there is nothing to synchronise on.
async fn settled_observation(
    client: &jcode_lsp::Client,
    uri: &str,
    after: u64,
) -> jcode_lsp::Observation {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observation = client.observation_for(uri).await;
        if observation.generation > after {
            return observation;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no publish advanced the generation past {after} within 5s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **A server that renormalizes the URI still matches its own publish.**
///
/// omp's freshness suite has a server publishing for `/%72enormalized.ts` after being
/// given `/renormalized.ts`, and their diagnostics map is keyed through an equivalence
/// function so it matches anyway.
///
/// `equivalent_uris` existed here, with tests, while both lookups on the client did
/// exact-string `get` -- so the function was dead code on the hot path and this whole
/// case was unhandled. A reviewer caught it; an earlier commit of mine had even improved
/// that function without noticing nothing called it.
///
/// Consequence if unmatched: the freshness wait sees no publish, waits out its full
/// timeout, and reports no diagnostics for a file the server analysed correctly.
#[tokio::test]
async fn a_renormalized_publish_still_matches_the_uri_we_asked_about() {
    let client = start(&[], json!({})).await;
    let ours = "file:///tmp/renormalized.rs";
    // What a lax server might publish instead: percent-encoded first letter of the
    // basename, exactly as omp's fixture does.
    let theirs = "file:///tmp/%72enormalized.rs";

    client
        .notify("test/republish", json!({"uri": theirs, "version": 1}))
        .await
        .expect("republish");

    // Poll until the publish lands, then assert we can find it under *our* spelling.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client.diagnostics_for(ours).await.is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a publish under an equivalent URI was never matched; equivalent_uris is \
             not wired into the lookup"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // And the observation path, which is what FreshnessWait consumes.
    let observation = client.observation_for(ours).await;
    assert!(
        observation.diagnostics.is_some(),
        "observation_for did not match the renormalized publish"
    );
}

/// A different file is still a different file.
///
/// The equivalence scan must not become "any URI matches any publish", which would make
/// diagnostics appear against files that have none.
#[tokio::test]
async fn an_unrelated_uri_does_not_match_a_publish() {
    let client = start(&[], json!({})).await;

    client
        .notify(
            "test/republish",
            json!({"uri": "file:///tmp/one.rs", "version": 1}),
        )
        .await
        .expect("republish");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while client.diagnostics_for("file:///tmp/one.rs").await.is_none() {
        assert!(std::time::Instant::now() < deadline, "no publish arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        client.diagnostics_for("file:///tmp/two.rs").await.is_none(),
        "an unrelated URI matched another file's diagnostics"
    );
    // A prefix of the real path must not match either.
    assert!(
        client.diagnostics_for("file:///tmp/one").await.is_none(),
        "a prefix matched"
    );
}

/// **A server that renormalizes mid-session must not leave a stale entry behind.**
///
/// The subtle half of URI equivalence, and a wrong answer rather than a missed one.
///
/// Keying the map by the spelling the server used let two spellings of one file coexist
/// as separate entries. An exact-match lookup then returned whichever the *caller* asked
/// with, which is the older entry when a server publishes raw first and encoded later.
///
/// Measured before the fix: publish `/renorm.rs` at v1, then `/%72enorm.rs` at v7, and
/// `observation_for("/renorm.rs")` returned generation 2 with **version 1**. A freshness
/// waiter sees the generation move, re-reads, exact-hits the stale entry and settles on
/// pre-edit content. That is precisely what the freshness module exists to prevent, and
/// it was introduced by the commit that fixed the missed-publish case.
///
/// Found by an adversarial reviewer on the fourth pass, in code the third pass approved.
#[tokio::test]
async fn a_renormalized_republish_replaces_the_earlier_spelling() {
    let client = start(&[], json!({})).await;
    let raw = "file:///tmp/renorm.rs";
    let encoded = "file:///tmp/%72enorm.rs";

    client
        .notify("test/republish", json!({"uri": raw, "version": 1}))
        .await
        .expect("first publish");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while client.diagnostics_for(raw).await.is_none() {
        assert!(std::time::Instant::now() < deadline, "no first publish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let first = client.observation_for(raw).await;
    assert_eq!(first.version, Some(1));

    // The same file, spelled differently, at a newer version.
    client
        .notify("test/republish", json!({"uri": encoded, "version": 7}))
        .await
        .expect("second publish");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while client.observation_for(raw).await.generation == first.generation {
        assert!(std::time::Instant::now() < deadline, "no second publish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let second = client.observation_for(raw).await;
    assert_eq!(
        second.version,
        Some(7),
        "asking with the original spelling returned the stale entry; two spellings of \
         one file are coexisting in the map"
    );
    // And asking with the server's new spelling agrees.
    assert_eq!(client.observation_for(encoded).await.version, Some(7));
}

/// The server's own spelling survives, since later requests have to echo it.
///
/// This is why the map is not simply keyed by the normalized form with the spelling
/// discarded: a server that publishes `%72enorm.rs` may only recognise that spelling.
#[tokio::test]
async fn the_servers_spelling_is_retained_alongside_the_normalized_key() {
    let client = start(&[], json!({})).await;
    let encoded = "file:///tmp/%72etained.rs";

    client
        .notify("test/republish", json!({"uri": encoded, "version": 3}))
        .await
        .expect("publish");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while client.diagnostics_for(encoded).await.is_none() {
        assert!(std::time::Instant::now() < deadline, "no publish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Found by the decoded spelling, but reporting the encoded one.
    let published = client
        .diagnostics_for("file:///tmp/retained.rs")
        .await
        .expect("must be found under the equivalent spelling");
    assert_eq!(
        published.uri, encoded,
        "the server's spelling was lost, so a later request would use one it may not \
         recognise"
    );
}
