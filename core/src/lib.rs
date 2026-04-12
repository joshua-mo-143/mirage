//! Core Mirage abstractions built directly on top of Rig.
//!
//! Venice exposes an OpenAI-compatible chat completions API, so this crate keeps
//! only the Venice-specific configuration layer and re-exports Rig's agent,
//! tool, and prompting APIs for higher-level use.

pub mod session;
pub mod tools;
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
