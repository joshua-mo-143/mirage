//! Mirage HTTP server, SSE transport, scheduler, and Telegram bot entrypoint.
#![warn(missing_docs)]

use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use mirage_core::{
    VeniceClient, VeniceConfig,
    agent::{MultiTurnStreamItem, Text},
    debug_stream::StreamDebugLogger,
    personality::load_runtime_personality,
    prompts::{build_mirage_preamble, has_custom_prompt_configuration, resolve_system_prompt},
    session::{StreamEvent, TranscriptItem, TranscriptKind, summarize_tool_call},
    skills::ResolvedSkill,
    streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt},
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
    PromptRequest, ServiceConfig, SessionService,
    api::{
        CreateSessionRequest, ErrorResponse, HealthResponse, ScheduleTelegramHelloRequest,
        ScheduledJobResponse, SessionSnapshot, SubmitMessageRequest,
    },
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    convert::Infallible,
    env,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, broadcast, mpsc, oneshot},
    time,
};
use uuid::Uuid;

/// Process-wide configuration loaded from environment variables at startup.
#[derive(Debug, Clone)]
struct ServerConfig {
    bind_addr: String,
    admin_api_key: String,
    service: ServiceConfig,
    system_prompt: Option<String>,
    personality: Option<String>,
    telegram: TelegramConfig,
}

impl ServerConfig {
    /// Builds the server configuration from environment variables.
    fn from_env() -> Result<Self, ApiError> {
        let bind_addr =
            env::var("MIRAGE_SERVER_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_owned());
        let admin_api_key = env::var("MIRAGE_ADMIN_API_KEY").map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MIRAGE_ADMIN_API_KEY must be set",
            )
        })?;
        let model =
            env::var("VENICE_MODEL").unwrap_or_else(|_| "arcee-trinity-large-thinking".to_owned());
        let system_prompt = resolve_system_prompt();
        let personality = load_runtime_personality().map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load Mirage personality: {error}"),
            )
        })?;
        let uncensored = env::var("MIRAGE_UNCENSORED")
            .ok()
            .as_deref()
            .map(parse_env_bool)
            .transpose()?
            .unwrap_or(false);
        let max_turns = env::var("MIRAGE_MAX_TURNS")
            .ok()
            .as_deref()
            .map(parse_env_usize)
            .transpose()?
            .unwrap_or(100);
        let authority = env::var("MIRAGE_AUTHORITY").unwrap_or_else(|_| "api.venice.ai".to_owned());
        let base_path = env::var("MIRAGE_BASE_PATH").unwrap_or_else(|_| "/api/v1".to_owned());
        let default_chat_id = env::var("TELEGRAM_DEFAULT_CHAT_ID").ok();
        let allowed_chat_ids = env::var("TELEGRAM_ALLOWED_CHAT_IDS")
            .ok()
            .map(|value| parse_csv_list(&value))
            .filter(|values| !values.is_empty())
            .unwrap_or_else(|| {
                default_chat_id
                    .as_ref()
                    .map(|chat_id| vec![chat_id.clone()])
                    .unwrap_or_default()
            });

        Ok(Self {
            bind_addr,
            admin_api_key,
            service: ServiceConfig {
                model,
                max_turns,
                authority,
                base_path,
                uncensored,
                system_prompt_configured: has_custom_prompt_configuration(
                    system_prompt.as_deref(),
                    personality.as_deref(),
                ),
            },
            system_prompt,
            personality,
            telegram: TelegramConfig {
                bot_token: env::var("TELEGRAM_BOT_TOKEN").ok(),
                default_chat_id,
                allowed_chat_ids,
            },
        })
    }
}

/// Telegram-specific configuration for the example scheduled job flow.
#[derive(Debug, Clone)]
struct TelegramConfig {
    bot_token: Option<String>,
    default_chat_id: Option<String>,
    allowed_chat_ids: Vec<String>,
}

