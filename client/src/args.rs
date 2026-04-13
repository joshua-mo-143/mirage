use clap::Parser;

/// Command-line arguments accepted by the Mirage TUI client.
#[derive(Debug, Parser)]
#[command(about = "Interactive Venice chat with a Cursor-style terminal UI")]
pub(crate) struct Args {
    /// Optional initial user prompt to send immediately on startup.
    pub(crate) prompt: Option<String>,

    /// Model name to request from the Venice API.
    #[arg(
        long,
        env = "VENICE_MODEL",
        default_value = "arcee-trinity-large-thinking"
    )]
    pub(crate) model: String,

    /// Optional system prompt prepended to the chat history.
    #[arg(long, env = "VENICE_SYSTEM_PROMPT")]
    pub(crate) system_prompt: Option<String>,

    /// Optional sampling temperature.
    #[arg(long)]
    pub(crate) temperature: Option<f32>,

    /// Optional response token cap.
    #[arg(long)]
    pub(crate) max_completion_tokens: Option<u32>,

    /// Enable Venice's built-in uncensoring system prompt (note: this will use more tokens!).
    #[arg(long, default_value_t = false)]
    pub(crate) uncensored: bool,

    /// Maximum Rig multi-turn depth so tool calls can continue before final text.
    #[arg(long, default_value_t = 100)]
    pub(crate) max_turns: usize,

    /// Override the API authority for testing.
    #[arg(long, default_value = "api.venice.ai")]
    pub(crate) authority: String,

    /// Override the API base path for testing.
    #[arg(long, default_value = "/api/v1")]
    pub(crate) base_path: String,

    /// Connect to a Mirage server instead of running the local backend.
    #[arg(long, env = "MIRAGE_SERVER_URL")]
    pub(crate) server_url: Option<String>,

    /// Admin API key for the Mirage server.
    #[arg(long, env = "MIRAGE_ADMIN_API_KEY")]
    pub(crate) admin_key: Option<String>,

    /// Force the local backend even if a remote server is configured.
    #[arg(long, default_value_t = false)]
    pub(crate) local: bool,

    /// Start a local Mirage server process before opening the TUI.
    #[arg(long, default_value_t = false)]
    pub(crate) start_server: bool,

    /// Stop a configured Mirage server and exit.
    #[arg(long, default_value_t = false)]
    pub(crate) stop_server: bool,

    /// Force-restart the local Mirage server before opening the TUI.
    #[arg(long, default_value_t = false)]
    pub(crate) restart_server: bool,

    /// Reattach to the most recent persisted TUI conversation instead of starting fresh.
    #[arg(long, default_value_t = false)]
    pub(crate) resume_last: bool,

    /// Append raw streamed assistant/tool events as JSONL for debugging.
    #[arg(long, env = "MIRAGE_DEBUG_STREAM_LOG")]
    pub(crate) debug_stream_log: Option<String>,
}
