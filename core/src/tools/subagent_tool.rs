use crate::tools::cursor_session::{CursorSessionError, CursorSessionStore};
use crate::{Tool, completion::ToolDefinition, session::SubagentProgressEvent};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
};

/// Arguments accepted by the `subagent` tool.
#[derive(Debug, Deserialize)]
pub struct SubagentArgs {
    prompt: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

/// Concrete Cursor CLI invocation derived from a `subagent` request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubagentInvocation {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

/// Errors returned while running a child Cursor agent.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum SubagentToolError {
    #[error("failed to spawn subagent: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse subagent stream JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to join subagent stderr task: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Session(#[from] CursorSessionError),
    #[error("subagent exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },
    #[error("subagent reported an error: {0}")]
    ReportedError(String),
    #[error("subagent stdout was not captured")]
    MissingStdout,
    #[error("subagent stderr was not captured")]
    MissingStderr,
}

/// Tool implementation that spawns a child Cursor agent and forwards progress events back to Mirage.
#[derive(Clone)]
pub struct SubagentTool {
    progress_tx: mpsc::UnboundedSender<SubagentProgressEvent>,
    session_store: Arc<CursorSessionStore>,
}

impl SubagentTool {
    /// Creates a subagent tool backed by a progress channel and shared Cursor session cache.
    pub fn new(
        progress_tx: mpsc::UnboundedSender<SubagentProgressEvent>,
        session_store: Arc<CursorSessionStore>,
    ) -> Self {
        Self {
            progress_tx,
            session_store,
        }
    }
}

impl Tool for SubagentTool {
    const NAME: &'static str = "subagent";

    type Error = SubagentToolError;
    type Args = SubagentArgs;
    type Output = String;

    /// Returns the schema exposed to the model for the `subagent` tool.
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Spawn a child Cursor agent for a delegated task, stream its progress into Mirage, and return the child agent's final answer.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Task to delegate to the child agent."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the child agent."
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional child model override."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["ask", "plan"],
                        "description": "Optional read-only child mode."
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    /// Runs the delegated child-agent task and returns its final answer.
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        run_subagent(args, self.progress_tx.clone(), self.session_store.clone()).await
    }
}

/// Spawns and supervises a child Cursor agent for a delegated task.
async fn run_subagent(
    args: SubagentArgs,
    progress_tx: mpsc::UnboundedSender<SubagentProgressEvent>,
    session_store: Arc<CursorSessionStore>,
) -> Result<String, SubagentToolError> {
    let id = unique_subagent_id();
    let summary = truncate_text(&args.prompt, 100);
    let _ = progress_tx.send(SubagentProgressEvent::Started {
        id: id.clone(),
        summary,
    });

    let cwd = args.cwd.clone();
    let session_id =
        tokio::task::spawn_blocking(move || session_store.get_or_create_blocking(cwd.as_deref()))
            .await??;

    let invocation = build_subagent_invocation(&args, &session_id);
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(cwd) = invocation.cwd {
        command.current_dir(cwd);
    }

    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or(SubagentToolError::MissingStdout)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(SubagentToolError::MissingStderr)?;

    let stderr_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut lines = Vec::new();
        while let Some(line) = reader.next_line().await? {
            lines.push(line);
        }
        Ok::<String, std::io::Error>(lines.join("\n"))
    });

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut final_result = None;
    let mut result_error = None;

    while let Some(line) = stdout_reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        handle_stream_json_line(
            &id,
            &line,
            &progress_tx,
            &mut final_result,
            &mut result_error,
        )?;
    }

    let status = child.wait().await?;
    let stderr = stderr_handle.await??;

    if let Some(error) = result_error {
        let _ = progress_tx.send(SubagentProgressEvent::Failed {
            id,
            error: error.clone(),
        });
        return Err(SubagentToolError::ReportedError(error));
    }

    if !status.success() {
        let error = stderr.trim().to_owned();
        let _ = progress_tx.send(SubagentProgressEvent::Failed {
            id,
            error: error.clone(),
        });
        return Err(SubagentToolError::CommandFailed {
            status: status.code().unwrap_or(-1),
            stderr: error,
        });
    }

    let result = final_result.unwrap_or_default();
    let _ = progress_tx.send(SubagentProgressEvent::Finished { id });
    Ok(result)
}

/// Builds the concrete process invocation for a `subagent` request.
fn build_subagent_invocation(args: &SubagentArgs, session_id: &str) -> SubagentInvocation {
    let mut invocation_args = vec![
        "-p".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--stream-partial-output".to_owned(),
        "--trust".to_owned(),
        "--resume".to_owned(),
        session_id.to_owned(),
    ];

    if let Some(model) = args.model.as_deref() {
        invocation_args.push("--model".to_owned());
        invocation_args.push(model.to_owned());
    }

    if let Some(mode) = args.mode.as_deref() {
        invocation_args.push("--mode".to_owned());
        invocation_args.push(mode.to_owned());
    }

    if let Some(cwd) = args.cwd.as_deref() {
        invocation_args.push("--workspace".to_owned());
        invocation_args.push(cwd.to_owned());
    }

    invocation_args.push(args.prompt.clone());

    SubagentInvocation {
        program: "agent".to_owned(),
        args: invocation_args,
        cwd: args.cwd.as_ref().map(PathBuf::from),
    }
}