/// Shared application state held by the Axum server.
#[derive(Clone)]
struct ServerState {
    admin_api_key: Arc<String>,
    venice_client: VeniceClient,
    base_service_config: ServiceConfig,
    default_system_prompt: Option<String>,
    default_personality: Option<String>,
    sessions: Arc<RwLock<HashMap<String, Arc<SessionRuntime>>>>,
    scheduler: Arc<SchedulerState>,
    http_client: reqwest::Client,
    telegram: TelegramConfig,
    telegram_sessions: Arc<Mutex<HashMap<String, String>>>,
    cursor_sessions: Arc<CursorSessionStore>,
    stream_debug_logger: Option<StreamDebugLogger>,
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

/// Runtime state associated with a single in-memory session.
struct SessionRuntime {
    service: Mutex<SessionService>,
    agent_preamble: String,
    events_tx: broadcast::Sender<SessionSnapshot>,
}

/// In-memory scheduler state containing all registered jobs.
#[derive(Default)]
struct SchedulerState {
    jobs: Mutex<HashMap<String, ScheduledJob>>,
}

/// Persisted description of a scheduled in-memory job.
#[derive(Debug, Clone)]
struct ScheduledJob {
    id: String,
    every: Duration,
    next_run_at: Instant,
    task: ScheduledTask,
}

/// Supported scheduled task variants handled by the in-process scheduler.
#[derive(Debug, Clone)]
enum ScheduledTask {
    TelegramHello { text: String, chat_id: String },
}

/// Internal HTTP error type converted into structured API responses.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

/// Telegram `getUpdates` envelope returned by the Bot API.
#[derive(Debug, Deserialize)]
struct TelegramUpdatesResponse {
    ok: bool,
    result: Vec<TelegramUpdate>,
}

/// Single Telegram update delivered by the Bot API.
#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

/// Minimal Telegram message payload used by Mirage chat ingestion.
#[derive(Debug, Clone, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
}

/// Minimal Telegram chat payload used for chat-scoped session mapping.
#[derive(Debug, Clone, Deserialize)]
struct TelegramChat {
    id: i64,
}

/// Supported slash commands for the Telegram chat interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelegramCommand {
    Start,
    Help,
    New,
    Clear,
    Status,
}

