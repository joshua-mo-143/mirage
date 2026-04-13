use crate::{
    Tool,
    completion::ToolDefinition,
    tools::playwright_driver_assets::{PLAYWRIGHT_DRIVER_INDEX_JS, PLAYWRIGHT_DRIVER_PACKAGE_JSON},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

/// Arguments accepted by the `playwright` browser automation tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightArgs {
    action: PlaywrightAction,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    wait_until: Option<PlaywrightWaitUntil>,
}

impl PlaywrightArgs {
    /// Validates that the action-specific fields required by the browser driver are present.
    fn validate(&self) -> Result<(), PlaywrightToolError> {
        if self.action.requires_session() && self.session_id.is_none() {
            return Err(PlaywrightToolError::InvalidArguments(format!(
                "`session_id` is required for `{}`",
                self.action.as_str()
            )));
        }

        match self.action {
            PlaywrightAction::CreateSession | PlaywrightAction::CloseSession => {}
            PlaywrightAction::Navigate => {
                if self.url.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(
                        "`url` is required for `navigate`".to_owned(),
                    ));
                }
            }
            PlaywrightAction::Click | PlaywrightAction::WaitFor => {
                if self.selector.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(format!(
                        "`selector` is required for `{}`",
                        self.action.as_str()
                    )));
                }
            }
            PlaywrightAction::Fill => {
                if self.selector.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(
                        "`selector` is required for `fill`".to_owned(),
                    ));
                }
                if self.text.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(
                        "`text` is required for `fill`".to_owned(),
                    ));
                }
            }
            PlaywrightAction::Press => {
                if self.selector.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(
                        "`selector` is required for `press`".to_owned(),
                    ));
                }
                if self.key.is_none() {
                    return Err(PlaywrightToolError::InvalidArguments(
                        "`key` is required for `press`".to_owned(),
                    ));
                }
            }
            PlaywrightAction::ExtractText | PlaywrightAction::Screenshot => {}
        }

        Ok(())
    }
}

/// Supported browser actions exposed through the `playwright` tool.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaywrightAction {
    CreateSession,
    Navigate,
    Click,
    Fill,
    Press,
    WaitFor,
    ExtractText,
    Screenshot,
    CloseSession,
}

impl PlaywrightAction {
    /// Returns the wire-format action name expected by the Node driver.
    fn as_str(self) -> &'static str {
        match self {
            Self::CreateSession => "create_session",
            Self::Navigate => "navigate",
            Self::Click => "click",
            Self::Fill => "fill",
            Self::Press => "press",
            Self::WaitFor => "wait_for",
            Self::ExtractText => "extract_text",
            Self::Screenshot => "screenshot",
            Self::CloseSession => "close_session",
        }
    }

    /// Returns whether this action requires an existing browser session.
    fn requires_session(self) -> bool {
        !matches!(self, Self::CreateSession)
    }
}

/// Supported Playwright navigation wait strategies.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaywrightWaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle,
}

/// Structured result returned by the browser driver for a successful action.
#[allow(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlaywrightResult {
    pub ok: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub screenshot_path: Option<String>,
}

/// Errors returned while resolving Mirage-managed Playwright filesystem paths.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum PlaywrightPathError {
    #[error("unable to determine Mirage config directory for Playwright")]
    ConfigDirectoryUnavailable,
    #[error("unable to determine Mirage state directory for Playwright")]
    StateDirectoryUnavailable,
    #[error("failed to prepare Mirage-managed Playwright driver files: {0}")]
    Io(#[from] io::Error),
}

/// Describes whether the local Playwright runtime is available for Mirage to use.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaywrightRuntimeStatus {
    Ready,
    MissingNode,
    MissingDriverEntrypoint(PathBuf),
    MissingPackage,
    MissingBrowser,
    CheckFailed(String),
}

impl PlaywrightRuntimeStatus {
    /// Returns whether the local Playwright runtime is ready for use.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Returns whether Mirage can likely fix the missing runtime by running the local installer flow.
    pub fn can_auto_install(&self) -> bool {
        matches!(self, Self::MissingPackage | Self::MissingBrowser)
    }
}

/// Wire-format request sent to the Node Playwright driver.
#[derive(Debug, Clone, Serialize)]
struct PlaywrightDriverRequest {
    id: u64,
    #[serde(flatten)]
    payload: PlaywrightArgs,
}

