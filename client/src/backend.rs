use crate::{args::Args, config::RemoteServerConfig, streaming::stream_agent_response};
use futures::StreamExt;
use mirage_core::{
    VeniceAgent, VeniceClient, VeniceConfig,
    debug_stream::StreamDebugLogger,
    session::{StreamEvent, SubagentProgressEvent},
    skills::ResolvedSkill,
    tools::{
        bash_tool::BashTool,
        cursor_session::CursorSessionStore,
        file_tools::{EditFileTool, ReadFileTool, WriteFileTool},
        playwright_tool::PlaywrightTool,
        prompt_cursor_tool::PromptCursorTool,
        subagent_tool::SubagentTool,
    },
};
use mirage_service::{
    ServiceConfig, SessionService,
    api::{
        CreateSessionRequest, ErrorResponse, HealthResponse, SessionSnapshot, SubmitMessageRequest,
    },
};
use reqwest::Url;
use serde::de::DeserializeOwned;
use std::{
    env,
    error::Error,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::sleep,
};

/// Events emitted by either the local or remote backend into the UI event loop.
pub(crate) enum BackendEvent {
    Stream(StreamEvent),
    Subagent(SubagentProgressEvent),
    RemoteSnapshot(SessionSnapshot),
    RemoteError(String),
}

/// Active backend implementation used by the Mirage client.
pub(crate) enum ClientBackend {
    Local(LocalBackend),
    Remote(RemoteBackend),
}

impl ClientBackend {
    /// Returns a human-readable description of the active backend.
    pub(crate) fn description(&self) -> String {
        match self {
            Self::Local(_) => "local".to_owned(),
            Self::Remote(backend) => format!("remote ({})", backend.server_url),
        }
    }

    /// Submits a prompt through whichever backend is currently active.
    pub(crate) fn submit_prompt(
        &mut self,
        service: &mut SessionService,
        prompt: String,
        resolved_skills: Vec<ResolvedSkill>,
    ) {
        match self {
            Self::Local(backend) => backend.submit_prompt(service, prompt, resolved_skills),
            Self::Remote(backend) => backend.submit_prompt(service, prompt, resolved_skills),
        }
    }

    /// Clears the current conversation using the active backend semantics.
    pub(crate) fn clear_conversation(&mut self, service: &mut SessionService) {
        match self {
            Self::Local(backend) => backend.clear_conversation(service),
            Self::Remote(backend) => backend.clear_conversation(service),
        }
    }
}

/// Builds either a local or remote backend along with its initial service state.
pub(crate) async fn build_backend(
    args: &Args,
    cursor_sessions: Arc<CursorSessionStore>,
    tx: mpsc::UnboundedSender<BackendEvent>,
    remote: Option<RemoteServerConfig>,
) -> Result<(SessionService, ClientBackend), Box<dyn Error>> {
    if let Some(remote) = remote {
        let (service, backend) = RemoteBackend::connect(args, remote, tx).await?;
        return Ok((service, ClientBackend::Remote(backend)));
    }

    let service = SessionService::new(
        service_config_from_args(args),
        args.system_prompt.as_deref(),
    );
    let backend = LocalBackend::new(args, cursor_sessions, tx)?;
    Ok((service, ClientBackend::Local(backend)))
}

/// Starts a local Mirage server process if one is not already reachable.
pub(crate) async fn launch_local_server(
    remote: &RemoteServerConfig,
    debug_stream_log: Option<&str>,
) -> Result<LaunchLocalServerResult, Box<dyn Error>> {
    if wait_for_server(remote, Duration::from_millis(300))
        .await
        .is_ok()
    {
        return Ok(LaunchLocalServerResult {
            already_running: true,
        });
    }

    let bind_addr = bind_addr_from_server_url(&remote.server_url)?;
    let startup_timeout = spawn_server_command(
        "mirage-server",
        &bind_addr,
        &remote.admin_api_key,
        debug_stream_log,
    )
    .map(|_| Duration::from_secs(15))
        .or_else(|primary_error| {
            let Some(workspace_root) = workspace_root() else {
                return Err(primary_error);
            };

            spawn_server_cargo_fallback(
                &workspace_root,
                &bind_addr,
                &remote.admin_api_key,
                debug_stream_log,
            )
                .map(|_| Duration::from_secs(90))
                .map_err(|fallback_error| {
                    format!(
                        "failed to launch `mirage-server` ({primary_error}); cargo fallback also failed ({fallback_error})"
                    )
                })
        })
        .map_err(|error| -> Box<dyn Error> { error.into() })?;

    wait_for_server(remote, startup_timeout)
        .await
        .map_err(|error| {
            format!(
                "{error}. If this was the first local server launch, it may still have been compiling in the background. Try again in a moment, or run with an explicit `--admin-key` so the started server is easier to reconnect to."
            )
        })?;
    Ok(LaunchLocalServerResult {
        already_running: false,
    })
}

