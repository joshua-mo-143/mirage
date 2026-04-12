use crate::session::StreamEvent;
use serde::Serialize;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

#[derive(Clone)]
pub struct StreamDebugLogger {
    path: Arc<PathBuf>,
    file: Arc<Mutex<std::fs::File>>,
}

impl StreamDebugLogger {
    pub fn from_env(var_name: &str) -> Result<Option<Self>, StreamDebugLoggerError> {
        match env::var(var_name) {
            Ok(value) => match parse_logger_path(&value)? {
                Some(path) => Ok(Some(Self::new(path)?)),
                None => Ok(None),
            },
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(StreamDebugLoggerError::InvalidPath),
        }
    }

    pub fn from_optional_path_or_env(
        var_name: &str,
        explicit_path: Option<&str>,
    ) -> Result<Option<Self>, StreamDebugLoggerError> {
        if let Some(path) = explicit_path {
            return match parse_logger_path(path)? {
                Some(path) => Ok(Some(Self::new(path)?)),
                None => Ok(None),
            };
        }

        if let Some(logger) = Self::from_env(var_name)? {
            return Ok(Some(logger));
        }

        if cfg!(debug_assertions) {
            return Ok(Some(Self::new(default_debug_log_path()?)?));
        }

        Ok(None)
    }

    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StreamDebugLoggerError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(StreamDebugLoggerError::InvalidPath);
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path: Arc::new(path),
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn log_stream_event(
        &self,
        source: &str,
        session_id: Option<&str>,
        event: &StreamEvent,
    ) -> Result<(), StreamDebugLoggerError> {
        let record = StreamDebugRecord {
            timestamp_ms: now_millis(),
            pid: std::process::id(),
            source: source.to_owned(),
            session_id: session_id.map(str::to_owned),
            event: DebugStreamEvent::from_stream_event(event),
        };
        let encoded = serde_json::to_string(&record)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| StreamDebugLoggerError::LockPoisoned)?;
        writeln!(file, "{encoded}")?;
        file.flush()?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum StreamDebugLoggerError {
    #[error("invalid stream debug log path")]
    InvalidPath,
    #[error("unable to determine stream debug state directory")]
    StateDirectoryUnavailable,
    #[error("failed to write stream debug log: {0}")]
    Io(#[from] io::Error),
    #[error("failed to encode stream debug log: {0}")]
    Json(#[from] serde_json::Error),
    #[error("stream debug logger lock was poisoned")]
    LockPoisoned,
}

#[derive(Serialize)]
struct StreamDebugRecord {
    timestamp_ms: u128,
    pid: u32,
    source: String,
    session_id: Option<String>,
    #[serde(flatten)]
    event: DebugStreamEvent,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DebugStreamEvent {
    AssistantText {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        summary: String,
    },
    ToolResult {
        id: String,
    },
    Final {
        response: String,
        history_messages: Option<usize>,
        input_tokens: u64,
        output_tokens: u64,
    },
    Error {
        message: String,
    },
}

impl DebugStreamEvent {
    fn from_stream_event(event: &StreamEvent) -> Self {
        match event {
            StreamEvent::AssistantText(text) => Self::AssistantText { text: text.clone() },
            StreamEvent::ToolCall { id, name, summary } => Self::ToolCall {
                id: id.clone(),
                name: name.clone(),
                summary: summary.clone(),
            },
            StreamEvent::ToolResult { id } => Self::ToolResult { id: id.clone() },
            StreamEvent::Final(final_response) => {
                let usage = final_response.usage();
                Self::Final {
                    response: final_response.response().to_owned(),
                    history_messages: final_response.history().map(<[_]>::len),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                }
            }
            StreamEvent::Error(message) => Self::Error {
                message: message.clone(),
            },
        }
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_logger_path(value: &str) -> Result<Option<PathBuf>, StreamDebugLoggerError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Ok(None);
    }

    let lowered = normalized.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "0" | "false" | "off" | "disabled" | "none"
    ) {
        return Ok(None);
    }

    Ok(Some(PathBuf::from(normalized)))
}

fn default_debug_log_path() -> Result<PathBuf, StreamDebugLoggerError> {
    let base = if let Some(path) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".local").join("state")
    } else {
        return Err(StreamDebugLoggerError::StateDirectoryUnavailable);
    };

    Ok(base.join("mirage").join("stream-debug.jsonl"))
}
