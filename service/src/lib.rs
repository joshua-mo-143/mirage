pub mod api;

use crate::api::SessionSnapshot;
use mirage_core::{
    message::Message,
    session::{Session, StreamEvent, SubagentProgressEvent},
    skills::{ResolvedSkill, prompt_with_resolved_skills},
};

/// Static configuration shared by a session service instance.
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
#[derive(Debug, Clone)]
pub struct PromptRequest {
    pub prompt: String,
    pub effective_prompt: String,
    pub resolved_skills: Vec<ResolvedSkill>,
    pub history: Vec<Message>,
    pub max_turns: usize,
}

/// Read-only snapshot of the service configuration and derived session metadata.
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
    pub fn new(config: ServiceConfig, system_prompt: Option<&str>) -> Self {
        Self {
            session: Session::new(system_prompt),
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
        let history = self.session.history.clone();
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

#[cfg(test)]
mod tests {
    use super::{ServiceConfig, SessionService};
    use crate::api::SessionSnapshot;
    use mirage_core::session::{TranscriptEntry, TranscriptItem};

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
        let mut service = SessionService::new(config(), None);

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
        let mut service = SessionService::new(config(), None);
        service.submit_prompt("hello".to_owned(), Vec::new());

        let snapshot = service.status_snapshot();

        assert_eq!(snapshot.model, "test-model");
        assert_eq!(snapshot.max_turns, 8);
        assert_eq!(snapshot.history_messages, 0);
    }

    /// Verifies that applying a remote snapshot replaces local transcript and status state.
    #[test]
    fn apply_remote_snapshot_replaces_transcript_and_status() {
        let mut service = SessionService::new(config(), None);
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
}
