use std::{
    env, fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Errors that can occur while resolving Mirage's runtime personality.
#[derive(Debug, Error)]
pub enum PersonalityError {
    /// Mirage could not determine an XDG-compatible config directory.
    #[error("unable to determine Mirage config directory for personality")]
    ConfigDirectoryUnavailable,
    /// Mirage failed while reading the configured personality file from disk.
    #[error("failed to read personality file `{path}`: {error}")]
    ReadFile {
        /// The path Mirage attempted to read.
        path: PathBuf,
        /// The underlying filesystem read error.
        #[source]
        error: std::io::Error,
    },
}

/// Loads Mirage's runtime personality from environment or disk.
///
/// Resolution order:
/// 1. `MIRAGE_PERSONALITY` environment variable
/// 2. `MIRAGE_PERSONALITY_FILE` path
/// 3. `~/.config/mirage/PERSONALITY.md` or `$XDG_CONFIG_HOME/mirage/PERSONALITY.md`
pub fn load_runtime_personality() -> Result<Option<String>, PersonalityError> {
    if let Ok(value) = env::var("MIRAGE_PERSONALITY") {
        return Ok(normalize_personality(&value));
    }

    if let Ok(path) = env::var("MIRAGE_PERSONALITY_FILE") {
        return load_personality_file(Path::new(&path));
    }

    let default_path = default_personality_path()?;
    if !default_path.is_file() {
        return Ok(None);
    }

    load_personality_file(&default_path)
}

/// Returns the default XDG-compatible personality file path.
pub fn default_personality_path() -> Result<PathBuf, PersonalityError> {
    let base = if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err(PersonalityError::ConfigDirectoryUnavailable);
    };

    Ok(base.join("mirage").join("PERSONALITY.md"))
}

/// Loads and normalizes a personality file from disk.
pub fn load_personality_file(path: &Path) -> Result<Option<String>, PersonalityError> {
    let contents = fs::read_to_string(path).map_err(|error| PersonalityError::ReadFile {
        path: path.to_path_buf(),
        error,
    })?;
    Ok(normalize_personality(&contents))
}

/// Trims personality content and drops empty values.
fn normalize_personality(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::load_personality_file;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    /// Verifies that personality files are trimmed before use.
    #[test]
    fn loads_trimmed_personality_file() {
        let path = unique_temp_path("mirage-personality-test.md");
        fs::write(&path, "\n  Dry, concise, and practical.  \n").unwrap();

        let loaded = load_personality_file(&path).unwrap();

        fs::remove_file(&path).ok();
        assert_eq!(loaded.as_deref(), Some("Dry, concise, and practical."));
    }

    /// Verifies that whitespace-only personality files are ignored.
    #[test]
    fn ignores_empty_personality_file() {
        let path = unique_temp_path("mirage-empty-personality-test.md");
        fs::write(&path, " \n\t ").unwrap();

        let loaded = load_personality_file(&path).unwrap();

        fs::remove_file(&path).ok();
        assert!(loaded.is_none());
    }

    /// Builds a temp file path that is unlikely to collide across test runs.
    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{nanos}-{name}"))
    }
}
