use super::*;
use crate::storage::jcode_dir;
use std::path::PathBuf;

/// The config as this process last read it from disk.
///
/// [`Config::save`] needs to tell "the caller set this" apart from "the caller
/// never touched this and is holding a stale copy". Comparing against the
/// current file cannot distinguish them; comparing against what we loaded can.
static LOADED_SNAPSHOT: std::sync::RwLock<Option<toml::Value>> = std::sync::RwLock::new(None);

fn record_loaded_snapshot(config: &Config) {
    if let Ok(value) = toml::Value::try_from(config)
        && let Ok(mut slot) = LOADED_SNAPSHOT.write()
    {
        *slot = Some(value);
    }
}

/// Forget the recorded state, so the next save has no baseline to diff against.
///
/// Used when the file is absent or unparseable. `save_baseline` then falls back
/// to an empty table, in which every field the caller holds reads as a
/// deliberate change, which is the correct reading: there is nothing on disk
/// that those values could have come from.
fn clear_loaded_snapshot() {
    if let Ok(mut slot) = LOADED_SNAPSHOT.write() {
        *slot = None;
    }
}

/// The baseline to diff a save against: what we loaded, or failing that, the
/// current file. Falling back to the file keeps a never-loaded config from
/// treating every default as an intentional change.
fn save_baseline() -> toml::Value {
    if let Ok(slot) = LOADED_SNAPSHOT.read()
        && let Some(value) = slot.as_ref()
    {
        return value.clone();
    }
    Config::load_from_file()
        .and_then(|cfg| toml::Value::try_from(&cfg).ok())
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()))
}

/// One key's fate in a save: the caller set it, or the caller removed it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConfigChange {
    Set(toml::Value),
    Remove,
}

/// A single change, addressed by its path from the document root.
pub(crate) type ChangeEntry = (Vec<String>, ConfigChange);

/// The keys where `desired` differs from `baseline`, as a flat change set.
///
/// Three-way merge, decision half: `baseline` is what this process loaded and
/// `desired` is what it wants. A key matching the baseline was never touched by
/// this caller and so produces no change, which is what lets a concurrent edit
/// to that key survive. Tables recurse; anything else is replaced wholesale,
/// since a partial merge of an array has no meaning here.
///
/// Returning a change set rather than a merged document is what allows the
/// application half to patch the user's file in place, preserving comments and
/// key order for everything untouched.
pub(crate) fn changed_keys(baseline: &toml::Value, desired: &toml::Value) -> Vec<ChangeEntry> {
    let mut changes = Vec::new();
    collect_changed_keys(&mut Vec::new(), baseline, desired, &mut changes);
    changes
}

fn collect_changed_keys(
    prefix: &mut Vec<String>,
    baseline: &toml::Value,
    desired: &toml::Value,
    out: &mut Vec<ChangeEntry>,
) {
    let (Some(desired_table), Some(baseline_table)) = (desired.as_table(), baseline.as_table())
    else {
        if desired != baseline {
            out.push((prefix.clone(), ConfigChange::Set(desired.clone())));
        }
        return;
    };

    for (key, desired_value) in desired_table {
        let baseline_value = baseline_table.get(key);
        if baseline_value == Some(desired_value) {
            // Untouched by this caller: leave the file's value alone.
            continue;
        }

        prefix.push(key.clone());
        match (
            desired_value.as_table(),
            baseline_value.and_then(|value| value.as_table()),
        ) {
            // Both sides are tables, so recurse and change only the leaves that
            // actually differ. Replacing the whole table here would clobber
            // sibling keys a concurrent edit had added.
            (Some(_), Some(_)) => {
                let baseline_child = baseline_value.expect("matched as a table above");
                collect_changed_keys(prefix, baseline_child, desired_value, out);
            }
            // A table with no table baseline is wholly new to this caller.
            // Recursing against an empty baseline emits its leaves one by one,
            // which lets the writer build the section without flattening it.
            (Some(_), None) => {
                let empty = toml::Value::Table(toml::map::Map::new());
                collect_changed_keys(prefix, &empty, desired_value, out);
            }
            _ => out.push((prefix.clone(), ConfigChange::Set(desired_value.clone()))),
        }
        prefix.pop();
    }

    // A key the caller removed relative to its baseline is a deletion.
    for key in baseline_table.keys() {
        if !desired_table.contains_key(key) {
            prefix.push(key.clone());
            out.push((prefix.clone(), ConfigChange::Remove));
            prefix.pop();
        }
    }
}

