pub mod tools;

use arboard::Clipboard;
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use mirage_core::{
    VeniceAgent, VeniceClient, VeniceConfig,
    agent::{FinalResponse, MultiTurnStreamItem, Text},
    completion::Usage,
    message::Message,
    streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text as TuiText},
    widgets::{Paragraph, Wrap},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    io::{self, Stdout},
    sync::Arc,
};
use tokio::sync::mpsc;

use crate::tools::{
    bash_tool::BashTool,
    cursor_session::CursorSessionStore,
    file_tools::{EditFileTool, ReadFileTool, WriteFileTool},
    prompt_cursor_tool::PromptCursorTool,
    subagent_tool::{SubagentProgressEvent, SubagentTool},
};

#[derive(Debug, Parser)]
#[command(about = "Interactive Venice chat with a Cursor-style terminal UI")]
struct Args {
    /// Optional initial user prompt to send immediately on startup.
    prompt: Option<String>,

    /// Model name to request from the Venice API.
    #[arg(
        long,
        env = "VENICE_MODEL",
        default_value = "arcee-trinity-large-thinking"
    )]
    model: String,

    /// Optional system prompt prepended to the chat history.
    #[arg(long, env = "VENICE_SYSTEM_PROMPT")]
    system_prompt: Option<String>,

    /// Optional sampling temperature.
    #[arg(long)]
    temperature: Option<f32>,

    /// Optional response token cap.
    #[arg(long)]
    max_completion_tokens: Option<u32>,

    /// Enable Venice's built-in uncensoring system prompt (note: this will use more tokens!).
    #[arg(long, default_value_t = false)]
    uncensored: bool,

    /// Maximum Rig multi-turn depth so tool calls can continue before final text.
    #[arg(long, default_value_t = 100)]
    max_turns: usize,

    /// Override the API authority for testing.
    #[arg(long, default_value = "api.venice.ai")]
    authority: String,

    /// Override the API base path for testing.
    #[arg(long, default_value = "/api/v1")]
    base_path: String,
}

#[derive(Clone, Copy)]
enum TranscriptKind {
    Meta,
    User,
    Assistant,
    Tool,
    Error,
}

struct TranscriptEntry {
    kind: TranscriptKind,
    title: String,
    body: String,
}

impl TranscriptEntry {
    fn meta(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Meta,
            title: title.into(),
            body: body.into(),
        }
    }

    fn user(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::User,
            title: "You".to_owned(),
            body: body.into(),
        }
    }

    fn assistant(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Assistant,
            title: "Assistant".to_owned(),
            body: body.into(),
        }
    }

    fn tool(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Tool,
            title: title.into(),
            body: body.into(),
        }
    }

    fn error(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Error,
            title: "Error".to_owned(),
            body: body.into(),
        }
    }

    fn title_style(&self) -> Style {
        match self.kind {
            TranscriptKind::Meta => Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            TranscriptKind::User => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            TranscriptKind::Assistant => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            TranscriptKind::Tool => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            TranscriptKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }

    fn body_style(&self) -> Style {
        match self.kind {
            TranscriptKind::Meta => Style::default().fg(Color::Gray),
            TranscriptKind::Error => Style::default().fg(Color::Red),
            _ => Style::default(),
        }
    }

    fn to_plaintext(&self, title_indent: &str, body_indent: &str) -> String {
        let mut lines = vec![format!("{title_indent}{}", self.title)];
        if self.body.is_empty() {
            return lines.join("\n");
        }

        for line in self.body.lines() {
            lines.push(format!("{body_indent}{line}"));
        }
        lines.join("\n")
    }
}

enum TranscriptItem {
    Entry(TranscriptEntry),
    SubagentGroup(SubagentGroup),
}

