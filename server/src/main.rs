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
    session::{StreamEvent, summarize_tool_call},
    streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt},
    tools::{
        bash_tool::BashTool,
        cursor_session::CursorSessionStore,
        file_tools::{EditFileTool, ReadFileTool, WriteFileTool},
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

#[derive(Debug, Clone)]
struct ServerConfig {
    bind_addr: String,
    admin_api_key: String,
    service: ServiceConfig,
    system_prompt: Option<String>,
    telegram: TelegramConfig,
}

impl ServerConfig {
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
        let system_prompt = env::var("VENICE_SYSTEM_PROMPT").ok();
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

        Ok(Self {
            bind_addr,
            admin_api_key,
            service: ServiceConfig {
                model,
                max_turns,
                authority,
                base_path,
                uncensored,
                system_prompt_configured: system_prompt.is_some(),
            },
            system_prompt,
            telegram: TelegramConfig {
                bot_token: env::var("TELEGRAM_BOT_TOKEN").ok(),
                default_chat_id: env::var("TELEGRAM_DEFAULT_CHAT_ID").ok(),
            },
        })
    }
}

#[derive(Debug, Clone)]
struct TelegramConfig {
    bot_token: Option<String>,
    default_chat_id: Option<String>,
}

#[derive(Clone)]
struct ServerState {
    admin_api_key: Arc<String>,
    venice_client: VeniceClient,
    base_service_config: ServiceConfig,
    default_system_prompt: Option<String>,
    sessions: Arc<RwLock<HashMap<String, Arc<SessionRuntime>>>>,
    scheduler: Arc<SchedulerState>,
    http_client: reqwest::Client,
    telegram: TelegramConfig,
    cursor_sessions: Arc<CursorSessionStore>,
    stream_debug_logger: Option<StreamDebugLogger>,
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

struct SessionRuntime {
    service: Mutex<SessionService>,
    system_prompt: Option<String>,
    events_tx: broadcast::Sender<SessionSnapshot>,
}

#[derive(Default)]
struct SchedulerState {
    jobs: Mutex<HashMap<String, ScheduledJob>>,
}

#[derive(Debug, Clone)]
struct ScheduledJob {
    id: String,
    every: Duration,
    next_run_at: Instant,
    task: ScheduledTask,
}

#[derive(Debug, Clone)]
enum ScheduledTask {
    TelegramHello { text: String, chat_id: String },
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ApiError {}

impl IntoResponse for ApiError {
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

type ApiResult<T> = Result<Json<T>, ApiError>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
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
        sessions: Arc::new(RwLock::new(HashMap::new())),
        scheduler: Arc::new(SchedulerState::default()),
        http_client: reqwest::Client::new(),
        telegram: server_config.telegram,
        cursor_sessions: Arc::new(CursorSessionStore::default()),
        stream_debug_logger,
        shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
    });

    tokio::spawn(run_scheduler(state.clone()));

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

async fn shutdown_signal(mut shutdown_rx: oneshot::Receiver<()>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = &mut shutdown_rx => {}
    }
}

async fn health(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> ApiResult<HealthResponse> {
    require_admin(&state, &headers)?;
    Ok(Json(HealthResponse {
        status: "ok".to_owned(),
    }))
}

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

async fn create_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    payload: Option<Json<CreateSessionRequest>>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;

    let request = payload.map(|json| json.0).unwrap_or_default();
    let session_id = Uuid::new_v4().to_string();
    let system_prompt = request
        .system_prompt
        .or_else(|| state.default_system_prompt.clone());
    let mut config = state.base_service_config.clone();
    config.system_prompt_configured = system_prompt.is_some();
    let service = SessionService::new(config, system_prompt.as_deref());
    let snapshot = snapshot_from_service(&session_id, &service);
    let (events_tx, _) = broadcast::channel(128);
    let runtime = Arc::new(SessionRuntime {
        service: Mutex::new(service),
        system_prompt,
        events_tx,
    });

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), runtime.clone());
    let _ = runtime.events_tx.send(snapshot.clone());

    Ok(Json(snapshot))
}

