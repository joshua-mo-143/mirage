use super::{App, TranscriptScrollMode};
use crate::{
    args::Args,
    transcript::{build_transcript_lines, wrapped_line_count},
};
use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
use mirage_core::{
    session::{SubagentProgressEvent, TranscriptEntry, TranscriptItem},
    tools::cursor_session::CursorSessionStore,
};
use ratatui::layout::Rect;
use std::sync::Arc;

fn test_args() -> Args {
    Args {
        prompt: None,
        model: "test-model".to_owned(),
        system_prompt: None,
        temperature: None,
        max_completion_tokens: None,
        uncensored: false,
        max_turns: 8,
        authority: "api.venice.ai".to_owned(),
        base_path: "/api/v1".to_owned(),
        server_url: None,
        admin_key: None,
        local: false,
        start_server: false,
        stop_server: false,
        restart_server: false,
        debug_stream_log: None,
    }
}

fn app_with_transcript() -> App {
    let mut app = App::new(&test_args(), Arc::new(CursorSessionStore::default()));
    app.service
        .session_mut()
        .push_entry(TranscriptEntry::user("hello"));
    app.service.session_mut().streaming = true;
    app
}

#[test]
fn wrapped_line_count_accounts_for_wrapped_visual_rows() {
    let lines = vec![
        ratatui::text::Line::raw("12345"),
        ratatui::text::Line::raw(""),
        ratatui::text::Line::raw("123456789"),
    ];

    assert_eq!(wrapped_line_count(&lines, 5), 4);
}

#[test]
fn collapsed_subagent_groups_hide_child_entries_in_rendered_transcript() {
    let mut app = app_with_transcript();
    app.service
        .session_mut()
        .apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
    app.service
        .session_mut()
        .apply_subagent_event(SubagentProgressEvent::AssistantDelta {
            id: "subagent-1".to_owned(),
            text: "Thinking...".to_owned(),
        });

    let collapsed = build_transcript_lines(
        &app.service.session().transcript,
        Some(app.selected_transcript),
    );
    let collapsed_text = collapsed
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(collapsed_text.contains("[+] Subagent running"));
    assert!(!collapsed_text.contains("Thinking..."));

    if let Some(TranscriptItem::SubagentGroup(group)) =
        app.service.session_mut().transcript.last_mut()
    {
        group.expanded = true;
    }

    let expanded = build_transcript_lines(
        &app.service.session().transcript,
        Some(app.selected_transcript),
    );
    let expanded_text = expanded
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded_text.contains("Thinking..."));
}

#[test]
fn selected_transcript_text_serializes_subagent_group() {
    let mut app = app_with_transcript();

    app.service
        .session_mut()
        .apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
    app.service
        .session_mut()
        .apply_subagent_event(SubagentProgressEvent::AssistantDelta {
            id: "subagent-1".to_owned(),
            text: "Thinking...".to_owned(),
        });
    app.selected_transcript = app.service.session().transcript.len() - 1;

    let text = app.selected_transcript_text().unwrap();

    assert!(text.contains("[+] Subagent running (1 entries): Inspect the repo"));
    assert!(text.contains("  Assistant"));
    assert!(text.contains("    Thinking..."));
}

#[test]
fn full_transcript_text_includes_top_level_entries() {
    let mut app = app_with_transcript();
    app.push_session_entry(TranscriptEntry::assistant("Done."));

    let text = app.full_transcript_text();

    assert!(text.contains("Mirage"));
    assert!(text.contains("You"));
    assert!(text.contains("hello"));
    assert!(text.contains("Assistant"));
    assert!(text.contains("Done."));
}

#[test]
fn page_up_enters_manual_scroll_from_tail() {
    let mut app = app_with_transcript();
    app.last_transcript_max_scroll = 120;
    app.last_transcript_scroll = 120;
    app.last_transcript_page_height = 20;
    app.transcript_scroll_mode = TranscriptScrollMode::FollowTail;

    app.scroll_transcript_page_up();

    assert!(matches!(
        app.transcript_scroll_mode,
        TranscriptScrollMode::Manual
    ));
    assert_eq!(app.transcript_scroll, 101);
}

#[test]
fn page_down_clamps_manual_scroll_to_max() {
    let mut app = app_with_transcript();
    app.last_transcript_max_scroll = 80;
    app.last_transcript_scroll = 75;
    app.last_transcript_page_height = 20;
    app.transcript_scroll_mode = TranscriptScrollMode::Manual;
    app.transcript_scroll = 75;

    app.scroll_transcript_page_down();

    assert_eq!(app.transcript_scroll, 80);
}

#[test]
fn mouse_wheel_scrolls_transcript_inside_transcript_area() {
    let mut app = app_with_transcript();
    app.last_transcript_area = Rect::new(5, 5, 40, 10);
    app.last_transcript_max_scroll = 80;
    app.last_transcript_scroll = 20;
    app.transcript_scroll_mode = TranscriptScrollMode::Manual;
    app.transcript_scroll = 20;

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 8,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.transcript_scroll, 17);
}

#[test]
fn mouse_wheel_ignores_events_outside_transcript_area() {
    let mut app = app_with_transcript();
    app.last_transcript_area = Rect::new(5, 5, 40, 10);
    app.last_transcript_max_scroll = 80;
    app.last_transcript_scroll = 20;
    app.transcript_scroll_mode = TranscriptScrollMode::Manual;
    app.transcript_scroll = 20;

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.transcript_scroll, 20);
}

#[test]
fn selection_mode_methods_toggle_state() {
    let mut app = app_with_transcript();

    app.toggle_selection_mode();

    assert!(app.selection_mode);
    assert!(app.service.session().status.contains("Ctrl+G"));
}

#[test]
fn selection_mode_methods_exit_without_quitting() {
    let mut app = app_with_transcript();
    app.set_selection_mode(true);
    app.set_selection_mode(false);

    assert!(!app.selection_mode);
    assert!(!app.should_quit);
}
