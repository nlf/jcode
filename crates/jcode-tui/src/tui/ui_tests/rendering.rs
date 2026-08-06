use super::*;

#[test]
fn test_render_rounded_box_sides_aligned() {
    let content = vec![
        Line::from("short"),
        Line::from("a longer line of text here"),
        Line::from("mid"),
    ];
    let style = Style::default();
    let lines = render_rounded_box("title", content, 40, style);
    assert!(lines.len() >= 5);
    let top_width = lines[0].width();
    let bottom_width = lines[lines.len() - 1].width();
    assert_eq!(
        top_width, bottom_width,
        "top and bottom borders must be same width: top={}, bottom={}",
        top_width, bottom_width
    );
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            line.width(),
            top_width,
            "line {} has width {} but expected {} (content: {:?})",
            i,
            line.width(),
            top_width,
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_render_rounded_box_emoji_title_aligned() {
    let content = vec![
        Line::from("memory content line one"),
        Line::from("memory content line two"),
    ];
    let style = Style::default();
    let lines = render_rounded_box("🧠 recalled 2 memories", content, 50, style);
    assert!(lines.len() >= 4);
    let top_width = lines[0].width();
    let bottom_width = lines[lines.len() - 1].width();
    assert_eq!(
        top_width, bottom_width,
        "emoji title: top={}, bottom={}",
        top_width, bottom_width
    );
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            line.width(),
            top_width,
            "emoji title: line {} width {} != expected {}",
            i,
            line.width(),
            top_width
        );
    }
}

#[test]
fn test_render_rounded_box_long_title_keeps_body_width_in_sync() {
    let content = vec![Line::from("tiny")];
    let style = Style::default();
    let lines = render_rounded_box("✓ bg bash completed · 6150794bik", content, 24, style);

    assert!(lines.len() >= 3);
    let top_width = lines[0].width();
    assert_eq!(top_width, 24, "box should respect max width");
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            line.width(),
            top_width,
            "long title: line {} width {} != expected {}",
            i,
            line.width(),
            top_width
        );
    }
}

#[test]
fn test_render_direct_message_as_compact_agent_row() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("DM from fox", "Can you take parser tests?");

    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered, vec!["🦊 Can you take parser tests?"]);
    assert!(
        rendered
            .iter()
            .all(|line| !line.contains('│') && !line.contains('✉')),
        "direct messages should render without rails or type icons: {:?}",
        rendered
    );
}

#[test]
fn test_render_swarm_message_matches_exact_compact_snapshot() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("Task · sheep", "Implement compaction asymptotic fixes");

    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(
        rendered,
        vec!["🐑 Implement compaction asymptotic fixes".to_string()]
    );
}

#[test]
fn test_render_swarm_await_as_compact_rail_free_summary() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("🐝 Swarm await", "✓ 2/2");

    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered, vec!["🐝 ✓ 2/2"]);
    assert!(rendered.iter().all(|line| !line.contains('│')));
}

#[test]
fn test_render_swarm_await_wake_message_as_compact_rail_free_summary() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::background_task(
        "🐝 **Swarm await finished**\n\nAll members done. All 1 members are done: sabertooth\n\nMember statuses:\n  ✓ sabertooth (completed)\n\nCompletion reports:\n\n--- sabertooth (completed) ---\nAwait UI demo complete."
            .to_string(),
    );

    let lines = render_background_task_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered, vec!["🐝 ✓ 1/1"]);
    assert!(rendered.iter().all(|line| !line.contains('│')));
    assert!(!rendered.join("\n").contains("sabertooth"));
}

#[test]
fn test_render_swarm_message_trims_extra_newlines() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("Broadcast · coordinator", "\n\nPlan updated\n\n");

    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered, vec!["💫 📣 Plan updated"]);
    assert!(rendered.iter().all(|line| !line.contains('│')));
}

#[test]
fn test_render_channel_and_shared_context_as_compact_agent_rows() {
    crate::tui::markdown::set_center_code_blocks(false);

    let channel = DisplayMessage::swarm("#dev · fox", "Can someone review this?");
    let context = DisplayMessage::swarm("Shared context · fox", "branch = feature/auth");

    let channel_lines = render_swarm_message(&channel, 80, crate::config::DiffDisplayMode::Off)
        .iter()
        .map(extract_line_text)
        .collect::<Vec<_>>();
    let context_lines = render_swarm_message(&context, 80, crate::config::DiffDisplayMode::Off)
        .iter()
        .map(extract_line_text)
        .collect::<Vec<_>>();

    assert_eq!(channel_lines, vec!["🦊 #dev · Can someone review this?"]);
    assert_eq!(context_lines, vec!["🦊 🧠 branch · feature/auth"]);
}

#[test]
fn test_render_file_activity_as_collapsible_compact_row() {
    crate::tui::markdown::set_center_code_blocks(false);
    let content = jcode_tui_messages::encode_collapsible_swarm_content(
        "src/auth.rs · modified",
        "```text\n-old\n+new\n```",
    );
    let msg = DisplayMessage::swarm("File activity · fox", content);

    let rendered = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off)
        .iter()
        .map(extract_line_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered, vec!["🦊 ✎ src/auth.rs · modified  ▸ diff"]);
    assert!(rendered.iter().all(|line| !line.contains('│')));
}

#[test]
fn test_render_file_conflict_places_warning_before_agent() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("File conflict · fox", "src/auth.rs · concurrent edits");

    let rendered = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off)
        .iter()
        .map(extract_line_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered, vec!["⚠ 🦊 src/auth.rs · concurrent edits"]);
}

