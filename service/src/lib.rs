//! Service-layer orchestration over the shared Mirage session reducer.
#![warn(missing_docs)]

/// Shared API DTOs used by the Mirage service and server crates.
pub mod api;

use crate::api::SessionSnapshot;
use mirage_core::{
    OneOrMany,
    message::{AssistantContent, Message, ToolResultContent, UserContent},
    session::{Session, SessionPersistedState, StreamEvent, SubagentProgressEvent},
    skills::{ResolvedSkill, prompt_with_resolved_skills},
};

const MAX_ESTIMATED_HISTORY_TOKENS: usize = 80_000;
const TARGET_RECENT_HISTORY_TOKENS: usize = 24_000;
const MIN_RECENT_HISTORY_MESSAGES: usize = 12;
const MIN_RECENT_HISTORY_FLOOR: usize = 4;
const ESTIMATED_CHARS_PER_TOKEN: usize = 4;
const MESSAGE_TOKEN_OVERHEAD: usize = 12;
const COMPACTED_SUMMARY_MAX_CHARS: usize = 12_000;
const COMPACTED_SUMMARY_MAX_LINES: usize = 48;
const COMPACTED_LINE_MAX_CHARS: usize = 240;

/// Static configuration shared by a session service instance.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub model: String,
    pub max_turns: usize,
    pub authority: String,
    pub base_path: String,
    pub uncensored: bool,
    pub system_prompt_configured: bool,
}

/// Prompt submission payload returned when the service begins a new request.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct PromptRequest {
    pub prompt: String,
    pub effective_prompt: String,
    pub resolved_skills: Vec<ResolvedSkill>,
    pub history: Vec<Message>,
    pub max_turns: usize,
}

/// Read-only snapshot of the service configuration and derived session metadata.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatusSnapshot {
    pub model: String,
    pub authority: String,
    pub base_path: String,
    pub max_turns: usize,
    pub uncensored: bool,
    pub system_prompt_configured: bool,
    pub history_messages: usize,
}

/// High-level orchestration wrapper around the shared session reducer.
#[derive(Debug)]
pub struct SessionService {
    session: Session,
    config: ServiceConfig,
    history_messages_override: Option<usize>,
}

impl SessionService {
    /// Creates a new service with a fresh reducer-backed session.
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            session: Session::new(),
            config,
            history_messages_override: None,
        }
    }

    /// Returns an immutable reference to the underlying session reducer state.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Returns a mutable reference to the underlying session reducer state.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Returns the configured model name for this service.
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Returns whether the built-in uncensoring prompt is enabled.
    pub fn uncensored(&self) -> bool {
        self.config.uncensored
    }

    /// Returns whether the service can accept a new prompt right now.
    pub fn can_submit(&self, input: &str) -> bool {
        !self.session.streaming && !input.trim().is_empty()
    }

    /// Begins a new prompt submission and returns the payload needed to execute it.
    pub fn submit_prompt(
        &mut self,
        prompt: String,
        resolved_skills: Vec<ResolvedSkill>,
    ) -> PromptRequest {
        self.history_messages_override = None;
        let history = compact_history(&self.session.history);
        let max_turns = self.config.max_turns;
        self.session.begin_prompt(prompt.clone());
        let effective_prompt = prompt_with_resolved_skills(&prompt, &resolved_skills);
        PromptRequest {
            prompt,
            effective_prompt,
            resolved_skills,
            history,
            max_turns,
        }
    }

    /// Applies a parent-agent stream event to the underlying reducer state.
    pub fn apply_stream_event(&mut self, event: StreamEvent) {
        self.history_messages_override = None;
        self.session.apply_stream_event(event);
    }

    /// Applies a child-agent progress event to the underlying reducer state.
    pub fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        self.history_messages_override = None;
        self.session.apply_subagent_event(event);
    }

    /// Clears the current session and replaces it with a notice entry and status message.
    pub fn clear_with_notice(
        &mut self,
        transcript_notice: impl Into<String>,
        status: impl Into<String>,
    ) {
        self.history_messages_override = None;
        self.session.clear_with_notice(transcript_notice, status);
    }

    /// Replaces the local reducer state with previously persisted session history and transcript data.
    pub fn apply_persisted_state(&mut self, state: SessionPersistedState) {
        self.history_messages_override = None;
        self.session.replace_persisted_state(state);
    }

    /// Replaces the local reducer state with a remote session snapshot.
    pub fn apply_remote_snapshot(&mut self, snapshot: SessionSnapshot) {
        self.config.model = snapshot.model;
        self.config.authority = snapshot.authority;
        self.config.base_path = snapshot.base_path;
        self.config.max_turns = snapshot.max_turns;
        self.config.uncensored = snapshot.uncensored;
        self.config.system_prompt_configured = snapshot.system_prompt_configured;
        self.history_messages_override = Some(snapshot.history_messages);
        self.session
            .replace_remote_state(snapshot.transcript, snapshot.status, snapshot.streaming);
    }

    /// Returns a read-only snapshot of the service configuration and current history metrics.
    pub fn status_snapshot(&self) -> ServiceStatusSnapshot {
        ServiceStatusSnapshot {
            model: self.config.model.clone(),
            authority: self.config.authority.clone(),
            base_path: self.config.base_path.clone(),
            max_turns: self.config.max_turns,
            uncensored: self.config.uncensored,
            system_prompt_configured: self.config.system_prompt_configured,
            history_messages: self
                .history_messages_override
                .unwrap_or(self.session.history.len()),
        }
    }
}

