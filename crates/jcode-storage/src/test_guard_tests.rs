//! Tests for the real-home guard.
//!
//! These matter more than most: the guard's entire value is that it fires on
//! an escape and stays quiet on a correctly sandboxed test. A guard that fires
//! on healthy work becomes noise and gets disabled, which is the cry-wolf
//! failure this codebase has hit before.
//!
//! Note these run only with `--features test-guard`, since without it
//! `check_not_real_home` compiles to nothing. The crate's own suite is run
//! both ways in CI for that reason.

#![cfg(feature = "test-guard")]

use std::path::PathBuf;

/// Serialize env mutation across these tests. They all edit process-global
/// state, so without this they corrupt each other's setup non-deterministically
/// (the flake shape that hid a real defect in the config-save work).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn real_home() -> PathBuf {
    // Same source the guard uses, so the test cannot pass by comparing a
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

/// The escape this whole mechanism exists to catch: `JCODE_HOME` unset, `HOME`
/// pointing at the developer's real directory. This is the exact shape of all
/// three historical incidents.
#[test]
fn an_unsandboxed_resolution_panics() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("JCODE_HOME");
    unsafe { std::env::remove_var("JCODE_HOME") };

    let result = std::panic::catch_unwind(crate::jcode_dir);

    if let Some(prev) = prev {
        unsafe { std::env::set_var("JCODE_HOME", prev) };
    }

    let err = result.expect_err("resolving the real home must panic under the guard");
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("escaped its sandbox"),
        "panic should explain the escape, got: {msg}"
    );
    assert!(
        msg.contains("JCODE_HOME"),
        "panic should name the fix, got: {msg}"
    );
}

/// A properly sandboxed test must be untouched. Without this, the guard could
/// pass its other test by panicking unconditionally.
#[test]
fn a_sandboxed_resolution_is_allowed() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().expect("temp dir");
    unsafe { std::env::set_var("JCODE_HOME", temp.path()) };

    let resolved = crate::jcode_dir().expect("sandboxed resolution should succeed");

    match prev {
        Some(prev) => unsafe { std::env::set_var("JCODE_HOME", prev) },
        None => unsafe { std::env::remove_var("JCODE_HOME") },
    }
    assert_eq!(resolved, temp.path());
}

/// The false positive that would have made this unusable.
///
/// `save_github_token_creates_config_dir` unsets `JCODE_HOME` on purpose, to
/// exercise the XDG fallback, and redirects `HOME` so the fallback still lands
/// in a temp dir. That is correct and must not fire. A guard keyed on
/// "`JCODE_HOME` is unset" would fail it, which is why the guard compares the
/// resolved path against the passwd home instead.
#[test]
fn unsetting_jcode_home_is_fine_when_home_is_redirected() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_jcode_home = std::env::var_os("JCODE_HOME");
    let prev_home = std::env::var_os("HOME");
    let temp = tempfile::TempDir::new().expect("temp dir");

    unsafe {
        std::env::remove_var("JCODE_HOME");
        std::env::set_var("HOME", temp.path());
    }

    let result = std::panic::catch_unwind(crate::jcode_dir);

    unsafe {
        match prev_home {
            Some(prev) => std::env::set_var("HOME", prev),
            None => std::env::remove_var("HOME"),
        }
        if let Some(prev) = prev_jcode_home {
            std::env::set_var("JCODE_HOME", prev);
        }
    }

    let resolved = result
        .expect("redirected HOME is a correct sandbox and must not panic")
        .expect("resolution should succeed");
    assert!(
        resolved.starts_with(temp.path()),
        "should resolve inside the temp home, got {}",
        resolved.display()
    );
    // Not `starts_with(real_home())`: TMPDIR on this machine is
    // `~/.jcode/scratch`, so the sandbox legitimately lives *under* the real
    // home. What must not happen is resolving to the real state dir itself.
    assert_ne!(
        resolved,
        real_home().join(".jcode"),
        "must not resolve to the real jcode state directory"
    );
}

/// The opt-out has to work, or a legitimately-real-home test has no way out.
#[test]
fn the_explicit_opt_out_is_honoured() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("JCODE_HOME");
    unsafe {
        std::env::remove_var("JCODE_HOME");
        std::env::set_var("JCODE_ALLOW_REAL_HOME_IN_TESTS", "1");
    }

    let result = std::panic::catch_unwind(crate::jcode_dir);

    unsafe {
        std::env::remove_var("JCODE_ALLOW_REAL_HOME_IN_TESTS");
        if let Some(prev) = prev {
            std::env::set_var("JCODE_HOME", prev);
        }
    }

    result
        .expect("the opt-out must suppress the panic")
        .expect("resolution should succeed");
}
