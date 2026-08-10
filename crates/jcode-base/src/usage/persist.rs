//! Cross-process persistence for the Anthropic usage cache.
//!
//! The in-memory cache is per-process (`OnceLock` statics), but the rate limit
//! it protects is per-account. A machine running a shared server, a menubar
//! process, and several self-dev instances therefore multiplied the intended
//! ~12 requests/hour by the number of live processes, and each one backed off
//! independently after a 429 while the others kept knocking.
//!
//! This module gives every process a single shared view on disk, so one
//! process's successful fetch serves the others and one process's 429 backoff
//! is respected by all of them.
//!
//! `Instant` is a process-local monotonic clock and cannot be serialized, so
//! `fetched_at` is stored as wall-clock unix millis and converted back into an
//! `Instant` on load by subtracting the observed age. A snapshot from the
//! future (clock skew, edited file) is treated as "just fetched" rather than
//! producing a nonsensical negative age.

use super::{ModelScopedUsageWindow, UsageData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Filename under `~/.jcode/`. Not a secret: utilization percentages and reset
/// timestamps only, never tokens or account identifiers beyond the cache key.
const USAGE_CACHE_FILE: &str = "usage-cache.json";

/// Discard anything older than this on load. A day-old snapshot is worse than
/// no snapshot: the windows it describes have almost certainly rolled over.
const MAX_PERSISTED_AGE: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct PersistedUsageFile {
    #[serde(default)]
    entries: HashMap<String, PersistedUsageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PersistedUsageEntry {
    #[serde(default)]
    five_hour: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    five_hour_resets_at: Option<String>,
    #[serde(default)]
    seven_day: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seven_day_resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seven_day_opus: Option<f32>,
    #[serde(default)]
    model_scoped: Vec<PersistedModelWindow>,
    #[serde(default)]
    extra_usage_enabled: bool,
    /// Wall-clock fetch time, unix millis. The bridge across processes.
    fetched_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    /// Server-directed retry delay in seconds, when a 429 supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PersistedModelWindow {
    model_name: String,
    utilization: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<String>,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Map an in-memory cache key to the on-disk key.
///
/// In-memory keys are either `label:<account>` or, for the genuinely
/// label-less case, `token:<first 20 chars of an OAuth access token>`. That
/// token prefix must never be written out verbatim: this file is ordinary
/// JSON under `~/.jcode` that exists to share advisory usage numbers between
/// processes, not to hold credential material. Hashing preserves the "same
/// account maps to the same entry" property without persisting any part of
/// the secret. Label keys are already non-sensitive and stay readable so the
/// file can be understood at a glance.
fn disk_key(cache_key: &str) -> String {
    match cache_key.strip_prefix("token:") {
        Some(token_prefix) => {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            token_prefix.hash(&mut hasher);
            format!("token-hash:{:016x}", hasher.finish())
        }
        None => cache_key.to_string(),
    }
}

pub(super) fn usage_cache_path() -> Option<PathBuf> {
    crate::storage::jcode_dir()
        .ok()
        .map(|d| d.join(USAGE_CACHE_FILE))
}

impl PersistedUsageEntry {
    fn from_usage(data: &UsageData) -> Option<Self> {
        // Only snapshots that actually reached the API are worth sharing. An
        // entry with no `fetched_at` carries no timing information, which is
        // the whole point of the shared file.
        data.fetched_at?;

        Some(Self {
            five_hour: data.five_hour,
            five_hour_resets_at: data.five_hour_resets_at.clone(),
            seven_day: data.seven_day,
            seven_day_resets_at: data.seven_day_resets_at.clone(),
            seven_day_opus: data.seven_day_opus,
            model_scoped: data
                .model_scoped
                .iter()
                .map(|w| PersistedModelWindow {
                    model_name: w.model_name.clone(),
                    utilization: w.utilization,
                    resets_at: w.resets_at.clone(),
                })
                .collect(),
            extra_usage_enabled: data.extra_usage_enabled,
            fetched_at_unix_ms: now_unix_ms(),
            last_error: data.last_error.clone(),
            retry_after_secs: data.retry_after.map(|d| d.as_secs()),
        })
    }

    /// Rebuild a `UsageData`, translating the stored wall-clock time back into
    /// this process's monotonic clock. Returns `None` when the entry is older
    /// than [`MAX_PERSISTED_AGE`].
    fn into_usage(self) -> Option<UsageData> {
        let age = age_of(self.fetched_at_unix_ms)?;

        Some(UsageData {
            five_hour: self.five_hour,
            five_hour_resets_at: self.five_hour_resets_at,
            seven_day: self.seven_day,
            seven_day_resets_at: self.seven_day_resets_at,
            seven_day_opus: self.seven_day_opus,
            model_scoped: self
                .model_scoped
                .into_iter()
                .map(|w| ModelScopedUsageWindow {
                    model_name: w.model_name,
                    utilization: w.utilization,
                    resets_at: w.resets_at,
                })
                .collect(),
            extra_usage_enabled: self.extra_usage_enabled,
            // Backdate so `is_stale()` measures the true age of the data
            // rather than the moment this process happened to read the file.
            fetched_at: Instant::now().checked_sub(age),
            last_error: self.last_error,
            retry_after: self.retry_after_secs.map(Duration::from_secs),
        })
    }
}

/// Age of a stored timestamp, or `None` if too old to be useful. A future
/// timestamp (clock skew) is clamped to zero rather than rejected.
fn age_of(fetched_at_unix_ms: u64) -> Option<Duration> {
    let now = now_unix_ms();
    let age = Duration::from_millis(now.saturating_sub(fetched_at_unix_ms));
    (age <= MAX_PERSISTED_AGE).then_some(age)
}

/// Read the shared snapshot for `cache_key`, if one is present and fresh
/// enough to be meaningful. Failures are silent by design: the persistent
/// cache is an optimization, and a missing or corrupt file must never block a
/// live fetch.
pub(super) fn load_entry(cache_key: &str) -> Option<UsageData> {
    let path = usage_cache_path()?;
    if !path.exists() {
        return None;
    }
    let file: PersistedUsageFile = crate::storage::read_json(&path).ok()?;
    file.entries.get(&disk_key(cache_key))?.clone().into_usage()
}

/// Merge one entry into the shared file, preserving other accounts' entries.
///
/// Read-modify-write races between processes are possible but benign: the
/// values are advisory, every writer is writing a snapshot of the same
/// upstream state, and the underlying write is atomic (temp + rename), so a
/// reader never observes a torn file. The worst case is one process's snapshot
/// briefly losing to another's, which self-corrects on the next refresh.
pub(super) fn store_entry(cache_key: &str, data: &UsageData) {
    let Some(path) = usage_cache_path() else {
        return;
    };
    let Some(entry) = PersistedUsageEntry::from_usage(data) else {
        return;
    };

    let mut file: PersistedUsageFile = if path.exists() {
        crate::storage::read_json(&path).unwrap_or_default()
    } else {
        PersistedUsageFile::default()
    };

    // Drop entries that have aged out so the file cannot grow without bound
    // as tokens rotate and cache keys change.
    file.entries
        .retain(|_, entry| age_of(entry.fetched_at_unix_ms).is_some());
    file.entries.insert(disk_key(cache_key), entry);

    if let Err(e) = crate::storage::write_json_secret(&path, &file) {
        crate::logging::warn(&format!("Failed to persist usage cache: {}", e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_usage() -> UsageData {
        // Reset timestamps must be relative to now: `is_stale()` treats a
        // passed reset as stale, so hardcoded dates turn these into tests that
        // silently start failing once the wall clock rolls past them.
        let in_an_hour = chrono::Utc::now() + chrono::Duration::hours(1);
        let next_week = chrono::Utc::now() + chrono::Duration::days(7);

        UsageData {
            five_hour: 0.42,
            five_hour_resets_at: Some(in_an_hour.to_rfc3339()),
            seven_day: 0.13,
            seven_day_resets_at: Some(next_week.to_rfc3339()),
            seven_day_opus: Some(0.07),
            model_scoped: vec![ModelScopedUsageWindow {
                model_name: "Opus".to_string(),
                utilization: 0.5,
                resets_at: None,
            }],
            extra_usage_enabled: true,
            fetched_at: Some(Instant::now()),
            last_error: None,
            retry_after: None,
        }
    }

    #[test]
    fn round_trip_preserves_values() {
        let original = sample_usage();
        let entry = PersistedUsageEntry::from_usage(&original).expect("has fetched_at");
        let restored = entry.into_usage().expect("fresh entry");

        assert_eq!(restored.five_hour, original.five_hour);
        assert_eq!(restored.seven_day, original.seven_day);
        assert_eq!(restored.seven_day_opus, original.seven_day_opus);
        assert_eq!(restored.five_hour_resets_at, original.five_hour_resets_at);
        assert_eq!(restored.extra_usage_enabled, original.extra_usage_enabled);
        assert_eq!(restored.model_scoped.len(), 1);
        assert_eq!(restored.model_scoped[0].model_name, "Opus");
        assert!(restored.fetched_at.is_some());
    }

    #[test]
    fn retry_after_survives_the_round_trip() {
        // The whole point of sharing state: a 429 backoff observed by one
        // process must be visible to the next one that starts.
        let mut data = sample_usage();
        data.last_error = Some("Usage API error (429 Too Many Requests)".to_string());
        data.retry_after = Some(Duration::from_secs(45));

        let restored = PersistedUsageEntry::from_usage(&data)
            .expect("has fetched_at")
            .into_usage()
            .expect("fresh entry");

        assert_eq!(restored.retry_after, Some(Duration::from_secs(45)));
        assert!(restored.last_error.is_some());
    }

    #[test]
    fn entries_older_than_the_cap_are_rejected() {
        let mut entry = PersistedUsageEntry::from_usage(&sample_usage()).expect("has fetched_at");
        // Backdate beyond MAX_PERSISTED_AGE.
        entry.fetched_at_unix_ms = now_unix_ms() - (MAX_PERSISTED_AGE.as_millis() as u64 + 60_000);

        assert!(entry.into_usage().is_none(), "stale entry must be dropped");
    }

    #[test]
    fn age_is_preserved_so_staleness_is_measured_from_the_original_fetch() {
        // A snapshot fetched 200s ago must still read as 200s old in the
        // process that loads it, otherwise every new process would treat a
        // shared entry as brand new and defeat the shared backoff.
        let mut entry = PersistedUsageEntry::from_usage(&sample_usage()).expect("has fetched_at");
        entry.fetched_at_unix_ms = now_unix_ms() - 200_000;

        let restored = entry.into_usage().expect("within cap");
        let age = restored.fetched_at.expect("has fetched_at").elapsed();

        assert!(
            age >= Duration::from_secs(199) && age <= Duration::from_secs(205),
            "expected ~200s age, got {:?}",
            age
        );
    }

    #[test]
    fn future_timestamps_clamp_to_zero_age() {
        let mut entry = PersistedUsageEntry::from_usage(&sample_usage()).expect("has fetched_at");
        entry.fetched_at_unix_ms = now_unix_ms() + 60_000;

        let restored = entry.into_usage().expect("clock skew is tolerated");
        assert!(restored.fetched_at.expect("has fetched_at").elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn usage_without_fetched_at_is_not_persisted() {
        let data = UsageData::default();
        assert!(PersistedUsageEntry::from_usage(&data).is_none());
    }

    // ─── Real on-disk round trips ────────────────────────────────────────
    //
    // The tests above only exercise struct conversion. These drive the actual
    // file through `store_entry`/`load_entry` with `JCODE_HOME` redirected to
    // a temp dir, which is what the cross-process fix actually depends on.

    /// Point `jcode_dir()` at a temp directory for the duration of the test.
    /// `lock_test_env` serializes this, since `JCODE_HOME` is process-global.
    fn with_temp_home<T>(body: impl FnOnce() -> T) -> T {
        let _guard = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let previous = std::env::var("JCODE_HOME").ok();
        unsafe { std::env::set_var("JCODE_HOME", dir.path()) };

        let result = body();

        match previous {
            Some(value) => unsafe { std::env::set_var("JCODE_HOME", value) },
            None => unsafe { std::env::remove_var("JCODE_HOME") },
        }
        result
    }

    #[test]
    fn store_then_load_returns_the_same_numbers_through_a_real_file() {
        with_temp_home(|| {
            let data = sample_usage();
            store_entry("label:claude-1", &data);

            let path = usage_cache_path().expect("path");
            assert!(path.exists(), "store_entry must create the file");

            let loaded = load_entry("label:claude-1").expect("entry present on disk");
            assert_eq!(loaded.five_hour, data.five_hour);
            assert_eq!(loaded.seven_day, data.seven_day);
            assert_eq!(loaded.five_hour_resets_at, data.five_hour_resets_at);
        });
    }

    #[test]
    fn a_second_process_inherits_the_429_backoff() {
        // The core cross-process guarantee: process A gets rate limited, and
        // process B (a cold start, empty in-memory cache) must see the
        // backoff rather than immediately re-fetching and compounding it.
        with_temp_home(|| {
            let mut rate_limited = sample_usage();
            rate_limited.last_error =
                Some("Usage API error (429 Too Many Requests): rate_limit_error".to_string());
            rate_limited.retry_after = Some(Duration::from_secs(300));
            store_entry("label:claude-1", &rate_limited);

            let seen_by_other_process = load_entry("label:claude-1").expect("shared entry");
            assert_eq!(
                seen_by_other_process.retry_after,
                Some(Duration::from_secs(300))
            );
            assert!(
                !seen_by_other_process.is_stale(),
                "within the server-directed retry window, the entry must read \
                 as fresh so the second process does not re-fetch"
            );
        });
    }

    #[test]
    fn other_accounts_survive_a_write() {
        with_temp_home(|| {
            store_entry("label:claude-1", &sample_usage());

            let mut second = sample_usage();
            second.five_hour = 0.9;
            store_entry("label:claude-2", &second);

            assert!(
                load_entry("label:claude-1").is_some(),
                "writing one account must not evict another"
            );
            let reloaded = load_entry("label:claude-2").expect("second account");
            assert!((reloaded.five_hour - 0.9).abs() < f32::EPSILON);
        });
    }

    #[test]
    fn missing_and_corrupt_files_are_not_fatal() {
        with_temp_home(|| {
            assert!(
                load_entry("label:absent").is_none(),
                "a missing file is a cache miss, not an error"
            );

            let path = usage_cache_path().expect("path");
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, b"{ this is not json").expect("write garbage");

            assert!(
                load_entry("label:claude-1").is_none(),
                "corrupt cache must degrade to a miss rather than panicking"
            );

            // And a subsequent write must recover the file rather than wedge.
            store_entry("label:claude-1", &sample_usage());
            assert!(load_entry("label:claude-1").is_some());
        });
    }

    #[test]
    fn expired_entries_are_pruned_on_write() {
        with_temp_home(|| {
            store_entry("label:old", &sample_usage());

            // Age the stored entry past the cap by rewriting the file.
            let path = usage_cache_path().expect("path");
            let mut file: PersistedUsageFile = crate::storage::read_json(&path).expect("read back");
            let entry = file.entries.get_mut("label:old").expect("entry");
            entry.fetched_at_unix_ms =
                now_unix_ms() - (MAX_PERSISTED_AGE.as_millis() as u64 + 60_000);
            crate::storage::write_json_secret(&path, &file).expect("rewrite");

            // Writing a different account should evict the aged-out one.
            store_entry("label:new", &sample_usage());

            let after: PersistedUsageFile = crate::storage::read_json(&path).expect("read");
            assert!(
                !after.entries.contains_key("label:old"),
                "aged-out entries must be pruned so the file cannot grow forever"
            );
            assert!(after.entries.contains_key("label:new"));
        });
    }

    #[test]
    fn token_derived_keys_never_reach_disk_verbatim() {
        // In-memory keys can embed the first 20 characters of a live OAuth
        // access token. The shared file is ordinary JSON under ~/.jcode, so
        // that prefix must be hashed rather than written out.
        with_temp_home(|| {
            let token_key = "token:sk-ant-oat01-r3YAwo8";
            store_entry(token_key, &sample_usage());

            let path = usage_cache_path().expect("path");
            let raw = std::fs::read_to_string(&path).expect("read file");

            assert!(
                !raw.contains("sk-ant-oat01"),
                "token material must never be persisted: {raw}"
            );
            assert!(
                raw.contains("token-hash:"),
                "token keys should persist as a hash: {raw}"
            );
            // The hash must still round-trip, or the shared cache silently
            // stops working for token-keyed lookups.
            assert!(
                load_entry(token_key).is_some(),
                "hashed keys must still resolve on read"
            );
        });
    }

    #[test]
    fn label_keys_stay_readable() {
        with_temp_home(|| {
            store_entry("label:claude-1", &sample_usage());
            let raw = std::fs::read_to_string(usage_cache_path().expect("path")).expect("read");
            assert!(
                raw.contains("label:claude-1"),
                "non-sensitive label keys stay readable for debugging: {raw}"
            );
        });
    }

    /// The cache is written with `write_json_secret`, which is what keeps it
    /// owner-only. Nothing asserted that, so a future switch back to
    /// `write_json_fast` (which is otherwise a reasonable-looking change for a
    /// non-durable cache) would silently widen the permissions. Pin it, and
    /// pin the `.bak` sibling that the atomic write leaves behind, which is
    /// easy to forget precisely because no code names it.
    #[cfg(unix)]
    #[test]
    fn cache_file_and_its_backup_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_home(|| {
            let path = usage_cache_path().expect("path");

            // Write twice: the second write is what creates the .bak sibling.
            store_entry("label:claude-1", &sample_usage());
            store_entry("label:claude-1", &sample_usage());

            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "usage cache must be owner-only, got {mode:o}");

            let bak = path.with_extension("bak");
            if bak.exists() {
                let bak_mode = std::fs::metadata(&bak)
                    .expect("bak metadata")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    bak_mode, 0o600,
                    "the backup sibling must be owner-only too, got {bak_mode:o}"
                );
            }
        });
    }
}
