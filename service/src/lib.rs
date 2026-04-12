pub mod api;

use crate::api::SessionSnapshot;
use mirage_core::{
    message::Message,
    session::{Session, StreamEvent, SubagentProgressEvent},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub model: String,
    pub max_turns: usize,
    pub authority: String,
    pub base_path: String,
    pub uncensored: bool,
    pub system_prompt_configured: bool,
}

#[derive(Debug, Clone)]
pub struct PromptRequest {
    pub prompt: String,
    pub history: Vec<Message>,
    pub max_turns: usize,
}

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

#[derive(Debug)]
pub struct SessionService {
    session: Session,
    config: ServiceConfig,
    history_messages_override: Option<usize>,
}

impl SessionService {
    pub fn new(config: ServiceConfig, system_prompt: Option<&str>) -> Self {
        Self {
            session: Session::new(system_prompt),
            config,
            history_messages_override: None,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn uncensored(&self) -> bool {
        self.config.uncensored
    }

    pub fn can_submit(&self, input: &str) -> bool {
        !self.session.streaming && !input.trim().is_empty()
    }

    pub fn submit_prompt(&mut self, prompt: String) -> PromptRequest {
        self.history_messages_override = None;
        let history = self.session.history.clone();
        let max_turns = self.config.max_turns;
        self.session.begin_prompt(prompt.clone());
        PromptRequest {
            prompt,
            history,
            max_turns,
        }
    }

    pub fn apply_stream_event(&mut self, event: StreamEvent) {
        self.history_messages_override = None;
        self.session.apply_stream_event(event);
    }

    pub fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        self.history_messages_override = None;
        self.session.apply_subagent_event(event);
    }

    pub fn clear_with_notice(
        &mut self,
        transcript_notice: impl Into<String>,
        status: impl Into<String>,
    ) {
        self.history_messages_override = None;
        self.session.clear_with_notice(transcript_notice, status);
    }

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

    #[test]
    fn submit_prompt_returns_request_and_updates_session() {
        let mut service = SessionService::new(config(), None);

        let request = service.submit_prompt("hello".to_owned());

        assert_eq!(request.prompt, "hello");
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

    #[test]
    fn status_snapshot_reflects_configuration_and_history_count() {
        let mut service = SessionService::new(config(), None);
        service.submit_prompt("hello".to_owned());

        let snapshot = service.status_snapshot();

        assert_eq!(snapshot.model, "test-model");
        assert_eq!(snapshot.max_turns, 8);
        assert_eq!(snapshot.history_messages, 0);
    }

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