/// Compacts long histories into a summary plus a recent verbatim suffix.
fn compact_history(history: &[Message]) -> Vec<Message> {
    if history.is_empty() || estimate_history_tokens(history) <= MAX_ESTIMATED_HISTORY_TOKENS {
        return history.to_vec();
    }

    let leading_system_count = history
        .iter()
        .take_while(|message| matches!(message, Message::System { .. }))
        .count();
    let leading_system = &history[..leading_system_count];
    let non_system = &history[leading_system_count..];
    if non_system.is_empty() {
        return history.to_vec();
    }

    let mut recent_start = select_recent_start(non_system);
    let mut compacted = build_compacted_history(leading_system, non_system, recent_start);

    while estimate_history_tokens(&compacted) > MAX_ESTIMATED_HISTORY_TOKENS
        && non_system.len().saturating_sub(recent_start) > MIN_RECENT_HISTORY_FLOOR
    {
        recent_start += 1;
        compacted = build_compacted_history(leading_system, non_system, recent_start);
    }

    compacted
}

/// Chooses the earliest history index that should remain verbatim after compaction.
fn select_recent_start(non_system: &[Message]) -> usize {
    let mut recent_tokens = 0;
    let mut recent_count = 0;
    let mut recent_start = non_system.len();

    for (index, message) in non_system.iter().enumerate().rev() {
        let message_tokens = estimate_message_tokens(message);
        let must_keep = recent_count < MIN_RECENT_HISTORY_MESSAGES;
        let fits_target = recent_tokens + message_tokens <= TARGET_RECENT_HISTORY_TOKENS;
        if !must_keep && !fits_target {
            break;
        }
        recent_tokens += message_tokens;
        recent_count += 1;
        recent_start = index;
    }

    recent_start
}

/// Builds a compacted history payload from a summary and a recent verbatim suffix.
fn build_compacted_history(
    leading_system: &[Message],
    non_system: &[Message],
    recent_start: usize,
) -> Vec<Message> {
    if recent_start == 0 {
        return leading_system
            .iter()
            .cloned()
            .chain(non_system.iter().cloned())
            .collect();
    }

    let mut compacted = Vec::with_capacity(
        leading_system.len() + 1 + non_system.len().saturating_sub(recent_start),
    );
    compacted.extend_from_slice(leading_system);
    compacted.push(Message::system(build_compacted_history_summary(
        &non_system[..recent_start],
    )));
    compacted.extend(non_system[recent_start..].iter().cloned());
    compacted
}

/// Renders a deterministic summary of compacted-away history messages.
fn build_compacted_history_summary(messages: &[Message]) -> String {
    let mut summary = format!(
        "Conversation memory generated by Mirage to keep the prompt within its history budget. {} older messages were compacted. If this summary conflicts with the recent verbatim turns that follow, prefer the recent turns.\n\nKey earlier context:",
        messages.len()
    );
    let mut shown = 0;

    for message in messages {
        if shown >= COMPACTED_SUMMARY_MAX_LINES {
            break;
        }
        let line = format!("\n- {}", summarize_message(message));
        if summary.len() + line.len() > COMPACTED_SUMMARY_MAX_CHARS {
            break;
        }
        summary.push_str(&line);
        shown += 1;
    }

    if shown < messages.len() {
        summary.push_str(&format!(
            "\n- ... {} additional older messages omitted from this compact summary.",
            messages.len() - shown
        ));
    }

    summary
}