/// Stops a configured Mirage server, preferring graceful shutdown when possible.
pub(crate) async fn stop_server(
    remote: &RemoteServerConfig,
) -> Result<StopServerResult, Box<dyn Error>> {
    let server_was_reachable = wait_for_server(remote, Duration::from_millis(300))
        .await
        .is_ok();

    if server_was_reachable {
        match request_server_shutdown(remote).await {
            Ok(_) => {
                wait_for_server_stop(remote, Duration::from_secs(15)).await?;
                return Ok(StopServerResult {
                    stopped: true,
                    method: StopServerMethod::HttpShutdown,
                });
            }
            Err(error) if is_local_server_url(&remote.server_url) => {
                if kill_local_server_processes()? {
                    wait_for_server_stop(remote, Duration::from_secs(15)).await?;
                    return Ok(StopServerResult {
                        stopped: true,
                        method: StopServerMethod::LocalProcessKill,
                    });
                }
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        }
    }

    if is_local_server_url(&remote.server_url) && kill_local_server_processes()? {
        wait_for_server_stop(remote, Duration::from_secs(15)).await?;
        return Ok(StopServerResult {
            stopped: true,
            method: StopServerMethod::LocalProcessKill,
        });
    }

    Ok(StopServerResult {
        stopped: false,
        method: StopServerMethod::NotRunning,
    })
}

/// Result returned after attempting to launch a local Mirage server.
pub(crate) struct LaunchLocalServerResult {
    pub(crate) already_running: bool,
}

/// Result returned after attempting to stop a Mirage server.
pub(crate) struct StopServerResult {
    pub(crate) stopped: bool,
    pub(crate) method: StopServerMethod,
}

/// Mechanism used to stop a Mirage server.
pub(crate) enum StopServerMethod {
    HttpShutdown,
    LocalProcessKill,
    NotRunning,
}

/// In-process backend that runs the Venice agent directly inside the client.
pub(crate) struct LocalBackend {
    agent: VeniceAgent,
    debug_logger: Option<StreamDebugLogger>,
    tx: mpsc::UnboundedSender<BackendEvent>,
}

impl LocalBackend {
    /// Builds the local backend and configures the full local tool runtime.
    fn new(
        args: &Args,
        cursor_sessions: Arc<CursorSessionStore>,
        tx: mpsc::UnboundedSender<BackendEvent>,
    ) -> Result<Self, Box<dyn Error>> {
        let debug_logger = StreamDebugLogger::from_optional_path_or_env(
            "MIRAGE_DEBUG_STREAM_LOG",
            args.debug_stream_log.as_deref(),
        )?;
        let config = VeniceConfig::from_env()?
            .with_authority(args.authority.clone())
            .with_base_path(args.base_path.clone());
        let client = VeniceClient::new(config)?;
        let mut agent_builder = client
            .agent(args.model.clone())
            .default_max_turns(args.max_turns);

        agent_builder = agent_builder.additional_params(serde_json::json!({
            "venice_parameters": {
                "include_venice_system_prompt": args.uncensored
            }
        }));

        if let Some(system_prompt) = args.system_prompt.as_deref() {
            agent_builder = agent_builder.preamble(system_prompt);
        }

        agent_builder = agent_builder.append_preamble(
            "Tool usage guidance:
- Prefer discovering capabilities by using `bash` instead of assuming what commands, binaries, files, or directories are available.
- Use `bash` freely for arbitrary shell commands, environment inspection, and capability discovery.
- Use `playwright` for headless browser automation when a task needs webpage interaction, form filling, visible text extraction, or screenshots.
- Use `subagent` when you want to delegate a deeper investigation or planning task to a child Cursor agent and incorporate its final answer.
- Use `read_file` to inspect files before editing them when needed.
- Prefer `edit_file` for modifying part of an existing file.
- Use `write_file` only when creating a new file, replacing an entire file, or appending whole-file content intentionally.
- Use `prompt_cursor` when you want the local Cursor agent CLI (`agent -p`) to answer or inspect something.",
        );

        if let Some(temperature) = args.temperature {
            agent_builder = agent_builder.temperature(f64::from(temperature));
        }

        if let Some(max_tokens) = args.max_completion_tokens {
            agent_builder = agent_builder.max_tokens(u64::from(max_tokens));
        }

        let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
        let subagent_events_tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = subagent_rx.recv().await {
                let _ = subagent_events_tx.send(BackendEvent::Subagent(event));
            }
        });

        let agent = agent_builder
            .tool(BashTool)
            .tool(PlaywrightTool::new())
            .tool(PromptCursorTool::new(cursor_sessions.clone()))
            .tool(SubagentTool::new(subagent_tx, cursor_sessions.clone()))
            .tool(ReadFileTool)
            .tool(EditFileTool)
            .tool(WriteFileTool)
            .build();

        Ok(Self {
            agent,
            debug_logger,
            tx,
        })
    }

    /// Starts a local prompt run and forwards streamed events back into the UI loop.
    fn submit_prompt(
        &mut self,
        service: &mut SessionService,
        prompt: String,
        resolved_skills: Vec<ResolvedSkill>,
    ) {
        let request = service.submit_prompt(prompt, resolved_skills);
        let agent = self.agent.clone();
        let debug_logger = self.debug_logger.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            stream_agent_response(
                agent,
                request.effective_prompt,
                request.history,
                request.max_turns,
                tx,
                debug_logger,
            )
            .await;
        });
    }

    /// Clears the local conversation while preserving the backend instance.
    fn clear_conversation(&mut self, service: &mut SessionService) {
        service.clear_with_notice(
            "Conversation cleared, including Cursor session state.",
            "Cleared conversation history and Cursor session state.",
        );
    }
}

