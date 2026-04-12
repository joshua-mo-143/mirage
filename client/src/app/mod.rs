use mirage_core::session::Session;
use ratatui::layout::Rect;
use std::sync::Arc;

use crate::{args::Args, tools::cursor_session::CursorSessionStore};

mod commands;
mod events;
mod helpers;
mod input;
mod state;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusArea {
    Composer,
    Transcript,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptScrollMode {
    FollowTail,
    FollowSelection,
    Manual,
}

pub(crate) struct App {
    pub(crate) session: Session,
    input: String,
    cursor: usize,
    pub(crate) should_quit: bool,
    pub(crate) model: String,
    max_turns: usize,
    authority: String,
    base_path: String,
    system_prompt_configured: bool,
    pub(crate) uncensored: bool,
    pub(crate) selection_mode: bool,
    pub(crate) focus: FocusArea,
    pub(crate) selected_transcript: usize,
    pub(crate) transcript_scroll: u16,
    pub(crate) transcript_scroll_mode: TranscriptScrollMode,
    pub(crate) last_transcript_scroll: u16,
    pub(crate) last_transcript_max_scroll: u16,
    pub(crate) last_transcript_page_height: u16,
    pub(crate) last_transcript_area: Rect,
    cursor_sessions: Arc<CursorSessionStore>,
}
