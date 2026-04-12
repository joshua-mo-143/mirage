use mirage_core::{agent::FinalResponse, completion::Usage, message::Message};
use ratatui::layout::Rect;
use std::{collections::HashMap, sync::Arc};

use crate::{args::Args, tools::cursor_session::CursorSessionStore, transcript::TranscriptItem};

mod commands;
mod events;
mod helpers;
mod input;
mod state;

#[cfg(test)]
mod tests;

pub(crate) use helpers::summarize_tool_call;

struct PendingSubagent {
    transcript_index: usize,
    pending_entry_index: Option<usize>,
    tool_entry_index: Option<usize>,
    tool_call_count: usize,
    pending_tool_calls: usize,
    latest_tool_description: String,
}

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

pub(crate) enum StreamEvent {
    AssistantText(String),
    ToolCall {
        id: String,
        name: String,
        summary: String,
    },
    ToolResult {
        id: String,
    },
    Final(FinalResponse),
    Error(String),
}

struct PendingToolCall {
    transcript_index: usize,
}

struct ToolAggregate {
    name: String,
    label: String,
    latest_detail: String,
    total_calls: usize,
    pending_calls: usize,
}

pub(crate) struct App {
    pub(crate) transcript: Vec<TranscriptItem>,
    input: String,
    cursor: usize,
    history: Vec<Message>,
    pub(crate) status: String,
    pub(crate) usage: Option<Usage>,
    pending_assistant: Option<usize>,
    pending_tool_calls: HashMap<String, PendingToolCall>,
    active_tool_aggregates: HashMap<usize, ToolAggregate>,
    pending_subagents: HashMap<String, PendingSubagent>,
    pub(crate) streaming: bool,
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
