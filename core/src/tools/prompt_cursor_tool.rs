use crate::tools::cursor_session::{CursorSessionError, CursorSessionStore};
use crate::{Tool, completion::ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use std::process::Command;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

/// Arguments accepted by the `prompt_cursor` tool.
#[derive(Debug, Deserialize)]
pub struct PromptCursorArgs {
    prompt: String,
    #[serde(default)]
    cwd: Option<String>,
}

/// Concrete Cursor CLI invocation derived from a `prompt_cursor` request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptCursorInvocation {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

/// Errors returned while invoking the local Cursor CLI in print mode.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum PromptCursorToolError {
    #[error("failed to start Cursor CLI: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to join Cursor CLI task: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Session(#[from] CursorSessionError),
    #[error("Cursor CLI exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },
}

/// Tool implementation that delegates a task to the local Cursor CLI and returns its final output.
#[derive(Clone)]
pub struct PromptCursorTool {
    session_store: Arc<CursorSessionStore>,
}

impl PromptCursorTool {
    /// Creates a prompt tool backed by a shared Cursor session cache.
    pub fn new(session_store: Arc<CursorSessionStore>) -> Self {
        Self { session_store }
    }
}

impl Tool for PromptCursorTool {
    const NAME: &'static str = "prompt_cursor";

    type Error = PromptCursorToolError;
    type Args = PromptCursorArgs;
    type Output = String;

    /// Returns the schema exposed to the model for the `prompt_cursor` tool.
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description:
                "Prompt Cursor by running the local Cursor agent CLI in print mode (`agent -p`) and return its output. Prefer this for non-minor coding tasks."
                    .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The prompt to send to `agent -p`."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the Cursor CLI command."
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    /// Executes the requested Cursor CLI prompt inside a blocking task.
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let session_store = self.session_store.clone();
        tokio::task::spawn_blocking(move || run_prompt_cursor(args, session_store)).await?
    }
}

/// Executes a `prompt_cursor` request synchronously inside a blocking task.
fn run_prompt_cursor(
    args: PromptCursorArgs,
    session_store: Arc<CursorSessionStore>,
) -> Result<String, PromptCursorToolError> {
    let session_id = session_store.get_or_create_blocking(args.cwd.as_deref())?;
    let invocation = build_prompt_cursor_invocation(&args.prompt, args.cwd.as_deref(), &session_id);
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.args);

    if let Some(cwd) = invocation.cwd {
        command.current_dir(cwd);
    }

    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(PromptCursorToolError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Builds the concrete process invocation for a `prompt_cursor` request.
fn build_prompt_cursor_invocation(
    prompt: &str,
    cwd: Option<&str>,
    session_id: &str,
) -> PromptCursorInvocation {
    PromptCursorInvocation {
        program: "agent".to_owned(),
        args: vec![
            "-p".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
            prompt.to_owned(),
        ],
        cwd: cwd.map(PathBuf::from),
    }
}

#[cfg(test)]
mod tests {
    use super::build_prompt_cursor_invocation;

    /// Verifies that prompt invocations include the expected resume flag and working directory.
    #[test]
    fn builds_prompt_cursor_command() {
        let invocation =
            build_prompt_cursor_invocation("Summarize this repo", Some("/tmp/project"), "chat-123");

        assert_eq!(invocation.program, "agent");
        assert_eq!(
            invocation.args,
            vec!["-p", "--resume", "chat-123", "Summarize this repo"]
        );
        assert_eq!(
            invocation.cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
    }

    /// Verifies that prompt invocations omit the working directory when one is not provided.
    #[test]
    fn builds_prompt_cursor_command_without_cwd() {
        let invocation = build_prompt_cursor_invocation("Hello", None, "chat-456");

        assert_eq!(invocation.args, vec!["-p", "--resume", "chat-456", "Hello"]);
        assert!(invocation.cwd.is_none());
    }
}
