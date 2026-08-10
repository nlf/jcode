//! Registry tests: sharing, replacement, and reaping.
//!
//! Group A's registry-semantics cases, which were blocked on the lifetime question. Uses the fake
//! server, so "a client" is a real process with a real handshake.
//!
//! An integration test rather than a unit one, because `CARGO_BIN_EXE_fake_lsp_server` is only set
//! for integration targets -- the registry's whole job is managing real processes, so testing it
//! without one would test nothing.

use std::sync::Arc;
use std::time::Duration;

use jcode_lsp::client::Client;
use jcode_lsp::registry::Registry;

use jcode_lsp::config::{Available, ServerConfig};
use serde_json::json;

/// A server spec pointing at the fake server binary.
fn fake(name: &str) -> Available {
    Available {
        name: name.to_string(),
        config: ServerConfig {
            command: "fake".to_string(),
            args: Vec::new(),
            file_types: vec![".rs".to_string()],
            root_markers: vec!["Cargo.toml".to_string()],
            init_options: None,
            settings: Some(json!({})),
            is_linter: false,
            disabled: false,
            warmup_timeout_ms: None,
            capabilities: Default::default(),
        },
        resolved: std::path::PathBuf::from(env!("CARGO_BIN_EXE_fake_lsp_server")),
    }
}

fn timeout() -> Duration {
    Duration::from_secs(5)
}

/// A project root that exists on disk.
///
/// The spawn sets the child's working directory to the root, and `Command` reports a missing
/// working directory as `No such file or directory` -- which reads as "the language server binary
/// is missing" and cost me a few minutes rebuilding a binary that was already there. Worth a
/// helper so no test in this file can make that mistake again.
fn root(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("jcode-lsp-registry-{name}"));
    std::fs::create_dir_all(&path).expect("create the project root");
    path
}

/// **Two calls for one project share a client.**
///
/// The reason the registry exists. A cold `rust-analyzer` takes tens of seconds, so a client per
/// call would make every navigation request pay that and none of them would finish.
#[tokio::test]
async fn two_calls_for_one_project_share_a_client() {
    let registry = Registry::new();
    let root = root("share");
    let server = fake("rust-analyzer");

    let first = registry
        .get_or_start(&root, &server, timeout())
        .await
        .expect("first start");
    let second = registry
        .get_or_start(&root, &server, timeout())
        .await
        .expect("second start");

    assert!(
        Arc::ptr_eq(&first, &second),
        "the second call started a second language server"
    );
    assert_eq!(registry.len().await, 1);

    registry.shutdown_all().await;
}

/// **Concurrent callers do not start two servers.**
///
/// omp's A2 case, and the reason `get_or_start` holds its lock across the handshake rather than
/// doing get-then-insert. Two `definition` calls arriving together must not spawn two
/// `rust-analyzer`s: that doubles the cold start and the memory, and the second finishes no
/// sooner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_start_only_one_server() {
    let registry = Arc::new(Registry::new());
    let root = root("concurrent");

    let mut handles = Vec::new();
    for _ in 0..4 {
        let registry = Arc::clone(&registry);
        let root = root.clone();
        handles.push(tokio::spawn(async move {
            registry
                .get_or_start(&root, &fake("rust-analyzer"), timeout())
                .await
                .expect("start")
        }));
    }

    let clients: Vec<Arc<Client>> = futures_lite_join(handles).await;
    assert_eq!(
        registry.len().await,
        1,
        "concurrent callers started more than one server"
    );
    for client in &clients[1..] {
        assert!(
            Arc::ptr_eq(&clients[0], client),
            "callers received different clients for one project"
        );
    }

    registry.shutdown_all().await;
}

/// Await a set of join handles, returning their values in order.
///
/// Hand-rolled rather than pulling in `futures`: the crate has no such dependency and this is the
/// only place that wants a join-all.
async fn futures_lite_join<T>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("a task panicked"));
    }
    out
}

/// **Different projects get different servers.**
///
/// A language server's model is scoped to a root, so pointing one at two trees is not supported.
/// Sharing by server name alone would resolve imports against whichever project started first.
#[tokio::test]
async fn different_projects_get_different_clients() {
    let registry = Registry::new();
    let server = fake("rust-analyzer");

    let one = registry
        .get_or_start(&root("project-one"), &server, timeout())
        .await
        .expect("one");
    let two = registry
        .get_or_start(&root("project-two"), &server, timeout())
        .await
        .expect("two");

    assert!(
        !Arc::ptr_eq(&one, &two),
        "two projects shared one language server, so imports resolve against the wrong tree"
    );
    assert_eq!(registry.len().await, 2);

    registry.shutdown_all().await;
}

/// Different servers in one project are separate clients.
#[tokio::test]
async fn different_servers_in_one_project_are_separate() {
    let registry = Registry::new();
    let root = root("two-servers");

    let rust = registry
        .get_or_start(&root, &fake("rust-analyzer"), timeout())
        .await
        .expect("rust");
    let types = registry
        .get_or_start(&root, &fake("typescript-language-server"), timeout())
        .await
        .expect("ts");

    assert!(!Arc::ptr_eq(&rust, &types));
    assert_eq!(registry.len().await, 2);

    registry.shutdown_all().await;
}