/// Remote HTTP/SSE backend that talks to a Mirage server.
pub(crate) struct RemoteBackend {
    http_client: reqwest::Client,
    server_url: String,
    admin_api_key: String,
    system_prompt: Option<String>,
    session_state: Arc<Mutex<RemoteSessionState>>,
    tx: mpsc::UnboundedSender<BackendEvent>,
}

/// Mutable state associated with the currently connected remote session.
struct RemoteSessionState {
    session_id: String,
    events_task: Option<JoinHandle<()>>,
}

impl RemoteBackend {
    /// Connects to a remote Mirage server and creates the initial session.
    async fn connect(
        args: &Args,
        remote: RemoteServerConfig,
        tx: mpsc::UnboundedSender<BackendEvent>,
    ) -> Result<(SessionService, Self), Box<dyn Error>> {
        let http_client = reqwest::Client::new();
        let system_prompt = args.system_prompt.clone();
        let snapshot = create_remote_session(&http_client, &remote, system_prompt.clone()).await?;
        let mut service = SessionService::new(service_config_from_snapshot(&snapshot), None);
        service.apply_remote_snapshot(snapshot.clone());

        let backend = Self {
            http_client,
            server_url: remote.server_url,
            admin_api_key: remote.admin_api_key,
            system_prompt,
            session_state: Arc::new(Mutex::new(RemoteSessionState {
                session_id: snapshot.id.clone(),
                events_task: None,
            })),
            tx,
        };
        backend.restart_events_stream(snapshot.id).await;

        Ok((service, backend))
    }