/// Apply a change set to a parsed document, in place.
///
/// Only the addressed keys are touched. Everything else in the document keeps
/// the bytes it was parsed from, which is the whole point: comments, blank
/// lines, key order, and keys `Config` does not model all survive because they
/// are never rewritten.
pub(crate) fn apply_changes(doc: &mut toml_edit::Document, changes: &[ChangeEntry]) {
    for (path, change) in changes {
        let Some((leaf, parents)) = path.split_last() else {
            continue;
        };

        match change {
            ConfigChange::Set(value) => {
                let Some(table) = descend_or_create(doc, parents) else {
                    continue;
                };
                set_preserving_decor(table, leaf, value);
            }
            ConfigChange::Remove => {
                // Only descend; a removal has no reason to create the tables it
                // would then remove from.
                if let Some(table) = descend(doc, parents) {
                    table.remove(leaf);
                }
            }
        }
    }
}

/// Walk to the table at `path`, creating any missing tables on the way.
///
/// Missing tables are created as explicit `[section]` headers, matching the
/// shape of the shipped config template rather than dotted keys.
fn descend_or_create<'a>(
    doc: &'a mut toml_edit::Document,
    path: &[String],
) -> Option<&'a mut toml_edit::Table> {
    let mut table = doc.as_table_mut();
    for key in path {
        let entry = table
            .entry(key)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry.as_table_mut()?;
    }
    Some(table)
}

/// Walk to the table at `path` if it already exists.
fn descend<'a>(
    doc: &'a mut toml_edit::Document,
    path: &[String],
) -> Option<&'a mut toml_edit::Table> {
    let mut table = doc.as_table_mut();
    for key in path {
        table = table.get_mut(key)?.as_table_mut()?;
    }
    Some(table)
}

/// Write one key, keeping the formatting that surrounds it.
///
/// Replacing an existing key with a fresh item would discard its decor, which
/// is where `toml_edit` keeps the comment above it and the spacing around the
/// `=`. So assign into the existing value when there is one, and only insert a
/// whole new item when the key is genuinely new.
fn set_preserving_decor(table: &mut toml_edit::Table, key: &str, value: &toml::Value) {
    let new_value = to_edit_value(value);

    if let Some(existing) = table.get_mut(key) {
        // A table replaced by a non-table (or vice versa) cannot keep its
        // decor, so fall through to a plain assignment in that case.
        if let Some(slot) = existing.as_value_mut() {
            let decor = slot.decor().clone();
            *slot = new_value;
            *slot.decor_mut() = decor;
            return;
        }
        *existing = toml_edit::Item::Value(new_value);
        return;
    }

    table.insert(key, toml_edit::Item::Value(new_value));
}

/// Convert a `toml::Value` into the `toml_edit` representation.
///
/// The two crates model the same data with different types; `toml_edit` adds
/// formatting. Nested tables become inline tables here, which only happens for
/// a value written as a leaf, since [`changed_keys`] emits table *leaves*
/// individually rather than whole tables.
fn to_edit_value(value: &toml::Value) -> toml_edit::Value {
    match value {
        toml::Value::String(text) => toml_edit::Value::from(text.as_str()),
        toml::Value::Integer(number) => toml_edit::Value::from(*number),
        toml::Value::Float(number) => toml_edit::Value::from(*number),
        toml::Value::Boolean(flag) => toml_edit::Value::from(*flag),
        toml::Value::Datetime(stamp) => toml_edit::Value::from(stamp.to_string()),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_edit_value(item));
            }
            toml_edit::Value::Array(array)
        }
        toml::Value::Table(map) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, item) in map {
                inline.insert(key, to_edit_value(item));
            }
            toml_edit::Value::InlineTable(inline)
        }
    }
}

impl Config {
    /// Get the config file path
    pub fn path() -> Option<PathBuf> {
        jcode_dir().ok().map(|d| d.join("config.toml"))
    }

    /// Load config from file, with environment variable overrides
    pub fn load() -> Self {
        let mut config = Self::load_from_file().unwrap_or_default();
        config.apply_env_overrides();
        config
    }

    /// Load config from file, with environment variable overrides.
    ///
    /// Unlike [`Self::load`], this returns TOML/read errors to callers that need
    /// to distinguish a malformed config from an absent config.
    pub fn load_strict() -> anyhow::Result<Self> {
        let mut config = Self::load_from_file_strict()?.unwrap_or_default();
        config.apply_env_overrides();
        Ok(config)
    }

