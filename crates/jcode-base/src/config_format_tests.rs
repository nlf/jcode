//! Config tests for what a save does to the *file*, as opposed to the values.
//!
//! `config_color_tests.rs` covers the three-way merge's semantics: whose value
//! wins when two sessions save. These cover the other half of the same
//! contract, which the merge alone never gave: a config users are told to
//! hand-edit must come back out looking like the file they wrote. Saving one
//! setting used to strip every comment, alphabetize the keys, and silently drop
//! anything `Config` does not model, because the merge round-tripped through
//! `toml::Value` and re-serialized the whole document.
//!
//! Every requirement of `Config::save`, and where it is checked:
//!
//! | requirement | check | fails against old? |
//! |---|---|---|
//! | comments on untouched keys survive | `a_comment_on_an_untouched_setting_survives_a_save` | yes |
//! | comments on the *changed* key survive | `a_comment_on_the_changed_setting_survives_a_save` | yes |
//! | key order is preserved | `key_order_survives_a_save` | yes |
//! | unmodelled keys survive | `a_key_the_struct_does_not_model_survives_a_save` | yes |
//! | a no-op save changes no bytes | `a_save_that_changes_nothing_leaves_the_file_byte_identical` | yes |
//! | repeated saves stay stable | `a_second_save_does_not_write_the_whole_struct_out` | yes |
//! | array-of-tables is not flattened | `an_array_of_tables_survives_a_save_unchanged` | yes |
//! | the real template round-trips | `the_shipped_template_survives_a_save_with_its_comments` | yes |
//! | a new key can be added | `a_new_setting_lands_in_a_section_that_did_not_exist` | no (guard) |
//! | a removal clears the file text | `a_removal_deletes_the_key_from_the_file_text` | no (guard) |
//! | a corrupt file is still saveable | `a_corrupt_config_file_can_still_be_saved_over` | no (guard) |
//! | a first save creates the file | `a_save_with_no_existing_file_creates_one` | no (guard) |
//!
//! The merge semantics these sit on top of are covered next door, and must
//! keep passing: `saving_one_setting_does_not_revert_a_concurrent_edit` and
//! `save_still_applies_a_deliberate_removal` in `config_color_tests.rs`.
//!
//! "fails against old" means the test was run against the pre-`toml_edit`
//! implementation and observed to fail. The four marked no are regression
//! guards: they pass either way, because the old code also satisfied them, and
//! they exist so a future change cannot quietly break them. They are not
//! evidence that this change did anything.

use super::Config;

/// Run `body` against a config file seeded with `seed`, in a temp JCODE_HOME.
///
/// The env lock and the restore are what keep this from doing to the real
/// `~/.jcode/config.toml` what the TUI suite once did: writing to the user's
/// actual config because JCODE_HOME was never redirected.
fn with_seeded_config(seed: &str, body: impl FnOnce(&std::path::Path)) {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("JCODE_HOME", dir.path());
    Config::invalidate_cache();

    let path = Config::path().expect("config path");
    std::fs::write(&path, seed).expect("seed config");
    Config::invalidate_cache();

    body(&path);

    match prev_home {
        Some(prev) => crate::env::set_var("JCODE_HOME", prev),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    Config::invalidate_cache();
}

/// A comment above a setting nobody touched must survive a save.
///
/// The shipped template is mostly comments explaining each option. Losing them
/// on the first save turns a documented config into a bare list of values.
#[test]
fn a_comment_on_an_untouched_setting_survives_a_save() {
    let seed = "\
# How wide the transcript renders.
[display]
# Center the transcript in the terminal.
centered = false
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.provider.copilot_premium = Some("on".to_string());
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("# Center the transcript in the terminal."),
            "a comment on an untouched key must survive, got:\n{after}"
        );
        assert!(
            after.contains("# How wide the transcript renders."),
            "a comment on an untouched table must survive, got:\n{after}"
        );
    });
}

/// A comment on the very setting being changed must survive too.
///
/// This is the case a naive implementation gets wrong: replacing a key's value
/// by inserting a fresh item discards the decor `toml_edit` keeps the comment
/// in, so the value updates and its documentation quietly disappears.
#[test]
fn a_comment_on_the_changed_setting_survives_a_save() {
    let seed = "\
[display]
# Center the transcript in the terminal.
centered = false
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("centered = true"),
            "the change itself must land, got:\n{after}"
        );
        assert!(
            after.contains("# Center the transcript in the terminal."),
            "the comment on the changed key must survive, got:\n{after}"
        );
    });
}

/// Hand-written key order must survive, rather than being alphabetized.
///
/// Serializing a `toml::Value` emits map order, so a file grouped the way its
/// author wanted comes back sorted and re-sectioned. Diffing your own config
/// against version control then shows a whole-file change for a one-key edit.
#[test]
fn key_order_survives_a_save() {
    let seed = "\
[display]
centered = false
theme = \"dark\"
diagram_mode = \"ascii\"
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        let centered = after.find("centered").expect("centered present");
        let theme = after.find("theme").expect("theme present");
        let diagram = after.find("diagram_mode").expect("diagram_mode present");
        assert!(
            centered < theme && theme < diagram,
            "the author's key order must survive, got:\n{after}"
        );
    });
}