impl TranscriptItem {
    fn entry_mut(&mut self) -> Option<&mut TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    fn entry(&self) -> Option<&TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    fn to_plaintext(&self) -> String {
        match self {
            Self::Entry(entry) => entry.to_plaintext("", "  "),
            Self::SubagentGroup(group) => group.to_plaintext(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubagentStatus {
    Running,
    Complete,
    Failed,
}

struct SubagentGroup {
    summary: String,
    status: SubagentStatus,
    expanded: bool,
    entries: Vec<TranscriptEntry>,
}

impl SubagentGroup {
    fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            status: SubagentStatus::Running,
            expanded: false,
            entries: Vec::new(),
        }
    }

    fn to_plaintext(&self) -> String {
        let mut parts = vec![subagent_group_title(self)];
        for entry in &self.entries {
            parts.push(entry.to_plaintext("  ", "    "));
        }
        parts.join("\n")
    }
}

struct PendingSubagent {
    transcript_index: usize,
    pending_entry_index: Option<usize>,
    tool_entry_index: Option<usize>,
    tool_call_count: usize,
    pending_tool_calls: usize,
    latest_tool_description: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    Composer,
    Transcript,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TranscriptScrollMode {
    FollowTail,
    FollowSelection,
    Manual,
}

enum StreamEvent {
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

struct App {
    transcript: Vec<TranscriptItem>,
    input: String,
    cursor: usize,
    history: Vec<Message>,
    status: String,
    usage: Option<Usage>,
    pending_assistant: Option<usize>,
    pending_tool_calls: HashMap<String, PendingToolCall>,
    active_tool_aggregates: HashMap<usize, ToolAggregate>,
    pending_subagents: HashMap<String, PendingSubagent>,
    streaming: bool,
    should_quit: bool,
    model: String,
    max_turns: usize,
    authority: String,
    base_path: String,
    system_prompt_configured: bool,
    uncensored: bool,
    selection_mode: bool,
    focus: FocusArea,
    selected_transcript: usize,
    transcript_scroll: u16,
    transcript_scroll_mode: TranscriptScrollMode,
    last_transcript_scroll: u16,
    last_transcript_max_scroll: u16,
    last_transcript_page_height: u16,
    last_transcript_area: Rect,
    cursor_sessions: Arc<CursorSessionStore>,
}

impl App {
    fn new(args: &Args, cursor_sessions: Arc<CursorSessionStore>) -> Self {
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
            pending_tool_calls: HashMap::new(),
            active_tool_aggregates: HashMap::new(),
            pending_subagents: HashMap::new(),
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
            last_transcript_area: Rect::default(),
            cursor_sessions,
        }
    }

    fn can_submit(&self) -> bool {
        matches!(self.focus, FocusArea::Composer)
            && !self.streaming
            && !self.input.trim().is_empty()
    }

    fn push_transcript_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(TranscriptItem::Entry(entry));
        self.follow_transcript_tail_if_composing();
    }

    fn push_subagent_group(&mut self, id: String, summary: String) {
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

    fn clamp_selected_transcript(&mut self) {
        self.selected_transcript = self
            .selected_transcript
            .min(self.transcript.len().saturating_sub(1));
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusArea::Composer => FocusArea::Transcript,
            FocusArea::Transcript => FocusArea::Composer,
        };
        self.follow_transcript_tail_if_composing();
    }

    fn set_selection_mode(&mut self, enabled: bool) {
        self.selection_mode = enabled;
        self.status = if enabled {
            "Selection mode enabled. Drag with the mouse to select text; press Ctrl+G or Esc to return."
                .to_owned()
        } else {
            "Selection mode disabled. Mouse interactions restored to Mirage.".to_owned()
        };
    }

    fn toggle_selection_mode(&mut self) {
        self.set_selection_mode(!self.selection_mode);
    }

    fn select_previous_transcript_item(&mut self) {
        self.selected_transcript = self.selected_transcript.saturating_sub(1);
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    fn select_next_transcript_item(&mut self) {
        self.selected_transcript =
            (self.selected_transcript + 1).min(self.transcript.len().saturating_sub(1));
        self.transcript_scroll_mode = TranscriptScrollMode::FollowSelection;
    }

    fn toggle_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = !group.expanded;
        }
    }

    fn expand_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = true;
        }
    }

    fn collapse_selected_subagent_group(&mut self) {
        if let Some(TranscriptItem::SubagentGroup(group)) =
            self.transcript.get_mut(self.selected_transcript)
        {
            group.expanded = false;
        }
    }

    fn update_subagent_group<R>(
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

    fn selected_transcript_index(&self) -> Option<usize> {
        if self.transcript.is_empty() {
            None
        } else {
            Some(self.selected_transcript)
        }
    }

    fn selected_transcript_text(&self) -> Option<String> {
        self.transcript
            .get(self.selected_transcript)
            .map(TranscriptItem::to_plaintext)
    }

    fn full_transcript_text(&self) -> String {
        self.transcript
            .iter()
            .map(TranscriptItem::to_plaintext)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn copy_selected_transcript_item(&mut self) {
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

    fn copy_full_transcript(&mut self) {
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

    fn scroll_transcript_up(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current.saturating_sub(lines);
    }

    fn scroll_transcript_down(&mut self, lines: u16) {
        let current = self.current_transcript_scroll();
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = current
            .saturating_add(lines)
            .min(self.last_transcript_max_scroll);
    }

    fn scroll_transcript_page_up(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_up(page);
    }

    fn scroll_transcript_page_down(&mut self) {
        let page = self.last_transcript_page_height.saturating_sub(1).max(1);
        self.scroll_transcript_down(page);
    }

    fn scroll_transcript_to_top(&mut self) {
        self.transcript_scroll_mode = TranscriptScrollMode::Manual;
        self.transcript_scroll = 0;
    }

    fn scroll_transcript_to_bottom(&mut self) {
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

    fn clear_active_tool_aggregates(&mut self) {
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

    fn record_tool_call(&mut self, id: String, name: String, summary: String) {
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

    fn record_tool_result(&mut self, id: String) {
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

    fn update_subagent_tool_title(group: &mut SubagentGroup, pending: &mut PendingSubagent) {
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

    fn process_enter(&mut self, agent: VeniceAgent, tx: mpsc::UnboundedSender<StreamEvent>) {
        if matches!(self.focus, FocusArea::Transcript) {
            self.toggle_selected_subagent_group();
            return;
        }

        if !self.can_submit() {
            return;
        }

        let input = self.input.trim().to_owned();
        self.input.clear();
        self.cursor = 0;

        if input.starts_with('/') {
            self.handle_command(&input);
        } else {
            self.submit_prompt(agent, tx, input);
        }
    }

    fn submit_prompt(
        &mut self,
        agent: VeniceAgent,
        tx: mpsc::UnboundedSender<StreamEvent>,
        prompt: String,
    ) {
        let history = self.history.clone();
        self.clear_active_tool_aggregates();

        self.push_transcript_entry(TranscriptEntry::user(prompt.clone()));
        self.pending_assistant = None;
        self.streaming = true;
        self.status = "Streaming response...".to_owned();

        let max_turns = self.max_turns;
        tokio::spawn(async move {
            stream_agent_response(agent, prompt, history, max_turns, tx).await;
        });
    }

    fn handle_command(&mut self, command: &str) {
        match command {
            "/help" => {
                self.push_transcript_entry(TranscriptEntry::meta(
                    "Commands",
                    "/help\n/status\n/clear\n/quit\n\nNavigation:\n- Ctrl+G toggles selection mode for native terminal drag-to-select\n- Tab toggles composer/transcript focus\n- Up/Down moves transcript selection\n- PageUp/PageDown scroll the transcript\n- Home/End jump to the top or bottom\n- Left/Right collapses or expands a selected subagent\n- Enter/Space toggles a selected subagent\n- y copies the selected transcript item\n- Y copies the full transcript\n\nBuilt-in tools:\n- bash(command, cwd?)\n- prompt_cursor(prompt, cwd?)\n- subagent(prompt, cwd?, model?, mode?)\n- read_file(path, start_line?, line_count?)\n- edit_file(path, old_text, new_text, replace_all?)\n- write_file(path, content, append?, overwrite_existing?, create_parent_directories?)",
                ));
            }
            "/status" => {
                self.push_transcript_entry(TranscriptEntry::meta(
                    "Status",
                    format!(
                        "model: {}\nauthority: {}\nbase path: {}\nmax turns: {}\nvenice system prompt: {}\nuser system prompt: {}\nhistory messages: {}\ncursor sessions: {}\nselection mode: {}\nfocus: {}",
                        self.model,
                        self.authority,
                        self.base_path,
                        self.max_turns,
                        if self.uncensored { "enabled" } else { "disabled" },
                        if self.system_prompt_configured { "configured" } else { "unset" },
                        self.history.len(),
                        self.cursor_sessions.len(),
                        if self.selection_mode { "enabled" } else { "disabled" },
                        match self.focus {
                            FocusArea::Composer => "composer",
                            FocusArea::Transcript => "transcript",
                        }
                    ),
                ));
            }
            "/clear" => {
                self.history.clear();
                self.usage = None;
                self.pending_assistant = None;
                self.clear_active_tool_aggregates();
                self.pending_subagents.clear();
                self.cursor_sessions.clear();
                self.transcript.clear();
                self.transcript
                    .push(TranscriptItem::Entry(TranscriptEntry::meta(
                        "Mirage",
                        "Conversation cleared, including Cursor session state.",
                    )));
                self.focus = FocusArea::Composer;
                self.selection_mode = false;
                self.selected_transcript = 0;
                self.transcript_scroll = 0;
                self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
                self.last_transcript_scroll = 0;
                self.last_transcript_max_scroll = 0;
                self.last_transcript_page_height = 0;
                self.status = "Cleared conversation history and Cursor session state.".to_owned();
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            other => {
                self.push_transcript_entry(TranscriptEntry::error(format!(
                    "Unknown command: {other}"
                )));
            }
        }
    }

    fn apply_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::AssistantText(text) => {
                if self.pending_assistant.is_none() && text.trim().is_empty() {
                    return;
                }
                if let Some(index) = self.pending_assistant {
                    if let Some(entry) = self.transcript.get_mut(index) {
                        if let Some(entry) = entry.entry_mut() {
                            entry.body.push_str(&text);
                        }
                    }
                } else {
                    self.push_transcript_entry(TranscriptEntry::assistant(text));
                    self.pending_assistant = Some(self.transcript.len() - 1);
                }
            }
            StreamEvent::ToolCall { id, name, summary } => {
                self.pending_assistant = None;
                self.record_tool_call(id, name, summary);
            }
            StreamEvent::ToolResult { id } => {
                self.pending_assistant = None;
                self.record_tool_result(id);
            }
            StreamEvent::Final(final_response) => {
                if self.pending_assistant.is_none() && !final_response.response().is_empty() {
                    self.push_transcript_entry(TranscriptEntry::assistant(
                        final_response.response().to_owned(),
                    ));
                }
                self.pending_assistant = None;

                if let Some(history) = final_response.history() {
                    self.history = history.to_vec();
                }

                let usage = final_response.usage();
                self.usage = Some(usage);
                self.clear_active_tool_aggregates();
                self.streaming = false;
                self.status = format!(
                    "Ready. Last response used {} input / {} output tokens.",
                    usage.input_tokens, usage.output_tokens
                );
            }
            StreamEvent::Error(error) => {
                if let Some(index) = self.pending_assistant.take()
                    && self
                        .transcript
                        .get(index)
                        .and_then(TranscriptItem::entry)
                        .is_some_and(|entry| entry.body.is_empty())
                {
                    self.transcript.remove(index);
                    self.clamp_selected_transcript();
                }
                self.clear_active_tool_aggregates();
                self.pending_subagents.clear();
                self.streaming = false;
                self.status = "Last request failed.".to_owned();
                self.push_transcript_entry(TranscriptEntry::error(error));
            }
        }
    }

    fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        match event {
            SubagentProgressEvent::Started { id, summary } => {
                self.push_subagent_group(id, summary.clone());
                self.status = format!("Subagent running: {}", truncate_text(&summary, 80));
            }
            SubagentProgressEvent::AssistantDelta { id, text } => {
                self.status = "Streaming subagent output...".to_owned();
                let _ = self.update_subagent_group(&id, |group, pending| {
                    if pending.pending_entry_index.is_none() && text.trim().is_empty() {
                        return;
                    }
                    if let Some(index) = pending.pending_entry_index {
                        if let Some(entry) = group.entries.get_mut(index) {
                            entry.body.push_str(&text);
                            return;
                        }
                    }

                    group.entries.push(TranscriptEntry {
                        kind: TranscriptKind::Assistant,
                        title: "Assistant".to_owned(),
                        body: text,
                    });
                    pending.pending_entry_index = Some(group.entries.len() - 1);
                });
            }
            SubagentProgressEvent::ToolStarted { id, description } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.tool_call_count += 1;
                    pending.pending_tool_calls += 1;
                    pending.latest_tool_description = description;
                    Self::update_subagent_tool_title(group, pending);
                });
            }
            SubagentProgressEvent::ToolCompleted {
                id,
                description,
                output: _,
            } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = pending.pending_tool_calls.saturating_sub(1);
                    if pending.tool_call_count == 0 {
                        pending.tool_call_count = 1;
                    }
                    pending.latest_tool_description = description;
                    Self::update_subagent_tool_title(group, pending);
                });
            }
            SubagentProgressEvent::Finished { id } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = 0;
                    Self::update_subagent_tool_title(group, pending);
                    group.status = SubagentStatus::Complete;
                });
                self.pending_subagents.remove(&id);
                if self.streaming {
                    self.status = "Subagent finished; waiting for parent agent...".to_owned();
                }
            }
            SubagentProgressEvent::Failed { id, error } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = 0;
                    Self::update_subagent_tool_title(group, pending);
                    group.status = SubagentStatus::Failed;
                    group.entries.push(TranscriptEntry::error(error.clone()));
                });
                self.pending_subagents.remove(&id);
            }
        }
    }

    fn handle_key(
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

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !rect_contains_point(self.last_transcript_area, mouse.column, mouse.row) {
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_transcript_up(3),
            MouseEventKind::ScrollDown => self.scroll_transcript_down(3),
            _ => {}
        }
    }

    fn input_chars(&self) -> Vec<char> {
        self.input.chars().collect()
    }

    fn insert_char(&mut self, ch: char) {
        let mut chars = self.input_chars();
        chars.insert(self.cursor, ch);
        self.input = chars.into_iter().collect();
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut chars = self.input_chars();
        chars.remove(self.cursor - 1);
        self.input = chars.into_iter().collect();
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        let mut chars = self.input_chars();
        if self.cursor >= chars.len() {
            return;
        }

        chars.remove(self.cursor);
        self.input = chars.into_iter().collect();
    }

    fn input_view(&self, width: u16) -> (String, u16) {
        let available = width.saturating_sub(2) as usize;
        let chars = self.input_chars();
        let start = self.cursor.saturating_sub(available);
        let visible: String = chars.iter().skip(start).take(available).collect();
        let cursor = self.cursor.saturating_sub(start) as u16;
        (visible, cursor)
    }
}

struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse_capture_enabled: bool,
}

impl Tui {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            mouse_capture_enabled: true,
        })
    }

    fn draw(&mut self, app: &mut App) -> io::Result<()> {
        self.set_mouse_capture(!app.selection_mode)?;
        self.terminal.draw(|frame| render(frame, app))?;
        Ok(())
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if self.mouse_capture_enabled == enabled {
            return Ok(());
        }

        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }

        self.mouse_capture_enabled = enabled;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let config = VeniceConfig::from_env()?
        .with_authority(args.authority.clone())
        .with_base_path(args.base_path.clone());
    let client = VeniceClient::new(config)?;
    let mut agent_builder = client
        .agent(args.model.clone())
        .default_max_turns(args.max_turns);

    agent_builder = agent_builder.additional_params(serde_json::json!({
        "venice_parameters": {
            "include_venice_system_prompt": args.uncensored
        }
    }));

    if let Some(system_prompt) = args.system_prompt.as_deref() {
        agent_builder = agent_builder.preamble(system_prompt);
    }

    agent_builder = agent_builder.append_preamble(
        "Tool usage guidance:
- Prefer discovering capabilities by using `bash` instead of assuming what commands, binaries, files, or directories are available.
- Use `bash` freely for arbitrary shell commands, environment inspection, and capability discovery.
- Use `subagent` when you want to delegate a deeper investigation or planning task to a child Cursor agent and incorporate its final answer.
- Use `read_file` to inspect files before editing them when needed.
- Prefer `edit_file` for modifying part of an existing file.
- Use `write_file` only when creating a new file, replacing an entire file, or appending whole-file content intentionally.
- Use `prompt_cursor` when you want the local Cursor agent CLI (`agent -p`) to answer or inspect something.",
    );

    if let Some(temperature) = args.temperature {
        agent_builder = agent_builder.temperature(f64::from(temperature));
    }

    if let Some(max_tokens) = args.max_completion_tokens {
        agent_builder = agent_builder.max_tokens(u64::from(max_tokens));
    }

    let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
    let cursor_sessions = Arc::new(CursorSessionStore::default());
    let agent = agent_builder
        .tool(BashTool)
        .tool(PromptCursorTool::new(cursor_sessions.clone()))
        .tool(SubagentTool::new(subagent_tx, cursor_sessions.clone()))
        .tool(ReadFileTool)
        .tool(EditFileTool)
        .tool(WriteFileTool)
        .build();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(&args, cursor_sessions);
    let mut tui = Tui::new()?;
    let mut events = EventStream::new();

    if app.can_submit() {
        app.process_enter(agent.clone(), tx.clone());
    }

    while !app.should_quit {
        tui.draw(&mut app)?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind.is_press() => {
                        app.handle_key(key, agent.clone(), tx.clone());
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        app.handle_mouse(mouse);
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        app.apply_stream_event(StreamEvent::Error(format!("terminal event error: {error}")));
                    }
                    None => break,
                }
            }
            maybe_stream = rx.recv() => {
                if let Some(event) = maybe_stream {
                    app.apply_stream_event(event);
                } else {
                    break;
                }
            }
            maybe_subagent = subagent_rx.recv() => {
                if let Some(event) = maybe_subagent {
                    app.apply_subagent_event(event);
                }
            }
        }
    }

    Ok(())
}