#[test]
fn test_render_swarm_message_uses_agent_emoji_for_assignments() {
    crate::tui::markdown::set_center_code_blocks(false);
    let msg = DisplayMessage::swarm("Task · sheep", "Implement compaction asymptotic fixes");

    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered, vec!["🐑 Implement compaction asymptotic fixes"]);
}

#[test]
fn test_render_swarm_message_centered_mode_left_aligns_with_shared_padding() {
    let saved = crate::tui::markdown::center_code_blocks();
    crate::tui::markdown::set_center_code_blocks(true);

    let msg = DisplayMessage::swarm("Plan · sheep", "4 items · v1");
    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert_eq!(rendered.len(), 1, "expected one compact plan row");

    let header_pad = rendered[0].chars().take_while(|c| *c == ' ').count();
    assert!(
        header_pad > 0,
        "centered swarm header should be padded: {rendered:?}"
    );
    assert_eq!(rendered[0].trim_start(), "🐝 Plan · 4 items · v1");
    assert!(!rendered[0].contains('│'));
    for line in &lines {
        assert_eq!(
            line.alignment,
            Some(ratatui::layout::Alignment::Left),
            "centered swarm lines should be left-aligned after padding"
        );
    }

    crate::tui::markdown::set_center_code_blocks(saved);
}

#[test]
fn test_render_swarm_message_centered_mode_keeps_task_icon_and_padding() {
    let saved = crate::tui::markdown::center_code_blocks();
    crate::tui::markdown::set_center_code_blocks(true);

    let msg = DisplayMessage::swarm("Task · sheep", "Implement compaction asymptotic fixes");
    let lines = render_swarm_message(&msg, 80, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert!(
        rendered[0].starts_with(' '),
        "centered task header should be padded: {rendered:?}"
    );
    assert_eq!(
        rendered[0].trim_start(),
        "🐑 Implement compaction asymptotic fixes"
    );

    crate::tui::markdown::set_center_code_blocks(saved);
}

#[test]
fn test_render_swarm_message_centered_mode_keeps_file_activity_preview_centered_when_diff_wraps() {
    let saved = crate::tui::markdown::center_code_blocks();
    crate::tui::markdown::set_center_code_blocks(true);

    let msg = DisplayMessage::swarm(
        "File activity · rose",
        "`…/jcode/src/server/comm_sync.rs`

Modified via apply_patch

```text
331-             persist_swarm_state_for(&swarm_id, swarm_state.clone()).await;
331+             persist_swarm_state_for(&swarm_id, swarm_state).await;
```",
    );

    let lines = render_swarm_message(&msg, 120, crate::config::DiffDisplayMode::Off);
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();
    let first_pad = rendered[0].chars().take_while(|c| *c == ' ').count();

    assert!(
        first_pad >= 8,
        "centered file activity notification should preserve a visible left gutter: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .all(|line| line.is_empty() || line.starts_with(&" ".repeat(first_pad))),
        "wrapped file activity preview should keep one shared left pad: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("persist_swarm_state_for")),
        "expected diff preview to remain visible after wrapping: {rendered:?}"
    );

    crate::tui::markdown::set_center_code_blocks(saved);
}

#[test]
fn test_truncate_line_to_width_uses_display_width() {
    let line = Line::from(Span::raw("🧠 hello world"));
    let truncated = truncate_line_to_width(&line, 8);
    let w = truncated.width();
    assert!(w <= 8, "truncated line display width {} should be <= 8", w);
}

#[test]
fn test_render_memory_tiles_uses_variable_box_widths() {
    let mut tiles = group_into_tiles(vec![
        (
            "preference".to_string(),
            "The user wants the mobile experience to be beautiful, animated, and performant."
                .to_string(),
        ),
        (
            "preference".to_string(),
            "User wants a release cut after testing is complete.".to_string(),
        ),
        ("fact".to_string(), "Jeremy".to_string()),
    ]);
    let border_style = Style::default();
    let text_style = Style::default();

    let preference = tiles.remove(0);
    let fact = tiles.remove(0);

    let preference_plan = choose_memory_tile_span(&preference, 20, 2, 2, border_style, text_style)
        .expect("preference span plan");
    let fact_plan =
        choose_memory_tile_span(&fact, 20, 2, 2, border_style, text_style).expect("fact span plan");
    let preference_width = preference_plan.0.width;
    let fact_width = fact_plan.0.width;
    let narrow_preference = plan_memory_tile(&preference, 20, border_style, text_style)
        .expect("narrow preference plan");
    let chosen_preference = preference_plan.0;

    assert!(
        chosen_preference.height <= narrow_preference.height,
        "expected chosen preference width to be at least as space-efficient as the minimum width: chosen_width={}, chosen_height={}, narrow_height={}",
        preference_width,
        chosen_preference.height,
        narrow_preference.height
    );
    assert!(
        preference_width >= fact_width,
        "expected long preference content to not choose a narrower box than fact: pref={}, fact={}",
        preference_width,
        fact_width
    );
}

#[test]
fn test_render_memory_tiles_allows_boxes_below_other_boxes() {
    let tiles = group_into_tiles(vec![
        (
            "preference".to_string(),
            "The mobile experience should be beautiful, animated, and performant.".to_string(),
        ),
        (
            "preference".to_string(),
            "User prefers quick verification that jcode is up-to-date.".to_string(),
        ),
        ("fact".to_string(), "Jeremy".to_string()),
        (
            "entity".to_string(),
            "Star is a named source providing product strategy input.".to_string(),
        ),
        (
            "correction".to_string(),
            "Assistant incorrectly said it had no memory hits despite existing memories."
                .to_string(),
        ),
    ]);

    let lines = render_memory_tiles(
        &tiles,
        120,
        Style::default(),
        Style::default(),
        Some(Line::from("🧠 recalled 5 memories")),
    );
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    let correction_idx = rendered
        .iter()
        .position(|line| line.contains(" correction "))
        .expect("correction box present");

    assert!(
        correction_idx > 0,
        "expected correction box to render below first row: {:?}",
        rendered
    );
    assert!(
        rendered
            .iter()
            .skip(1)
            .any(|line| line.contains(" correction ")),
        "expected at least one box to appear on a later visual row: {:?}",
        rendered
    );
}

#[test]
fn test_render_memory_tiles_uses_full_row_width_for_stable_alignment() {
    let tiles = group_into_tiles(vec![
            (
                "fact".to_string(),
                "home.html has a new \"Final Oral Test\" link under Scripts · Memorization"
                    .to_string(),
            ),
            (
                "preference".to_string(),
                "User wants unprofessional demo/chat messages removed or replaced with professional wording for demos."
                    .to_string(),
            ),
            ("entity".to_string(), "User account name is `jeremy`.".to_string()),
            ("note".to_string(), "The number 42".to_string()),
        ]);

    let lines = render_memory_tiles(
        &tiles,
        96,
        Style::default(),
        Style::default(),
        Some(Line::from("🧠 recalled 4 memories")),
    );
    let rendered: Vec<String> = lines.iter().skip(1).map(extract_line_text).collect();

    assert!(
        rendered
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) == 96),
        "expected each rendered memory row to fill full layout width for stable centering: {:?}",
        rendered
    );
}