async fn get_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;
    let runtime = get_session_runtime(&state, &id).await?;
    Ok(Json(runtime.snapshot(&id).await))
}

async fn submit_message(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SubmitMessageRequest>,
) -> ApiResult<SessionSnapshot> {
    require_admin(&state, &headers)?;
    let runtime = get_session_runtime(&state, &id).await?;

    let prompt_request = {
        let mut service = runtime.service.lock().await;
        if !service.can_submit(&request.prompt) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "session is already streaming or the prompt was empty",
            ));
        }
        let prompt_request = service.submit_prompt(request.prompt);
        let snapshot = snapshot_from_service(&id, &service);
        let _ = runtime.events_tx.send(snapshot);
        prompt_request
    };

    tokio::spawn(run_prompt(
        state.clone(),
        runtime.clone(),
        id.clone(),
        prompt_request,
    ));

    Ok(Json(runtime.snapshot(&id).await))
}

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
        runtime.system_prompt.as_deref(),
        subagent_tx,
    );
    let mut stream = agent
        .stream_prompt(request.prompt)
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
            break;
        }
    }
}

fn build_agent(
    venice_client: &VeniceClient,
    cursor_sessions: Arc<CursorSessionStore>,
    model: &str,
    uncensored: bool,
    max_turns: usize,
    system_prompt: Option<&str>,
    subagent_tx: mpsc::UnboundedSender<mirage_core::session::SubagentProgressEvent>,
) -> mirage_core::VeniceAgent {
    let mut agent_builder = venice_client
        .agent(model.to_owned())
        .default_max_turns(max_turns)
        .additional_params(serde_json::json!({
            "venice_parameters": {
                "include_venice_system_prompt": uncensored
            }
        }))
        .append_preamble(
            "Tool usage guidance:
- Prefer discovering capabilities by using `bash` instead of assuming what commands, binaries, files, or directories are available.
- Use `bash` freely for arbitrary shell commands, environment inspection, and capability discovery.
- Use `subagent` when you want to delegate a deeper investigation or planning task to a child Cursor agent and incorporate its final answer.
- Use `read_file` to inspect files before editing them when needed.
- Prefer `edit_file` for modifying part of an existing file.
- Use `write_file` only when creating a new file, replacing an entire file, or appending whole-file content intentionally.
- Use `prompt_cursor` when you want the local Cursor agent CLI (`agent -p`) to answer or inspect something.",
        );

    if let Some(system_prompt) = system_prompt {
        agent_builder = agent_builder.preamble(system_prompt);
    }

    agent_builder
        .tool(BashTool)
        .tool(PromptCursorTool::new(cursor_sessions.clone()))
        .tool(SubagentTool::new(subagent_tx, cursor_sessions))
        .tool(ReadFileTool)
        .tool(EditFileTool)
        .tool(WriteFileTool)
        .build()
}

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

async fn execute_job(state: &ServerState, job: &ScheduledJob) -> Result<(), ApiError> {
    match &job.task {
        ScheduledTask::TelegramHello { text, chat_id } => {
            send_telegram_message(state, chat_id, text).await
        }
    }
}

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

impl SessionRuntime {
    async fn snapshot(&self, id: &str) -> SessionSnapshot {
        let service = self.service.lock().await;
        snapshot_from_service(id, &service)
    }
}

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

fn serialize_snapshot(snapshot: &SessionSnapshot) -> String {
    serde_json::to_string(snapshot).unwrap_or_else(|_| "{\"error\":\"snapshot\"}".to_owned())
}

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

fn parse_env_usize(value: &str) -> Result<usize, ApiError> {
    value.trim().parse::<usize>().map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("invalid integer value `{value}`"),
        )
    })
}
