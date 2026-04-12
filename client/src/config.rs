use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};
use thiserror::Error;

/// Persisted client configuration stored on the local machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct ClientConfig {
    pub(crate) remote: Option<RemoteServerConfig>,
}

/// Saved remote server connection details used as client defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RemoteServerConfig {
    pub(crate) server_url: String,
    pub(crate) admin_api_key: String,
}

/// Errors that can occur while loading or saving the client configuration file.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("unable to determine Mirage config directory")]
    ConfigDirectoryUnavailable,
    #[error("failed to read or write config: {0}")]
    Io(#[from] io::Error),
    #[error("failed to parse config: {0}")]
    Json(#[from] serde_json::Error),
}

impl ClientConfig {
    /// Loads the client configuration from disk or returns a default config if none exists.
    pub(crate) fn load_or_default() -> Result<Self, ConfigError> {
        let path = config_path()?;
        match fs::read_to_string(path) {
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ConfigError::Io(error)),
        }
    }

    /// Persists the client configuration to disk and returns the path that was written.
    pub(crate) fn save(&self) -> Result<PathBuf, ConfigError> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(path)
    }
}

/// Prompts the user to persist a remote server configuration as the default connection target.
pub(crate) fn maybe_prompt_to_save_remote(
    config: &mut ClientConfig,
    remote: &RemoteServerConfig,
) -> Result<Option<PathBuf>, ConfigError> {
    if config.remote.as_ref() == Some(remote) {
        return Ok(None);
    }

    let path = config_path()?;
    print!(
        "Save remote server {} as the default in {}? [y/N]: ",
        remote.server_url,
        path.display()
    );
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let normalized = answer.trim().to_ascii_lowercase();
    if normalized != "y" && normalized != "yes" {
        return Ok(None);
    }

    config.remote = Some(remote.clone());
    Ok(Some(config.save()?))
}

/// Returns the on-disk configuration path used by the Mirage client.
fn config_path() -> Result<PathBuf, ConfigError> {
    let base = if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err(ConfigError::ConfigDirectoryUnavailable);
    };

    Ok(base.join("mirage").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::RemoteServerConfig;

    /// Verifies that remote server configs compare using both the URL and admin key.
    #[test]
    fn remote_server_config_equality_matches_url_and_key() {
        let left = RemoteServerConfig {
            server_url: "http://127.0.0.1:3000".to_owned(),
            admin_api_key: "secret".to_owned(),
        };
        let right = RemoteServerConfig {
            server_url: "http://127.0.0.1:3000".to_owned(),
            admin_api_key: "secret".to_owned(),
        };

        assert_eq!(left, right);
    }
}
