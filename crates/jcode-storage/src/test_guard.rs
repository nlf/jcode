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
/// # Scope: writes, not reads
///
/// This guards the write path, not path resolution. Guarding resolution was
/// tried first and measured: it failed 123 tests in `jcode-base` alone, and
/// the backtraces showed almost all of them were the same thing — a
/// `LazyLock` config cache loading the real config on first touch, from
/// whichever test happened to run first. That is non-hermetic, but it is a
/// *read*, and a read has never destroyed anyone's settings.
///
/// All three historical incidents were saves. Guarding writes catches every
/// one of them while leaving the read noise alone. Making reads hermetic too
/// is worth doing, but it means giving `CONFIG_CACHE` a test seam rather than
/// editing 123 tests, and that is a separate piece of work.
///
/// # Containment, with one exception that matters
///
/// A write targets a file *inside* the protected directory, so this tests
/// containment rather than equality. The exception is that on this machine
/// `TMPDIR` is `~/.jcode/scratch`, so `TempDir::new()` returns a path
/// underneath the very directory being protected. A naive `starts_with`
/// therefore flags every correctly sandboxed test. Anything under a temp root
/// is excluded first, which is what makes the check usable here at all.
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
    // A temp dir can legitimately live *inside* the protected directory: on
    // this machine TMPDIR is ~/.jcode/scratch. Excluding temp roots first is
    // what makes containment usable; without it every sandboxed test trips.
    if under_temp_root(resolved) {
        return;
    }
    // The config entry is the platform config dir rather than a hardcoded
    // `.config`, since on macOS that is `~/Library/Application Support`.
    let mut protected = vec![real.join(".jcode")];
    if let Some(config) = dirs::config_dir() {
        protected.push(config.join("jcode"));
    }
    if !protected.iter().any(|p| resolved.starts_with(p)) {
        return;
    }
    panic!(
        "test wrote into the developer's real jcode state: {what} -> {}\n\
         \n\
         This write lands in the developer's actual ~/.jcode. On a config \
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
         writes nothing that matters.",
        resolved.display(),
    )
}
/// Whether a path lives under a temporary-directory root.
///
/// Load-bearing on this machine, where `TMPDIR` is `~/.jcode/scratch`: without
/// it, every correctly sandboxed test writing into its own `TempDir` would be
/// flagged as writing into the protected directory.
#[cfg(feature = "test-guard")]
fn under_temp_root(path: &std::path::Path) -> bool {
    let mut roots = vec![std::env::temp_dir()];
    for key in ["TMPDIR", "TMP", "TEMP", "JCODE_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            roots.push(std::path::PathBuf::from(value));
        }
    }
    roots.iter().any(|root| {
        // Compare canonicalized where possible: on macOS TMPDIR is often a
        // /var symlink to /private/var, and an uncanonicalized compare misses.
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        path.starts_with(root) || canonical_path.starts_with(&canonical_root)
    })
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