    /// Submits a prompt to the currently selected remote session.
    fn submit_prompt(
        &self,
        service: &mut SessionService,
        prompt: String,
        resolved_skills: Vec<ResolvedSkill>,
    ) {
        service.session_mut().streaming = true;
        service.session_mut().status = "Submitting remote request...".to_owned();

        let http_client = self.http_client.clone();
        let server_url = self.server_url.clone();
        let admin_api_key = self.admin_api_key.clone();
        let session_state = Arc::clone(&self.session_state);
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let session_id = {
                let state = session_state.lock().await;
                state.session_id.clone()
            };

            match submit_remote_message(
                &http_client,
                &server_url,
                &admin_api_key,
                &session_id,
                prompt,
                resolved_skills,
            )
            .await
            {
                Ok(snapshot) => {
                    let _ = tx.send(BackendEvent::RemoteSnapshot(snapshot));
                }
                Err(error) => {
                    let _ = tx.send(BackendEvent::RemoteError(error));
                }
            }
        });
    }

    /// Clears the remote conversation by creating and switching to a new remote session.
    fn clear_conversation(&self, service: &mut SessionService) {
        service.session_mut().streaming = true;
        service.session_mut().status = "Creating new remote session...".to_owned();

        let http_client = self.http_client.clone();
        let server_url = self.server_url.clone();
        let admin_api_key = self.admin_api_key.clone();
        let system_prompt = self.system_prompt.clone();
        let session_state = Arc::clone(&self.session_state);
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let remote = RemoteServerConfig {
                server_url: server_url.clone(),
                admin_api_key: admin_api_key.clone(),
            };

            match create_remote_session(&http_client, &remote, system_prompt).await {
                Ok(snapshot) => {
                    if let Err(error) = replace_remote_session(
                        &http_client,
                        &server_url,
                        &admin_api_key,
                        Arc::clone(&session_state),
                        tx.clone(),
                        snapshot.id.clone(),
                    )
                    .await
                    {
                        let _ = tx.send(BackendEvent::RemoteError(error));
                        return;
                    }

                    let _ = tx.send(BackendEvent::RemoteSnapshot(snapshot));
                }
                Err(error) => {
                    let _ = tx.send(BackendEvent::RemoteError(error));
                }
            }
        });
    }

    /// Restarts the SSE stream task for the provided remote session id.
    async fn restart_events_stream(&self, session_id: String) {
        let _ = replace_remote_session(
            &self.http_client,
            &self.server_url,
            &self.admin_api_key,
            Arc::clone(&self.session_state),
            self.tx.clone(),
            session_id,
        )
        .await;
    }
}

/// Builds a service configuration from local client arguments.
fn service_config_from_args(args: &Args) -> ServiceConfig {
    ServiceConfig {
        model: args.model.clone(),
        max_turns: args.max_turns,
        authority: args.authority.clone(),
        base_path: args.base_path.clone(),
        uncensored: args.uncensored,
        system_prompt_configured: args.system_prompt.is_some(),
    }
}

/// Builds a service configuration from a remote session snapshot.
fn service_config_from_snapshot(snapshot: &SessionSnapshot) -> ServiceConfig {
    ServiceConfig {
        model: snapshot.model.clone(),
        max_turns: snapshot.max_turns,
        authority: snapshot.authority.clone(),
        base_path: snapshot.base_path.clone(),
        uncensored: snapshot.uncensored,
        system_prompt_configured: snapshot.system_prompt_configured,
    }
}

/// Creates a new remote session through the Mirage server API.
async fn create_remote_session(
    http_client: &reqwest::Client,
    remote: &RemoteServerConfig,
    system_prompt: Option<String>,
) -> Result<SessionSnapshot, String> {
    let response = http_client
        .post(api_url(&remote.server_url, "/sessions"))
        .bearer_auth(&remote.admin_api_key)
        .json(&CreateSessionRequest { system_prompt })
        .send()
        .await
        .map_err(|error| format!("failed to create remote session: {error}"))?;
    parse_json_response(response).await
}

/// Submits a prompt to an existing remote session through the Mirage server API.
async fn submit_remote_message(
    http_client: &reqwest::Client,
    server_url: &str,
    admin_api_key: &str,
    session_id: &str,
    prompt: String,
    resolved_skills: Vec<ResolvedSkill>,
) -> Result<SessionSnapshot, String> {
    let response = http_client
        .post(api_url(
            server_url,
            &format!("/sessions/{session_id}/messages"),
        ))
        .bearer_auth(admin_api_key)
        .json(&SubmitMessageRequest {
            prompt,
            resolved_skills,
        })
        .send()
        .await
        .map_err(|error| format!("failed to submit remote prompt: {error}"))?;
    parse_json_response(response).await
}

/// Replaces the tracked remote session id and restarts the associated SSE task.
async fn replace_remote_session(
    http_client: &reqwest::Client,
    server_url: &str,
    admin_api_key: &str,
    session_state: Arc<Mutex<RemoteSessionState>>,
    tx: mpsc::UnboundedSender<BackendEvent>,
    session_id: String,
) -> Result<(), String> {
    let events_task = tokio::spawn(stream_remote_session_events(
        http_client.clone(),
        server_url.to_owned(),
        admin_api_key.to_owned(),
        session_id.clone(),
        tx,
    ));

    let mut state = session_state.lock().await;
    if let Some(existing_task) = state.events_task.take() {
        existing_task.abort();
    }
    state.session_id = session_id;
    state.events_task = Some(events_task);
    Ok(())
}

