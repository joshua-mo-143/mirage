use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};
use thiserror::Error;

/// Stores Cursor chat session ids keyed by workspace so local tool calls can resume prior conversations.
#[derive(Debug, Default)]
pub struct CursorSessionStore {
    sessions: Mutex<HashMap<SessionKey, String>>,
}

impl CursorSessionStore {
    /// Drops all cached session ids.
    pub fn clear(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    /// Returns the number of cached workspace-to-session mappings.
    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or(0)
    }

    /// Returns an existing session id for a workspace or creates a new Cursor chat on demand.
    pub fn get_or_create_blocking(&self, cwd: Option<&str>) -> Result<String, CursorSessionError> {
        let key = SessionKey::from_cwd(cwd)?;

        if let Some(session_id) = self.get(&key)? {
            return Ok(session_id);
        }

        let created = create_chat_blocking()?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| CursorSessionError::LockPoisoned)?;
        Ok(sessions.entry(key).or_insert(created).clone())
    }

    /// Looks up a cached session id for the given normalized workspace key.
    fn get(&self, key: &SessionKey) -> Result<Option<String>, CursorSessionError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| CursorSessionError::LockPoisoned)?;
        Ok(sessions.get(key).cloned())
    }
}

/// Normalized workspace key used for Cursor session reuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey(Option<PathBuf>);

impl SessionKey {
    /// Builds a workspace key from an optional current working directory string.
    fn from_cwd(cwd: Option<&str>) -> Result<Self, CursorSessionError> {
        let path = cwd.map(normalize_path).transpose()?;
        Ok(Self(path))
    }
}

/// Resolves a relative or absolute workspace path into a normalized absolute path.
fn normalize_path(path: &str) -> Result<PathBuf, CursorSessionError> {
    let raw = Path::new(path);
    if raw.is_absolute() {
        Ok(raw.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(raw))
    }
}

/// Invokes `agent create-chat` and returns the resulting Cursor chat id.
fn create_chat_blocking() -> Result<String, CursorSessionError> {
    let output = Command::new("agent").arg("create-chat").output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(CursorSessionError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    let session_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if session_id.is_empty() {
        return Err(CursorSessionError::EmptySessionId);
    }

    Ok(session_id)
}

/// Describes failures that can occur while resolving or creating Cursor chat sessions.
#[derive(Debug, Error)]
pub enum CursorSessionError {
    #[error("failed to create or resolve Cursor session: {0}")]
    Io(#[from] std::io::Error),
    #[error("Cursor session state lock was poisoned")]
    LockPoisoned,
    #[error("Cursor create-chat exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },
    #[error("Cursor create-chat returned an empty session id")]
    EmptySessionId,
}

#[cfg(test)]
mod tests {
    use super::SessionKey;

    /// Verifies that absolute workspace paths are preserved when deriving a session key.
    #[test]
    fn session_key_preserves_absolute_paths() {
        let key = SessionKey::from_cwd(Some("/tmp/project")).unwrap();
        assert_eq!(key.0.as_deref(), Some(std::path::Path::new("/tmp/project")));
    }

    /// Verifies that relative workspace paths are normalized against the current directory.
    #[test]
    fn session_key_normalizes_relative_paths_against_current_dir() {
        let expected = std::env::current_dir().unwrap().join("client");
        let key = SessionKey::from_cwd(Some("client")).unwrap();
        assert_eq!(key.0.unwrap(), expected);
    }
}
