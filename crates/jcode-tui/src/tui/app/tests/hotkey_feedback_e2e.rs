// End-to-end tests for inline hotkey feedback: rare-hotkey notes and
// unknown-chord near-miss suggestions, driven through App::handle_key.

#[test]
fn unknown_ctrl_chord_sets_hotkey_feedback_with_suggestion() {
    let mut app = create_test_app();
    assert!(app.hotkey_feedback.is_none());

    // Ctrl+M is unbound (no control-key handler claims 'm'); the nearest
    // known hotkey is Alt+M (side panel toggle).
    app.handle_key(KeyCode::Char('m'), KeyModifiers::CONTROL)
        .unwrap();

    let (message, _) = app
        .hotkey_feedback
        .clone()
        .expect("unknown chord should set feedback");
    assert!(message.contains("Ctrl+M"), "{message}");
    assert!(message.contains("isn't bound"), "{message}");
    assert!(
        message.contains(&jcode_tui_core::keybind::alt_chord("M")),
        "{message}"
    );
    assert!(message.contains("side panel"), "{message}");

    // The renderer consumes the trait accessor; it must surface the same text
    // (and expire it later) so the notification line actually shows it.
    {
        use crate::tui::TuiState as _;
        let visible = app
            .hotkey_feedback()
            .expect("trait accessor should expose fresh feedback");
        assert_eq!(visible, message);
    }
}

#[test]
fn rare_known_hotkey_sets_feedback_and_repeats_stop_once_familiar() {
    // The comment below assumed a fresh JCODE_HOME, but nothing established
    // one. Each Ctrl+T press records keybinding-proficiency state through
    // `app_config_dir()`, which re-reads JCODE_HOME every call, so this both
    // depended on the ambient home's usage history and wrote a config tree
    // into whichever temp home a concurrent test had installed. That is how it
    // broke `gather_ambient_info_filters_to_session_reminders_when_ambient_disabled`,
    // whose AmbientManager then read a directory this test had just created.
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let mut app = create_test_app();

    // Ctrl+T toggles queue mode; the temp home above has no usage history, so
    // the first press is "rare" and should explain itself.
    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL)
        .unwrap();
    let (message, _) = app
        .hotkey_feedback
        .clone()
        .expect("first use of a known hotkey should set feedback");
    assert!(message.contains("Ctrl+T"), "{message}");
    assert!(message.contains("queue mode"), "{message}");

    // After enough uses the action becomes familiar and the note stops.
    for _ in 0..8 {
        app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL)
            .unwrap();
    }
    app.hotkey_feedback = None;
    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL)
        .unwrap();
    assert!(
        app.hotkey_feedback.is_none(),
        "familiar hotkeys should not re-announce"
    );

    match prev_home {
        Some(value) => crate::env::set_var("JCODE_HOME", value),
        None => crate::env::remove_var("JCODE_HOME"),
    }
}

#[test]
fn plain_typing_never_sets_hotkey_feedback() {
    let mut app = create_test_app();
    app.handle_key(KeyCode::Char('h'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('I'), KeyModifiers::SHIFT)
        .unwrap();
    assert!(app.hotkey_feedback.is_none());
    assert_eq!(app.input, "hI");
}

#[test]
fn unknown_chord_notice_is_rate_limited_per_chord() {
    let mut app = create_test_app();

    // Ctrl+; is unbound with no near suggestion.
    for _ in 0..6 {
        app.handle_key(KeyCode::Char(';'), KeyModifiers::CONTROL)
            .unwrap();
        // Reset the time-based limiter so only the per-chord cap applies.
        app.last_unknown_hotkey_notice = None;
    }
    assert!(app.unknown_hotkey_seen.get("Ctrl+;").copied().unwrap_or(0) <= 3);
}