impl ApiError {
    /// Creates a new API error with the provided status code and message.
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    /// Formats the human-readable error message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ApiError {}

impl IntoResponse for ApiError {
    /// Converts an internal API error into an Axum JSON response.
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

/// Convenience result alias used by JSON-returning API handlers.
type ApiResult<T> = Result<Json<T>, ApiError>;

/// Starts the Mirage Axum server and the in-process scheduler.
pub async fn run() -> Result<(), Box<dyn Error>> {
    let server_config = ServerConfig::from_env()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let stream_debug_logger =
        StreamDebugLogger::from_optional_path_or_env("MIRAGE_DEBUG_STREAM_LOG", None)?;
    let venice_config = VeniceConfig::from_env()?
        .with_authority(server_config.service.authority.clone())
        .with_base_path(server_config.service.base_path.clone());
    let venice_client = VeniceClient::new(venice_config)?;
    let state = Arc::new(ServerState {
        admin_api_key: Arc::new(server_config.admin_api_key),
        venice_client,
        base_service_config: server_config.service,
        default_system_prompt: server_config.system_prompt,
        default_personality: server_config.personality,
        sessions: Arc::new(RwLock::new(HashMap::new())),
        scheduler: Arc::new(SchedulerState::default()),
        http_client: reqwest::Client::new(),
        telegram: server_config.telegram,
        telegram_sessions: Arc::new(Mutex::new(HashMap::new())),
        cursor_sessions: Arc::new(CursorSessionStore::default()),
        stream_debug_logger,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
    });

    tokio::spawn(run_scheduler(state.clone()));
    if state.telegram.bot_token.is_some() {
        tokio::spawn(run_telegram_bot(state.clone()));
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/shutdown", post(shutdown))
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/messages", post(submit_message))
        .route("/sessions/{id}/events", get(stream_session_events))
        .route("/jobs/telegram/hello", post(schedule_telegram_hello))
        .with_state(state.clone());

    let listener = TcpListener::bind(&server_config.bind_addr).await?;
    println!("mirage-server listening on {}", server_config.bind_addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
        .await?;
    Ok(())
}

/// Starts the Mirage Axum server and the in-process scheduler.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    run().await
}

/// Waits for either Ctrl+C or an authenticated shutdown request.
async fn shutdown_signal(mut shutdown_rx: oneshot::Receiver<()>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = &mut shutdown_rx => {}
    }
}

/// Returns a simple authenticated health response.
async fn health(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> ApiResult<HealthResponse> {
    require_admin(&state, &headers)?;
    Ok(Json(HealthResponse {
        status: "ok".to_owned(),
    }))
}

/// Gracefully shuts the server down after an authenticated request.
async fn shutdown(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> ApiResult<HealthResponse> {
    require_admin(&state, &headers)?;
    if let Some(shutdown_tx) = state.shutdown_tx.lock().await.take() {
        let _ = shutdown_tx.send(());
    }
    Ok(Json(HealthResponse {
        status: "shutting_down".to_owned(),
    }))
}

/// Creates a new in-memory session and returns its initial snapshot.
async fn create_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    _payload: Option<Json<CreateSessionRequest>>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;
    let (_, _, snapshot) = create_runtime(&state).await;
    Ok(Json(snapshot))
}

/// Returns the current snapshot for an existing session.
async fn get_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;
    let runtime = get_session_runtime(&state, &id).await?;
    Ok(Json(runtime.snapshot(&id).await))
}

/// Submits a prompt to an existing session and starts streaming its execution.
async fn submit_message(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SubmitMessageRequest>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;
    let runtime = get_session_runtime(&state, &id).await?;
    let prompt_request =
        begin_prompt(&runtime, &id, request.prompt, request.resolved_skills).await?;

    tokio::spawn(run_prompt(
        state.clone(),
        runtime.clone(),
        id.clone(),
        prompt_request,
    ));

    Ok(Json(runtime.snapshot(&id).await))
}

/// Polls Telegram for incoming chat messages and routes them into Mirage sessions.
async fn run_telegram_bot(state: Arc<ServerState>) {
    if state.telegram.allowed_chat_ids.is_empty() {
        eprintln!(
            "telegram bot loop not started because no allowed chat ids were configured; set TELEGRAM_ALLOWED_CHAT_IDS or TELEGRAM_DEFAULT_CHAT_ID"
        );
        return;
    }

    let mut next_update_offset = 0_i64;
    loop {
        match poll_telegram_updates(&state, next_update_offset).await {
            Ok(updates) => {
                for update in updates {
                    next_update_offset = next_update_offset.max(update.update_id.saturating_add(1));
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_telegram_update(state, update).await {
                            eprintln!("telegram update handling failed: {}", error);
                        }
                    });
                }
            }
            Err(error) => {
                eprintln!("telegram polling failed: {}", error);
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// Fetches the next batch of Telegram updates using long polling.
async fn poll_telegram_updates(
    state: &ServerState,
    offset: i64,
) -> Result<Vec<TelegramUpdate>, ApiError> {
    let bot_token = state.telegram.bot_token.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::FAILED_DEPENDENCY,
            "TELEGRAM_BOT_TOKEN is not configured",
        )
    })?;
    let url =
        format!("https://api.telegram.org/bot{bot_token}/getUpdates?timeout=30&offset={offset}");
    let response = state
        .http_client
        .get(url)
        .send()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "failed to read Telegram polling error body".to_owned());
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("telegram getUpdates failed with {status}: {body}"),
        ));
    }

    let payload: TelegramUpdatesResponse = response
        .json()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;
    if !payload.ok {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "telegram getUpdates returned ok=false",
        ));
    }
    Ok(payload.result)
}

/// Handles one Telegram update if it contains a supported text message.
async fn handle_telegram_update(
    state: Arc<ServerState>,
    update: TelegramUpdate,
) -> Result<(), ApiError> {
    let Some(message) = update.message else {
        return Ok(());
    };
    let Some(text) = message.text.as_deref().map(str::trim) else {
        return Ok(());
    };
    if text.is_empty() {
        return Ok(());
    }

    let chat_id = message.chat.id.to_string();
    if !telegram_chat_is_authorized(&state.telegram, &chat_id) {
        send_telegram_text(
            &state,
            &chat_id,
            "This Mirage Telegram bot is private. Your chat is not authorized.",
        )
        .await?;
        return Ok(());
    }

    if let Some(command) = parse_telegram_command(text) {
        handle_telegram_command(state, &chat_id, command).await
    } else {
        handle_telegram_prompt(state, &chat_id, text.to_owned()).await
    }
}