/// Estimates the total token footprint of a history payload without provider-side tokenization.
fn estimate_history_tokens(history: &[Message]) -> usize {
    history.iter().map(estimate_message_tokens).sum()
}

/// Estimates the token footprint of one message from its serialized size.
fn estimate_message_tokens(message: &Message) -> usize {
    let chars = format!("{message:?}").len();
    MESSAGE_TOKEN_OVERHEAD + (chars / ESTIMATED_CHARS_PER_TOKEN)
}

/// Converts a structured message into one compact human-readable line.
fn summarize_message(message: &Message) -> String {
    let summary = match message {
        Message::System { content } => format!("System: {}", single_line(content)),
        Message::User { content } => format!("User: {}", summarize_user_content(content)),
        Message::Assistant { content, .. } => {
            format!("Assistant: {}", summarize_assistant_content(content))
        }
    };
    truncate_text(&summary, COMPACTED_LINE_MAX_CHARS)
}

/// Summarizes user-side message content blocks.
fn summarize_user_content(content: &OneOrMany<UserContent>) -> String {
    summarize_content_parts(content.iter().map(|item| match item {
        UserContent::Text(text) => single_line(text.text()),
        UserContent::ToolResult(tool_result) => format!(
            "tool result {} => {}",
            tool_result.id,
            summarize_tool_result_content(&tool_result.content)
        ),
        UserContent::Image(_) => "[image]".to_owned(),
        UserContent::Audio(_) => "[audio]".to_owned(),
        UserContent::Video(_) => "[video]".to_owned(),
        UserContent::Document(_) => "[document]".to_owned(),
    }))
}

/// Summarizes assistant-side message content blocks.
fn summarize_assistant_content(content: &OneOrMany<AssistantContent>) -> String {
    summarize_content_parts(content.iter().map(|item| match item {
        AssistantContent::Text(text) => single_line(text.text()),
        AssistantContent::ToolCall(tool_call) => format!(
            "called {}({})",
            tool_call.function.name,
            truncate_text(
                &single_line(&tool_call.function.arguments.to_string()),
                COMPACTED_LINE_MAX_CHARS / 2
            )
        ),
        AssistantContent::Reasoning(reasoning) => {
            let reasoning = single_line(&reasoning.display_text());
            if reasoning.is_empty() {
                "[reasoning]".to_owned()
            } else {
                format!("reasoning: {reasoning}")
            }
        }
        AssistantContent::Image(_) => "[image]".to_owned(),
    }))
}

/// Summarizes tool result content blocks.
fn summarize_tool_result_content(content: &OneOrMany<ToolResultContent>) -> String {
    summarize_content_parts(content.iter().map(|item| match item {
        ToolResultContent::Text(text) => single_line(text.text()),
        ToolResultContent::Image(_) => "[image]".to_owned(),
    }))
}

/// Joins a bounded number of content parts into a compact single-line summary.
fn summarize_content_parts(parts: impl Iterator<Item = String>) -> String {
    let values = parts
        .take(3)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        "[empty]".to_owned()
    } else {
        truncate_text(&values.join("; "), COMPACTED_LINE_MAX_CHARS)
    }
}

/// Collapses whitespace so summaries stay compact and easy to read.
fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates a string to a bounded character count with an ellipsis.
fn truncate_text(value: &str, max_chars: usize) -> String {
    let total_chars = value.chars().count();
    if total_chars <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 1 {
        return ".".to_owned();
    }
    let kept = value.chars().take(max_chars - 1).collect::<String>();
    format!("{kept}.")
}

#[cfg(test)]
mod tests {
    use super::{ServiceConfig, SessionService};
    use crate::api::SessionSnapshot;
    use mirage_core::{
        message::Message,
        session::{TranscriptEntry, TranscriptItem},
    };

