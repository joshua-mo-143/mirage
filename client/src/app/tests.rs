use super::{App, StreamEvent, TranscriptScrollMode};
use crate::{
    args::Args,
    tools::{cursor_session::CursorSessionStore, subagent_tool::SubagentProgressEvent},
    transcript::{TranscriptItem, TranscriptKind, build_transcript_lines, wrapped_line_count},
};
use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
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
    }
}

fn streaming_app() -> App {
    let mut app = App::new(&test_args(), Arc::new(CursorSessionStore::default()));
    app.transcript.push(TranscriptItem::Entry(
        crate::transcript::TranscriptEntry::user("hello"),
    ));
    app.streaming = true;
    app
}

#[test]
fn interleaves_assistant_text_and_tool_entries() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::AssistantText("First chunk.".to_owned()));
    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "read_file".to_owned(),
        summary: "File read: src/main.rs".to_owned(),
    });
    app.apply_stream_event(StreamEvent::AssistantText("Second chunk.".to_owned()));

    let assistant_and_tool_entries: Vec<_> = app.transcript.iter().skip(2).collect();
    assert_eq!(assistant_and_tool_entries.len(), 3);

    let first = assistant_and_tool_entries[0].entry().unwrap();
    assert!(matches!(first.kind, TranscriptKind::Assistant));
    assert_eq!(first.body, "First chunk.");

    let second = assistant_and_tool_entries[1].entry().unwrap();
    assert!(matches!(second.kind, TranscriptKind::Tool));
    assert_eq!(second.title, "File read: src/main.rs (running)");

    let third = assistant_and_tool_entries[2].entry().unwrap();
    assert!(matches!(third.kind, TranscriptKind::Assistant));
    assert_eq!(third.body, "Second chunk.");
}

#[test]
fn tool_before_text_does_not_create_empty_assistant_entry() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "bash".to_owned(),
        summary: "Bash: ls".to_owned(),
    });

    let assistant_entries = app
        .transcript
        .iter()
        .filter_map(TranscriptItem::entry)
        .filter(|entry| matches!(entry.kind, TranscriptKind::Assistant))
        .count();

    assert_eq!(assistant_entries, 0);
    assert!(matches!(
        app.transcript.last().unwrap().entry().unwrap().kind,
        TranscriptKind::Tool
    ));
    assert_eq!(
        app.transcript.last().unwrap().entry().unwrap().title,
        "Bash: ls (running)"
    );
}

#[test]
fn updates_existing_tool_entry_when_tool_completes() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "bash".to_owned(),
        summary: "Bash: ls".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-1".to_owned(),
    });

    let tool_entries: Vec<_> = app
        .transcript
        .iter()
        .filter_map(TranscriptItem::entry)
        .filter(|entry| matches!(entry.kind, TranscriptKind::Tool))
        .collect();

    assert_eq!(tool_entries.len(), 1);
    assert_eq!(tool_entries[0].title, "Bash: ls");
}

#[test]
fn aggregates_repeated_tool_calls_into_one_entry() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "read_file".to_owned(),
        summary: "File read: src/main.rs".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-1".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-2".to_owned(),
        name: "read_file".to_owned(),
        summary: "File read: src/lib.rs".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-2".to_owned(),
    });

    let tool_entries: Vec<_> = app
        .transcript
        .iter()
        .filter_map(TranscriptItem::entry)
        .filter(|entry| matches!(entry.kind, TranscriptKind::Tool))
        .collect();

    assert_eq!(tool_entries.len(), 1);
    assert_eq!(tool_entries[0].title, "File read x2 (latest: src/lib.rs)");
}

#[test]
fn does_not_merge_non_consecutive_tool_calls() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "bash".to_owned(),
        summary: "Bash: ls".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-1".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-2".to_owned(),
        name: "read_file".to_owned(),
        summary: "File read: src/main.rs".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-2".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-3".to_owned(),
        name: "bash".to_owned(),
        summary: "Bash: pwd".to_owned(),
    });

    let tool_entries: Vec<_> = app
        .transcript
        .iter()
        .filter_map(TranscriptItem::entry)
        .filter(|entry| matches!(entry.kind, TranscriptKind::Tool))
        .collect();

    assert_eq!(tool_entries.len(), 3);
    assert_eq!(tool_entries[0].title, "Bash: ls");
    assert_eq!(tool_entries[1].title, "File read: src/main.rs");
    assert_eq!(tool_entries[2].title, "Bash: pwd (running)");
}