/// Wire-format response emitted by the Node Playwright driver.
#[derive(Debug, Clone, Deserialize)]
struct PlaywrightDriverResponse {
    id: u64,
    ok: bool,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    screenshot_path: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl PlaywrightDriverResponse {
    /// Converts a successful wire response into the public tool result type.
    fn into_result(self) -> PlaywrightResult {
        PlaywrightResult {
            ok: self.ok,
            session_id: self.session_id,
            url: self.url,
            title: self.title,
            text: self.text,
            screenshot_path: self.screenshot_path,
        }
    }
}

/// Errors returned while using the `playwright` browser automation tool.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum PlaywrightToolError {
    #[error("invalid playwright arguments: {0}")]
    InvalidArguments(String),
    #[error(transparent)]
    Driver(#[from] PlaywrightDriverClientError),
    #[error("failed to encode playwright result: {0}")]
    Json(#[from] serde_json::Error),
}

/// Tool implementation that delegates browser automation to a long-lived Playwright driver process.
#[derive(Clone)]
pub struct PlaywrightTool {
    driver: Arc<PlaywrightDriverClient>,
}

impl PlaywrightTool {
    /// Creates a Playwright tool backed by a shared long-lived driver process.
    pub fn new() -> Self {
        Self {
            driver: Arc::new(PlaywrightDriverClient::new()),
        }
    }
}

impl Default for PlaywrightTool {
    /// Creates the default Playwright tool wrapper.
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PlaywrightTool {
    const NAME: &'static str = "playwright";

    type Error = PlaywrightToolError;
    type Args = PlaywrightArgs;
    type Output = String;

    /// Returns the schema exposed to the model for the `playwright` tool.
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Drive a headless Chromium browser through Playwright for web navigation, form filling, clicking, waiting, text extraction, and screenshots. Create a session first, then reuse its `session_id` for later actions.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "create_session",
                            "navigate",
                            "click",
                            "fill",
                            "press",
                            "wait_for",
                            "extract_text",
                            "screenshot",
                            "close_session"
                        ],
                        "description": "Browser action to perform."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Existing browser session id returned by `create_session`."
                    },
                    "url": {
                        "type": "string",
                        "description": "Target URL for navigation."
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector used by click, fill, wait, press, and extract actions."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text value used by `fill`."
                    },
                    "key": {
                        "type": "string",
                        "description": "Keyboard key used by `press`, for example `Enter`."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout override in milliseconds."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional absolute or relative screenshot path for `screenshot`."
                    },
                    "wait_until": {
                        "type": "string",
                        "enum": ["load", "dom_content_loaded", "network_idle"],
                        "description": "Optional navigation wait strategy for `navigate`."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    /// Executes the requested browser action through the shared Playwright driver.
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        args.validate()?;
        let response = self.driver.send(args).await?;
        if !response.ok {
            return Err(PlaywrightToolError::Driver(
                PlaywrightDriverClientError::ReportedError(
                    response.error.unwrap_or_else(|| {
                        "playwright driver reported an unknown error".to_owned()
                    }),
                ),
            ));
        }
        Ok(serde_json::to_string_pretty(&response.into_result())?)
    }
}

/// Returns the root directory Mirage uses to store browser automation state.
pub fn playwright_state_root() -> Result<PathBuf, PlaywrightPathError> {
    if let Ok(path) = env::var("MIRAGE_PLAYWRIGHT_STATE_ROOT") {
        return Ok(PathBuf::from(path));
    }

    let base = if let Some(path) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".local").join("state")
    } else {
        return Err(PlaywrightPathError::StateDirectoryUnavailable);
    };

    Ok(base.join("mirage").join("browser"))
}

/// Returns the root directory Mirage uses to store Playwright driver configuration and packages.
pub fn playwright_config_root() -> Result<PathBuf, PlaywrightPathError> {
    if let Ok(path) = env::var("MIRAGE_PLAYWRIGHT_CONFIG_ROOT") {
        return Ok(PathBuf::from(path));
    }

    let base = if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err(PlaywrightPathError::ConfigDirectoryUnavailable);
    };

    Ok(base.join("mirage"))
}