    /// Load config from file only (no env overrides)
    fn load_from_file() -> Option<Self> {
        match Self::load_from_file_strict() {
            Ok(config) => config,
            Err(e) => {
                crate::logging::error(&format!("Failed to parse config file: {}", e));
                None
            }
        }
    }

    /// Load config from file only (no env overrides), preserving parse/read errors.
    fn load_from_file_strict() -> anyhow::Result<Option<Self>> {
        let Some(path) = Self::path() else {
            return Ok(None);
        };
        if !path.exists() {
            // No file means no recorded state. Clearing rather than leaving the
            // previous snapshot matters because the baseline decides what
            // counts as a change: a stale one from a different config would
            // make a genuine setting look untouched, and a save would drop it.
            clear_loaded_snapshot();
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {}: {}", path.display(), e))?;
        let mut config = toml::from_str::<Self>(&content).map_err(|e| {
            // An unreadable file records nothing, for the same reason as a
            // missing one: a baseline that describes some other file is worse
            // than no baseline, because it silently suppresses real changes.
            clear_loaded_snapshot();
            anyhow::anyhow!("Failed to parse config file {}: {}", path.display(), e)
        })?;
        config.display.apply_legacy_compat();
        // Snapshot before the repair, not after. The baseline is what the file
        // said; the repair is a change this process is making to it, and a save
        // must therefore write the removal out. Recording after would make the
        // repair invisible to the merge, leaving the frozen section on disk.
        record_loaded_snapshot(&config);
        config.repair_frozen_sponsors_optout(&content);
        Ok(Some(config))
    }

    /// Undo a machine-frozen partner-discovery opt-out.
    ///
    /// Discovery shipped opt-in (`enabled = false`), and because [`Self::save`]
    /// serializes the whole struct, any config write during that window baked
    /// the old default into the user's file. Those users keep discovery
    /// permanently disabled even after the default flipped to opt-out, and
    /// telemetry shows this is the single largest discovery blocker.
    ///
    /// A machine-written section is exactly `enabled` plus `endpoint` with a
    /// known default endpoint. A hand-written opt-out (`enabled = false` alone,
    /// or paired with a custom endpoint) is always respected. Repair happens in
    /// memory only; the section then disappears on the next save because it
    /// serializes back to the default.
    pub(crate) fn repair_frozen_sponsors_optout(&mut self, raw: &str) {
        if self.sponsors.enabled {
            return;
        }
        let Ok(doc) = raw.parse::<toml::Value>() else {
            return;
        };
        let Some(table) = doc.get("sponsors").and_then(toml::Value::as_table) else {
            return;
        };
        let machine_written = table.len() == 2
            && table.get("enabled").and_then(toml::Value::as_bool) == Some(false)
            && table
                .get("endpoint")
                .and_then(toml::Value::as_str)
                .is_some_and(super::is_default_discovery_endpoint);
        if !machine_written {
            return;
        }
        self.sponsors = SponsorsConfig::default();
        crate::logging::info(
            "config: restored integration discovery default (legacy opt-in value was frozen by an \
             earlier config save)",
        );
    }

    /// Save config to file, preserving concurrent edits to untouched settings
    /// and the formatting of the user's file.
    ///
    /// A whole-struct serialize would write back every field this process last
    /// loaded, so a save for one unrelated setting silently reverts anything
    /// another session (or the user's editor) changed in the meantime. That is
    /// not hypothetical: it is what
    /// [`Self::repair_frozen_sponsors_optout`] exists to clean up after, and it
    /// has eaten hand-written `[display.colors]` palettes.
    ///
    /// So diff against the config as this process loaded it and apply only the
    /// keys that actually changed, as edits to the file's own text. Patching
    /// the text rather than re-serializing a merged value is what additionally
    /// keeps comments, key order, and keys this struct does not model: a key
    /// nobody changed is never rewritten at all.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("No config path"))?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let desired = toml::Value::try_from(self)?;

        // The state this process started from. Anything differing from it is an
        // intentional change by this caller; anything matching it is untouched
        // and must not be forced back over a concurrent edit.
        let baseline = save_baseline();

        let changes = changed_keys(&baseline, &desired);

        // Patch the file's own text, so untouched keys keep their comments and
        // position. An unreadable or unparseable file yields an empty document,
        // which the changes then populate: a corrupt config still saves.
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let mut doc = existing
            .parse::<toml_edit::Document>()
            .unwrap_or_else(|_| toml_edit::Document::new());
        apply_changes(&mut doc, &changes);

        std::fs::write(&path, doc.to_string())?;

        // Re-snapshot from `self`, not from the text just written. The baseline
        // is always a serialized `Config`, in which a key absent from the file
        // still appears at its default. Parsing the text back would instead
        // record those keys as missing, so the very next save would see every
        // defaulted key as a change and write the whole struct out, undoing the
        // preservation this function exists for.
        record_loaded_snapshot(self);
        Self::invalidate_cache();
        Ok(())
    }

