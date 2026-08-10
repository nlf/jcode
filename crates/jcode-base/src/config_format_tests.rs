//! Config tests for what a save does to the *file*, as opposed to the values.
//!
//! `config_color_tests.rs` covers the three-way merge's semantics: whose value
//! wins when two sessions save. These cover the other half of the same
//! contract, which the merge alone never gave: a config users are told to
//! hand-edit must come back out looking like the file they wrote. Saving one
//! setting used to strip every comment, alphabetize the keys, and silently drop
//! anything `Config` does not model, because the merge round-tripped through
//! `toml::Value` and re-serialized the whole document.

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

