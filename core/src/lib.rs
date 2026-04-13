//! Core Mirage abstractions built directly on top of Rig.
//!
//! Venice exposes an OpenAI-compatible chat completions API, so this crate keeps
//! only the Venice-specific configuration layer and re-exports Rig's agent,
//! tool, and prompting APIs for higher-level use.
#![warn(missing_docs)]

/// Shared JSONL stream-debug logging utilities.
pub mod debug_stream;
/// Runtime personality loading helpers.
pub mod personality;
/// Shared Mirage preamble and prompt composition helpers.
pub mod prompts;
/// Reducer-backed session state and transcript modeling.
pub mod session;
/// Request-scoped skill loading, matching, and prompt assembly helpers.
pub mod skills;
/// Local tool implementations exposed to Mirage agents.
pub mod tools;
/// Venice provider configuration and client wrappers.
pub mod venice;

pub use rig::agent::{Agent, AgentBuilder, NoToolConfig, WithBuilderTools, WithToolServerHandle};
pub use rig::completion::{Chat, Completion, CompletionError, Prompt, PromptError};
pub use rig::tool::{
    Tool, ToolDyn, ToolEmbedding, ToolEmbeddingDyn, ToolError, ToolSet, ToolSetError,
};
pub use rig::{OneOrMany, agent, completion, message, prelude, providers, streaming, tool};

pub use venice::{
    VeniceAgent, VeniceAgentBuilder, VeniceApiKey, VeniceClient, VeniceCompletionModel,
    VeniceCompletionsClient, VeniceConfig, VeniceConfigError, VeniceError,
};