/// Returns the directory Mirage uses for Playwright-managed browser binaries.
pub fn playwright_browsers_dir() -> Result<PathBuf, PlaywrightPathError> {
    Ok(playwright_state_root()?.join("ms-playwright"))
}

/// Returns the directory Mirage uses for its dedicated Playwright browser profiles.
pub fn playwright_profiles_dir() -> Result<PathBuf, PlaywrightPathError> {
    Ok(playwright_state_root()?.join("profiles"))
}

/// Returns the default Mirage-managed persistent browser profile directory.
pub fn playwright_default_profile_dir() -> Result<PathBuf, PlaywrightPathError> {
    Ok(playwright_profiles_dir()?.join("default"))
}

/// Returns the default directory Mirage uses for browser screenshots.
pub fn playwright_screenshots_dir() -> Result<PathBuf, PlaywrightPathError> {
    Ok(playwright_state_root()?.join("screenshots"))
}

/// Returns the Mirage-managed Playwright driver directory under the user's config folder.
pub fn managed_playwright_driver_dir() -> Result<PathBuf, PlaywrightPathError> {
    Ok(playwright_config_root()?.join("playwright-driver"))
}

/// Ensures Mirage's embedded Playwright driver assets are materialized into the managed driver directory.
pub fn ensure_managed_playwright_driver_files() -> Result<PathBuf, PlaywrightPathError> {
    let package_dir = managed_playwright_driver_dir()?;
    fs::create_dir_all(&package_dir)?;
    write_driver_asset_if_needed(
        &package_dir.join("package.json"),
        PLAYWRIGHT_DRIVER_PACKAGE_JSON,
    )?;
    write_driver_asset_if_needed(&package_dir.join("index.js"), PLAYWRIGHT_DRIVER_INDEX_JS)?;
    Ok(package_dir)
}

/// Returns the package directory that contains the Node Playwright driver.
pub fn playwright_driver_package_dir() -> PathBuf {
    if let Ok(path) = env::var("MIRAGE_PLAYWRIGHT_DRIVER_DIR") {
        return PathBuf::from(path);
    }
    if let Ok(path) = env::var("MIRAGE_PLAYWRIGHT_DRIVER_ENTRY")
        && let Some(parent) = Path::new(&path).parent()
    {
        return parent.to_path_buf();
    }

    managed_playwright_driver_dir().unwrap_or_else(|_| PathBuf::from("playwright-driver"))
}

/// Returns the Node driver entrypoint used by the Playwright tool.
pub fn playwright_driver_entrypoint_path() -> PathBuf {
    if let Ok(path) = env::var("MIRAGE_PLAYWRIGHT_DRIVER_ENTRY") {
        return PathBuf::from(path);
    }

    playwright_driver_package_dir().join("index.js")
}

/// Checks whether Mirage's local Playwright runtime is ready to use.
pub async fn playwright_runtime_status() -> PlaywrightRuntimeStatus {
    if env::var("MIRAGE_PLAYWRIGHT_DRIVER_DIR").is_err()
        && env::var("MIRAGE_PLAYWRIGHT_DRIVER_ENTRY").is_err()
        && let Err(error) = ensure_managed_playwright_driver_files()
    {
        return PlaywrightRuntimeStatus::CheckFailed(error.to_string());
    }

    let node_binary = node_binary();
    let entrypoint = playwright_driver_entrypoint_path();
    if !entrypoint.is_file() {
        return PlaywrightRuntimeStatus::MissingDriverEntrypoint(entrypoint);
    }

    let browsers_dir = match playwright_browsers_dir() {
        Ok(path) => path,
        Err(error) => return PlaywrightRuntimeStatus::CheckFailed(error.to_string()),
    };
    let package_dir = playwright_driver_package_dir();

    let mut command = Command::new(&node_binary);
    command
        .arg("-e")
        .arg("const { chromium } = require('playwright'); process.stdout.write(chromium.executablePath());")
        .current_dir(package_dir)
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers_dir);

    match command.output().await {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                if stderr.contains("Cannot find module 'playwright'") {
                    return PlaywrightRuntimeStatus::MissingPackage;
                }
                return PlaywrightRuntimeStatus::CheckFailed(if stderr.is_empty() {
                    format!("Playwright runtime check exited with {}", output.status)
                } else {
                    stderr
                });
            }

            let executable_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if executable_path.is_empty() {
                return PlaywrightRuntimeStatus::MissingBrowser;
            }
            if Path::new(&executable_path).is_file() {
                PlaywrightRuntimeStatus::Ready
            } else {
                PlaywrightRuntimeStatus::MissingBrowser
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            PlaywrightRuntimeStatus::MissingNode
        }
        Err(error) => PlaywrightRuntimeStatus::CheckFailed(error.to_string()),
    }
}