/// Handles a supported Telegram slash command.
async fn handle_telegram_command(
    state: Arc<ServerState>,
    chat_id: &str,
    command: TelegramCommand,
) -> Result<(), ApiError> {
    match command {
        TelegramCommand::Start | TelegramCommand::Help => {
            send_telegram_text(
                &state,
                chat_id,
                "Mirage is ready.\n\nCommands:\n/start or /help - show this help\n/new or /clear - start a fresh conversation\n/status - show the current session status\n\nAny other text will be sent to the agent.",
            )
            .await
        }
        TelegramCommand::New | TelegramCommand::Clear => {
            let _ = create_or_replace_telegram_session(&state, chat_id).await?;
            send_telegram_text(&state, chat_id, "Started a fresh Mirage conversation for this chat.")
                .await
        }
        TelegramCommand::Status => {
            let (session_id, runtime) = get_or_create_telegram_session(&state, chat_id).await?;
            let snapshot = runtime.snapshot(&session_id).await;
            send_telegram_text(
                &state,
                chat_id,
                &format!(
                    "Session: {}\nStatus: {}\nStreaming: {}\nTranscript items: {}",
                    snapshot.id,
                    snapshot.status,
                    if snapshot.streaming { "yes" } else { "no" },
                    snapshot.transcript.len()
                ),
            )
            .await
        }
    }
}

/// Submits a normal Telegram chat message to the mapped Mirage session and returns the final reply.
async fn handle_telegram_prompt(
    state: Arc<ServerState>,
    chat_id: &str,
    prompt: String,
) -> Result<(), ApiError> {
    send_telegram_chat_action(&state, chat_id, "typing").await?;
    let (session_id, runtime) = get_or_create_telegram_session(&state, chat_id).await?;
    let prompt_request = match begin_prompt(&runtime, &session_id, prompt, Vec::new()).await {
        Ok(request) => request,
        Err(error) if error.status == StatusCode::CONFLICT => {
            send_telegram_text(
                &state,
                chat_id,
                "Mirage is still working on the previous message for this chat. Wait for it to finish, or use /new to start a fresh conversation.",
            )
            .await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    run_prompt(
        state.clone(),
        runtime.clone(),
        session_id.clone(),
        prompt_request,
    )
    .await;
    let snapshot = runtime.snapshot(&session_id).await;
    let reply = telegram_reply_text(&snapshot);
    send_telegram_text(&state, chat_id, &reply).await
}

/// Streams session snapshots over Server-Sent Events for interactive clients.
async fn stream_session_events(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    require_admin(&state, &headers)?;
    let runtime = get_session_runtime(&state, &id).await?;
    let initial_snapshot = runtime.snapshot(&id).await;
    let mut rx = runtime.events_tx.subscribe();

    let event_stream = stream! {
        yield Ok(Event::default().event("snapshot").data(serialize_snapshot(&initial_snapshot)));
        loop {
            match rx.recv().await {
                Ok(snapshot) => {
                    yield Ok(Event::default().event("snapshot").data(serialize_snapshot(&snapshot)));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()))
}

/// Registers a repeating Telegram hello job in the in-memory scheduler.
async fn schedule_telegram_hello(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<ScheduleTelegramHelloRequest>,
) -> ApiResult<ScheduledJobResponse> {
    require_admin(&state, &headers)?;

    if request.every_seconds == 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "`every_seconds` must be greater than zero",
        ));
    }

    let bot_token = state.telegram.bot_token.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::FAILED_DEPENDENCY,
            "TELEGRAM_BOT_TOKEN must be set before scheduling Telegram jobs",
        )
    })?;
    let chat_id = request
        .chat_id
        .or_else(|| state.telegram.default_chat_id.clone())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "a `chat_id` must be provided or TELEGRAM_DEFAULT_CHAT_ID must be set",
            )
        })?;
    drop(bot_token);

    let text = request
        .text
        .unwrap_or_else(|| "Hello from Mirage.".to_owned());
    let job_id = Uuid::new_v4().to_string();
    let job = ScheduledJob {
        id: job_id.clone(),
        every: Duration::from_secs(request.every_seconds),
        next_run_at: Instant::now() + Duration::from_secs(request.every_seconds),
        task: ScheduledTask::TelegramHello {
            text: text.clone(),
            chat_id: chat_id.clone(),
        },
    };

    state
        .scheduler
        .jobs
        .lock()
        .await
        .insert(job_id.clone(), job);

    Ok(Json(ScheduledJobResponse {
        id: job_id,
        kind: "telegram_hello".to_owned(),
        every_seconds: request.every_seconds,
        text,
        chat_id,
    }))
}