    /// Builds a deterministic service configuration for service-layer unit tests.
    fn config() -> ServiceConfig {
        ServiceConfig {
            model: "test-model".to_owned(),
            max_turns: 8,
            authority: "api.venice.ai".to_owned(),
            base_path: "/api/v1".to_owned(),
            uncensored: false,
            system_prompt_configured: false,
        }
    }

    /// Verifies that submitting a prompt returns an execution payload and updates reducer state.
    #[test]
    fn submit_prompt_returns_request_and_updates_session() {
        let mut service = SessionService::new(config());

        let request = service.submit_prompt("hello".to_owned(), Vec::new());

        assert_eq!(request.prompt, "hello");
        assert_eq!(request.effective_prompt, "hello");
        assert_eq!(request.max_turns, 8);
        assert!(request.history.is_empty());
        assert!(service.session().streaming);
        assert_eq!(
            service
                .session()
                .transcript
                .last()
                .unwrap()
                .entry()
                .unwrap()
                .body,
            "hello"
        );
    }

    /// Verifies that status snapshots reflect the configured settings and current history count.
    #[test]
    fn status_snapshot_reflects_configuration_and_history_count() {
        let mut service = SessionService::new(config());
        service.submit_prompt("hello".to_owned(), Vec::new());

        let snapshot = service.status_snapshot();

        assert_eq!(snapshot.model, "test-model");
        assert_eq!(snapshot.max_turns, 8);
        assert_eq!(snapshot.history_messages, 0);
    }

    /// Verifies that applying a remote snapshot replaces local transcript and status state.
    #[test]
    fn apply_remote_snapshot_replaces_transcript_and_status() {
        let mut service = SessionService::new(config());
        service.apply_remote_snapshot(SessionSnapshot {
            id: "session-1".to_owned(),
            model: "remote-model".to_owned(),
            authority: "example.test".to_owned(),
            base_path: "/remote".to_owned(),
            max_turns: 32,
            uncensored: true,
            system_prompt_configured: true,
            history_messages: 5,
            streaming: true,
            status: "Remote streaming".to_owned(),
            transcript: vec![TranscriptItem::Entry(TranscriptEntry::assistant("remote"))],
        });

        let snapshot = service.status_snapshot();

        assert_eq!(snapshot.model, "remote-model");
        assert_eq!(snapshot.authority, "example.test");
        assert_eq!(snapshot.base_path, "/remote");
        assert_eq!(snapshot.max_turns, 32);
        assert_eq!(snapshot.history_messages, 5);
        assert!(service.session().streaming);
        assert_eq!(service.session().status, "Remote streaming");
        assert_eq!(
            service.session().transcript[0].entry().unwrap().body,
            "remote"
        );
    }

    /// Verifies that oversized histories are compacted into a summary plus recent verbatim turns.
    #[test]
    fn submit_prompt_compacts_oversized_history() {
        let mut service = SessionService::new(config());
        let chunk = "abcd".repeat(2_500);
        let mut history = Vec::new();
        for index in 0..20 {
            history.push(Message::user(format!("user-{index} {chunk}")));
            history.push(Message::assistant(format!("assistant-{index} {chunk}")));
        }
        let original_last = history.last().cloned().unwrap();
        service.session_mut().history = history.clone();

        let request = service.submit_prompt("follow up".to_owned(), Vec::new());

        assert!(request.history.len() < history.len());
        assert_eq!(request.history.last(), Some(&original_last));
        match request.history.first().unwrap() {
            Message::System { content } => {
                assert!(content.contains("Conversation memory generated by Mirage"));
                assert!(content.contains("older messages were compacted"));
            }
            other => panic!("expected compacted history summary, got {other:?}"),
        }
    }

    /// Verifies that small histories pass through unchanged.
    #[test]
    fn submit_prompt_keeps_small_history_verbatim() {
        let mut service = SessionService::new(config());
        service.session_mut().history = vec![
            Message::system("Keep answers concise."),
            Message::user("hello"),
            Message::assistant("hi"),
        ];

        let request = service.submit_prompt("follow up".to_owned(), Vec::new());

        assert_eq!(
            request.history,
            vec![
                Message::system("Keep answers concise."),
                Message::user("hello"),
                Message::assistant("hi"),
            ]
        );
    }
}