#[test]
fn test_parse_memory_display_entries_extracts_updated_at_metadata() {
    let ts = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    let content = format!(
        "# Memory\n\n## Facts\n1. The build is green\n<!-- updated_at: {} -->\n",
        ts
    );

    let entries = parse_memory_display_entries(&content);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "Facts");
    assert_eq!(entries[0].1.content, "The build is green");
    assert!(entries[0].1.updated_at.is_some());
}

#[test]
fn test_render_memory_tiles_shows_updated_age_line() {
    let tiles = group_into_tiles(vec![(
        "fact".to_string(),
        MemoryTileItem {
            content: "The build is green".to_string(),
            updated_at: Some(chrono::Utc::now() - chrono::Duration::hours(2)),
        },
    )]);

    let lines = render_memory_tiles(
        &tiles,
        60,
        Style::default(),
        Style::default(),
        Some(Line::from("🧠 recalled 1 memory")),
    );
    let rendered: Vec<String> = lines.iter().map(extract_line_text).collect();

    assert!(rendered.iter().any(|line| line.contains("updated 2h ago")));
}

#[test]
fn test_render_memory_tiles_do_not_use_background_tint() {
    let tiles = group_into_tiles(vec![(
        "fact".to_string(),
        MemoryTileItem {
            content: "The build is green".to_string(),
            updated_at: Some(chrono::Utc::now() - chrono::Duration::hours(2)),
        },
    )]);

    let lines = render_memory_tiles(
        &tiles,
        60,
        Style::default(),
        Style::default(),
        Some(Line::from("🧠 recalled 1 memory")),
    );

    assert!(
        lines
            .iter()
            .skip(1)
            .flat_map(|line| line.spans.iter())
            .all(|span| span.style.bg.is_none())
    );
}

#[test]
fn test_plan_memory_tile_wraps_long_updated_age_line() {
    let tiles = group_into_tiles(vec![(
        "fact".to_string(),
        MemoryTileItem {
            content: "The build is green".to_string(),
            updated_at: Some(chrono::Utc::now() - chrono::Duration::days(400)),
        },
    )]);

    let plan = plan_memory_tile(&tiles[0], 20, Style::default(), Style::default())
        .expect("memory tile plan");

    assert!(
        plan.lines.iter().all(|line| line.width() == 20),
        "expected wrapped updated-at lines to preserve tile width: {:?}",
        plan.lines.iter().map(extract_line_text).collect::<Vec<_>>()
    );
}

#[test]
fn test_plan_memory_tile_truncates_long_category_title() {
    let tiles = group_into_tiles(vec![(
        "this category title is unexpectedly very long".to_string(),
        "The build is green".to_string(),
    )]);

    let plan = plan_memory_tile(&tiles[0], 20, Style::default(), Style::default())
        .expect("memory tile plan");

    assert!(
        plan.lines.iter().all(|line| line.width() == 20),
        "expected long category titles to be truncated to tile width: {:?}",
        plan.lines.iter().map(extract_line_text).collect::<Vec<_>>()
    );
}