/// Runs a parent-agent prompt to completion and broadcasts updated snapshots as events arrive.
async fn run_prompt(
    state: Arc<ServerState>,
    runtime: Arc<SessionRuntime>,
    session_id: String,
    request: PromptRequest,
) {
    let (model, uncensored, max_turns) = {
        let service = runtime.service.lock().await;
        (
            service.model().to_owned(),
            service.uncensored(),
            request.max_turns,
        )
    };

    let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
    let subagent_runtime = runtime.clone();
    let subagent_session_id = session_id.clone();
    tokio::spawn(async move {
        while let Some(event) = subagent_rx.recv().await {
            let snapshot = {
                let mut service = subagent_runtime.service.lock().await;
                service.apply_subagent_event(event);
                snapshot_from_service(&subagent_session_id, &service)
            };
            let _ = subagent_runtime.events_tx.send(snapshot);
        }
    });

    let agent = build_agent(
        &state.venice_client,
        state.cursor_sessions.clone(),
        &model,
        uncensored,
        max_turns,
        &runtime.agent_preamble,
        subagent_tx,
    );
    let mut stream = agent
        .stream_prompt(request.effective_prompt)
        .with_history(request.history)
        .multi_turn(request.max_turns)
        .await;

    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                Text { text },
            ))) => StreamEvent::AssistantText(text),
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                let name = tool_call.function.name;
                let summary = summarize_tool_call(&name, &tool_call.function.arguments);
                StreamEvent::ToolCall {
                    id: tool_call.id,
                    name,
                    summary,
                }
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => StreamEvent::ToolResult { id: tool_result.id },
            Ok(MultiTurnStreamItem::FinalResponse(final_response)) => {
                StreamEvent::Final(final_response)
            }
            Ok(_) => continue,
            Err(error) => StreamEvent::Error(error.to_string()),
        };

        if let Some(debug_logger) = &state.stream_debug_logger
            && let Err(error) = debug_logger.log_stream_event("server", Some(&session_id), &event)
        {
            eprintln!(
                "failed to write server stream debug event to {}: {}",
                debug_logger.path().display(),
                error
            );
        }

        let is_terminal = matches!(&event, StreamEvent::Final(_) | StreamEvent::Error(_));
        let snapshot = {
            let mut service = runtime.service.lock().await;
            service.apply_stream_event(event);
            snapshot_from_service(&session_id, &service)
        };
        let _ = runtime.events_tx.send(snapshot);

        if is_terminal {
            return;
        }
    }

    let event = StreamEvent::Error(
        "assistant stream ended before Mirage received a final response".to_owned(),
    );
    if let Some(debug_logger) = &state.stream_debug_logger
        && let Err(error) = debug_logger.log_stream_event("server", Some(&session_id), &event)
    {
        eprintln!(
            "failed to write server stream debug event to {}: {}",
            debug_logger.path().display(),
            error
        );
    }

    let snapshot = {
        let mut service = runtime.service.lock().await;
        service.apply_stream_event(event);
        snapshot_from_service(&session_id, &service)
    };
    let _ = runtime.events_tx.send(snapshot);
}

/// Begins a prompt on an existing runtime and broadcasts the updated streaming snapshot.
async fn begin_prompt(
    runtime: &SessionRuntime,
    session_id: &str,
    prompt: String,
    resolved_skills: Vec<ResolvedSkill>,
) -> Result<PromptRequest, ApiError> {
    let mut service = runtime.service.lock().await;
    if !service.can_submit(&prompt) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "session is already streaming or the prompt was empty",
        ));
    }
    let prompt_request = service.submit_prompt(prompt, resolved_skills);
    let snapshot = snapshot_from_service(session_id, &service);
    let _ = runtime.events_tx.send(snapshot);
    Ok(prompt_request)
}

