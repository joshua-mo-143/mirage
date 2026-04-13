use crate::config::RemoteServerConfig;
use mirage_core::{session::SessionPersistedState, skills::ResolvedSkill};
use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};
use thiserror::Error;

/// Persisted record of the most recent TUI session that Mirage can reattach to later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum PersistedLastSession {
    Local {
        session: SessionPersistedState,
        active_skill: Option<ResolvedSkill>,
    },
    Remote {
        remote: RemoteServerConfig,
        session_id: String,
        active_skill: Option<ResolvedSkill>,
    },
}

/// Errors that can occur while reading or writing the persisted TUI resume file.
#[derive(Debug, Error)]
pub(crate) enum ResumeError {
    #[error("unable to determine Mirage state directory")]
    StateDirectoryUnavailable,
    #[error("failed to read or write resume data: {0}")]
    Io(#[from] io::Error),
    #[error("failed to parse resume data: {0}")]
    Json(#[from] serde_json::Error),
}

/// Loads the last persisted TUI session record, if one exists.
pub(crate) fn load_last_session() -> Result<Option<PersistedLastSession>, ResumeError> {
    let path = resume_path()?;
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ResumeError::Io(error)),
    }
}

/// Saves the provided TUI session record as the last reattachable conversation.
pub(crate) fn save_last_session(session: &PersistedLastSession) -> Result<PathBuf, ResumeError> {
    let path = resume_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(session)?)?;
    Ok(path)
}

/// Returns the on-disk state path used for persisted TUI conversation resume data.
fn resume_path() -> Result<PathBuf, ResumeError> {
    Ok(state_dir()?.join("last-session.json"))
}

/// Returns Mirage's XDG-compatible state directory.
fn state_dir() -> Result<PathBuf, ResumeError> {
    let base = if let Some(path) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".local").join("state")
    } else {
        return Err(ResumeError::StateDirectoryUnavailable);
    };

    Ok(base.join("mirage"))
}

#[cfg(test)]
mod tests {
    use super::PersistedLastSession;
    use crate::config::RemoteServerConfig;

    /// Verifies that remote resume records round-trip through serde with their session id intact.
    #[test]
    fn remote_resume_record_round_trips_through_json() {
        let record = PersistedLastSession::Remote {
            remote: RemoteServerConfig {
                server_url: "http://127.0.0.1:3000".to_owned(),
                admin_api_key: "secret".to_owned(),
            },
            session_id: "session-123".to_owned(),
            active_skill: None,
        };

        let encoded = serde_json::to_string(&record).unwrap();
        let decoded: PersistedLastSession = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, record);
    }
}