/// Writes one embedded Playwright driver asset when it is missing or outdated.
fn write_driver_asset_if_needed(path: &Path, content: &str) -> Result<(), io::Error> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == content => Ok(()),
        Ok(_) => fs::write(path, content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::write(path, content),
        Err(error) => Err(error),
    }
}

/// Shared client that owns the long-lived Node Playwright driver process.
#[derive(Debug)]
struct PlaywrightDriverClient {
    state: Mutex<PlaywrightDriverState>,
}

impl PlaywrightDriverClient {
    /// Creates a new driver client using the default driver entrypoint configuration.
    fn new() -> Self {
        Self {
            state: Mutex::new(PlaywrightDriverState {
                next_request_id: 1,
                node_binary: node_binary(),
                entrypoint: playwright_driver_entrypoint_path(),
                process: None,
            }),
        }
    }

    /// Sends one browser action request to the long-lived Node driver and waits for its response.
    async fn send(
        &self,
        args: PlaywrightArgs,
    ) -> Result<PlaywrightDriverResponse, PlaywrightDriverClientError> {
        let mut state = self.state.lock().await;
        if state.process.is_none() {
            state.process =
                Some(spawn_driver_process(&state.node_binary, &state.entrypoint).await?);
        }

        let request_id = state.next_request_id;
        state.next_request_id += 1;
        let request = PlaywrightDriverRequest {
            id: request_id,
            payload: args,
        };
        let encoded = serde_json::to_string(&request)?;

        let Some(process) = state.process.as_mut() else {
            return Err(PlaywrightDriverClientError::MissingProcess);
        };

        if let Err(error) = process.stdin.write_all(encoded.as_bytes()).await {
            state.process = None;
            return Err(PlaywrightDriverClientError::Io(error));
        }
        if let Err(error) = process.stdin.write_all(b"\n").await {
            state.process = None;
            return Err(PlaywrightDriverClientError::Io(error));
        }
        if let Err(error) = process.stdin.flush().await {
            state.process = None;
            return Err(PlaywrightDriverClientError::Io(error));
        }

        let line = match process.stdout.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let status = process
                    .child
                    .wait()
                    .await
                    .ok()
                    .and_then(|status| status.code());
                state.process = None;
                return Err(PlaywrightDriverClientError::ProcessExited(match status {
                    Some(code) => format!("exit code {code}"),
                    None => "terminated without an exit code".to_owned(),
                }));
            }
            Err(error) => {
                state.process = None;
                return Err(PlaywrightDriverClientError::Io(error));
            }
        };

        let response: PlaywrightDriverResponse = serde_json::from_str(&line)?;
        if response.id != request_id {
            state.process = None;
            return Err(PlaywrightDriverClientError::MismatchedResponseId {
                expected: request_id,
                actual: response.id,
            });
        }
        Ok(response)
    }
}

/// Mutable state associated with the shared Playwright driver client.
#[derive(Debug)]
struct PlaywrightDriverState {
    next_request_id: u64,
    node_binary: String,
    entrypoint: PathBuf,
    process: Option<PlaywrightDriverProcess>,
}

/// Live Node driver process and its line-oriented stdin/stdout handles.
#[derive(Debug)]
struct PlaywrightDriverProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

/// Errors returned while managing or talking to the Node Playwright driver process.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum PlaywrightDriverClientError {
    #[error("failed to start playwright driver: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Path(#[from] PlaywrightPathError),
    #[error("playwright runtime is unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error("failed to encode or decode playwright driver JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("playwright driver process handle was unexpectedly missing")]
    MissingProcess,
    #[error("playwright driver exited before responding: {0}")]
    ProcessExited(String),
    #[error("playwright driver returned response id {actual} but {expected} was expected")]
    MismatchedResponseId { expected: u64, actual: u64 },
    #[error("playwright driver reported an error: {0}")]
    ReportedError(String),
}