#[test]
fn ignores_whitespace_only_assistant_chunks_between_tools() {
    let mut app = streaming_app();

    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-1".to_owned(),
        name: "bash".to_owned(),
        summary: "Bash: ls".to_owned(),
    });
    app.apply_stream_event(StreamEvent::ToolResult {
        id: "tool-1".to_owned(),
    });
    app.apply_stream_event(StreamEvent::AssistantText("\n\n   ".to_owned()));
    app.apply_stream_event(StreamEvent::ToolCall {
        id: "tool-2".to_owned(),
        name: "read_file".to_owned(),
        summary: "File read: src/main.rs".to_owned(),
    });

    let assistant_entries: Vec<_> = app
        .transcript
        .iter()
        .filter_map(TranscriptItem::entry)
        .filter(|entry| matches!(entry.kind, TranscriptKind::Assistant))
        .collect();

    assert!(assistant_entries.is_empty());
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
fn nests_subagent_events_inside_collapsible_group() {
    let mut app = streaming_app();

    app.apply_subagent_event(SubagentProgressEvent::Started {
        id: "subagent-1".to_owned(),
        summary: "Inspect the repo".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
        id: "subagent-1".to_owned(),
        text: "Thinking...".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolStarted {
        id: "subagent-1".to_owned(),
        description: "List files".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
        id: "subagent-1".to_owned(),
        description: "List files".to_owned(),
        output: Some("Cargo.toml".to_owned()),
    });
    app.apply_subagent_event(SubagentProgressEvent::Finished {
        id: "subagent-1".to_owned(),
    });

    let TranscriptItem::SubagentGroup(group) = app.transcript.last().unwrap() else {
        panic!("expected trailing subagent group");
    };

    assert!(!group.expanded);
    assert_eq!(group.entries.len(), 2);
    assert_eq!(group.entries[0].title, "Assistant");
    assert_eq!(group.entries[0].body, "Thinking...");
    assert_eq!(group.entries[1].title, "Tool: List files");
}

#[test]
fn collapsed_subagent_groups_hide_child_entries_in_rendered_transcript() {
    let mut app = streaming_app();
    app.apply_subagent_event(SubagentProgressEvent::Started {
        id: "subagent-1".to_owned(),
        summary: "Inspect the repo".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
        id: "subagent-1".to_owned(),
        text: "Thinking...".to_owned(),
    });

    let collapsed = build_transcript_lines(&app.transcript, Some(app.selected_transcript));
    let collapsed_text = collapsed
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(collapsed_text.contains("[+] Subagent running"));
    assert!(!collapsed_text.contains("Thinking..."));

    if let TranscriptItem::SubagentGroup(group) = app.transcript.last_mut().unwrap() {
        group.expanded = true;
    }

    let expanded = build_transcript_lines(&app.transcript, Some(app.selected_transcript));
    let expanded_text = expanded
        .lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded_text.contains("Thinking..."));
}

#[test]
fn aggregates_subagent_tool_updates_into_one_entry() {
    let mut app = streaming_app();

    app.apply_subagent_event(SubagentProgressEvent::Started {
        id: "subagent-1".to_owned(),
        summary: "Inspect the repo".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolStarted {
        id: "subagent-1".to_owned(),
        description: "List files".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
        id: "subagent-1".to_owned(),
        description: "List files".to_owned(),
        output: None,
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolStarted {
        id: "subagent-1".to_owned(),
        description: "Read Cargo.toml".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
        id: "subagent-1".to_owned(),
        description: "Read Cargo.toml".to_owned(),
        output: None,
    });

    let TranscriptItem::SubagentGroup(group) = app.transcript.last().unwrap() else {
        panic!("expected trailing subagent group");
    };

    let tool_entries: Vec<_> = group
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, TranscriptKind::Tool))
        .collect();
    assert_eq!(tool_entries.len(), 1);
    assert_eq!(tool_entries[0].title, "Tools x2 (latest: Read Cargo.toml)");
}

#[test]
fn ignores_whitespace_only_subagent_chunks() {
    let mut app = streaming_app();

    app.apply_subagent_event(SubagentProgressEvent::Started {
        id: "subagent-1".to_owned(),
        summary: "Inspect the repo".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
        id: "subagent-1".to_owned(),
        text: "\n  ".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::ToolStarted {
        id: "subagent-1".to_owned(),
        description: "List files".to_owned(),
    });

    let TranscriptItem::SubagentGroup(group) = app.transcript.last().unwrap() else {
        panic!("expected trailing subagent group");
    };

    let assistant_entries: Vec<_> = group
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, TranscriptKind::Assistant))
        .collect();
    assert!(assistant_entries.is_empty());
}

#[test]
fn selected_transcript_text_serializes_subagent_group() {
    let mut app = streaming_app();

    app.apply_subagent_event(SubagentProgressEvent::Started {
        id: "subagent-1".to_owned(),
        summary: "Inspect the repo".to_owned(),
    });
    app.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
        id: "subagent-1".to_owned(),
        text: "Thinking...".to_owned(),
    });
    app.selected_transcript = app.transcript.len() - 1;

    let text = app.selected_transcript_text().unwrap();

    assert!(text.contains("[+] Subagent running (1 entries): Inspect the repo"));
    assert!(text.contains("  Assistant"));
    assert!(text.contains("    Thinking..."));
}

#[test]
fn full_transcript_text_includes_top_level_entries() {
    let mut app = streaming_app();
    app.push_transcript_entry(crate::transcript::TranscriptEntry::assistant("Done."));

    let text = app.full_transcript_text();

    assert!(text.contains("Mirage"));
    assert!(text.contains("You"));
    assert!(text.contains("hello"));
    assert!(text.contains("Assistant"));
    assert!(text.contains("Done."));
}

#[test]
fn page_up_enters_manual_scroll_from_tail() {
    let mut app = streaming_app();
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
    let mut app = streaming_app();
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
    let mut app = streaming_app();
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
    let mut app = streaming_app();
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
    let mut app = streaming_app();

    app.toggle_selection_mode();

    assert!(app.selection_mode);
    assert!(app.status.contains("Ctrl+G"));
}

#[test]
fn selection_mode_methods_exit_without_quitting() {
    let mut app = streaming_app();
    app.set_selection_mode(true);
    app.set_selection_mode(false);

    assert!(!app.selection_mode);
    assert!(!app.should_quit);
}
