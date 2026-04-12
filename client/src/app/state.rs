use super::{
    App, Args, FocusArea, PendingSubagent, PendingToolCall, ToolAggregate, TranscriptScrollMode,
};
use crate::{
    app::helpers::{
        copy_text_to_clipboard, render_subagent_tool_aggregate_title, render_tool_aggregate_title,
        tool_detail_from_summary, tool_label, truncate_text,
    },
    tools::cursor_session::CursorSessionStore,
    transcript::{SubagentGroup, TranscriptEntry, TranscriptItem},
};
use std::sync::Arc;

impl App {
    pub(crate) fn new(args: &Args, cursor_sessions: Arc<CursorSessionStore>) -> Self {
        let mut transcript = vec![TranscriptItem::Entry(TranscriptEntry::meta(
            "Mirage",
            "Type a message below. Use /help for commands. Built-in tools: `bash`, `prompt_cursor`, `subagent`, `read_file`, `edit_file`, `write_file` (whole-file writes only).",
        ))];

        if let Some(system_prompt) = args.system_prompt.as_deref() {
            transcript.push(TranscriptItem::Entry(TranscriptEntry::meta(
                "System Prompt",
                truncate_text(system_prompt, 160),
            )));
        }

        let input = args.prompt.clone().unwrap_or_default();
        let cursor = input.chars().count();
        let selected_transcript = transcript.len().saturating_sub(1);

        Self {
            transcript,
            input,
            cursor,
            history: Vec::new(),
            status: "Ready.".to_owned(),
            usage: None,
            pending_assistant: None,
            pending_tool_calls: std::collections::HashMap::new(),
            active_tool_aggregates: std::collections::HashMap::new(),
            pending_subagents: std::collections::HashMap::new(),
            streaming: false,
            should_quit: false,
            model: args.model.clone(),
            max_turns: args.max_turns,
            authority: args.authority.clone(),
            base_path: args.base_path.clone(),
            system_prompt_configured: args.system_prompt.is_some(),
            uncensored: args.uncensored,
            selection_mode: false,
            focus: FocusArea::Composer,
            selected_transcript,
            transcript_scroll: 0,
            transcript_scroll_mode: TranscriptScrollMode::FollowTail,
            last_transcript_scroll: 0,
            last_transcript_max_scroll: 0,
            last_transcript_page_height: 0,
            last_transcript_area: ratatui::layout::Rect::default(),
            cursor_sessions,
        }
    }

    pub(crate) fn can_submit(&self) -> bool {
        matches!(self.focus, FocusArea::Composer)
            && !self.streaming
            && !self.input.trim().is_empty()
    }

