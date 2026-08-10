//! Does a real `Config::save()` preserve a real user's config file?
//!
//! Deliberately an integration test, outside the crate, so it exercises the
//! public API exactly as the TUI's `/colors`, `/widgets`, and model-switch
//! commands do. The unit tests use small hand-written fixtures; this uses the
//! shipped template, which is the file most users actually have.

use jcode_base::config::Config;

#[test]
fn a_real_save_preserves_a_real_users_config_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    unsafe { std::env::set_var("JCODE_HOME", dir.path()) };
    Config::invalidate_cache();

    // The shipped template: ~670 lines, ~500 of them comments.
    let template = Config::default_config_file_contents();
    let path = Config::path().expect("config path");
    std::fs::write(&path, &template).expect("seed");
    Config::invalidate_cache();

    let before = std::fs::read_to_string(&path).expect("before");
    let comments_before = before
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();

    // Exactly what `/centered` does.
    let mut cfg = Config::load();
    cfg.display.centered = !cfg.display.centered;
    cfg.save().expect("save");

    // And a second save, which is where the regression hid.
    let mut cfg2 = Config::load();
    cfg2.provider.copilot_premium = Some("on".to_string());
    cfg2.save().expect("second save");

    let after = std::fs::read_to_string(&path).expect("after");
    let comments_after = after
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();

    eprintln!(
        "lines {} -> {}, comments {} -> {}",
        before.lines().count(),
        after.lines().count(),
        comments_before,
        comments_after
    );

    assert!(
        comments_before > 400,
        "template should be comment-rich: {comments_before}"
    );
    assert_eq!(
        comments_before, comments_after,
        "two real saves must not cost a single comment line"
    );

    // Both changes landed.
    Config::invalidate_cache();
    let reloaded = Config::load();
    assert!(reloaded.display.centered, "first change must survive");
    assert_eq!(
        reloaded.provider.copilot_premium.as_deref(),
        Some("on"),
        "second change must survive"
    );

    // And nothing else moved.
    let added: Vec<&str> = after
        .lines()
        .filter(|line| !before.lines().any(|b| b == *line))
        .collect();
    eprintln!("added lines: {added:?}");
    assert!(
        added.len() <= 2,
        "two one-setting saves must change at most two lines, got {added:?}"
    );
}
