//! Tests for the real-home write guard.
//!
//! These matter more than most: the guard's entire value is that it fires on
//! an escaping write and stays quiet on a correctly sandboxed one. A guard
//! that fires on healthy work becomes noise and gets disabled, which is the
//! cry-wolf failure this codebase has hit before.
//!
//! They run only with `--features test-guard`, since without it the guard
//! compiles to nothing.

#![cfg(feature = "test-guard")]

use std::path::PathBuf;

/// Serialize env mutation across these tests. They edit process-global state,
/// so without this they corrupt each other non-deterministically.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn real_home() -> PathBuf {
    // The same source the guard uses, so a test cannot pass by comparing a
    // redirected $HOME against itself.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::ffi::OsStrExt;
        let pw = libc::getpwuid(libc::geteuid());
        assert!(!pw.is_null(), "no passwd entry for the current uid");
        let bytes = std::ffi::CStr::from_ptr((*pw).pw_dir).to_bytes();
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes).to_os_string())
    }
    #[cfg(not(unix))]
    PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"))
}

/// The escape this exists to catch, and the exact shape of all three
/// historical incidents: a write landing in the real `~/.jcode`.
///
/// Deliberately targets a file that does not exist and is never created: the
/// guard must panic before any filesystem work happens, which is the whole
/// point. If this test ever starts leaving a file behind, the guard has moved
/// to the wrong side of the write.
#[test]
fn a_write_into_the_real_jcode_home_panics() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let victim = real_home().join(".jcode").join("guard-probe-never-written");

    let result = std::panic::catch_unwind(|| crate::write_json(&victim, &serde_json::json!({})));

    let err = result.expect_err("a write into the real jcode home must panic");
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("real jcode state"),
        "panic should explain the escape, got: {msg}"
    );
    assert!(
        msg.contains("JCODE_HOME"),
        "panic should name the fix, got: {msg}"
    );
    assert!(
        !victim.exists(),
        "the guard must refuse before writing anything"
    );
}

/// A sandboxed write must be untouched. Without this the guard could pass its
/// other test by panicking unconditionally.
#[test]
fn a_write_into_a_temp_dir_is_allowed() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().expect("temp dir");
    let target = temp.path().join("nested").join("state.json");

    crate::write_json(&target, &serde_json::json!({"ok": true})).expect("sandboxed write");

    assert!(target.exists(), "the sandboxed write should have landed");
}

/// The case that makes containment usable on this machine.
///
/// `TMPDIR` here is `~/.jcode/scratch`, so a `TempDir` lives *inside* the very
/// directory being protected. A naive `starts_with` check flags every
/// correctly sandboxed test. This pins the exclusion so that regression is
/// caught rather than rediscovered.
#[test]
fn a_temp_dir_inside_the_protected_directory_is_still_allowed() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = real_home().join(".jcode").join("scratch");
    if !scratch.exists() {
        // Only meaningful on a machine whose TMPDIR is inside the home.
        return;
    }
    let temp = tempfile::Builder::new()
        .prefix("guard-containment-")
        .tempdir_in(&scratch)
        .expect("temp dir inside the protected directory");
    let target = temp.path().join("state.json");

    crate::write_json(&target, &serde_json::json!({"ok": true}))
        .expect("a temp dir inside the protected directory is still a sandbox");

    assert!(target.exists());
    assert!(
        target.starts_with(real_home().join(".jcode")),
        "this test is only meaningful if the path really is inside the protected dir"
    );
}

/// The opt-out has to work, or a test that legitimately writes to the real
/// home has no way out.
#[test]
fn the_explicit_opt_out_is_honoured() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().expect("temp dir");
    let target = temp.path().join("state.json");

    unsafe { std::env::set_var("JCODE_ALLOW_REAL_HOME_IN_TESTS", "1") };
    let result = std::panic::catch_unwind(|| crate::write_json(&target, &serde_json::json!({})));
    unsafe { std::env::remove_var("JCODE_ALLOW_REAL_HOME_IN_TESTS") };

    result
        .expect("the opt-out must suppress the panic")
        .expect("write should succeed");
}

/// Secret writes go through a different public entry point but the same
/// choke point. Pinned separately because auth tokens are exactly the kind of
/// thing a test should never scribble into the real home.
#[test]
fn a_secret_write_into_the_real_jcode_home_also_panics() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let victim = real_home().join(".jcode").join("guard-probe-secret");

    let result = std::panic::catch_unwind(|| crate::write_text_secret(&victim, "token"));

    result.expect_err("a secret write into the real jcode home must panic");
    assert!(!victim.exists(), "the guard must refuse before writing");
}