/// A key `Config` does not model must survive a save.
///
/// Nothing here uses `deny_unknown_fields`, so an unknown key loads without
/// error and is simply not represented in the struct. Re-serializing the struct
/// therefore deleted it: a typo'd key, a key from a newer version, or a section
/// another tool owns would vanish on the next unrelated save, with no message.
#[test]
fn a_key_the_struct_does_not_model_survives_a_save() {
    let seed = "\
[display]
centered = false

[some_future_feature]
enabled = true
threshold = 12
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("[some_future_feature]"),
            "an unmodelled section must survive, got:\n{after}"
        );
        assert!(
            after.contains("threshold = 12"),
            "an unmodelled key must survive, got:\n{after}"
        );
    });
}

/// A save that changes nothing must leave the file byte-identical.
///
/// The strongest statement of the whole contract, and the one that catches
/// reformatting no individual assertion above thought to look for. It is also
/// what makes a save safe to trigger from an unrelated code path: if nothing
/// changed, nothing about the user's file changes either.
#[test]
fn a_save_that_changes_nothing_leaves_the_file_byte_identical() {
    let seed = "\
# jcode configuration
# Hand-written, with deliberate spacing.

[display]
centered   = false     # aligned on purpose
theme = \"dark\"


[provider]
copilot_premium = \"on\"
";
    with_seeded_config(seed, |path| {
        let before = std::fs::read_to_string(path).expect("read before");
        let cfg = Config::load();
        cfg.save().expect("save");
        let after = std::fs::read_to_string(path).expect("read after");
        assert_eq!(
            before, after,
            "a save with no changes must not rewrite the file"
        );
    });
}

/// Two saves in a row must not be worse than one.
///
/// The bug this pins was invisible to every single-save test. `save` re-records
/// the process snapshot afterwards, and recording it by parsing the text just
/// written is wrong: a key absent from the file (because the template ships it
/// commented out) reads back as missing, while the baseline everywhere else is
/// a serialized `Config` where that key is present at its default. The second
/// save therefore saw every defaulted key as a fresh change and wrote the whole
/// struct out, restoring the exact behavior this work removes.
#[test]
fn a_second_save_does_not_write_the_whole_struct_out() {
    let seed = "\
# Only one setting is set here; everything else is a default.
[display]
centered = false
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("first save");
        let after_first = std::fs::read_to_string(path).expect("read after first");

        // A second save, changing nothing, is where the regression appeared.
        cfg.save().expect("second save");
        let after_second = std::fs::read_to_string(path).expect("read after second");

        assert_eq!(
            after_first, after_second,
            "a second save with no further change must not rewrite the file"
        );
        assert!(
            !after_second.contains("animation_fps"),
            "a defaulted key the user never set must not be written out:\n{after_second}"
        );
    });
}

/// The shipped template survives a save with its documentation intact.
///
/// The template is the file most users actually have, and it is ~500 comment
/// lines explaining ~160 settings. It is the realistic version of every
/// assertion above, and the case where losing formatting costs the most.
#[test]
fn the_shipped_template_survives_a_save_with_its_comments() {
    let template = Config::default_config_file_contents();
    let comments_before = template
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();
    assert!(
        comments_before > 100,
        "the template should be comment-rich, or this test proves little"
    );

    with_seeded_config(&template, |path| {
        let before = std::fs::read_to_string(path).expect("read before");
        let mut cfg = Config::load();
        cfg.display.centered = !cfg.display.centered;
        cfg.save().expect("save");
        let after = std::fs::read_to_string(path).expect("read after");

        let comments_after = after
            .lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .count();
        assert_eq!(
            comments_before, comments_after,
            "every comment in the shipped template must survive a save"
        );

        // Exactly one line differs: the setting that was actually changed.
        let added: Vec<&str> = after
            .lines()
            .filter(|line| !before.lines().any(|b| b == *line))
            .collect();
        assert_eq!(
            added,
            vec!["centered = true"],
            "a one-setting save must change exactly one line"
        );
    });
}

/// An array-of-tables section must survive, and stay an array of tables.
///
/// `jcode provider add` writes `[[providers.<name>.models]]`, so this shape is
/// in real users' files. It is the one case where the value conversion could
/// plausibly do damage: `to_edit_value` renders a table as an *inline* table,
/// so a change that rewrote the containing key would turn a readable
/// `[[section]]` block into one long `{ ... }` line, or worse, restructure it.
/// The change set only ever addresses leaves, which should mean the array is
/// never rewritten at all, but "should mean" is why this test exists.
#[test]
fn an_array_of_tables_survives_a_save_unchanged() {
    let seed = "\
[display]
centered = false

[providers.my-gateway]
type = \"openai-compatible\"
base_url = \"https://llm.example.com/v1\"

# The models list, as `provider add` writes it.
[[providers.my-gateway.models]]
id = \"some-model\"

[[providers.my-gateway.models]]
id = \"another-model\"
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("[[providers.my-gateway.models]]"),
            "the array-of-tables header must survive as-is:\n{after}"
        );
        assert_eq!(
            after.matches("[[providers.my-gateway.models]]").count(),
            2,
            "both entries must survive:\n{after}"
        );
        assert!(
            !after.contains("models = ["),
            "the array must not be rewritten as an inline array:\n{after}"
        );
        assert!(
            after.contains("# The models list, as `provider add` writes it."),
            "and its comment survives too:\n{after}"
        );

        // It must still load as the same provider profile.
        Config::invalidate_cache();
        let reloaded = crate::config::config();
        assert!(
            reloaded.providers.contains_key("my-gateway"),
            "the profile must still parse after a save"
        );
    });
}