    /// Mark the process-cached config as stale and notify dependent caches.
    pub fn invalidate_cache() {
        super::invalidate_config_cache();
    }

    /// Update the copilot premium mode in the config file.
    /// Reloads, patches, and saves so it doesn't clobber other fields.
    pub fn set_copilot_premium(mode: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.copilot_premium = mode.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved copilot_premium to config: {}",
            mode.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update just the default model and provider in the config file.
    /// This reloads, patches, and saves so it doesn't clobber other fields.
    pub fn set_default_model(model: Option<&str>, provider: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.default_model = model.map(|s| s.to_string());
        cfg.provider.default_provider = provider.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved default model: {}, provider: {}",
            model.unwrap_or("(none)"),
            provider.unwrap_or("(auto)")
        ));
        Ok(())
    }

    /// Update just the default provider in the config file.
    pub fn set_default_provider(provider: Option<&str>) -> anyhow::Result<()> {
        let cfg = Self::load();
        Self::set_default_model(cfg.provider.default_model.as_deref(), provider)
    }

    /// Update just the default model in the config file.
    pub fn set_default_model_only(model: Option<&str>) -> anyhow::Result<()> {
        let cfg = Self::load();
        Self::set_default_model(model, cfg.provider.default_provider.as_deref())
    }

    /// Update the persisted OpenAI reasoning effort preference.
    pub fn set_openai_reasoning_effort(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.openai_reasoning_effort = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_reasoning_effort to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted Anthropic reasoning effort preference.
    pub fn set_anthropic_reasoning_effort(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.anthropic_reasoning_effort = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved anthropic_reasoning_effort to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted OpenAI transport preference.
    pub fn set_openai_transport(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.openai_transport = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_transport to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted OpenAI service tier preference.
    pub fn set_openai_service_tier(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.provider.openai_service_tier = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_service_tier to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted default alignment preference.
    pub fn set_display_centered(centered: bool) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.centered = centered;
        cfg.save()?;
        crate::logging::info(&format!("Saved display.centered to config: {}", centered));
        Ok(())
    }

    /// Update the persisted reasoning display mode preference.
    pub fn set_reasoning_display(mode: ReasoningDisplayMode) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.set_reasoning_display(mode);
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved display.reasoning_display to config: {}",
            mode.label()
        ));
        Ok(())
    }

    /// Update the persisted compact-notifications preference.
    pub fn set_compact_notifications(compact: bool) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.compact_notifications = compact;
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved display.compact_notifications to config: {}",
            compact
        ));
        Ok(())
    }

    /// Update the persisted pinned-todos preference.
    pub fn set_pin_todos(pin: bool) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.pin_todos = pin;
        cfg.save()?;
        crate::logging::info(&format!("Saved display.pin_todos to config: {}", pin));
        Ok(())
    }

    /// Update the persisted info-widget set and order (`display.widgets`).
    /// An empty list restores the built-in priority-ordered default.
    pub fn set_widgets(names: &[String]) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.widgets = names.to_vec();
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved display.widgets to config: {}",
            if names.is_empty() {
                "(default)".to_string()
            } else {
                names.join(", ")
            }
        ));
        Ok(())
    }

    /// Update the persisted show-agentgrep-output preference.

    /// Update the persisted tool-call-details preference.
    pub fn set_tool_call_details(show: bool) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.display.tool_call_details = show;
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved display.tool_call_details to config: {}",
            show
        ));
        Ok(())
    }

    /// Persist the baked global launch-hotkey mapping.
    ///
    /// Auto-import calls this once with the per-repo chord -> directory layout it
    /// inferred. `imported` is set so the bake never runs twice and later manual
    /// edits are not clobbered.
    pub fn set_launch_hotkeys(
        entries: Vec<jcode_config_types::LaunchHotkeyEntry>,
        enabled: bool,
    ) -> anyhow::Result<()> {
        let mut cfg = Self::load();
        cfg.launch_hotkeys.entries = entries;
        cfg.launch_hotkeys.enabled = Some(enabled);
        cfg.launch_hotkeys.imported = true;
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved {} launch hotkey(s) to config (enabled={enabled})",
            cfg.launch_hotkeys.entries.len()
        ));
        Ok(())
    }

    /// One-time bake of per-repo launch hotkeys from session history.
    ///
    /// Scans `~/.jcode/sessions` for the directories the user works in most,
    /// ranks them (recency-weighted, git-root folded, home excluded), and writes
    /// a static chord -> directory mapping into config: top repo on `Cmd+;`, home
    /// on `Cmd+'`, and the next repos on `Cmd+[` / `Cmd+]` / `Cmd+\`.
    ///
    /// Idempotent and side-effect-light:
    /// - Runs only on platforms with global launch hotkeys (macOS, Linux,
    ///   Windows).
    /// - No-ops once `launch_hotkeys.imported` is set, so it bakes exactly once
    ///   and never overwrites later manual edits.
    /// - No-ops when there are not at least two rankable repos, so we do not
    ///   commit a degenerate "everything is home" layout on a fresh machine; the
    ///   built-in 3 hotkeys keep working until there is real history.
    ///
    /// Returns `true` when it wrote a baked mapping (so the caller can trigger a
    /// hotkey reinstall), `false` otherwise. Best-effort: errors are logged and
    /// swallowed.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    pub fn bake_launch_hotkeys_once() -> bool {
        use jcode_import_core::repo_ranking;

        let cfg = Self::load();
        if cfg.launch_hotkeys.imported {
            return false;
        }
        let Ok(jcode_dir) = jcode_dir() else {
            return false;
        };
        let sessions_dir = jcode_dir.join("sessions");
        let Some(home) = dirs::home_dir() else {
            return false;
        };

        // Cheap gate: count session files without reading them. Skip the full
        // scan until there is at least a little history, so brand-new installs do
        // not pay the read cost (and we do not bake a degenerate layout).
        let session_count = std::fs::read_dir(&sessions_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".json")))
                    .count()
            })
            .unwrap_or(0);
        const MIN_SESSIONS_TO_BAKE: usize = 3;
        const GIVE_UP_SESSION_COUNT: usize = 50;
        if session_count < MIN_SESSIONS_TO_BAKE {
            return false;
        }

        let plan = repo_ranking::plan_launch_hotkeys_from_sessions(
            &sessions_dir,
            &home,
            chrono::Utc::now(),
        );

        // `plan` always contains the home slot; a length of 1 means no rankable
        // repos were found.
        if plan.len() < 2 {
            // If the user has lots of history but still no rankable repos, stop
            // re-scanning on every launch: mark imported with no custom entries
            // (the built-in 3 hotkeys keep working).
            if session_count >= GIVE_UP_SESSION_COUNT
                && let Err(err) = Self::set_launch_hotkeys(Vec::new(), true)
            {
                crate::logging::warn(&format!("launch hotkey bake give-up persist failed: {err}"));
            }
            crate::logging::info(
                "launch hotkey bake: not enough repo history yet; keeping defaults",
            );
            return false;
        }

        let entries: Vec<jcode_config_types::LaunchHotkeyEntry> = plan
            .into_iter()
            .map(|p| jcode_config_types::LaunchHotkeyEntry {
                chord: p.chord,
                // Home keeps the dynamic sentinel so it tracks `$HOME`; repos are
                // baked to absolute paths.
                dir: if p.label == "home" {
                    "$HOME".to_string()
                } else {
                    p.dir
                },
                label: p.label,
                self_dev: false,
            })
            .collect();

        match Self::set_launch_hotkeys(entries, true) {
            Ok(()) => {
                crate::logging::info("launch hotkey bake: wrote per-repo mapping to config");
                true
            }
            Err(err) => {
                crate::logging::warn(&format!("launch hotkey bake failed to persist: {err}"));
                false
            }
        }
    }

    /// No-op bake on platforms without global launch hotkeys.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    pub fn bake_launch_hotkeys_once() -> bool {
        false
    }

    /// One-time migration: flip a persisted legacy `swarm_spawn_mode =
    /// "visible"` to the current `"inline"` default.
    ///
    /// Historically `visible` was the default, and any full-config
    /// `Config::save()` (model switches, display toggles, ...) baked that
    /// then-default into the user's config.toml. When the default changed to
    /// `inline`, those users stayed pinned to `visible` forever. This rewrites
    /// exactly that one line (preserving the rest of the file byte-for-byte)
    /// and drops a marker so it runs at most once. A user who explicitly sets
    /// `visible` after the migration is never flipped again.
    ///
    /// Returns `true` when it rewrote the config. Best-effort: errors are
    /// logged and swallowed.
    pub fn migrate_legacy_swarm_spawn_mode_once() -> bool {
        let Ok(dir) = jcode_dir() else {
            return false;
        };
        let marker = dir.join("migrations").join("swarm-spawn-mode-inline");
        if marker.exists() {
            return false;
        }
        let write_marker = || {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &marker,
                "swarm_spawn_mode default migration: visible -> inline\n",
            );
        };

        let path = dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            // No config file (fresh install): nothing to migrate.
            write_marker();
            return false;
        };

        let mut changed = false;
        let migrated: Vec<String> = content
            .lines()
            .map(|line| {
                if changed {
                    return line.to_string();
                }
                let trimmed = line.trim_start();
                let Some(rest) = trimmed.strip_prefix("swarm_spawn_mode") else {
                    return line.to_string();
                };
                let Some(value) = rest.trim_start().strip_prefix('=') else {
                    return line.to_string();
                };
                let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                if matches!(value, "visible" | "headed") {
                    changed = true;
                    let indent = &line[..line.len() - trimmed.len()];
                    format!("{indent}swarm_spawn_mode = \"inline\"")
                } else {
                    line.to_string()
                }
            })
            .collect();

        if !changed {
            write_marker();
            return false;
        }

        let mut new_content = migrated.join("\n");
        if content.ends_with('\n') {
            new_content.push('\n');
        }
        match std::fs::write(&path, new_content) {
            Ok(()) => {
                Self::invalidate_cache();
                write_marker();
                crate::logging::info(
                    "Migrated legacy swarm_spawn_mode \"visible\" to \"inline\" in config.toml",
                );
                true
            }
            Err(err) => {
                crate::logging::warn(&format!(
                    "swarm_spawn_mode migration failed to write config: {err}"
                ));
                false
            }
        }
    }

    /// One-time migration: flip a persisted `idle_animation = true` to `false`.
    ///
    /// The idle animation is being turned off for everyone. Users who toggled
    /// it on earlier (or had the old `true` default baked in by a full
    /// `Config::save()`) get flipped off once. This rewrites exactly that one
    /// line (preserving the rest of the file byte-for-byte) and drops a marker
    /// so it runs at most once. A user who explicitly re-enables it after the
    /// migration is never flipped again.
    ///
    /// Returns `true` when it rewrote the config. Best-effort: errors are
    /// logged and swallowed.
    pub fn migrate_idle_animation_off_once() -> bool {
        let Ok(dir) = jcode_dir() else {
            return false;
        };
        let marker = dir.join("migrations").join("idle-animation-off");
        if marker.exists() {
            return false;
        }
        let write_marker = || {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, "idle_animation forced migration: true -> false\n");
        };

        let path = dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            // No config file (fresh install): nothing to migrate.
            write_marker();
            return false;
        };

        let mut changed = false;
        let migrated: Vec<String> = content
            .lines()
            .map(|line| {
                if changed {
                    return line.to_string();
                }
                let trimmed = line.trim_start();
                let Some(rest) = trimmed.strip_prefix("idle_animation") else {
                    return line.to_string();
                };
                let Some(value) = rest.trim_start().strip_prefix('=') else {
                    return line.to_string();
                };
                let value = value.split('#').next().unwrap_or("");
                if value.trim() == "true" {
                    changed = true;
                    let indent = &line[..line.len() - trimmed.len()];
                    format!("{indent}idle_animation = false")
                } else {
                    line.to_string()
                }
            })
            .collect();

        if !changed {
            write_marker();
            return false;
        }

        let mut new_content = migrated.join("\n");
        if content.ends_with('\n') {
            new_content.push('\n');
        }
        match std::fs::write(&path, new_content) {
            Ok(()) => {
                Self::invalidate_cache();
                write_marker();
                crate::logging::info(
                    "Migrated idle_animation \"true\" to \"false\" in config.toml",
                );
                true
            }
            Err(err) => {
                crate::logging::warn(&format!(
                    "idle_animation migration failed to write config: {err}"
                ));
                false
            }
        }
    }

    fn normalize_external_auth_source_id(source_id: &str) -> String {
        source_id.trim().to_ascii_lowercase()
    }

    pub(crate) fn trusted_external_auth_path_entry(
        source_id: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<String> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            anyhow::bail!("External auth source id cannot be empty");
        }
        let canonical = crate::storage::validate_external_auth_file(path)?;
        Ok(format!(
            "{}|{}",
            source_id,
            canonical.to_string_lossy().to_ascii_lowercase()
        ))
    }

    pub fn external_auth_source_allowed(source_id: &str) -> bool {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            return false;
        }

        let cfg = Self::load();
        cfg.auth
            .trusted_external_sources
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&source_id))
    }

    pub fn external_auth_source_allowed_for_path(source_id: &str, path: &std::path::Path) -> bool {
        let Ok(entry) = Self::trusted_external_auth_path_entry(source_id, path) else {
            return false;
        };

        let cfg = Self::load();
        cfg.auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
    }

    /// Startup-sensitive variant that uses the process-cached config snapshot.
    ///
    /// This avoids reloading config.toml repeatedly during cold-start probes.
    pub fn external_auth_source_allowed_for_path_cached(
        source_id: &str,
        path: &std::path::Path,
    ) -> bool {
        let Ok(entry) = Self::trusted_external_auth_path_entry(source_id, path) else {
            return false;
        };

        if config()
            .auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
        {
            return true;
        }

        // The global config snapshot can be initialized before an auth flow saves
        // a new path-bound trust decision, or before tests switch JCODE_HOME. Fall
        // back to a fresh load on cache misses so fast auth probes remain correct
        // without penalizing the common already-trusted path.
        Self::load()
            .auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
    }

    pub fn allow_external_auth_source(source_id: &str) -> anyhow::Result<()> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            anyhow::bail!("External auth source id cannot be empty");
        }

        let mut cfg = Self::load();
        if !cfg
            .auth
            .trusted_external_sources
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&source_id))
        {
            cfg.auth.trusted_external_sources.push(source_id.clone());
            cfg.auth.trusted_external_sources.sort();
            cfg.auth.trusted_external_sources.dedup();
            cfg.save()?;
        }

        crate::logging::info(&format!(
            "Saved trusted external auth source to config: {}",
            source_id
        ));
        Ok(())
    }

    pub fn allow_external_auth_source_for_path(
        source_id: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let entry = Self::trusted_external_auth_path_entry(source_id, path)?;
        let mut cfg = Self::load();
        if !cfg
            .auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
        {
            cfg.auth.trusted_external_source_paths.push(entry.clone());
            cfg.auth.trusted_external_source_paths.sort();
            cfg.auth.trusted_external_source_paths.dedup();
            cfg.save()?;
        }
        crate::logging::info(&format!(
            "Saved trusted external auth source path: {}",
            entry
        ));
        Ok(())
    }

    pub fn revoke_external_auth_source_for_path(
        source_id: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let entry = Self::trusted_external_auth_path_entry(source_id, path)?;
        let mut cfg = Self::load();
        let before = cfg.auth.trusted_external_source_paths.len();
        cfg.auth
            .trusted_external_source_paths
            .retain(|value| !value.trim().eq_ignore_ascii_case(&entry));
        if cfg.auth.trusted_external_source_paths.len() != before {
            cfg.save()?;
            crate::logging::info(&format!(
                "Removed trusted external auth source path: {}",
                entry
            ));
        }
        Ok(())
    }

    /// Remove a source-level (non-path) trust decision, e.g. for credentials
    /// that have no stable on-disk path (macOS Keychain items).
    pub fn revoke_external_auth_source(source_id: &str) -> anyhow::Result<()> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            return Ok(());
        }
        let mut cfg = Self::load();
        let before = cfg.auth.trusted_external_sources.len();
        cfg.auth
            .trusted_external_sources
            .retain(|value| !value.trim().eq_ignore_ascii_case(&source_id));
        if cfg.auth.trusted_external_sources.len() != before {
            cfg.save()?;
            crate::logging::info(&format!(
                "Removed trusted external auth source: {}",
                source_id
            ));
        }
        Ok(())
    }
}