/// Streams remote session snapshots from the server over SSE.
async fn stream_remote_session_events(
    http_client: reqwest::Client,
    server_url: String,
    admin_api_key: String,
    session_id: String,
    tx: mpsc::UnboundedSender<BackendEvent>,
) {
    let response = match http_client
        .get(api_url(
            &server_url,
            &format!("/sessions/{session_id}/events"),
        ))
        .bearer_auth(&admin_api_key)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = tx.send(BackendEvent::RemoteError(format!(
                "remote event stream connection failed: {error}"
            )));
            return;
        }
    };

    if !response.status().is_success() {
        let error = decode_error_response(response)
            .await
            .unwrap_or_else(|message| message);
        let _ = tx.send(BackendEvent::RemoteError(format!(
            "remote event stream failed: {error}"
        )));
        return;
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(frame) = take_next_sse_frame(&mut buffer) {
                    let Some(data) = extract_sse_data(&frame) else {
                        continue;
                    };

                    match serde_json::from_str::<SessionSnapshot>(&data) {
                        Ok(snapshot) => {
                            let _ = tx.send(BackendEvent::RemoteSnapshot(snapshot));
                        }
                        Err(error) => {
                            let _ = tx.send(BackendEvent::RemoteError(format!(
                                "failed to parse remote snapshot: {error}"
                            )));
                        }
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(BackendEvent::RemoteError(format!(
                    "remote event stream error: {error}"
                )));
                return;
            }
        }
    }
}

/// Extracts the next complete SSE frame from a buffered text stream.
fn take_next_sse_frame(buffer: &mut String) -> Option<String> {
    if let Some(index) = buffer.find("\r\n\r\n") {
        let frame = buffer[..index].replace("\r\n", "\n");
        buffer.drain(..index + 4);
        return Some(frame);
    }

    if let Some(index) = buffer.find("\n\n") {
        let frame = buffer[..index].to_owned();
        buffer.drain(..index + 2);
        return Some(frame);
    }

    None
}

/// Extracts and joins `data:` lines from a single SSE frame.
fn extract_sse_data(frame: &str) -> Option<String> {
    let data_lines = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.trim_start().to_owned())
        .collect::<Vec<_>>();

    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

/// Polls a Mirage server until it becomes healthy or the timeout expires.
async fn wait_for_server(
    remote: &RemoteServerConfig,
    timeout: Duration,
) -> Result<HealthResponse, String> {
    let deadline = Instant::now() + timeout;
    let http_client = reqwest::Client::new();
    let mut last_error = String::from("server did not respond");

    while Instant::now() < deadline {
        match http_client
            .get(api_url(&remote.server_url, "/health"))
            .bearer_auth(&remote.admin_api_key)
            .send()
            .await
        {
            Ok(response) => match parse_json_response::<HealthResponse>(response).await {
                Ok(health) => return Ok(health),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error.to_string(),
        }

        sleep(Duration::from_millis(250)).await;
    }

    Err(format!(
        "timed out waiting for Mirage server at {}: {last_error}",
        remote.server_url
    ))
}

/// Polls a Mirage server until it stops responding or the timeout expires.
async fn wait_for_server_stop(
    remote: &RemoteServerConfig,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let http_client = reqwest::Client::new();

    while Instant::now() < deadline {
        match http_client
            .get(api_url(&remote.server_url, "/health"))
            .send()
            .await
        {
            Ok(_) => sleep(Duration::from_millis(250)).await,
            Err(_) => return Ok(()),
        }
    }

    Err(format!(
        "timed out waiting for Mirage server at {} to stop",
        remote.server_url
    ))
}

/// Requests graceful shutdown from a Mirage server.
async fn request_server_shutdown(remote: &RemoteServerConfig) -> Result<HealthResponse, String> {
    let response = reqwest::Client::new()
        .post(api_url(&remote.server_url, "/shutdown"))
        .bearer_auth(&remote.admin_api_key)
        .send()
        .await
        .map_err(|error| format!("failed to request server shutdown: {error}"))?;
    parse_json_response(response).await
}

/// Decodes a JSON response body or converts HTTP and parsing failures into readable errors.
async fn parse_json_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read response body: {error}"))?;

    if !status.is_success() {
        if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(&bytes) {
            return Err(format!(
                "server returned {status}: {}",
                error_response.error
            ));
        }

        let body = String::from_utf8_lossy(&bytes);
        return Err(format!("server returned {status}: {body}"));
    }

    serde_json::from_slice::<T>(&bytes)
        .map_err(|error| format!("failed to decode response: {error}"))
}