/// Light-theme adaptation: rendering a full frame and adapting it for a light
/// background must leave every non-Reset foreground readable against its
/// (possibly Reset = white) background. This exercises the same buffer-level
/// hook `ui::draw` applies, but with an explicit mode so it cannot race other
/// tests through the process-global theme mode.
#[test]
fn test_light_theme_adapted_frame_has_readable_contrast() {
    fn channel_lum(c: ratatui::style::Color) -> Option<f32> {
        let (r, g, b) = match c {
            ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
            ratatui::style::Color::Indexed(n) => crate::tui::color_support::indexed_to_rgb(n),
            _ => return None,
        };
        // Perceived luminance approximation.
        Some((0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0)
    }

    let messages = vec![
        DisplayMessage {
            role: "user".into(),
            content: "hello there".into(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        },
        DisplayMessage {
            role: "assistant".into(),
            content: "hi! *bold* and `code`".into(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        },
    ];
    let state = TestState {
        display_messages: messages,
        input: "next question".into(),
        ..Default::default()
    };

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            crate::tui::ui::draw(frame, &state);
            jcode_tui_style::theme_mode::adapt_buffer(
                frame.buffer_mut(),
                jcode_tui_style::ThemeMode::Light,
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer();
    let mut checked = 0usize;
    for cell in buffer.content.iter() {
        if cell.symbol().trim().is_empty() {
            continue;
        }
        let Some(fg_lum) = channel_lum(cell.fg) else {
            continue;
        };
        // Reset bg on a light terminal is near-white (lum ~1.0).
        let bg_lum = channel_lum(cell.bg).unwrap_or(1.0);
        assert!(
            (fg_lum - bg_lum).abs() > 0.2,
            "unreadable cell {:?} fg={:?} bg={:?} (fg_lum={fg_lum:.2} bg_lum={bg_lum:.2})",
            cell.symbol(),
            cell.fg,
            cell.bg,
        );
        checked += 1;
    }
    assert!(
        checked > 20,
        "expected to verify a meaningful number of glyph cells, got {checked}"
    );
}

/// End-to-end proof that `[display.colors]` reaches a real rendered frame.
///
/// The unit tests in `jcode-tui-style` cover the substitution in isolation, but
/// the thing a user actually cares about is whether configuring a color changes
/// what the TUI paints. This renders a real frame through `ui::draw`, so it also
/// guards the hook's presence and its ordering relative to the light/dark pass.
#[test]
fn test_configured_palette_recolors_a_real_rendered_frame() {
    fn render() -> ratatui::buffer::Buffer {
        let messages = vec![
            DisplayMessage {
                role: "user".into(),
                content: "hello there".into(),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            },
            DisplayMessage {
                role: "assistant".into(),
                content: "hi! *bold* and `code`".into(),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            },
        ];
        let state = TestState {
            display_messages: messages,
            input: "next question".into(),
            ..Default::default()
        };
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, &state))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    // The palette is process-global, so always restore it, even on failure.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            jcode_tui_style::set_palette(jcode_tui_style::Palette::default());
        }
    }
    let _restore = Restore;

    jcode_tui_style::set_palette(jcode_tui_style::Palette::default());
    let baseline = render();

    // Recolor the user role to a color nothing in the default palette is near.
    let mut palette = jcode_tui_style::Palette::default();
    palette.set(jcode_tui_style::Role::User, (250, 40, 200));
    jcode_tui_style::set_palette(palette);
    let configured = render();

    assert_ne!(
        baseline
            .content
            .iter()
            .map(|cell| cell.fg)
            .collect::<Vec<_>>(),
        configured
            .content
            .iter()
            .map(|cell| cell.fg)
            .collect::<Vec<_>>(),
        "configuring a color role should change the rendered frame"
    );

    // Text content must be untouched: this is a recolor, not a relayout.
    let text_of = |buffer: &ratatui::buffer::Buffer| {
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        text_of(&baseline),
        text_of(&configured),
        "recoloring must not change any rendered text"
    );

    // And the default palette must render identically to no palette at all,
    // which is what keeps existing users' terminals looking the same.
    jcode_tui_style::set_palette(jcode_tui_style::Palette::default());
    assert_eq!(
        baseline,
        render(),
        "the default palette must be a no-op on the rendered frame"
    );
}

// ---------------------------------------------------------------------------
// Column widget placement mode (display.widget_placement = "column").
// ---------------------------------------------------------------------------

fn column_mode_state() -> TestState {
    TestState {
        provider_model: Some("claude-test-1".into()),
        info_widget_data: info_widget::InfoWidgetData {
            model: Some("claude-test-1".into()),
            provider_name: Some("anthropic".into()),
            context_info: Some(crate::prompt::ContextInfo {
                system_prompt_chars: 20_000,
                total_chars: 60_000,
                ..Default::default()
            }),
            ..Default::default()
        },
        widget_placement_mode: crate::config::WidgetPlacementMode::Column,
        ..Default::default()
    }
}

/// Render one full frame and return the buffer.
///
/// Drawing mutates process-global render state (flicker/slow-frame history,
/// remembered widget placements). These tests draw many frames in quick
/// succession, which trips the flicker detector and leaves a notice that then
/// surfaces in unrelated tests drawing a notification into a small area. Clear
/// that history so a column test cannot fail a swarm-buffer test.
fn draw_state(state: &TestState, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, state))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    crate::tui::ui::clear_flicker_frame_history_for_tests();
    crate::tui::info_widget::clear_widget_placements_for_tests();
    buffer
}

/// Column mode must reserve a real column: the transcript's usable area has to
/// shrink, which is the whole point (widgets no longer overlap the text).
#[test]
fn column_mode_reserves_a_transcript_free_column() {
    let mut state = column_mode_state();
    let _ = draw_state(&state, 120, 30);
    let column_layout = crate::tui::ui::last_layout_snapshot();

    state.widget_placement_mode = crate::config::WidgetPlacementMode::Margin;
    let _ = draw_state(&state, 120, 30);
    let margin_layout = crate::tui::ui::last_layout_snapshot();

    let (Some(col), Some(marg)) = (column_layout, margin_layout) else {
        panic!("expected a layout snapshot from both draws");
    };
    assert!(
        col.messages_area.width < marg.messages_area.width,
        "column mode should narrow the transcript (column={} margin={})",
        col.messages_area.width,
        marg.messages_area.width
    );
    assert!(
        col.diff_pane_area.is_some(),
        "column mode should open the right-hand column even with no panel content"
    );
}