async fn stream_agent_response(
    agent: VeniceAgent,
    prompt: String,
    history: Vec<Message>,
    max_turns: usize,
    tx: mpsc::UnboundedSender<StreamEvent>,
) {
    let mut stream = agent
        .stream_prompt(prompt)
        .with_history(history)
        .multi_turn(max_turns)
        .await;

    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                Text { text },
            ))) => StreamEvent::AssistantText(text),
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                let name = tool_call.function.name;
                let summary = summarize_tool_call(&name, &tool_call.function.arguments);
                StreamEvent::ToolCall {
                    id: tool_call.id,
                    name,
                    summary,
                }
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => StreamEvent::ToolResult { id: tool_result.id },
            Ok(MultiTurnStreamItem::FinalResponse(final_response)) => {
                StreamEvent::Final(final_response)
            }
            Ok(_) => continue,
            Err(error) => StreamEvent::Error(error.to_string()),
        };

        let is_terminal = matches!(event, StreamEvent::Final(_) | StreamEvent::Error(_));
        if tx.send(event).is_err() {
            break;
        }
        if is_terminal {
            break;
        }
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = centered_content_area(frame.area());
    let [
        header_area,
        transcript_area,
        divider_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area);

    let status_text = app
        .usage
        .map(|usage| format!("  {} in / {} out", usage.input_tokens, usage.output_tokens))
        .unwrap_or_default();
    let mode_style = if app.streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let header = Paragraph::new(TuiText::from(vec![
        Line::from(vec![
            Span::styled("Mirage", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("  ", Style::default()),
            Span::styled(app.model.clone(), Style::default().fg(Color::Cyan)),
            Span::styled("  ", Style::default()),
            Span::styled(
                if app.streaming { "streaming" } else { "ready" },
                mode_style,
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                if app.uncensored {
                    "uncensored"
                } else {
                    "guarded"
                },
                if app.uncensored {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(status_text),
        ]),
        Line::from(Span::styled(
            format!(
                "{}  Focus: {}  Selection: {}",
                app.status,
                match app.focus {
                    FocusArea::Composer => "composer",
                    FocusArea::Transcript => "transcript",
                },
                if app.selection_mode { "on" } else { "off" }
            ),
            Style::default().fg(Color::Gray),
        )),
    ]));
    frame.render_widget(header, header_area);

    let rendered_transcript = build_transcript_lines(
        &app.transcript,
        matches!(app.focus, FocusArea::Transcript)
            .then(|| app.selected_transcript_index())
            .flatten(),
    );
    app.last_transcript_area = transcript_area;
    let transcript_height = transcript_area.height;
    let transcript_visual_height =
        wrapped_line_count(&rendered_transcript.lines, transcript_area.width);
    let transcript_max_scroll = transcript_visual_height.saturating_sub(transcript_height);
    let selection_scroll = rendered_transcript
        .selected_line_index
        .map(|line_index| {
            let selected_visual_start = wrapped_line_count(
                &rendered_transcript.lines[..line_index],
                transcript_area.width,
            );
            selected_visual_start.saturating_sub(1)
        })
        .unwrap_or(0);
    let transcript_scroll = match app.transcript_scroll_mode {
        TranscriptScrollMode::FollowTail => transcript_max_scroll,
        TranscriptScrollMode::FollowSelection => selection_scroll.min(transcript_max_scroll),
        TranscriptScrollMode::Manual => app.transcript_scroll.min(transcript_max_scroll),
    };
    app.last_transcript_scroll = transcript_scroll;
    app.last_transcript_max_scroll = transcript_max_scroll;
    app.last_transcript_page_height = transcript_height;
    let transcript = Paragraph::new(TuiText::from(rendered_transcript.lines))
        .wrap(Wrap { trim: false })
        .scroll((transcript_scroll, 0));
    frame.render_widget(transcript, transcript_area);

    let divider = Paragraph::new(Line::from(Span::styled(
        "─".repeat(divider_area.width as usize),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(divider, divider_area);

    let composer_prompt = if app.streaming { "… " } else { "> " };
    let prompt_width = composer_prompt.chars().count() as u16;
    let composer_width = composer_area.width.saturating_sub(prompt_width);
    let (visible_input, cursor_offset) = app.input_view(composer_width);
    let composer_text = if visible_input.is_empty() && !app.streaming {
        Line::from(vec![
            Span::styled(
                composer_prompt,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Message Mirage...", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                composer_prompt,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(visible_input),
        ])
    };
    let composer = Paragraph::new(composer_text);
    frame.render_widget(composer, composer_area);

    let footer = Paragraph::new(Line::from(Span::styled(
        if app.selection_mode {
            "Selection mode: drag to select text, use terminal copy, Ctrl+G or Esc returns"
        } else {
            "Ctrl+G selection mode, Tab focus, PageUp/PageDown scroll, y copy item, Y copy all, Esc quits"
        },
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(footer, footer_area);

    if !app.streaming && matches!(app.focus, FocusArea::Composer) {
        let cursor_x = composer_area.x + prompt_width + cursor_offset;
        let cursor_y = composer_area.y;
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

fn centered_content_area(area: Rect) -> Rect {
    let horizontal_margin = if area.width > 112 {
        (area.width - 104) / 2
    } else {
        3.min(area.width.saturating_sub(1) / 2)
    };
    let vertical_margin = 1.min(area.height.saturating_sub(1) / 2);

    Rect::new(
        area.x + horizontal_margin,
        area.y + vertical_margin,
        area.width.saturating_sub(horizontal_margin * 2),
        area.height.saturating_sub(vertical_margin * 2),
    )
}

fn rect_contains_point(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_text(text.to_owned())
        .map_err(|error| error.to_string())
}

struct RenderedTranscript {
    lines: Vec<Line<'static>>,
    selected_line_index: Option<usize>,
}

fn build_transcript_lines(
    entries: &[TranscriptItem],
    selected_index: Option<usize>,
) -> RenderedTranscript {
    let mut lines = Vec::new();
    let mut selected_line_index = None;

    for (index, entry) in entries.iter().enumerate() {
        let is_selected = selected_index == Some(index);
        if is_selected {
            selected_line_index = Some(lines.len());
        }

        match entry {
            TranscriptItem::Entry(entry) => {
                push_entry_lines(&mut lines, entry, is_selected, "", "  ", true);
            }
            TranscriptItem::SubagentGroup(group) => {
                lines.push(Line::from(Span::styled(
                    subagent_group_title(group),
                    selectable_style(subagent_group_style(group), is_selected),
                )));

                if group.expanded {
                    for child in &group.entries {
                        push_entry_lines(&mut lines, child, false, "  ", "    ", false);
                    }
                }

                lines.push(Line::raw(String::new()));
            }
        }
    }

    RenderedTranscript {
        lines,
        selected_line_index,
    }
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }

    lines
        .iter()
        .map(|line| {
            let visual_width = line.width();
            let wrapped = if visual_width == 0 {
                1
            } else {
                visual_width.div_ceil(width as usize)
            };
            wrapped.min(u16::MAX as usize) as u16
        })
        .sum()
}

fn push_entry_lines(
    lines: &mut Vec<Line<'static>>,
    entry: &TranscriptEntry,
    selected: bool,
    title_indent: &str,
    body_indent: &str,
    trailing_blank: bool,
) {
    lines.push(Line::from(vec![
        Span::styled(
            title_indent.to_owned(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            entry.title.clone(),
            selectable_style(entry.title_style(), selected),
        ),
    ]));

    if entry.body.is_empty() {
        lines.push(Line::raw(body_indent.to_owned()));
    } else {
        for line in entry.body.lines() {
            lines.push(Line::from(vec![
                Span::styled(body_indent.to_owned(), Style::default().fg(Color::DarkGray)),
                Span::styled(line.to_owned(), entry.body_style()),
            ]));
        }
    }

    if trailing_blank {
        lines.push(Line::raw(String::new()));
    }
}

fn selectable_style(style: Style, selected: bool) -> Style {
    if selected {
        style.fg(Color::Black).bg(Color::White)
    } else {
        style
    }
}

fn subagent_group_title(group: &SubagentGroup) -> String {
    let marker = if group.expanded { "[-]" } else { "[+]" };
    let status = match group.status {
        SubagentStatus::Running => "running",
        SubagentStatus::Complete => "complete",
        SubagentStatus::Failed => "failed",
    };
    format!(
        "{marker} Subagent {status} ({} entries): {}",
        group.entries.len(),
        group.summary
    )
}

fn subagent_group_style(group: &SubagentGroup) -> Style {
    match group.status {
        SubagentStatus::Running => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        SubagentStatus::Complete => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        SubagentStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn summarize_tool_call(name: &str, arguments: &impl std::fmt::Display) -> String {
    let arguments_text = arguments.to_string();
    let json = serde_json::from_str::<Value>(&arguments_text).ok();

    match name {
        "read_file" => format!(
            "File read: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "edit_file" => format!(
            "File edit: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "write_file" => format!(
            "File write: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "bash" => format!(
            "Bash: {}",
            summarize_argument_field(&json, "command", &arguments_text)
        ),
        "prompt_cursor" => format!(
            "Prompt Cursor: {}",
            summarize_argument_field(&json, "prompt", &arguments_text)
        ),
        "subagent" => format!(
            "Subagent: {}",
            summarize_argument_field(&json, "prompt", &arguments_text)
        ),
        _ => format!(
            "Tool {name}: {}",
            truncate_text(&single_line(&arguments_text), 80)
        ),
    }
}

fn tool_label(name: &str) -> &'static str {
    match name {
        "read_file" => "File read",
        "edit_file" => "File edit",
        "write_file" => "File write",
        "bash" => "Bash",
        "prompt_cursor" => "Prompt Cursor",
        "subagent" => "Subagent",
        _ => "Tool",
    }
}

fn tool_detail_from_summary(summary: &str) -> String {
    summary
        .split_once(": ")
        .map(|(_, detail)| detail.to_owned())
        .unwrap_or_else(|| summary.to_owned())
}

fn render_tool_aggregate_title(
    label: &str,
    latest_detail: &str,
    total_calls: usize,
    pending_calls: usize,
) -> String {
    let count_suffix = if total_calls > 1 {
        format!(" x{total_calls}")
    } else {
        String::new()
    };
    let running_suffix = match pending_calls {
        0 => String::new(),
        1 => " (running)".to_owned(),
        count => format!(" ({count} running)"),
    };

    if latest_detail.is_empty() {
        format!("{label}{count_suffix}{running_suffix}")
    } else if total_calls > 1 {
        format!("{label}{count_suffix} (latest: {latest_detail}){running_suffix}")
    } else {
        format!("{label}: {latest_detail}{running_suffix}")
    }
}

fn render_subagent_tool_aggregate_title(
    latest_description: &str,
    total_calls: usize,
    pending_calls: usize,
) -> String {
    let latest_description = if latest_description.is_empty() {
        "Child tool call"
    } else {
        latest_description
    };
    let count_suffix = if total_calls > 1 {
        format!(" x{total_calls}")
    } else {
        String::new()
    };
    let running_suffix = match pending_calls {
        0 => String::new(),
        1 => " (running)".to_owned(),
        count => format!(" ({count} running)"),
    };

    if total_calls > 1 {
        format!("Tools{count_suffix} (latest: {latest_description}){running_suffix}")
    } else {
        format!("Tool: {latest_description}{running_suffix}")
    }
}

fn summarize_argument_field(json: &Option<Value>, field: &str, fallback: &str) -> String {
    json.as_ref()
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .map(|value| truncate_text(&single_line(value), 80))
        .unwrap_or_else(|| truncate_text(&single_line(fallback), 80))
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }

    let head: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::{App, Args, StreamEvent, SubagentProgressEvent, TranscriptItem, TranscriptKind};
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
        let mut app = App::new(&test_args(), Arc::new(super::CursorSessionStore::default()));
        app.transcript
            .push(TranscriptItem::Entry(super::TranscriptEntry::user("hello")));
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

        assert_eq!(super::wrapped_line_count(&lines, 5), 4);
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

        let collapsed =
            super::build_transcript_lines(&app.transcript, Some(app.selected_transcript));
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

        let expanded =
            super::build_transcript_lines(&app.transcript, Some(app.selected_transcript));
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
        app.push_transcript_entry(super::TranscriptEntry::assistant("Done."));

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
        app.transcript_scroll_mode = super::TranscriptScrollMode::FollowTail;

        app.scroll_transcript_page_up();

        assert!(matches!(
            app.transcript_scroll_mode,
            super::TranscriptScrollMode::Manual
        ));
        assert_eq!(app.transcript_scroll, 101);
    }

    #[test]
    fn page_down_clamps_manual_scroll_to_max() {
        let mut app = streaming_app();
        app.last_transcript_max_scroll = 80;
        app.last_transcript_scroll = 75;
        app.last_transcript_page_height = 20;
        app.transcript_scroll_mode = super::TranscriptScrollMode::Manual;
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
        app.transcript_scroll_mode = super::TranscriptScrollMode::Manual;
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
        app.transcript_scroll_mode = super::TranscriptScrollMode::Manual;
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
}