/// Decodes an error response body into a readable string.
async fn decode_error_response(response: reqwest::Response) -> Result<String, String> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read error response: {error}"))?;

    if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(&body) {
        return Ok(format!(
            "server returned {status}: {}",
            error_response.error
        ));
    }

    Ok(format!(
        "server returned {status}: {}",
        String::from_utf8_lossy(&body)
    ))
}

/// Joins a server base URL with a relative API path.
fn api_url(server_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        server_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Derives a bind address from a server URL.
fn bind_addr_from_server_url(server_url: &str) -> Result<String, String> {
    let url = Url::parse(server_url).map_err(|error| format!("invalid server URL: {error}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| "server URL must include a host".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "server URL must include or imply a port".to_owned())?;
    Ok(format!("{host}:{port}"))
}

/// Returns whether the server URL points at a loopback address.
fn is_local_server_url(server_url: &str) -> bool {
    Url::parse(server_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"))
}

/// Attempts to terminate local Mirage server processes using `pkill`.
fn kill_local_server_processes() -> Result<bool, Box<dyn Error>> {
    let output = std::process::Command::new("pkill")
        .arg("-f")
        .arg("mirage-server")
        .output()?;

    match output.status.code().unwrap_or(-1) {
        0 => Ok(true),
        1 => Ok(false),
        status => Err(format!(
            "pkill failed with status {status}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()),
    }
}

/// Spawns a `mirage-server` binary with the required environment.
fn spawn_server_command(
    command_name: &str,
    bind_addr: &str,
    admin_api_key: &str,
    debug_stream_log: Option<&str>,
) -> Result<(), String> {
    let mut command = Command::new(command_name);
    command
        .env("MIRAGE_SERVER_BIND", bind_addr)
        .env("MIRAGE_ADMIN_API_KEY", admin_api_key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = debug_stream_log {
        command.env("MIRAGE_DEBUG_STREAM_LOG", path);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Spawns the server through `cargo run -p mirage-server` as a fallback.
fn spawn_server_cargo_fallback(
    workspace_root: &PathBuf,
    bind_addr: &str,
    admin_api_key: &str,
    debug_stream_log: Option<&str>,
) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("-p")
        .arg("mirage-server")
        .current_dir(workspace_root)
        .env("MIRAGE_SERVER_BIND", bind_addr)
        .env("MIRAGE_ADMIN_API_KEY", admin_api_key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = debug_stream_log {
        command.env("MIRAGE_DEBUG_STREAM_LOG", path);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Returns the workspace root if the client crate appears to live inside the Mirage workspace.
fn workspace_root() -> Option<PathBuf> {
    let client_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = client_dir.parent()?.to_path_buf();
    workspace_root
        .join("Cargo.toml")
        .is_file()
        .then_some(workspace_root)
}

#[cfg(test)]
mod tests {
    use super::{
        api_url, bind_addr_from_server_url, extract_sse_data, is_local_server_url,
        take_next_sse_frame,
    };

    /// Verifies that API URL joining avoids duplicate slashes.
    #[test]
    fn api_url_joins_without_duplicate_slashes() {
        assert_eq!(
            api_url("http://127.0.0.1:3000/", "/sessions"),
            "http://127.0.0.1:3000/sessions"
        );
    }

    /// Verifies that known default ports are inferred from the server URL scheme.
    #[test]
    fn bind_addr_uses_known_default_port() {
        assert_eq!(
            bind_addr_from_server_url("http://127.0.0.1").unwrap(),
            "127.0.0.1:80"
        );
    }

    /// Verifies loopback hostnames and addresses are treated as local servers.
    #[test]
    fn detects_local_server_urls() {
        assert!(is_local_server_url("http://127.0.0.1:3000"));
        assert!(is_local_server_url("http://localhost:3000"));
        assert!(!is_local_server_url("https://example.com"));
    }

    /// Verifies SSE frame parsing extracts the expected payload text.
    #[test]
    fn extracts_sse_frames_and_data() {
        let mut buffer = "event: snapshot\ndata: {\"id\":\"1\"}\n\n".to_owned();
        let frame = take_next_sse_frame(&mut buffer).unwrap();

        assert_eq!(extract_sse_data(&frame).unwrap(), "{\"id\":\"1\"}");
        assert!(buffer.is_empty());
    }
}