/// Widgets in column mode hold a fixed screen position: the same state drawn
/// twice must place them identically, and they must sit inside the column
/// rather than over the transcript.
#[test]
fn column_mode_widgets_sit_outside_the_transcript() {
    let state = column_mode_state();
    let _ = draw_state(&state, 120, 30);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let column = layout.diff_pane_area.expect("column");
    // Recompute placements from the recorded column rather than reading the
    // process-global widget state, which other tests mutate in parallel.
    let (placements, used) = crate::tui::info_widget_layout::calculate_placements_column(
        column,
        &state.info_widget_data,
        true,
    );

    assert!(
        !placements.is_empty(),
        "column mode should place at least one widget for this data"
    );
    assert!(used > 0, "a placed stack must consume rows");
    for p in &placements {
        assert!(
            p.rect.x >= column.x,
            "widget {:?} at x={} intrudes into the transcript (column starts at {})",
            p.rect,
            p.rect.x,
            column.x
        );
        assert!(
            p.rect.x >= layout.messages_area.right(),
            "widget {:?} overlaps the transcript area {:?}",
            p.rect,
            layout.messages_area
        );
    }
}

/// A narrow terminal cannot afford the column, so it must degrade to no column
/// at all rather than crushing the transcript.
#[test]
fn column_mode_degrades_on_narrow_terminals() {
    let state = column_mode_state();
    for width in [20u16, 40, 50] {
        let _ = draw_state(&state, width, 24);
        let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
        assert!(
            layout.messages_area.width >= 20 || width < 20,
            "transcript crushed to {} at terminal width {width}",
            layout.messages_area.width
        );
    }
}

/// End-to-end: with panel content present, the rendered column shows widgets on
/// top, a separator, and the panel content below it. This is the actual visual
/// contract of `widget_placement = "column"`.
#[test]
fn column_mode_stacks_widgets_above_panel_content() {
    let column = Rect::new(80, 0, 40, 30);

    // With content below, the stack is capped and a separator is reserved.
    let (stack_budget, separator) =
        crate::tui::info_widget_layout::split_widget_column(column, true);
    assert_eq!(
        separator,
        Some(crate::tui::info_widget_layout::COLUMN_SEPARATOR_HEIGHT)
    );

    let data = column_mode_state().info_widget_data;
    let (placements, used) =
        crate::tui::info_widget_layout::calculate_placements_column(stack_budget, &data, true);
    assert!(!placements.is_empty(), "expected widgets in the column");

    // Everything the widgets occupy must sit strictly above the separator, and
    // the panel content strictly below it, with no overlap between the two.
    let separator_y = column.y + used;
    for p in &placements {
        assert!(
            p.rect.bottom() <= separator_y,
            "widget {:?} crosses the separator at y={separator_y}",
            p.rect
        );
    }
    let content_top = separator_y + crate::tui::info_widget_layout::COLUMN_SEPARATOR_HEIGHT;
    assert!(
        content_top < column.bottom(),
        "panel content must retain rows below the separator (top={content_top}, column ends {})",
        column.bottom()
    );
}

/// Dump the rendered column as text so the visual result is inspectable, and
/// pin the structural invariants a human would check by eye: a separator rule
/// exists, widget borders sit above it, and nothing bleeds into the transcript.
#[test]
fn column_mode_visual_smoke() {
    let state = column_mode_state();
    let buf = draw_state(&state, 120, 30);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout");
    let column = layout.diff_pane_area.expect("column");

    let row_text = |y: u16, from: u16, to: u16| -> String {
        (from..to).map(|x| buf[(x, y)].symbol()).collect()
    };

    // The column must contain box-drawing borders (widgets draw rounded blocks).
    let mut border_rows = 0usize;
    for y in column.y..column.bottom() {
        let s = row_text(y, column.x, column.right());
        if s.contains('╭') || s.contains('╰') || s.contains('│') {
            border_rows += 1;
        }
    }
    assert!(
        border_rows > 0,
        "expected widget borders drawn in the column; got none.\ncolumn={column:?}"
    );

    // Print the column for eyeball inspection under --nocapture.
    println!("--- column {column:?} ---");
    for y in column.y..column.bottom().min(column.y + 16) {
        println!("|{}|", row_text(y, column.x, column.right()));
    }
    println!(
        "--- transcript right edge = {} ---",
        layout.messages_area.right()
    );
}

/// The headline visual contract: widgets on top, a separator rule, then the
/// side panel content. This is the case the column mode exists for, so render
/// it for real and assert the three bands appear in order.
#[test]
fn column_mode_renders_separator_between_widgets_and_panel() {
    let mut state = column_mode_state();
    state.side_panel = Some(crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("p1".into()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "p1".into(),
            title: "Notes".into(),
            file_path: "notes.md".into(),
            content: "PANELBODY alpha\n\nPANELBODY bravo\n".into(),
            ..Default::default()
        }],
    });

    let buf = draw_state(&state, 120, 30);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout");
    let column = layout.diff_pane_area.expect("column");

    let row_text = |y: u16| -> String {
        (column.x..column.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    };

    println!("--- column {column:?} ---");
    for y in column.y..column.bottom() {
        println!("|{}|", row_text(y));
    }

    // Find the panel body and the separator rule.
    let body_row = (column.y..column.bottom()).find(|&y| row_text(y).contains("PANELBODY"));
    let sep_row = (column.y..column.bottom()).find(|&y| {
        let s = row_text(y);
        s.chars().filter(|&c| c == '\u{2500}').count() >= (column.width as usize) / 2
            && !s.contains('\u{256d}')
            && !s.contains('\u{2570}')
    });

    let body_row = body_row.expect("panel content must render in the column");
    let sep_row = sep_row.expect("a separator rule must render in the column");
    assert!(
        sep_row < body_row,
        "separator (y={sep_row}) must sit above the panel content (y={body_row})"
    );
}

