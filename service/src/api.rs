use mirage_core::session::TranscriptItem;
use serde::{Deserialize, Serialize};

/// Request body used to create a new server-side session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CreateSessionRequest {
    pub system_prompt: Option<String>,
}

/// Request body used to submit a prompt to an existing session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubmitMessageRequest {
    pub prompt: String,
}

/// Serialized snapshot of a session exposed over the Mirage service API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub id: String,
    pub model: String,
    pub authority: String,
    pub base_path: String,
    pub max_turns: usize,
    pub uncensored: bool,
    pub system_prompt_configured: bool,
    pub history_messages: usize,
    pub streaming: bool,
    pub status: String,
    pub transcript: Vec<TranscriptItem>,
}

/// Minimal health-check response returned by the service API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
}

/// Request body used to schedule the example Telegram hello job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleTelegramHelloRequest {
    pub every_seconds: u64,
    pub text: Option<String>,
    pub chat_id: Option<String>,
}

/// Serialized description of a scheduled Telegram hello job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledJobResponse {
    pub id: String,
    pub kind: String,
    pub every_seconds: u64,
    pub text: String,
    pub chat_id: String,
}

/// Generic structured error payload returned by the service API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub error: String,
}
