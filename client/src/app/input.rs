use super::{App, FocusArea};
use crate::app::helpers::rect_contains_point;
use crate::backend::ClientBackend;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

impl App {
    /// Handles a keyboard event according to the current focus and selection mode.
    pub(crate) fn handle_key(&mut self, key: KeyEvent, backend: &mut ClientBackend) -> bool {
        if matches!(key.code, KeyCode::Char('g')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.toggle_selection_mode();
            return false;
        }

        if self.selection_mode {
            if matches!(key.code, KeyCode::Esc) {
                self.set_selection_mode(false);
            }
            return false;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                false
            }
            KeyCode::Tab => {
                self.toggle_focus();
                false
            }
            KeyCode::Esc => {
                self.should_quit = true;
                false
            }
            KeyCode::Enter => self.process_enter(backend),
            KeyCode::Char(' ') if matches!(self.focus, FocusArea::Transcript) => {
                self.toggle_selected_subagent_group();
                false
            }
            KeyCode::Char('y') if matches!(self.focus, FocusArea::Transcript) => {
                self.copy_selected_transcript_item();
                false
            }
            KeyCode::Char('Y') if matches!(self.focus, FocusArea::Transcript) => {
                self.copy_full_transcript();
                false
            }
            KeyCode::Up if matches!(self.focus, FocusArea::Transcript) => {
                self.select_previous_transcript_item();
                false
            }
            KeyCode::Down if matches!(self.focus, FocusArea::Transcript) => {
                self.select_next_transcript_item();
                false
            }
            KeyCode::PageUp if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_page_up();
                false
            }
            KeyCode::PageDown if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_page_down();
                false
            }
            KeyCode::Home if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_to_top();
                false
            }
            KeyCode::End if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_to_bottom();
                false
            }
            KeyCode::Left if matches!(self.focus, FocusArea::Transcript) => {
                self.collapse_selected_subagent_group();
                false
            }
            KeyCode::Right if matches!(self.focus, FocusArea::Transcript) => {
                self.expand_selected_subagent_group();
                false
            }
            KeyCode::Backspace
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.backspace();
                false
            }
            KeyCode::Delete
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.delete();
                false
            }
            KeyCode::Left
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.cursor = self.cursor.saturating_sub(1);
                false
            }
            KeyCode::Right
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.cursor = (self.cursor + 1).min(self.input_chars().len());
                false
            }
            KeyCode::Home
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.cursor = 0;
                false
            }
            KeyCode::End
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming =>
            {
                self.cursor = self.input_chars().len();
                false
            }
            KeyCode::Char(ch)
                if matches!(self.focus, FocusArea::Composer)
                    && !self.service.session().streaming
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.insert_char(ch);
                false
            }
            _ => false,
        }
    }

    /// Handles transcript-related mouse input such as wheel scrolling.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !rect_contains_point(self.last_transcript_area, mouse.column, mouse.row) {
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_transcript_up(3),
            MouseEventKind::ScrollDown => self.scroll_transcript_down(3),
            _ => {}
        }
    }
}