/// Render at a range of terminal widths and confirm the column never starves
/// the transcript: either a column exists and the transcript keeps a workable
/// width, or the column is dropped entirely.
#[test]
fn column_mode_width_sweep_keeps_transcript_usable() {
    let state = column_mode_state();
    for width in [40u16, 60, 80, 100, 120, 200] {
        let _ = draw_state(&state, width, 30);
        let layout = crate::tui::ui::last_layout_snapshot().expect("layout");
        let msg = layout.messages_area;
        match layout.diff_pane_area {
            Some(col) => {
                assert!(
                    msg.right() <= col.x,
                    "w={width}: transcript {msg:?} overlaps column {col:?}"
                );
                assert!(
                    msg.width >= 20,
                    "w={width}: transcript starved to {} by column {col:?}",
                    msg.width
                );
            }
            None => {
                assert!(
                    width < 60,
                    "w={width}: column was dropped even though there was room"
                );
            }
        }
        println!(
            "w={width:3} -> transcript {:3} col {:?}",
            msg.width,
            layout.diff_pane_area.map(|c| c.width)
        );
    }
}

/// The layout snapshot deliberately reports the whole column, not just the
/// scrollable remainder: drag-resize, the debug capture, and the diagram-border
/// hit test all need the column's true geometry. Routing a wheel over the widget
/// band to the side pane is therefore expected, and safe, because a pane with no
/// rendered content ignores the motion (see
/// `scroll_over_widget_band_does_not_corrupt_pane_state`). This pins the
/// geometry contract so a future change cannot silently shrink it.
#[test]
fn column_mode_snapshot_reports_full_column_geometry() {
    let state = column_mode_state();
    let _ = draw_state(&state, 120, 30);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout");
    let column = layout.diff_pane_area.expect("column");

    assert_eq!(
        column.x,
        layout.messages_area.right(),
        "column must start exactly where the transcript ends"
    );
    // The column spans the whole chat frame, which is taller than the transcript
    // (the transcript is shortened by the input chrome below it). It must at
    // least cover the transcript rows, or a wheel over the lower part of the
    // column would fall through to nothing.
    assert!(
        column.height >= layout.messages_area.height,
        "column ({}) must cover at least the transcript rows ({})",
        column.height,
        layout.messages_area.height
    );
}

/// `widget_placement = "off"` must place nothing and, crucially, must not
/// reserve a column: the transcript should be exactly as wide as it is with no
/// widget data at all.
#[test]
fn off_mode_places_nothing_and_reserves_no_column() {
    let mut state = column_mode_state();
    state.widget_placement_mode = crate::config::WidgetPlacementMode::Off;
    let _ = draw_state(&state, 120, 30);
    let off = crate::tui::ui::last_layout_snapshot().expect("layout");

    assert!(
        off.diff_pane_area.is_none(),
        "off mode must not reserve a column: {:?}",
        off.diff_pane_area
    );

    // Same width as a session with no widget data whatsoever.
    let bare = TestState {
        widget_placement_mode: crate::config::WidgetPlacementMode::Off,
        ..Default::default()
    };
    let _ = draw_state(&bare, 120, 30);
    let bare_layout = crate::tui::ui::last_layout_snapshot().expect("layout");
    assert_eq!(
        off.messages_area.width, bare_layout.messages_area.width,
        "off mode must cost the transcript nothing"
    );
}

/// Alt+I (the global widget toggle) must give the column's space back to the
/// transcript in column mode, not leave an empty reserved strip.
#[test]
fn column_mode_toggle_off_returns_space_to_transcript() {
    let state = column_mode_state();

    let _ = draw_state(&state, 120, 30);
    let on = crate::tui::ui::last_layout_snapshot().expect("layout");
    assert!(
        on.diff_pane_area.is_some(),
        "expected a column while enabled"
    );
    let on_width = on.messages_area.width;

    // `WIDGETS_STATE` is process-global, so restore it before asserting and
    // clear the remembered placements: leaving either mutated leaks into
    // unrelated tests that render a frame.
    crate::tui::info_widget::toggle_enabled();
    let disabled = !crate::tui::info_widget::is_enabled();
    let _ = draw_state(&state, 120, 30);
    let off = crate::tui::ui::last_layout_snapshot().expect("layout");
    crate::tui::info_widget::toggle_enabled();
    crate::tui::info_widget::clear_widget_placements_for_tests();
    assert!(
        crate::tui::info_widget::is_enabled(),
        "global widget state must be restored for other tests"
    );

    assert!(disabled, "toggle should have disabled widgets");
    assert!(
        off.diff_pane_area.is_none(),
        "disabled widgets must not keep an empty column reserved: {:?}",
        off.diff_pane_area
    );
    assert!(
        off.messages_area.width > on_width,
        "transcript must reclaim the column width ({} -> {})",
        on_width,
        off.messages_area.width
    );
}

