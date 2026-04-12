use super::{App, Args, FocusArea, TranscriptScrollMode};
use crate::{app::helpers::copy_text_to_clipboard, tools::cursor_session::CursorSessionStore};
use mirage_core::session::{TranscriptEntry, TranscriptItem};
use std::sync::Arc;

impl App {
    pub(crate) fn new(args: &Args, cursor_sessions: Arc<CursorSessionStore>) -> Self {
        let session = mirage_core::session::Session::new(args.system_prompt.as_deref());
        let input = args.prompt.clone().unwrap_or_default();
        let cursor = input.chars().count();
        let selected_transcript = session.transcript.len().saturating_sub(1);

        Self {
            session,
            input,
            cursor,
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
            && !self.session.streaming
            && !self.input.trim().is_empty()
    }

    pub(super) fn push_session_entry(&mut self, entry: TranscriptEntry) {
        self.session.push_entry(entry);
        self.follow_transcript_tail_if_composing();
    }

    pub(super) fn follow_transcript_tail_if_composing(&mut self) {
        if matches!(self.focus, FocusArea::Composer) {
            self.selected_transcript = self.session.transcript.len().saturating_sub(1);
            self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
        }
    }

    pub(super) fn clamp_selected_transcript(&mut self) {
        self.selected_transcript = self
            .selected_transcript
            .min(self.session.transcript.len().saturating_sub(1));
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
        self.session.status = if enabled {
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
            (self.selected_transcript + 1).min(self.session.transcript.len().saturating_sub(1));
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    pub(super) fn toggle_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.session.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = !group.expanded;
        }
    }

    pub(super) fn expand_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.session.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = true;
        }
    }

    pub(super) fn collapse_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.session.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = false;
        }
    }

    pub(super) fn selected_transcript_text(&self) -> Option<String> {
        self.session.transcript_text(self.selected_transcript)
    }

    pub(super) fn full_transcript_text(&self) -> String {
        self.session.full_transcript_text()
    }

    pub(super) fn copy_selected_transcript_item(&mut self) {
        let Some(text) = self.selected_transcript_text() else {
            self.session.status = "Nothing to copy.".to_owned();
            return;
        };

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.session.status = "Copied selected transcript item to clipboard.".to_owned();
            }
            Err(error) => {
                self.session.status = format!("Clipboard copy failed: {error}");
            }
        }
    }

    pub(super) fn copy_full_transcript(&mut self) {
        let text = self.full_transcript_text();
        if text.trim().is_empty() {
            self.session.status = "Nothing to copy.".to_owned();
            return;
        }

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.session.status = "Copied full transcript to clipboard.".to_owned();
            }
            Err(error) => {
                self.session.status = format!("Clipboard copy failed: {error}");
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
