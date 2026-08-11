use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Regression coverage for the menu bar counting phantom sessions.
///
/// Every server-owned session records the shared daemon's PID in the presence
/// registry, so the "is the recorded PID alive?" liveness check cannot tell a
/// real session from a marker orphaned by a lifecycle path that dropped the
/// session without unregistering it. Both look alive for as long as the daemon
/// runs, and `jcode menubar` counts both.
///
/// These cover the reconcile that makes such a marker self-healing: the live
/// session map is the authority on what the daemon hosts.
#[tokio::test]
async fn reconcile_drops_presence_markers_for_sessions_the_daemon_no_longer_hosts() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::TempDir::new().expect("temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    let daemon_pid = std::process::id();

    // Two sessions the daemon still hosts, plus an orphaned marker recorded
    // under the same PID by a session that is gone from the map.
    crate::storage::register_active_pid("session_live_one", daemon_pid);
    crate::storage::register_active_pid("session_live_two", daemon_pid);
    crate::storage::register_active_pid("session_orphaned", daemon_pid);

    // A marker owned by a different live process must never be touched: it
    // belongs to another jcode, not to this daemon.
    let other_pid = if daemon_pid == 1 { 2 } else { 1 };
    crate::storage::register_active_pid("session_other_process", other_pid);

    assert_eq!(
        crate::storage::session_presence().len(),
        4,
        "precondition: every marker looks alive under a shared-PID check"
    );

    let sessions: Arc<RwLock<HashMap<String, ()>>> = Arc::new(RwLock::new(HashMap::from([
        ("session_live_one".to_string(), ()),
        ("session_live_two".to_string(), ()),
    ])));

    let removed = super::reconcile_owned_session_presence(&sessions).await;
    assert_eq!(removed, vec!["session_orphaned".to_string()]);

    let live: Vec<String> = crate::storage::session_presence()
        .into_iter()
        .map(|presence| presence.session_id)
        .collect();
    assert!(
        !live.contains(&"session_orphaned".to_string()),
        "the orphaned marker must be dropped: {live:?}"
    );
    assert!(
        live.contains(&"session_live_one".to_string())
            && live.contains(&"session_live_two".to_string()),
        "hosted sessions must survive reconcile: {live:?}"
    );
    assert!(
        live.contains(&"session_other_process".to_string()),
        "another process's marker must not be touched: {live:?}"
    );

    // This is what the menu bar actually reads.
    assert_eq!(crate::storage::user_session_counts().total, 3);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

/// Reconcile must be a no-op when nothing is orphaned, so a healthy daemon
/// never removes a marker for a session it is still hosting.
#[tokio::test]
async fn reconcile_is_a_no_op_when_every_marker_is_hosted() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::TempDir::new().expect("temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    crate::storage::register_active_pid("session_only", std::process::id());
    let sessions: Arc<RwLock<HashMap<String, ()>>> =
        Arc::new(RwLock::new(HashMap::from([("session_only".to_string(), ())])));

    let removed = super::reconcile_owned_session_presence(&sessions).await;
    assert!(removed.is_empty(), "nothing should be removed: {removed:?}");
    assert_eq!(crate::storage::session_presence().len(), 1);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}