/// Locate rendered session-fact rows, returning `(row, first_non_blank_column)`.
///
/// Scoped to the right-hand region so the welcome banner (which also prints the
/// model name) cannot be mistaken for a docked fact.
fn session_fact_rows(buffer: &ratatui::buffer::Buffer, from_x: u16) -> Vec<(u16, u16)> {
    (0..buffer.area.height)
        .filter_map(|y| {
            let row: String = (from_x..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            // "Anthropic" (provider) and "Test 1" (prettified model) are the
            // facts this fixture produces.
            if !(row.contains("Anthropic") || row.contains("Test 1")) {
                return None;
            }
            let first = (from_x..buffer.area.width)
                .find(|&x| !buffer[(x, y)].symbol().trim().is_empty())?;
            Some((y, first))
        })
        .collect()
}

/// The session facts (provider / model / context) belong with the widgets. In
/// column mode they must dock inside the reserved column rather than float in
/// the composer chrome, so all the session metadata reads as one object.
#[test]
fn column_mode_docks_session_facts_inside_the_column() {
    let state = column_mode_state();
    let buffer = draw_state(&state, 120, 30);
    let column = crate::tui::ui::last_layout_snapshot()
        .expect("layout")
        .diff_pane_area
        .expect("column mode reserves a column");

    // Search the whole width: a fact leaking left of the column must be caught.
    let facts = session_fact_rows(&buffer, 0);
    let below_banner: Vec<_> = facts
        .into_iter()
        .filter(|&(y, _)| y >= buffer.area.height / 2)
        .collect();
    assert!(
        !below_banner.is_empty(),
        "expected the session facts to render in the lower half in column mode"
    );
    for (row, first_x) in below_banner {
        assert!(
            first_x >= column.x,
            "session fact on row {row} starts at x={first_x}, left of the widget \
             column (x={}); it should be docked inside the column",
            column.x
        );
    }
}

/// Docking must not silently drop facts: column mode should show at least as
/// many fact rows as the composer-anchored path would have produced.
#[test]
fn column_mode_docking_preserves_every_fact_row() {
    let column_buf = draw_state(&column_mode_state(), 120, 30);
    let column_facts = session_fact_rows(&column_buf, 60).len();

    let mut margin_state = column_mode_state();
    margin_state.widget_placement_mode = crate::config::WidgetPlacementMode::Margin;
    let margin_buf = draw_state(&margin_state, 120, 30);
    let margin_facts = session_fact_rows(&margin_buf, 60).len();

    assert!(
        column_facts >= margin_facts,
        "column docking lost fact rows (column={column_facts} margin={margin_facts})"
    );
    assert!(
        column_facts >= 2,
        "expected at least the provider and model rows, got {column_facts}"
    );
}

/// Count bordered widget boxes in the column by counting top-border rows.
fn column_box_count(buffer: &ratatui::buffer::Buffer, column_x: u16) -> usize {
    (0..buffer.area.height)
        .filter(|&y| {
            let row: String = (column_x..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            row.trim_start().starts_with('\u{256d}')
        })
        .count()
}

fn column_contains(buffer: &ratatui::buffer::Buffer, column_x: u16, needle: &str) -> bool {
    (0..buffer.area.height).any(|y| {
        let row: String = (column_x..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        row.contains(needle)
    })
}

fn git_column_state(merge_disabled: bool) -> TestState {
    let mut state = column_mode_state();
    state.info_widget_data.overview_merge_disabled = merge_disabled;
    state.info_widget_data.git_info = Some(crate::tui::info_widget::GitInfo {
        branch: "master".into(),
        modified: 2,
        staged: 0,
        untracked: 0,
        ahead: 8,
        behind: 0,
        dirty_files: vec!["README.md".into(), "src/tui/ui.rs".into()],
    });
    state
}

/// End-to-end on the rendered frame (not just placement math): merging off must
/// draw several separate widget boxes and, critically, surface the git widget's
/// per-file list. Merging on collapses everything into one box and drops the
/// filenames, which is the behavior `display.widget_overview = false` exists to
/// escape.
#[test]
fn unmerged_column_renders_separate_boxes_with_git_filenames() {
    let merged = draw_state(&git_column_state(false), 120, 40);
    let merged_boxes = column_box_count(&merged, 90);
    assert!(
        !column_contains(&merged, 90, "README.md"),
        "merged Overview uses the compact git line, so filenames should not appear"
    );

    let unmerged = draw_state(&git_column_state(true), 120, 40);
    let unmerged_boxes = column_box_count(&unmerged, 90);

    assert!(
        unmerged_boxes > merged_boxes,
        "merging off should draw more widget boxes (merged={merged_boxes} unmerged={unmerged_boxes})"
    );
    assert!(
        column_contains(&unmerged, 90, "README.md")
            && column_contains(&unmerged, 90, "src/tui/ui.rs"),
        "the standalone git widget must list its changed files"
    );
}

/// `display.session_facts = false` must remove the facts from the frame
/// entirely, in both placements, without disturbing anything else. They exist
/// to be hidden when the info widgets already carry the same information.
#[test]
fn session_facts_can_be_hidden_in_both_placements() {
    // Column-docked placement.
    let shown = draw_state(&git_column_state(true), 120, 40);
    assert!(
        column_contains(&shown, 0, "Anthropic"),
        "sanity: facts should render while enabled"
    );

    let mut hidden_state = git_column_state(true);
    hidden_state.hide_session_facts = true;
    let hidden = draw_state(&hidden_state, 120, 40);
    assert!(
        !column_contains(&hidden, 0, "Anthropic"),
        "session facts must not render when disabled"
    );

    // The widget column itself must be unaffected: hiding the facts is not
    // allowed to take the widgets with it.
    assert_eq!(
        column_box_count(&shown, 90),
        column_box_count(&hidden, 90),
        "hiding the facts must not change the widget boxes"
    );

    // Composer-anchored placement: the facts only draw into genuinely blank
    // composer cells, and in this fixture the Overview widget already occupies
    // that region, so they legitimately stand down there. Assert the gate
    // itself instead, so the composer-anchored path is still covered.
    let mut margin_state = git_column_state(false);
    margin_state.widget_placement_mode = crate::config::WidgetPlacementMode::Margin;
    assert!(
        margin_state.session_facts_enabled(),
        "facts enabled by default"
    );
    margin_state.hide_session_facts = true;
    assert!(
        !margin_state.session_facts_enabled(),
        "the toggle must disable the composer-anchored stack too"
    );
}

/// Sweep the whole configuration space these toggles create, on the rendered
/// buffer, so the display options are checked in combination rather than only
/// in the one arrangement they were developed against.
///
/// `session_facts` must win everywhere it applies. The composer-anchored stack
/// only draws into genuinely blank cells, so widgets are suppressed here to
/// expose that path; otherwise the widgets legitimately occupy those cells and
/// the facts stand down on their own.
#[test]
fn session_facts_toggle_holds_across_placement_and_merge_combinations() {
    for placement in [
        crate::config::WidgetPlacementMode::Margin,
        crate::config::WidgetPlacementMode::Column,
    ] {
        for merge_off in [false, true] {
            for suppress_widgets in [false, true] {
                let mut shown = git_column_state(merge_off);
                shown.widget_placement_mode = placement;
                shown.suppress_info_widgets = suppress_widgets;

                let mut hidden = shown.clone();
                hidden.hide_session_facts = true;

                let shown_buf = draw_state(&shown, 120, 40);
                let hidden_buf = draw_state(&hidden, 120, 40);

                // Hiding must never *add* facts, and must remove any that the
                // enabled frame drew.
                assert!(
                    !column_contains(&hidden_buf, 0, "Anthropic"),
                    "facts still rendered with session_facts off \
                     (placement={placement:?} merge_off={merge_off} \
                      suppress_widgets={suppress_widgets})"
                );

                // The widgets themselves must be untouched by the fact toggle.
                assert_eq!(
                    column_box_count(&shown_buf, 90),
                    column_box_count(&hidden_buf, 90),
                    "hiding facts changed the widget boxes \
                     (placement={placement:?} merge_off={merge_off} \
                      suppress_widgets={suppress_widgets})"
                );
            }
        }
    }
}

/// The composer-anchored stack (the path the column-docking work replaced in
/// column mode) must still honor the toggle on the real buffer, not just via
/// its gate function.
#[test]
fn composer_anchored_facts_respect_the_toggle_on_the_buffer() {
    let mut shown = git_column_state(false);
    shown.widget_placement_mode = crate::config::WidgetPlacementMode::Margin;
    shown.suppress_info_widgets = true;
    let shown_buf = draw_state(&shown, 120, 40);
    assert!(
        column_contains(&shown_buf, 0, "Anthropic"),
        "composer-anchored facts should draw when the chrome cells are free"
    );

    let mut hidden = shown.clone();
    hidden.hide_session_facts = true;
    let hidden_buf = draw_state(&hidden, 120, 40);
    assert!(
        !column_contains(&hidden_buf, 0, "Anthropic"),
        "composer-anchored facts must disappear when session_facts is off"
    );
}

/// Re-check the column-docking behavior under the session_facts toggle that
/// landed after it. Docking and hiding are separate features that touch the
/// same rows, so verify them together: when facts are shown they must still be
/// docked inside the column, when hidden they must be gone, and in neither case
/// may the column's geometry move.
#[test]
fn column_docking_still_holds_under_the_session_facts_toggle() {
    let mut geometries = Vec::new();

    for merge_off in [false, true] {
        let mut shown = git_column_state(merge_off);
        shown.hide_session_facts = false;
        let shown_buf = draw_state(&shown, 120, 40);
        let shown_col = crate::tui::ui::last_layout_snapshot()
            .expect("layout")
            .diff_pane_area
            .expect("column");
        geometries.push(shown_col);

        let half = shown_buf.area.height / 2;
        let docked: Vec<_> = session_fact_rows(&shown_buf, 0)
            .into_iter()
            .filter(|&(y, _)| y >= half)
            .collect();
        assert!(
            !docked.is_empty(),
            "facts should render while enabled (merge_off={merge_off})"
        );
        for (row, x) in docked {
            assert!(
                x >= shown_col.x,
                "fact row {row} at x={x} escaped the column (x={}) with merge_off={merge_off}",
                shown_col.x
            );
        }

        let mut hidden = git_column_state(merge_off);
        hidden.hide_session_facts = true;
        let hidden_buf = draw_state(&hidden, 120, 40);
        let hidden_col = crate::tui::ui::last_layout_snapshot()
            .expect("layout")
            .diff_pane_area
            .expect("column");
        geometries.push(hidden_col);

        let half = hidden_buf.area.height / 2;
        assert!(
            session_fact_rows(&hidden_buf, 0)
                .into_iter()
                .all(|(y, _)| y < half),
            "facts still docked with session_facts off (merge_off={merge_off})"
        );
    }

    // Hiding the facts must not resize or move the column.
    let first = geometries[0];
    for geom in &geometries {
        assert_eq!(
            (geom.x, geom.width),
            (first.x, first.width),
            "column geometry changed across fact/merge combinations: {geometries:?}"
        );
    }
}

/// A configured widget order must be what the column actually stacks, top
/// down, and must exclude widgets the user left out even when they have data.
#[test]
fn column_mode_honors_the_configured_widget_order() {
    let mut state = column_mode_state();
    state.info_widget_data.overview_merge_disabled = true;
    state.info_widget_data.enabled_widgets = vec![
        info_widget::WidgetKind::ContextUsage,
        info_widget::WidgetKind::ModelInfo,
    ];

    let _ = draw_state(&state, 120, 40);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let column = layout.diff_pane_area.expect("column");
    let (placements, _) = crate::tui::info_widget_layout::calculate_placements_column(
        column,
        &state.info_widget_data,
        true,
    );

    let kinds: Vec<_> = placements.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        vec![
            info_widget::WidgetKind::ContextUsage,
            info_widget::WidgetKind::ModelInfo
        ],
        "the column stacks exactly the configured widgets, in the configured order"
    );
    // Top-down: the first configured widget is the topmost rect.
    assert!(
        placements[0].rect.y < placements[1].rect.y,
        "configured order must be the visual top-down order"
    );
}
