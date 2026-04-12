use mirage_core::{Tool, completion::ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Deserialize)]
pub struct BashArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BashInvocation {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum BashToolError {
    #[error("failed to start bash: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to join bash task: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub struct BashTool;

impl Tool for BashTool {
    const NAME: &'static str = "bash";

    type Error = BashToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Run any shell command through `bash -lc`. Use this to discover what is installed, inspect the environment, and execute arbitrary commands instead of assuming limitations.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The exact shell command to execute with `bash -lc`."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the shell command."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::task::spawn_blocking(move || run_bash(args)).await?
    }
}

fn run_bash(args: BashArgs) -> Result<String, BashToolError> {
    let invocation = build_bash_invocation(&args.command, args.cwd.as_deref());
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.args);

    if let Some(cwd) = invocation.cwd {
        command.current_dir(cwd);
    }

    let output = command.output()?;
    Ok(format_bash_output(output))
}

fn build_bash_invocation(command: &str, cwd: Option<&str>) -> BashInvocation {
    BashInvocation {
        program: "bash".to_owned(),
        args: vec!["-lc".to_owned(), command.to_owned()],
        cwd: cwd.map(PathBuf::from),
    }
}

fn format_bash_output(output: std::process::Output) -> String {
    let status = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    format!(
        "exit_code: {status}\nstdout:\n{}\n\nstderr:\n{}",
        if stdout.is_empty() {
            "(empty)"
        } else {
            stdout.as_str()
        },
        if stderr.is_empty() {
            "(empty)"
        } else {
            stderr.as_str()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::{build_bash_invocation, format_bash_output};
    use std::os::unix::process::ExitStatusExt;
    use std::process::Output;

    #[test]
    fn builds_bash_command() {
        let invocation = build_bash_invocation("ls -la", Some("/tmp/project"));

        assert_eq!(invocation.program, "bash");
        assert_eq!(invocation.args, vec!["-lc", "ls -la"]);
        assert_eq!(
            invocation.cwd.as_deref(),
            Some(std::path::Path::new("/tmp/project"))
        );
    }

    #[test]
    fn formats_bash_output_with_empty_streams() {
        let output = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };

        let formatted = format_bash_output(output);
        assert!(formatted.contains("exit_code: 0"));
        assert!(formatted.contains("stdout:\n(empty)"));
        assert!(formatted.contains("stderr:\n(empty)"));
    }
}