/// Builds a Venice agent configured with the local tool runtime used by the server.
fn build_agent(
    venice_client: &VeniceClient,
    cursor_sessions: Arc<CursorSessionStore>,
    model: &str,
    uncensored: bool,
    max_turns: usize,
    agent_preamble: &str,
    subagent_tx: mpsc::UnboundedSender<mirage_core::session::SubagentProgressEvent>,
) -> mirage_core::VeniceAgent {
    let agent_builder = venice_client
        .agent(model.to_owned())
        .default_max_turns(max_turns)
        .preamble(agent_preamble)
        .additional_params(serde_json::json!({
            "venice_parameters": {
                "include_venice_system_prompt": uncensored
            }
        }));

    agent_builder
        .tool(BashTool)
        .tool(PlaywrightTool::new())
        .tool(PromptCursorTool::new(cursor_sessions.clone()))
        .tool(SubagentTool::new(subagent_tx, cursor_sessions))
        .tool(ReadFileTool)
        .tool(EditFileTool)
        .tool(WriteFileTool)
        .build()
}

/// Runs the in-process scheduler loop and executes due jobs.
async fn run_scheduler(state: Arc<ServerState>) {
    let mut interval = time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        let due_jobs = {
            let mut jobs = state.scheduler.jobs.lock().await;
            let now = Instant::now();
            let mut due = Vec::new();
            for job in jobs.values_mut() {
                if job.next_run_at <= now {
                    job.next_run_at = now + job.every;
                    due.push(job.clone());
                }
            }
            due
        };

        for job in due_jobs {
            if let Err(error) = execute_job(&state, &job).await {
                eprintln!("scheduled job {} failed: {}", job.id, error);
            }
        }
    }
}

/// Executes a single scheduled job.
async fn execute_job(state: &ServerState, job: &ScheduledJob) -> Result<(), ApiError> {
    match &job.task {
        ScheduledTask::TelegramHello { text, chat_id } => {
            send_telegram_message(state, chat_id, text).await
        }
    }
}

/// Sends a Telegram message using the configured bot token.
async fn send_telegram_message(
    state: &ServerState,
    chat_id: &str,
    text: &str,
) -> Result<(), ApiError> {
    let bot_token = state.telegram.bot_token.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::FAILED_DEPENDENCY,
            "TELEGRAM_BOT_TOKEN is not configured",
        )
    })?;
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let response = state
        .http_client
        .post(url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        }))
        .send()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "failed to read Telegram error body".to_owned());
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("telegram sendMessage failed with {status}: {body}"),
        ));
    }

    Ok(())
}

/// Sends a plain-text Telegram message, splitting it into multiple messages when needed.
async fn send_telegram_text(
    state: &ServerState,
    chat_id: &str,
    text: &str,
) -> Result<(), ApiError> {
    for chunk in split_telegram_message(text, 4096) {
        send_telegram_message(state, chat_id, &chunk).await?;
    }
    Ok(())
}

/// Sends a transient Telegram chat action such as `typing`.
async fn send_telegram_chat_action(
    state: &ServerState,
    chat_id: &str,
    action: &str,
) -> Result<(), ApiError> {
    let bot_token = state.telegram.bot_token.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::FAILED_DEPENDENCY,
            "TELEGRAM_BOT_TOKEN is not configured",
        )
    })?;
    let url = format!("https://api.telegram.org/bot{bot_token}/sendChatAction");
    let response = state
        .http_client
        .post(url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "action": action,
        }))
        .send()
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;

    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "failed to read Telegram chat action error body".to_owned());
        Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("telegram sendChatAction failed with {status}: {body}"),
        ))
    }
}

/// Returns whether a Telegram chat id is allowed to interact with Mirage.
fn telegram_chat_is_authorized(config: &TelegramConfig, chat_id: &str) -> bool {
    config
        .allowed_chat_ids
        .iter()
        .any(|allowed| allowed == chat_id)
}

