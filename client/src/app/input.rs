use super::{App, FocusArea, StreamEvent};
use crate::app::helpers::rect_contains_point;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use mirage_core::VeniceAgent;
use tokio::sync::mpsc;

impl App {
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        agent: VeniceAgent,
        tx: mpsc::UnboundedSender<StreamEvent>,
    ) {
        if matches!(key.code, KeyCode::Char('g')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.toggle_selection_mode();
            return;
        }

        if self.selection_mode {
            if matches!(key.code, KeyCode::Esc) {
                self.set_selection_mode(false);
            }
            return;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Tab => {
                self.toggle_focus();
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                self.process_enter(agent, tx);
            }
            KeyCode::Char(' ') if matches!(self.focus, FocusArea::Transcript) => {
                self.toggle_selected_subagent_group();
            }
            KeyCode::Char('y') if matches!(self.focus, FocusArea::Transcript) => {
                self.copy_selected_transcript_item();
            }
            KeyCode::Char('Y') if matches!(self.focus, FocusArea::Transcript) => {
                self.copy_full_transcript();
            }
            KeyCode::Up if matches!(self.focus, FocusArea::Transcript) => {
                self.select_previous_transcript_item();
            }
            KeyCode::Down if matches!(self.focus, FocusArea::Transcript) => {
                self.select_next_transcript_item();
            }
            KeyCode::PageUp if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_page_up();
            }
            KeyCode::PageDown if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_page_down();
            }
            KeyCode::Home if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_to_top();
            }
            KeyCode::End if matches!(self.focus, FocusArea::Transcript) => {
                self.scroll_transcript_to_bottom();
            }
            KeyCode::Left if matches!(self.focus, FocusArea::Transcript) => {
                self.collapse_selected_subagent_group();
            }
            KeyCode::Right if matches!(self.focus, FocusArea::Transcript) => {
                self.expand_selected_subagent_group();
            }
            KeyCode::Backspace if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.backspace();
            }
            KeyCode::Delete if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.delete();
            }
            KeyCode::Left if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            KeyCode::Right if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.cursor = (self.cursor + 1).min(self.input_chars().len());
            }
            KeyCode::Home if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.cursor = 0;
            }
            KeyCode::End if matches!(self.focus, FocusArea::Composer) && !self.streaming => {
                self.cursor = self.input_chars().len();
            }
            KeyCode::Char(ch)
                if matches!(self.focus, FocusArea::Composer)
                    && !self.streaming
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.insert_char(ch);
            }
            _ => {}
        }
    }

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