/// Spawns the long-lived Node Playwright driver process.
async fn spawn_driver_process(
    node_binary: &str,
    entrypoint: &Path,
) -> Result<PlaywrightDriverProcess, PlaywrightDriverClientError> {
    match playwright_runtime_status().await {
        PlaywrightRuntimeStatus::Ready => {}
        PlaywrightRuntimeStatus::MissingNode => {
            return Err(PlaywrightDriverClientError::RuntimeUnavailable(
                "Node.js is not installed or is not on PATH".to_owned(),
            ));
        }
        PlaywrightRuntimeStatus::MissingDriverEntrypoint(path) => {
            return Err(PlaywrightDriverClientError::RuntimeUnavailable(format!(
                "the Playwright driver entrypoint is missing at {}",
                path.display()
            )));
        }
        PlaywrightRuntimeStatus::MissingPackage => {
            return Err(PlaywrightDriverClientError::RuntimeUnavailable(
                "the local Playwright package is not installed; run the Mirage browser install flow"
                    .to_owned(),
            ));
        }
        PlaywrightRuntimeStatus::MissingBrowser => {
            return Err(PlaywrightDriverClientError::RuntimeUnavailable(
                "the managed Chromium browser binary is not installed; run the Mirage browser install flow"
                    .to_owned(),
            ));
        }
        PlaywrightRuntimeStatus::CheckFailed(error) => {
            return Err(PlaywrightDriverClientError::RuntimeUnavailable(error));
        }
    }

    let state_root = playwright_state_root()?;
    let browsers_dir = playwright_browsers_dir()?;
    let profile_dir = playwright_default_profile_dir()?;
    let screenshots_dir = playwright_screenshots_dir()?;

    let mut command = Command::new(node_binary);
    command
        .arg(entrypoint)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .env("MIRAGE_PLAYWRIGHT_STATE_ROOT", &state_root)
        .env("MIRAGE_PLAYWRIGHT_PROFILE_DIR", &profile_dir)
        .env("MIRAGE_PLAYWRIGHT_SCREENSHOT_DIR", &screenshots_dir)
        .env("PLAYWRIGHT_BROWSERS_PATH", &browsers_dir);

    if let Some(parent) = entrypoint.parent() {
        command.current_dir(parent);
    }

    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or(PlaywrightDriverClientError::MissingProcess)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(PlaywrightDriverClientError::MissingProcess)?;

    Ok(PlaywrightDriverProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
    })
}

/// Returns the Node.js binary used to launch the Playwright driver.
fn node_binary() -> String {
    env::var("MIRAGE_PLAYWRIGHT_NODE_BINARY").unwrap_or_else(|_| "node".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{PlaywrightAction, PlaywrightArgs};
    use crate::tools::playwright_driver_assets::{
        PLAYWRIGHT_DRIVER_INDEX_JS, PLAYWRIGHT_DRIVER_PACKAGE_JSON,
    };

    /// Verifies that navigation requests require both a session id and URL.
    #[test]
    fn validate_rejects_navigate_without_url() {
        let args = PlaywrightArgs {
            action: PlaywrightAction::Navigate,
            session_id: Some("browser-1".to_owned()),
            url: None,
            selector: None,
            text: None,
            key: None,
            timeout_ms: None,
            path: None,
            wait_until: None,
        };

        let error = args.validate().unwrap_err().to_string();
        assert!(error.contains("`url` is required"));
    }

    /// Verifies that text extraction allows omitting the selector so the driver can default to `body`.
    #[test]
    fn validate_allows_extract_text_without_selector() {
        let args = PlaywrightArgs {
            action: PlaywrightAction::ExtractText,
            session_id: Some("browser-1".to_owned()),
            url: None,
            selector: None,
            text: None,
            key: None,
            timeout_ms: None,
            path: None,
            wait_until: None,
        };

        assert!(args.validate().is_ok());
    }

    /// Verifies that the embedded Playwright driver assets are compiled into the Mirage binary.
    #[test]
    fn embeds_playwright_driver_assets() {
        assert!(PLAYWRIGHT_DRIVER_INDEX_JS.contains("handleRequest"));
        assert!(PLAYWRIGHT_DRIVER_PACKAGE_JSON.contains("\"playwright\""));
    }
}