/// Parses a supported Telegram slash command from the incoming message text.
fn parse_telegram_command(text: &str) -> Option<TelegramCommand> {
    let command = text.split_whitespace().next()?;
    match command.split('@').next()? {
        "/start" => Some(TelegramCommand::Start),
        "/help" => Some(TelegramCommand::Help),
        "/new" => Some(TelegramCommand::New),
        "/clear" => Some(TelegramCommand::Clear),
        "/status" => Some(TelegramCommand::Status),
        _ => None,
    }
}

/// Returns the final Telegram-visible reply extracted from a completed session snapshot.
fn telegram_reply_text(snapshot: &SessionSnapshot) -> String {
    for item in snapshot.transcript.iter().rev() {
        if let TranscriptItem::Entry(entry) = item {
            match entry.kind {
                TranscriptKind::Assistant if !entry.body.trim().is_empty() => {
                    return entry.body.clone();
                }
                TranscriptKind::Error if !entry.body.trim().is_empty() => {
                    return format!("Error: {}", entry.body);
                }
                _ => {}
            }
        }
    }

    if snapshot.streaming {
        "Mirage is still working on that request.".to_owned()
    } else if snapshot.status.trim().is_empty() {
        "Mirage finished, but there was no final assistant text to send.".to_owned()
    } else {
        snapshot.status.clone()
    }
}

/// Splits a long Telegram reply into chunks that fit within the platform message limit.
fn split_telegram_message(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec!["(empty)".to_owned()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0_usize;

    for line in text.lines() {
        let line_len = line.chars().count();
        if line_len > max_chars {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
                current_len = 0;
            }
            let mut segment = String::new();
            let mut segment_len = 0_usize;
            for ch in line.chars() {
                if segment_len == max_chars {
                    chunks.push(segment);
                    segment = String::new();
                    segment_len = 0;
                }
                segment.push(ch);
                segment_len += 1;
            }
            if !segment.is_empty() {
                current = segment;
                current_len = segment_len;
            }
            continue;
        }

        let additional_len = if current.is_empty() {
            line_len
        } else {
            1 + line_len
        };
        if current_len + additional_len > max_chars && !current.is_empty() {
            chunks.push(current);
            current = line.to_owned();
            current_len = line_len;
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
            current_len += additional_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push("(empty)".to_owned());
    }
    chunks
}

/// Returns the mapped session for a Telegram chat, creating one if necessary.
async fn get_or_create_telegram_session(
    state: &ServerState,
    chat_id: &str,
) -> Result<(String, Arc<SessionRuntime>), ApiError> {
    let existing_id = {
        let sessions = state.telegram_sessions.lock().await;
        sessions.get(chat_id).cloned()
    };
    if let Some(existing_id) = existing_id
        && let Ok(runtime) = get_session_runtime(state, &existing_id).await
    {
        return Ok((existing_id, runtime));
    }

    create_or_replace_telegram_session(state, chat_id).await
}

/// Creates a fresh Mirage session and maps it to the provided Telegram chat id.
async fn create_or_replace_telegram_session(
    state: &ServerState,
    chat_id: &str,
) -> Result<(String, Arc<SessionRuntime>), ApiError> {
    let (session_id, runtime, _) = create_runtime(state).await;
    state
        .telegram_sessions
        .lock()
        .await
        .insert(chat_id.to_owned(), session_id.clone());
    Ok((session_id, runtime))
}

/// Looks up a session runtime by id or returns a not-found error.
async fn get_session_runtime(
    state: &ServerState,
    id: &str,
) -> Result<Arc<SessionRuntime>, ApiError> {
    state
        .sessions
        .read()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("unknown session `{id}`")))
}

/// Creates and stores a new session runtime, returning its id, runtime, and initial snapshot.
async fn create_runtime(
    state: &ServerState,
) -> (String, Arc<SessionRuntime>, SessionSnapshot) {
    let session_id = Uuid::new_v4().to_string();
    let system_prompt = state.default_system_prompt.clone();
    let personality = state.default_personality.clone();
    let mut config = state.base_service_config.clone();
    config.system_prompt_configured =
        has_custom_prompt_configuration(system_prompt.as_deref(), personality.as_deref());
    let agent_preamble = build_mirage_preamble(system_prompt.as_deref(), personality.as_deref());
    let service = SessionService::new(config);
    let snapshot = snapshot_from_service(&session_id, &service);
    let (events_tx, _) = broadcast::channel(128);
    let runtime = Arc::new(SessionRuntime {
        service: Mutex::new(service),
        agent_preamble,
        events_tx,
    });

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), runtime.clone());
    let _ = runtime.events_tx.send(snapshot.clone());
    (session_id, runtime, snapshot)
}

