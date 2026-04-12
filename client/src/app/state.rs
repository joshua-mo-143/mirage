use super::{App, FocusArea, TranscriptScrollMode};
use crate::app::helpers::copy_text_to_clipboard;
#[cfg(test)]
use crate::args::Args;
use mirage_core::{
    session::{TranscriptEntry, TranscriptItem},
    tools::cursor_session::CursorSessionStore,
};
#[cfg(test)]
use mirage_service::ServiceConfig;
use mirage_service::SessionService;
use std::sync::Arc;

impl App {
    #[cfg(test)]
    /// Creates a test-oriented app instance directly from command-line arguments.
    pub(crate) fn new(args: &Args, cursor_sessions: Arc<CursorSessionStore>) -> Self {
        let service = SessionService::new(
            ServiceConfig {
                model: args.model.clone(),
                max_turns: args.max_turns,
                authority: args.authority.clone(),
                base_path: args.base_path.clone(),
                uncensored: args.uncensored,
                system_prompt_configured: args.system_prompt.is_some(),
            },
            args.system_prompt.as_deref(),
        );
        Self::from_service(
            service,
            args.prompt.clone().unwrap_or_default(),
            cursor_sessions,
            "local".to_owned(),
        )
    }

    /// Creates an app instance from an existing session service and initial UI state.
    pub(crate) fn from_service(
        service: SessionService,
        input: String,
        cursor_sessions: Arc<CursorSessionStore>,
        backend_description: String,
    ) -> Self {
        let cursor = input.chars().count();
        let selected_transcript = service.session().transcript.len().saturating_sub(1);

        Self {
            service,
            backend_description,
            input,
            cursor,
            should_quit: false,
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

    /// Returns whether the current UI state can submit the composer contents.
    pub(crate) fn can_submit(&self) -> bool {
        matches!(self.focus, FocusArea::Composer) && self.service.can_submit(&self.input)
    }

    /// Appends a session entry and updates transcript-follow behavior if needed.
    pub(super) fn push_session_entry(&mut self, entry: TranscriptEntry) {
        self.service.session_mut().push_entry(entry);
        self.follow_transcript_tail_if_composing();
    }

    /// Keeps the transcript selection pinned to the tail while the composer owns focus.
    pub(super) fn follow_transcript_tail_if_composing(&mut self) {
        if matches!(self.focus, FocusArea::Composer) {
            self.selected_transcript = self.service.session().transcript.len().saturating_sub(1);
            self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
        }
    }

    /// Clamps the selected transcript index so it remains within bounds.
    pub(super) fn clamp_selected_transcript(&mut self) {
        self.selected_transcript = self
            .selected_transcript
            .min(self.service.session().transcript.len().saturating_sub(1));
    }

    /// Toggles keyboard focus between the composer and transcript.
    pub(super) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusArea::Composer => FocusArea::Transcript,
            FocusArea::Transcript => FocusArea::Composer,
        };
        self.follow_transcript_tail_if_composing();
    }

    /// Enables or disables native terminal text selection mode.
    pub(super) fn set_selection_mode(&mut self, enabled: bool) {
        self.selection_mode = enabled;
        self.service.session_mut().status = if enabled {
            "Selection mode enabled. Drag with the mouse to select text; press Ctrl+G or Esc to return."
                .to_owned()
        } else {
            "Selection mode disabled. Mouse interactions restored to Mirage.".to_owned()
        };
    }

    /// Toggles native terminal text selection mode.
    pub(super) fn toggle_selection_mode(&mut self) {
        self.set_selection_mode(!self.selection_mode);
    }

    /// Moves transcript selection to the previous top-level item.
    pub(super) fn select_previous_transcript_item(&mut self) {
        self.selected_transcript = self.selected_transcript.saturating_sub(1);
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    /// Moves transcript selection to the next top-level item.
    pub(super) fn select_next_transcript_item(&mut self) {
        self.selected_transcript = (self.selected_transcript + 1)
            .min(self.service.session().transcript.len().saturating_sub(1));
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    /// Toggles expansion on the currently selected subagent group.
    pub(super) fn toggle_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) = self
            .service
            .session_mut()
            .transcript
            .get_mut(self.selected_transcript)
        {
            group.expanded = !group.expanded;
        }
    }

