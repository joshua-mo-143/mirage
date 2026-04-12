use crate::{agent::FinalResponse, completion::Usage, message::Message};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const DEFAULT_WELCOME_BODY: &str = "Type a message below. Use /help for commands. Built-in tools: `bash`, `prompt_cursor`, `subagent`, `read_file`, `edit_file`, `write_file` (whole-file writes only).";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptKind {
    Meta,
    User,
    Assistant,
    Tool,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    pub title: String,
    pub body: String,
}

impl TranscriptEntry {
    pub fn meta(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Meta,
            title: title.into(),
            body: body.into(),
        }
    }

    pub fn user(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::User,
            title: "You".to_owned(),
            body: body.into(),
        }
    }

    pub fn assistant(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Assistant,
            title: "Assistant".to_owned(),
            body: body.into(),
        }
    }

    pub fn tool(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Tool,
            title: title.into(),
            body: body.into(),
        }
    }

    pub fn error(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Error,
            title: "Error".to_owned(),
            body: body.into(),
        }
    }

    pub fn to_plaintext(&self, title_indent: &str, body_indent: &str) -> String {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptItem {
    Entry(TranscriptEntry),
    SubagentGroup(SubagentGroup),
}

impl TranscriptItem {
    pub fn entry_mut(&mut self) -> Option<&mut TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    pub fn entry(&self) -> Option<&TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    pub fn to_plaintext(&self) -> String {
        match self {
            Self::Entry(entry) => entry.to_plaintext("", "  "),
            Self::SubagentGroup(group) => group.to_plaintext(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentStatus {
    Running,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentGroup {
    pub summary: String,
    pub status: SubagentStatus,
    pub expanded: bool,
    pub entries: Vec<TranscriptEntry>,
}

impl SubagentGroup {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            status: SubagentStatus::Running,
            expanded: false,
            entries: Vec::new(),
        }
    }

    pub fn to_plaintext(&self) -> String {
        let mut parts = vec![subagent_group_title(self)];
        for entry in &self.entries {
            parts.push(entry.to_plaintext("  ", "    "));
        }
        parts.join("\n")
    }
}

#[derive(Debug)]
pub enum StreamEvent {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentProgressEvent {
    Started {
        id: String,
        summary: String,
    },
    AssistantDelta {
        id: String,
        text: String,
    },
    ToolStarted {
        id: String,
        description: String,
    },
    ToolCompleted {
        id: String,
        description: String,
        output: Option<String>,
    },
    Finished {
        id: String,
    },
    Failed {
        id: String,
        error: String,
    },
}

#[derive(Debug)]
struct PendingSubagent {
    transcript_index: usize,
    pending_entry_index: Option<usize>,
    tool_entry_index: Option<usize>,
    tool_call_count: usize,
    pending_tool_calls: usize,
    latest_tool_description: String,
}

#[derive(Debug)]
struct PendingToolCall {
    transcript_index: usize,
}

#[derive(Debug)]
struct ToolAggregate {
    name: String,
    label: String,
    latest_detail: String,
    total_calls: usize,
    pending_calls: usize,
}

#[derive(Debug)]
pub struct Session {
    pub transcript: Vec<TranscriptItem>,
    pub history: Vec<Message>,
    pub status: String,
    pub usage: Option<Usage>,
    pub streaming: bool,
    pending_assistant: Option<usize>,
    pending_tool_calls: HashMap<String, PendingToolCall>,
    active_tool_aggregates: HashMap<usize, ToolAggregate>,
    pending_subagents: HashMap<String, PendingSubagent>,
}

impl Session {
    pub fn new(system_prompt: Option<&str>) -> Self {
        let mut transcript = vec![TranscriptItem::Entry(TranscriptEntry::meta(
            "Mirage",
            DEFAULT_WELCOME_BODY,
        ))];

        if let Some(system_prompt) = system_prompt {
            transcript.push(TranscriptItem::Entry(TranscriptEntry::meta(
                "System Prompt",
                truncate_text(system_prompt, 160),
            )));
        }

        Self {
            transcript,
            history: Vec::new(),
            status: "Ready.".to_owned(),
            usage: None,
            streaming: false,
            pending_assistant: None,
            pending_tool_calls: HashMap::new(),
            active_tool_aggregates: HashMap::new(),
            pending_subagents: HashMap::new(),
        }
    }

    pub fn push_entry(&mut self, entry: TranscriptEntry) {
        self.transcript.push(TranscriptItem::Entry(entry));
    }

    pub fn begin_prompt(&mut self, prompt: String) {
        self.clear_active_tool_aggregates();
        self.push_entry(TranscriptEntry::user(prompt));
        self.pending_assistant = None;
        self.streaming = true;
        self.status = "Streaming response...".to_owned();
    }

    pub fn clear_with_notice(
        &mut self,
        transcript_notice: impl Into<String>,
        status: impl Into<String>,
    ) {
        self.history.clear();
        self.usage = None;
        self.pending_assistant = None;
        self.clear_active_tool_aggregates();
        self.pending_subagents.clear();
        self.transcript.clear();
        self.transcript
            .push(TranscriptItem::Entry(TranscriptEntry::meta(
                "Mirage",
                transcript_notice,
            )));
        self.streaming = false;
        self.status = status.into();
    }

    pub fn replace_remote_state(
        &mut self,
        transcript: Vec<TranscriptItem>,
        status: impl Into<String>,
        streaming: bool,
    ) {
        self.transcript = transcript;
        self.status = status.into();
        self.streaming = streaming;
        self.usage = None;
        self.history.clear();
        self.pending_assistant = None;
        self.pending_subagents.clear();
        self.clear_active_tool_aggregates();
    }

    pub fn transcript_text(&self, index: usize) -> Option<String> {
        self.transcript.get(index).map(TranscriptItem::to_plaintext)
    }

    pub fn full_transcript_text(&self) -> String {
        self.transcript
            .iter()
            .map(TranscriptItem::to_plaintext)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn apply_stream_event(&mut self, event: StreamEvent) {
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
                    self.push_entry(TranscriptEntry::assistant(text));
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
                    self.push_entry(TranscriptEntry::assistant(
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
                }
                self.clear_active_tool_aggregates();
                self.pending_subagents.clear();
                self.streaming = false;
                self.status = "Last request failed.".to_owned();
                self.push_entry(TranscriptEntry::error(error));
            }
        }
    }

    pub fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
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
            self.push_entry(TranscriptEntry::tool(String::new(), String::new()));
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
            self.push_entry(TranscriptEntry::tool(
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
}

pub fn summarize_tool_call(name: &str, arguments: &impl std::fmt::Display) -> String {
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
    use super::{Session, StreamEvent, SubagentProgressEvent, TranscriptItem, TranscriptKind};

    fn streaming_session() -> Session {
        let mut session = Session::new(None);
        session.push_entry(super::TranscriptEntry::user("hello"));
        session.streaming = true;
        session
    }

    #[test]
    fn interleaves_assistant_text_and_tool_entries() {
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::AssistantText("First chunk.".to_owned()));
        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "read_file".to_owned(),
            summary: "File read: src/main.rs".to_owned(),
        });
        session.apply_stream_event(StreamEvent::AssistantText("Second chunk.".to_owned()));

        let assistant_and_tool_entries: Vec<_> = session.transcript.iter().skip(2).collect();
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
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "bash".to_owned(),
            summary: "Bash: ls".to_owned(),
        });

        let assistant_entries = session
            .transcript
            .iter()
            .filter_map(TranscriptItem::entry)
            .filter(|entry| matches!(entry.kind, TranscriptKind::Assistant))
            .count();

        assert_eq!(assistant_entries, 0);
        assert!(matches!(
            session.transcript.last().unwrap().entry().unwrap().kind,
            TranscriptKind::Tool
        ));
        assert_eq!(
            session.transcript.last().unwrap().entry().unwrap().title,
            "Bash: ls (running)"
        );
    }

    #[test]
    fn updates_existing_tool_entry_when_tool_completes() {
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "bash".to_owned(),
            summary: "Bash: ls".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-1".to_owned(),
        });

        let tool_entries: Vec<_> = session
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
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "read_file".to_owned(),
            summary: "File read: src/main.rs".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-1".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-2".to_owned(),
            name: "read_file".to_owned(),
            summary: "File read: src/lib.rs".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-2".to_owned(),
        });

        let tool_entries: Vec<_> = session
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
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "bash".to_owned(),
            summary: "Bash: ls".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-1".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-2".to_owned(),
            name: "read_file".to_owned(),
            summary: "File read: src/main.rs".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-2".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-3".to_owned(),
            name: "bash".to_owned(),
            summary: "Bash: pwd".to_owned(),
        });

        let tool_entries: Vec<_> = session
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
        let mut session = streaming_session();

        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-1".to_owned(),
            name: "bash".to_owned(),
            summary: "Bash: ls".to_owned(),
        });
        session.apply_stream_event(StreamEvent::ToolResult {
            id: "tool-1".to_owned(),
        });
        session.apply_stream_event(StreamEvent::AssistantText("\n\n   ".to_owned()));
        session.apply_stream_event(StreamEvent::ToolCall {
            id: "tool-2".to_owned(),
            name: "read_file".to_owned(),
            summary: "File read: src/main.rs".to_owned(),
        });

        let assistant_entries: Vec<_> = session
            .transcript
            .iter()
            .filter_map(TranscriptItem::entry)
            .filter(|entry| matches!(entry.kind, TranscriptKind::Assistant))
            .collect();

        assert!(assistant_entries.is_empty());
    }

    #[test]
    fn nests_subagent_events_inside_collapsible_group() {
        let mut session = streaming_session();

        session.apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
            id: "subagent-1".to_owned(),
            text: "Thinking...".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolStarted {
            id: "subagent-1".to_owned(),
            description: "List files".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
            id: "subagent-1".to_owned(),
            description: "List files".to_owned(),
            output: Some("Cargo.toml".to_owned()),
        });
        session.apply_subagent_event(SubagentProgressEvent::Finished {
            id: "subagent-1".to_owned(),
        });

        let TranscriptItem::SubagentGroup(group) = session.transcript.last().unwrap() else {
            panic!("expected trailing subagent group");
        };

        assert!(!group.expanded);
        assert_eq!(group.entries.len(), 2);
        assert_eq!(group.entries[0].title, "Assistant");
        assert_eq!(group.entries[0].body, "Thinking...");
        assert_eq!(group.entries[1].title, "Tool: List files");
    }

    #[test]
    fn aggregates_subagent_tool_updates_into_one_entry() {
        let mut session = streaming_session();

        session.apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolStarted {
            id: "subagent-1".to_owned(),
            description: "List files".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
            id: "subagent-1".to_owned(),
            description: "List files".to_owned(),
            output: None,
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolStarted {
            id: "subagent-1".to_owned(),
            description: "Read Cargo.toml".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolCompleted {
            id: "subagent-1".to_owned(),
            description: "Read Cargo.toml".to_owned(),
            output: None,
        });

        let TranscriptItem::SubagentGroup(group) = session.transcript.last().unwrap() else {
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
        let mut session = streaming_session();

        session.apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
            id: "subagent-1".to_owned(),
            text: "\n  ".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::ToolStarted {
            id: "subagent-1".to_owned(),
            description: "List files".to_owned(),
        });

        let TranscriptItem::SubagentGroup(group) = session.transcript.last().unwrap() else {
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
    fn transcript_text_serializes_subagent_group() {
        let mut session = streaming_session();

        session.apply_subagent_event(SubagentProgressEvent::Started {
            id: "subagent-1".to_owned(),
            summary: "Inspect the repo".to_owned(),
        });
        session.apply_subagent_event(SubagentProgressEvent::AssistantDelta {
            id: "subagent-1".to_owned(),
            text: "Thinking...".to_owned(),
        });

        let text = session
            .transcript_text(session.transcript.len() - 1)
            .unwrap();

        assert!(text.contains("[+] Subagent running (1 entries): Inspect the repo"));
        assert!(text.contains("  Assistant"));
        assert!(text.contains("    Thinking..."));
    }

    #[test]
    fn full_transcript_text_includes_top_level_entries() {
        let mut session = streaming_session();
        session.push_entry(super::TranscriptEntry::assistant("Done."));

        let text = session.full_transcript_text();

        assert!(text.contains("Mirage"));
        assert!(text.contains("You"));
        assert!(text.contains("hello"));
        assert!(text.contains("Assistant"));
        assert!(text.contains("Done."));
    }

    #[test]
    fn clear_with_notice_resets_transcript_and_state() {
        let mut session = streaming_session();
        session.clear_with_notice(
            "Conversation cleared, including Cursor session state.",
            "Cleared conversation history and Cursor session state.",
        );

        assert_eq!(session.transcript.len(), 1);
        assert_eq!(session.history.len(), 0);
        assert!(!session.streaming);
        assert_eq!(
            session.transcript[0].entry().unwrap().body,
            "Conversation cleared, including Cursor session state."
        );
    }
}