impl SessionRuntime {
    /// Computes the current serialized snapshot for this runtime.
    async fn snapshot(&self, id: &str) -> SessionSnapshot {
        let service = self.service.lock().await;
        snapshot_from_service(id, &service)
    }
}

/// Converts a service instance into the wire-format session snapshot returned by the API.
fn snapshot_from_service(id: &str, service: &SessionService) -> SessionSnapshot {
    let status = service.status_snapshot();
    let session = service.session();

    SessionSnapshot {
        id: id.to_owned(),
        model: status.model,
        authority: status.authority,
        base_path: status.base_path,
        max_turns: status.max_turns,
        uncensored: status.uncensored,
        system_prompt_configured: status.system_prompt_configured,
        history_messages: status.history_messages,
        streaming: session.streaming,
        status: session.status.clone(),
        transcript: session.transcript.clone(),
    }
}

/// Serializes a session snapshot for SSE delivery.
fn serialize_snapshot(snapshot: &SessionSnapshot) -> String {
    serde_json::to_string(snapshot).unwrap_or_else(|_| "{\"error\":\"snapshot\"}".to_owned())
}

/// Verifies that the request carries valid admin credentials.
fn require_admin(state: &ServerState, headers: &HeaderMap) -> Result<(), ApiError> {
    let authorized = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value == state.admin_api_key.as_str())
        .unwrap_or(false)
        || headers
            .get("x-mirage-admin-key")
            .and_then(|value| value.to_str().ok())
            .map(|value| value == state.admin_api_key.as_str())
            .unwrap_or(false);

    if authorized {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "admin auth required",
        ))
    }
}

/// Parses a boolean environment variable value.
fn parse_env_bool(value: &str) -> Result<bool, ApiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("invalid boolean value `{value}`"),
        )),
    }
}

/// Parses an unsigned integer environment variable value into a `usize`.
fn parse_env_usize(value: &str) -> Result<usize, ApiError> {
    value.trim().parse::<usize>().map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("invalid integer value `{value}`"),
        )
    })
}

/// Parses a comma-separated environment variable into trimmed non-empty string values.
fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        TelegramCommand, parse_csv_list, parse_telegram_command, split_telegram_message,
        telegram_chat_is_authorized,
    };

    /// Verifies that Telegram command parsing ignores bot mentions and trailing arguments.
    #[test]
    fn parses_supported_telegram_commands() {
        assert_eq!(
            parse_telegram_command("/start"),
            Some(TelegramCommand::Start)
        );
        assert_eq!(
            parse_telegram_command("/status@mirage_bot extra"),
            Some(TelegramCommand::Status)
        );
        assert_eq!(parse_telegram_command("hello"), None);
    }

    /// Verifies that long Telegram replies are split into chunks within the maximum size.
    #[test]
    fn splits_telegram_messages_to_max_length() {
        let chunks = split_telegram_message("12345\n67890\nabcde", 5);
        assert_eq!(chunks, vec!["12345", "67890", "abcde"]);
    }

    /// Verifies that comma-separated env parsing trims values and drops empties.
    #[test]
    fn parses_csv_list_values() {
        assert_eq!(
            parse_csv_list(" 123 , , 456,789 "),
            vec!["123", "456", "789"]
        );
    }

    /// Verifies that Telegram chat authorization checks the configured allowlist.
    #[test]
    fn authorizes_only_allowed_telegram_chats() {
        let config = super::TelegramConfig {
            bot_token: Some("token".to_owned()),
            default_chat_id: Some("111".to_owned()),
            allowed_chat_ids: vec!["111".to_owned(), "222".to_owned()],
        };
        assert!(telegram_chat_is_authorized(&config, "222"));
        assert!(!telegram_chat_is_authorized(&config, "333"));
    }
}
