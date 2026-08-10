//! A guard against tests escaping to the developer's real jcode home.
//!
//! # Why this exists
//!
//! Three separate test suites have written to the real `~/.jcode/config.toml`:
//! the `jcode-tui` command tests (which cleared `display.colors` and saved),
//! `save_github_token_creates_config_dir` (which unsets `JCODE_HOME`
//! deliberately to exercise the XDG fallback, but then records a trusted auth
//! source, which saves), and a third in the same family. Each was caught by
//! accident, months later, by a human noticing their settings had vanished.
//!
//! The common shape is not that someone forgot a guard. It is that **forgetting
//! is silent**: a test that escapes its sandbox passes exactly like one that
//! does not. Every existing helper (`EnvVarGuard`, `ScopedEnvVar`, `EnvGuard`,
//! `lock_test_env` — four spellings across the workspace) works correctly when
//! used. The gap was never ergonomics, it was detection.
//!
//! # How it works, and why it is a feature rather than `cfg!(test)`
//!
//! `cfg!(test)` is per-crate: it is true only while compiling this crate's own
//! tests, and false when another crate's tests call in. All three real
//! incidents were exactly that shape, so `cfg!(test)` would have caught none of
//! them.
//!
//! Instead this is a cargo feature that consumers enable **through their
//! `[dev-dependencies]`**. Dev-dependencies are compiled only for test targets,
//! so under feature unification the guard is active for a crate's tests and
//! absent from every shipped binary.
//!
//! # It must never fire in a real build
//!
//! A developer running a debug or self-dev build has to be able to write to
//! their real config. Approaches keyed on `debug_assertions` would break that:
//! a plain `cargo build` binary has `debug_assertions = true`. Measured on this
//! workspace, `profile.selfdev` inherits `release` and reports
//! `debug_assertions=false`, but that is incidental and not something to rely
//! on. Gating on a dev-dependency feature is structural: there is no profile
//! and no flag that turns it on for a binary.
//!
//! Verified before this was written, on a scratch workspace of the same shape:
//! the guard fired in the test target and did not fire in either the debug or
//! the release binary.

/// Panic if a test is about to resolve the developer's *real* jcode home.
///
/// Called from each resolution point in this crate *after* the `JCODE_HOME`
/// check, so a properly sandboxed test never reaches it. In a non-test build
/// this compiles to nothing.
///
/// # Why this compares paths instead of checking `JCODE_HOME`
///
/// The obvious implementation — "panic whenever `JCODE_HOME` is unset" — is
/// wrong, and `save_github_token_creates_config_dir` is the proof. That test
/// unsets `JCODE_HOME` *deliberately*, because the XDG fallback it exercises
/// only applies when it is unset, and then redirects `HOME` to a temp dir so
/// the fallback still lands in the sandbox. That is correct, and a guard
/// keyed on `JCODE_HOME` would fail it.
///
/// So the question is not "is the sandbox configured" but "is the path we are
/// about to hand back the developer's real one". The real home is read from
/// the passwd database rather than `$HOME`, precisely because a careful test
/// redirects `$HOME`: comparing against `$HOME` would compare the sandbox to
/// itself and never fire.
///
/// # Why it compares exact paths rather than `starts_with`
///
/// "Inside the real home" is too coarse, and this machine proves it: `TMPDIR`
/// is `~/.jcode/scratch`, so `TempDir::new()` hands back a path *underneath*
/// the real home. A `starts_with` test flags every correctly sandboxed test on
/// such a setup. Only the specific protected paths count, so a temp dir that
/// happens to live under home is fine while `~/.jcode` itself is not.
///
/// This panics rather than silently redirecting: a redirect would let a
/// misconfigured test keep passing, which is the failure this exists to end.
#[cfg(feature = "test-guard")]
#[track_caller]
pub(crate) fn check_not_real_home(what: &str, resolved: &std::path::Path) {
    // An explicit opt-out for a test that must reach the real home.
    // Deliberately verbose so it is obvious in a diff.
    if std::env::var_os("JCODE_ALLOW_REAL_HOME_IN_TESTS").is_some() {
        return;
    }
    let Some(real) = real_home_from_passwd() else {
        // No passwd entry to compare against: fail open rather than block a
        // suite on an exotic platform. The C-net redirect still applies.
        return;
    };
    // Exact match against the specific protected paths, not containment: see
    // the note above about TMPDIR living inside the home directory.
    //
    // The config entry is the platform config dir rather than a hardcoded
    // `.config`, since on macOS that is `~/Library/Application Support`. It is
    // recomputed here rather than taken from the caller so the guard states
    // its own idea of what is protected.
    let mut protected = vec![real.join(".jcode")];
    if let Some(config) = dirs::config_dir() {
        protected.push(config.join("jcode"));
    }
    if !protected.iter().any(|p| resolved == p) {
        return;
    }
    panic!(
        "test escaped its sandbox: {what}() resolved {}, which is the \
         developer's real jcode state directory.\n\
         \n\
         This test reads or writes the developer's actual jcode state. On a \
         save() path it silently destroys their settings: three suites have \
         done exactly this, and each took months to find, because an escaping \
         test passes just like a sandboxed one.\n\
         \n\
         Fix: point JCODE_HOME at a temp dir for the duration of the test.\n\
         \n\
             let temp = tempfile::TempDir::new().unwrap();\n\
             let _guard = EnvVarGuard::set(\"JCODE_HOME\", temp.path());\n\
         \n\
         If the test deliberately exercises the no-JCODE_HOME fallback, set \
         HOME to a temp dir too, so the fallback lands inside the sandbox \
         (see save_github_token_creates_config_dir). As a last resort set \
         JCODE_ALLOW_REAL_HOME_IN_TESTS=1, but only for a test that genuinely \
         neither reads nor writes.",
        resolved.display(),
    )
}
/// The real home directory, from the passwd database rather than `$HOME`.
///
/// `$HOME` is what a careful test redirects, so it cannot be the reference
/// point: comparing the resolved path against a redirected `$HOME` would
/// compare the sandbox to itself and the guard would never fire.
#[cfg(all(feature = "test-guard", unix))]
fn real_home_from_passwd() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns a pointer into a static buffer owned by libc.
    // We copy the string out immediately and never retain the pointer.
    unsafe {
        let pw = libc::getpwuid(libc::geteuid());
        if pw.is_null() {
            return None;
        }
        let dir = (*pw).pw_dir;
        if dir.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(dir).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(std::path::PathBuf::from(
            std::ffi::OsStr::from_bytes(bytes).to_os_string(),
        ))
    }
}
#[cfg(all(feature = "test-guard", not(unix)))]
fn real_home_from_passwd() -> Option<std::path::PathBuf> {
    // No passwd database. USERPROFILE is the closest equivalent and is not
    // routinely redirected by our tests, unlike HOME.
    std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
}
/// No-op in every build that is not a test target.
#[cfg(not(feature = "test-guard"))]
#[inline(always)]
pub(crate) fn check_not_real_home(_what: &str, _resolved: &std::path::Path) {}