/// A config file that does not parse must still be saveable.
///
/// Claimed in `save`'s comment but never checked. The writer parses the
/// existing text and falls back to an empty document on failure, so a save
/// against a corrupt file has to still land the caller's change rather than
/// erroring or silently doing nothing. The user loses the unparseable content,
/// which is unavoidable, but does not lose the ability to fix their settings.
#[test]
fn a_corrupt_config_file_can_still_be_saved_over() {
    with_seeded_config("this is not valid toml at all [[[\n", |path| {
        let mut cfg = Config::load();
        cfg.display.centered = true;
        cfg.save().expect("a corrupt file must not make save fail");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("centered = true"),
            "the caller's change must land even over a corrupt file:\n{after}"
        );

        Config::invalidate_cache();
        assert!(
            crate::config::config().display.centered,
            "and it must read back"
        );
    });
}

/// Saving when no config file exists yet must create one.
///
/// The `read_to_string(...).unwrap_or_default()` path. A first-run save has no
/// file to patch, so the change set has to populate an empty document.
///
/// This test was flaky when first written, and the flake was a real defect
/// rather than a test problem. `LOADED_SNAPSHOT` is a process global, and
/// `load_from_file_strict` used to return early for a missing file without
/// touching it, leaving whatever a previously-loaded config had recorded. The
/// baseline decides what counts as a change, so a stale one made a genuine
/// setting look untouched and the save dropped it. In a test binary the
/// previous config is another test's; in production it would be the config
/// from before a `JCODE_HOME` switch or a deleted file. `load_from_file_strict`
/// now clears the snapshot in both the missing and unparseable cases.
#[test]
fn a_save_with_no_existing_file_creates_one() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("JCODE_HOME", dir.path());
    Config::invalidate_cache();

    let path = Config::path().expect("config path");
    assert!(!path.exists(), "precondition: no config file yet");

    let mut cfg = Config::load();
    cfg.display.centered = true;
    cfg.save().expect("save with no existing file");

    assert!(path.exists(), "a save must create the file");
    let written = std::fs::read_to_string(&path).expect("read back");
    assert!(
        written.contains("centered = true"),
        "the change must be in the new file:\n{written}"
    );

    match prev_home {
        Some(prev) => crate::env::set_var("JCODE_HOME", prev),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    Config::invalidate_cache();
}

/// A removal must delete the key from the file's text, not just the value.
///
/// `save_still_applies_a_deliberate_removal` checks the *semantics* (the
/// setting is gone after a reload). This checks the *file*: the writer's
/// `ConfigChange::Remove` arm has to actually remove the line, or a cleared
/// setting would linger in the text while reading back as absent.
#[test]
fn a_removal_deletes_the_key_from_the_file_text() {
    let seed = "\
[display]
centered = false

[display.colors]
error = \"#fb4934\"
ai = \"#b8bb26\"
";
    with_seeded_config(seed, |path| {
        let mut cfg = Config::load();
        assert_eq!(cfg.display.colors.len(), 2, "precondition");
        cfg.display.colors.remove("error");
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            !after.contains("#fb4934"),
            "the removed key must be gone from the text:\n{after}"
        );
        assert!(
            after.contains("#b8bb26"),
            "its sibling must remain:\n{after}"
        );
    });
}

/// A new setting must still be writable into a file that lacks its section.
///
/// The counterweight to everything above: preserving the file must not come at
/// the cost of being unable to add to it. A change set that cannot create a
/// missing `[section]` would silently drop the write.
#[test]
fn a_new_setting_lands_in_a_section_that_did_not_exist() {
    with_seeded_config("[display]\ncentered = false\n", |path| {
        let mut cfg = Config::load();
        cfg.display
            .colors
            .insert("error".to_string(), "#fb4934".to_string());
        cfg.save().expect("save");

        let after = std::fs::read_to_string(path).expect("read back");
        assert!(
            after.contains("#fb4934"),
            "a new value must land even when its section is missing, got:\n{after}"
        );

        Config::invalidate_cache();
        let reloaded = crate::config::config();
        assert_eq!(
            reloaded.display.colors.get("error").map(String::as_str),
            Some("#fb4934"),
            "and it must read back through the normal load path"
        );
    });
}

