//! Serializing tests that mutate the process-wide `HOME`.
//!
//! Several tests point `HOME` at a temp directory to exercise home-directory
//! protection, then restore it. `HOME` is process-wide but the test harness is
//! multi-threaded, so two such tests overlapping means one restores `HOME`
//! while the other still depends on it. The victim then reads the real `HOME`,
//! its `rm -rf $TMPDIR` no longer looks like deleting a home directory, the
//! gate correctly lets it through, and the test fails claiming the gate is
//! broken.
//!
//! That produced a flake in `bash_refuses_to_delete_the_home_directory` on
//! roughly one run in three, on unmodified `master`. The tests carried a
//! comment reading "SAFETY: single-threaded test setup", which was not true.
//!
//! Holding this lock for the whole set-use-restore window makes it true.

use std::sync::{Mutex, MutexGuard, OnceLock};

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Point `HOME` at `home` until the returned guard drops.
///
/// Restoration happens in `Drop` rather than at the end of the test body, so a
/// failing assertion cannot leave every later test looking at a temp directory
/// that no longer exists.
pub struct HomeOverride {
    previous: Option<String>,
    // Held for the guard's life. Poisoning is ignored: a panicking test still
    // needs the next one to be able to take the lock.
    _guard: MutexGuard<'static, ()>,
}

impl HomeOverride {
    pub fn set(home: impl AsRef<std::path::Path>) -> Self {
        let guard = home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("HOME").ok();
        // SAFETY: the lock above makes this the only thread touching HOME.
        unsafe { std::env::set_var("HOME", home.as_ref()) };
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for HomeOverride {
    fn drop(&mut self) {
        // SAFETY: the guard is still held, so no other test is reading HOME.
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