    pub(super) fn push_transcript_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(TranscriptItem::Entry(entry));
        self.follow_transcript_tail_if_composing();
    }

    pub(super) fn push_subagent_group(&mut self, id: String, summary: String) {
        let transcript_index = self.transcript.len();
        self.transcript
            .push(TranscriptItem::SubagentGroup(SubagentGroup::new(summary)));
        self.pending_subagents.insert(
            id,
            PendingSubagent {
                transcript_index,
                pending_entry_index: None,
                tool_entry_index: None,
                tool_call_count: 0,
                pending_tool_calls: 0,
                latest_tool_description: String::new(),
            },
        );
        self.follow_transcript_tail_if_composing();
    }

    fn follow_transcript_tail_if_composing(&mut self) {
        if matches!(self.focus, FocusArea::Composer) {
            self.selected_transcript = self.transcript.len().saturating_sub(1);
            self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
        }
    }

    pub(super) fn clamp_selected_transcript(&mut self) {
        self.selected_transcript = self
            .selected_transcript
            .min(self.transcript.len().saturating_sub(1));
    }

    pub(super) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusArea::Composer => FocusArea::Transcript,
            FocusArea::Transcript => FocusArea::Composer,
        };
        self.follow_transcript_tail_if_composing();
    }

    pub(super) fn set_selection_mode(&mut self, enabled: bool) {
        self.selection_mode = enabled;
        self.status = if enabled {
            "Selection mode enabled. Drag with the mouse to select text; press Ctrl+G or Esc to return."
                .to_owned()
        } else {
            "Selection mode disabled. Mouse interactions restored to Mirage.".to_owned()
        };
    }

    pub(super) fn toggle_selection_mode(&mut self) {
        self.set_selection_mode(!self.selection_mode);
    }

    pub(super) fn select_previous_transcript_item(&mut self) {
        self.selected_transcript = self.selected_transcript.saturating_sub(1);
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    pub(super) fn select_next_transcript_item(&mut self) {
        self.selected_transcript =
            (self.selected_transcript + 1).min(self.transcript.len().saturating_sub(1));
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    pub(super) fn toggle_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = !group.expanded;
        }
    }

    pub(super) fn expand_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = true;
        }
    }

    pub(super) fn collapse_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = false;
        }
    }

    pub(super) fn update_subagent_group<R>(
        &mut self,
        id: &str,
        update: impl FnOnce(&mut SubagentGroup, &mut PendingSubagent) -> R,
    ) -> Option<R> {
        let transcript_index = self.pending_subagents.get(id)?.transcript_index;
        let TranscriptItem::SubagentGroup(group) = self.transcript.get_mut(transcript_index)?
        else {
            return None;
        };
        let pending = self.pending_subagents.get_mut(id)?;
        Some(update(group, pending))
    }

    pub(super) fn selected_transcript_text(&self) -> Option<String> {
        self.transcript
            .get(self.selected_transcript)
            .map(TranscriptItem::to_plaintext)
    }

    pub(super) fn full_transcript_text(&self) -> String {
        self.transcript
            .iter()
            .map(TranscriptItem::to_plaintext)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(super) fn copy_selected_transcript_item(&mut self) {
        let Some(text) = self.selected_transcript_text() else {
            self.status = "Nothing to copy.".to_owned();
            return;
        };

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.status = "Copied selected transcript item to clipboard.".to_owned();
            }
            Err(error) => {
                self.status = format!("Clipboard copy failed: {error}");
            }
        }
    }

    pub(super) fn copy_full_transcript(&mut self) {
        let text = self.full_transcript_text();
        if text.trim().is_empty() {
            self.status = "Nothing to copy.".to_owned();
            return;
        }

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.status = "Copied full transcript to clipboard.".to_owned();
            }
            Err(error) => {
                self.status = format!("Clipboard copy failed: {error}");
            }
        }
    }

    pub(super) fn scroll_transcript_up(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current.saturating_sub(lines);
    }

    pub(super) fn scroll_transcript_down(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current
            .saturating_add(lines)
            .min(self.last_transcript_max_scroll);
    }

    pub(super) fn scroll_transcript_page_up(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_up(page);
    }

    pub(super) fn scroll_transcript_page_down(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_down(page);
    }

    pub(super) fn scroll_transcript_to_top(&mut self) {
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = 0;
    }

    pub(super) fn scroll_transcript_to_bottom(&mut self) {
        self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
    }

    fn current_transcript_scroll(&self) -> u16 {
        match self.transcript_scroll_mode {
            TranscriptScrollMode::FollowTail => self.last_transcript_max_scroll,
            TranscriptScrollMode::FollowSelection | TranscriptScrollMode::Manual => {
                self.last_transcript_scroll
            }
        }
    }

    pub(super) fn clear_active_tool_aggregates(&mut self) {
        self.pending_tool_calls.clear();
        self.active_tool_aggregates.clear();
    }

    fn update_tool_aggregate_title(&mut self, transcript_index: usize) {
        let Some(aggregate) = self.active_tool_aggregates.get(&transcript_index) else {
            return;
        };
        let title = render_tool_aggregate_title(
            &aggregate.label,
            &aggregate.latest_detail,
            aggregate.total_calls,
            aggregate.pending_calls,
        );
        if let Some(entry) = self
            .transcript
            .get_mut(transcript_index)
            .and_then(TranscriptItem::entry_mut)
        {
            entry.title = title;
        }
    }

    pub(super) fn record_tool_call(&mut self, id: String, name: String, summary: String) {
        let label = tool_label(&name).to_owned();
        let detail = tool_detail_from_summary(&summary);
        let existing_transcript_index = self.transcript.len().checked_sub(1).filter(|index| {
            self.active_tool_aggregates
                .get(index)
                .is_some_and(|aggregate| aggregate.name == name)
        });

        let transcript_index = existing_transcript_index.unwrap_or_else(|| {
            self.push_transcript_entry(TranscriptEntry::tool(String::new(), String::new()));
            let transcript_index = self.transcript.len() - 1;
            self.active_tool_aggregates.insert(
                transcript_index,
                ToolAggregate {
                    name,
                    label,
                    latest_detail: String::new(),
                    total_calls: 0,
                    pending_calls: 0,
                },
            );
            transcript_index
        });

        if let Some(aggregate) = self.active_tool_aggregates.get_mut(&transcript_index) {
            aggregate.total_calls += 1;
            aggregate.pending_calls += 1;
            aggregate.latest_detail = detail;
        }
        self.pending_tool_calls
            .insert(id, PendingToolCall { transcript_index });
        self.update_tool_aggregate_title(transcript_index);
    }

    pub(super) fn record_tool_result(&mut self, id: String) {
        let Some(pending) = self.pending_tool_calls.remove(&id) else {
            self.push_transcript_entry(TranscriptEntry::tool(
                format!("Tool: {}", truncate_text(&id, 80)),
                String::new(),
            ));
            return;
        };

        if let Some(aggregate) = self
            .active_tool_aggregates
            .get_mut(&pending.transcript_index)
        {
            aggregate.pending_calls = aggregate.pending_calls.saturating_sub(1);
        }
        self.update_tool_aggregate_title(pending.transcript_index);
    }

    pub(super) fn update_subagent_tool_title(
        group: &mut SubagentGroup,
        pending: &mut PendingSubagent,
    ) {
        if pending.tool_call_count == 0 && pending.tool_entry_index.is_none() {
            return;
        }

        let title = render_subagent_tool_aggregate_title(
            &pending.latest_tool_description,
            pending.tool_call_count,
            pending.pending_tool_calls,
        );

        if let Some(index) = pending.tool_entry_index {
            if let Some(entry) = group.entries.get_mut(index) {
                entry.title = title;
                return;
            }
        }

        group
            .entries
            .push(TranscriptEntry::tool(title, String::new()));
        pending.tool_entry_index = Some(group.entries.len() - 1);
    }

    pub(super) fn input_chars(&self) -> Vec<char> {
        self.input.chars().collect()
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        let mut chars = self.input_chars();
        chars.insert(self.cursor, ch);
        self.input = chars.into_iter().collect();
        self.cursor += 1;
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut chars = self.input_chars();
        chars.remove(self.cursor - 1);
        self.input = chars.into_iter().collect();
        self.cursor -= 1;
    }

    pub(super) fn delete(&mut self) {
        let mut chars = self.input_chars();
        if self.cursor >= chars.len() {
            return;
        }

        chars.remove(self.cursor);
        self.input = chars.into_iter().collect();
    }

    pub(crate) fn input_view(&self, width: u16) -> (String, u16) {
        let available = width.saturating_sub(2) as usize;
        let chars = self.input_chars();
        let start = self.cursor.saturating_sub(available);
        let visible: String = chars.iter().skip(start).take(available).collect();
        let cursor = self.cursor.saturating_sub(start) as u16;
        (visible, cursor)
    }
}