/// Parses a single streamed JSON line emitted by the child Cursor agent.
fn handle_stream_json_line(
    id: &str,
    line: &str,
    progress_tx: &mpsc::UnboundedSender<SubagentProgressEvent>,
    final_result: &mut Option<String>,
    result_error: &mut Option<String>,
) -> Result<(), SubagentToolError> {
    let value: Value = serde_json::from_str(line)?;

    match value.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if value.get("timestamp_ms").is_some() {
                let text = extract_text(&value);
                if !text.is_empty() {
                    let _ = progress_tx.send(SubagentProgressEvent::AssistantDelta {
                        id: id.to_owned(),
                        text,
                    });
                }
            }
        }
        Some("tool_call") => {
            let description = extract_tool_description(&value);
            match value.get("subtype").and_then(Value::as_str) {
                Some("started") => {
                    let _ = progress_tx.send(SubagentProgressEvent::ToolStarted {
                        id: id.to_owned(),
                        description,
                    });
                }
                Some("completed") => {
                    let output = extract_tool_output_excerpt(&value);
                    let _ = progress_tx.send(SubagentProgressEvent::ToolCompleted {
                        id: id.to_owned(),
                        description,
                        output,
                    });
                }
                _ => {}
            }
        }
        Some("result") => match value.get("subtype").and_then(Value::as_str) {
            Some("success") => {
                *final_result = Some(
                    value
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            _ => {
                *result_error = Some(
                    value
                        .get("result")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("child agent returned an error result")
                        .to_owned(),
                );
            }
        },
        _ => {}
    }

    Ok(())
}

/// Extracts assistant text from a streamed Cursor JSON event.
fn extract_text(value: &Value) -> String {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

/// Extracts a human-readable tool description from a streamed Cursor JSON event.
fn extract_tool_description(value: &Value) -> String {
    value
        .pointer("/tool_call/shellToolCall/args/description")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/tool_call/description")
                .and_then(Value::as_str)
        })
        .unwrap_or("Child tool call")
        .to_owned()
}

/// Extracts a short stdout excerpt from a completed child tool call event.
fn extract_tool_output_excerpt(value: &Value) -> Option<String> {
    value
        .pointer("/tool_call/shellToolCall/result/success/stdout")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| truncate_text(text, 160))
}

/// Generates a best-effort unique identifier for a child agent run.
fn unique_subagent_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("subagent-{nanos}")
}

/// Truncates text to a bounded character count while preserving a visual ellipsis.
fn truncate_text(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }

    let head: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::{
        SubagentArgs, SubagentProgressEvent, build_subagent_invocation, handle_stream_json_line,
    };
    use tokio::sync::mpsc;

    /// Verifies that optional subagent CLI flags are translated into the expected command line.
    #[test]
    fn builds_subagent_command_with_optional_flags() {
        let invocation = build_subagent_invocation(
            &SubagentArgs {
                prompt: "Inspect the repo".to_owned(),
                cwd: Some("/tmp/project".to_owned()),
                model: Some("gpt-5".to_owned()),
                mode: Some("plan".to_owned()),
            },
            "chat-123",
        );

        assert_eq!(invocation.program, "agent");
        assert_eq!(
            invocation.args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust",
                "--resume",
                "chat-123",
                "--model",
                "gpt-5",
                "--mode",
                "plan",
                "--workspace",
                "/tmp/project",
                "Inspect the repo",
            ]
        );
        assert_eq!(
            invocation.cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
    }

    /// Verifies that streamed assistant deltas are forwarded as progress events.
    #[test]
    fn forwards_partial_assistant_deltas() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut final_result = None;
        let mut result_error = None;

        handle_stream_json_line(
            "subagent-1",
            r#"{"type":"assistant","timestamp_ms":1,"message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
            &tx,
            &mut final_result,
            &mut result_error,
        )
        .unwrap();

        match rx.try_recv().unwrap() {
            SubagentProgressEvent::AssistantDelta { id, text } => {
                assert_eq!(id, "subagent-1");
                assert_eq!(text, "hello");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(final_result.is_none());
        assert!(result_error.is_none());
    }

    /// Verifies that a successful streamed result captures the child agent's final answer.
    #[test]
    fn captures_successful_result() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut final_result = None;
        let mut result_error = None;

        handle_stream_json_line(
            "subagent-1",
            r#"{"type":"result","subtype":"success","result":"done"}"#,
            &tx,
            &mut final_result,
            &mut result_error,
        )
        .unwrap();

        assert_eq!(final_result.as_deref(), Some("done"));
        assert!(result_error.is_none());
    }
}