/// **An idle client is reaped.**
///
/// The behaviour that inverts omp. Their default is no timeout, which suits a process that exits;
/// our daemon is long-lived, so never reaping means one language server per project visited since
/// boot, resident forever with its index in memory.
#[tokio::test]
async fn an_idle_client_is_shut_down() {
    // A timeout short enough to test without sleeping for two minutes.
    let registry = Registry::with_idle_timeout(Some(Duration::from_millis(50)));
    let root = root("idle");

    let client = registry
        .get_or_start(&root, &fake("rust-analyzer"), timeout())
        .await
        .expect("start");
    assert_eq!(registry.len().await, 1);
    // Drop the caller's handle, or the registry cannot be the last owner and will decline to
    // shut it down -- which is itself deliberate, and tested below.
    drop(client);

    tokio::time::sleep(Duration::from_millis(80)).await;
    registry.reap_now().await;

    assert!(
        registry.is_empty().await,
        "an idle client was kept, so a daemon accumulates one server per project forever"
    );
}

/// A client still in use is not reaped from under its caller.
///
/// The registry only shuts down a client it is the last owner of. Tearing down a transport while a
/// caller is mid-request would fail that request for a reason having nothing to do with it.
#[tokio::test]
async fn a_client_still_held_by_a_caller_is_not_torn_down() {
    let registry = Registry::with_idle_timeout(Some(Duration::from_millis(50)));
    let root = root("in-use");

    let held = registry
        .get_or_start(&root, &fake("rust-analyzer"), timeout())
        .await
        .expect("start");

    tokio::time::sleep(Duration::from_millis(80)).await;
    registry.reap_now().await;

    // Removed from the map, so a later call starts fresh -- but the caller's client still works,
    // which is the property that matters.
    let echoed = held
        .request("test/echo", json!({"still": "alive"}), timeout())
        .await
        .expect("a held client must keep working after an idle sweep");
    assert_eq!(echoed["still"], "alive");
}

/// Using a client resets its idle clock.
///
/// Without this a busy client is reaped mid-conversation on a fixed schedule, and every reap costs
/// a cold start that the traffic pattern did not call for.
#[tokio::test]
async fn using_a_client_keeps_it_alive() {
    let registry = Registry::with_idle_timeout(Some(Duration::from_millis(120)));
    let root = root("refresh");
    let server = fake("rust-analyzer");

    let first = registry
        .get_or_start(&root, &server, timeout())
        .await
        .expect("start");
    drop(first);

    // Three touches inside the window, totalling more than the timeout.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let client = registry
            .get_or_start(&root, &server, timeout())
            .await
            .expect("reuse");
        drop(client);
    }

    assert_eq!(
        registry.len().await,
        1,
        "a client in continuous use was reaped anyway"
    );
    registry.shutdown_all().await;
}

/// With no timeout, nothing is reaped. omp's default, still available.
#[tokio::test]
async fn without_a_timeout_clients_are_kept() {
    let registry = Registry::with_idle_timeout(None);
    let root = root("forever");

    let client = registry
        .get_or_start(&root, &fake("rust-analyzer"), timeout())
        .await
        .expect("start");
    drop(client);

    tokio::time::sleep(Duration::from_millis(60)).await;
    registry.reap_now().await;

    assert_eq!(registry.len().await, 1, "a disabled timeout still reaped");
    registry.shutdown_all().await;
}

/// **A wedged client is replaced rather than handed out again.**
///
/// `unusable` means a partial frame desynchronised the stream, so nothing sent will ever be
/// understood. Returning it would produce one failure per call until the idle timeout reaped it,
/// and each failure would look like a language server problem.
#[tokio::test]
async fn a_wedged_client_is_replaced() {
    let registry = Registry::new();
    let root = root("wedged");
    let server = fake("rust-analyzer");

    let first = registry
        .get_or_start(&root, &server, timeout())
        .await
        .expect("start");

    // Wedge it: the server asks a question it will never read the answer to.
    let _ = first
        .request("test/askThenDeafen", json!({}), Duration::from_secs(20))
        .await;
    assert!(first.unusable(), "the fixture failed to wedge the client");

    let second = registry
        .get_or_start(&root, &server, timeout())
        .await
        .expect("replacement");

    assert!(
        !Arc::ptr_eq(&first, &second),
        "a wedged client was handed out again; every call would fail until it idled out"
    );
    assert!(!second.unusable(), "the replacement is not wedged");
    assert_eq!(registry.len().await, 1);

    registry.shutdown_all().await;
}

/// `shutdown_all` empties the registry.
///
/// For daemon exit. A language server that outlives its client is the leak with no symptom that
/// `Transport::wait_for_exit` documents.
#[tokio::test]
async fn shutdown_all_stops_every_client() {
    let registry = Registry::new();
    let server = fake("rust-analyzer");
    for project in ["a", "b", "c"] {
        let client = registry
            .get_or_start(&root(project), &server, timeout())
            .await
            .expect("start");
        drop(client);
    }
    assert_eq!(registry.len().await, 3);

    registry.shutdown_all().await;
    assert!(registry.is_empty().await);
}