    /// Expands the currently selected subagent group if one is selected.
    pub(super) fn expand_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) = self
            .service
            .session_mut()
            .transcript
            .get_mut(self.selected_transcript)
        {
            group.expanded = true;
        }
    }

    /// Collapses the currently selected subagent group if one is selected.
    pub(super) fn collapse_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) = self
            .service
            .session_mut()
            .transcript
            .get_mut(self.selected_transcript)
        {
            group.expanded = false;
        }
    }

    /// Returns the plain-text representation of the selected transcript item.
    pub(super) fn selected_transcript_text(&self) -> Option<String> {
        self.service
            .session()
            .transcript_text(self.selected_transcript)
    }

    /// Returns the plain-text representation of the full transcript.
    pub(super) fn full_transcript_text(&self) -> String {
        self.service.session().full_transcript_text()
    }

    /// Copies the selected transcript item into the system clipboard.
    pub(super) fn copy_selected_transcript_item(&mut self) {
        let Some(text) = self.selected_transcript_text() else {
            self.service.session_mut().status = "Nothing to copy.".to_owned();
            return;
        };

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.service.session_mut().status =
                    "Copied selected transcript item to clipboard.".to_owned();
            }
            Err(error) => {
                self.service.session_mut().status = format!("Clipboard copy failed: {error}");
            }
        }
    }

    /// Copies the entire transcript into the system clipboard.
    pub(super) fn copy_full_transcript(&mut self) {
        let text = self.full_transcript_text();
        if text.trim().is_empty() {
            self.service.session_mut().status = "Nothing to copy.".to_owned();
            return;
        }

        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.service.session_mut().status =
                    "Copied full transcript to clipboard.".to_owned();
            }
            Err(error) => {
                self.service.session_mut().status = format!("Clipboard copy failed: {error}");
            }
        }
    }

    /// Scrolls the transcript viewport upward by the requested number of lines.
    pub(super) fn scroll_transcript_up(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current.saturating_sub(lines);
    }

    /// Scrolls the transcript viewport downward by the requested number of lines.
    pub(super) fn scroll_transcript_down(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current
            .saturating_add(lines)
            .min(self.last_transcript_max_scroll);
    }

    /// Scrolls the transcript viewport upward by roughly one page.
    pub(super) fn scroll_transcript_page_up(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_up(page);
    }

    /// Scrolls the transcript viewport downward by roughly one page.
    pub(super) fn scroll_transcript_page_down(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_down(page);
    }

    /// Moves the transcript viewport to the top.
    pub(super) fn scroll_transcript_to_top(&mut self) {
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = 0;
    }

    /// Moves the transcript viewport back to tail-follow mode.
    pub(super) fn scroll_transcript_to_bottom(&mut self) {
        self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
    }

    /// Returns the effective transcript scroll offset for the current scroll mode.
    fn current_transcript_scroll(&self) -> u16 {
        match self.transcript_scroll_mode {
            TranscriptScrollMode::FollowTail => self.last_transcript_max_scroll,
            TranscriptScrollMode::FollowSelection | TranscriptScrollMode::Manual => {
                self.last_transcript_scroll
            }
        }
    }

    /// Returns the current composer contents as a character vector for editing operations.
    pub(super) fn input_chars(&self) -> Vec<char> {
        self.input.chars().collect()
    }

    /// Inserts a character at the current composer cursor position.
    pub(super) fn insert_char(&mut self, ch: char) {
        let mut chars = self.input_chars();
        chars.insert(self.cursor, ch);
        self.input = chars.into_iter().collect();
        self.cursor += 1;
    }

    /// Deletes the character immediately before the composer cursor.
    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut chars = self.input_chars();
        chars.remove(self.cursor - 1);
        self.input = chars.into_iter().collect();
        self.cursor -= 1;
    }

    /// Deletes the character at the current composer cursor position.
    pub(super) fn delete(&mut self) {
        let mut chars = self.input_chars();
        if self.cursor >= chars.len() {
            return;
        }

        chars.remove(self.cursor);
        self.input = chars.into_iter().collect();
    }

    /// Returns the visible composer substring and cursor offset for the available width.
    pub(crate) fn input_view(&self, width: u16) -> (String, u16) {
        let available = width.saturating_sub(2) as usize;
        let chars = self.input_chars();
        let start = self.cursor.saturating_sub(available);
        let visible: String = chars.iter().skip(start).take(available).collect();
        let cursor = self.cursor.saturating_sub(start) as u16;
        (visible, cursor)
    }
}
