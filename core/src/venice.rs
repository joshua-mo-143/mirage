use rig::agent::{Agent, AgentBuilder};
use rig::http_client::Error as RigClientError;
use rig::prelude::CompletionClient;
use rig::providers::openai;
use std::env;
use std::fmt;
use thiserror::Error;

const DEFAULT_AUTHORITY: &str = "api.venice.ai";
const DEFAULT_BASE_PATH: &str = "/api/v1";

pub type VeniceCompletionsClient = openai::CompletionsClient;
pub type VeniceCompletionModel = openai::CompletionModel;
pub type VeniceAgentBuilder = AgentBuilder<VeniceCompletionModel>;
pub type VeniceAgent = Agent<VeniceCompletionModel>;

/// Secret API key wrapper that avoids leaking the key in debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct VeniceApiKey(String);

impl VeniceApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for VeniceApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VeniceApiKey(REDACTED)")
    }
}

/// Provider configuration independent from the transport implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VeniceConfig {
    pub authority: String,
    pub base_path: String,
    pub api_key: VeniceApiKey,
}

impl VeniceConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            authority: DEFAULT_AUTHORITY.to_owned(),
            base_path: DEFAULT_BASE_PATH.to_owned(),
            api_key: VeniceApiKey::new(api_key),
        }
    }

    pub fn from_env() -> Result<Self, VeniceConfigError> {
        let api_key = env::var("VENICE_API_KEY").map_err(|_| VeniceConfigError::MissingApiKey)?;
        Ok(Self::new(api_key))
    }

    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        self
    }

    pub fn with_base_path(mut self, base_path: impl Into<String>) -> Self {
        self.base_path = normalize_base_path(base_path.into());
        self
    }

    pub fn base_url(&self) -> String {
        format!(
            "https://{}{}",
            self.authority.trim().trim_end_matches('/'),
            normalize_base_path(self.base_path.clone())
        )
    }

    pub fn chat_completions_path(&self) -> String {
        format!(
            "{}/chat/completions",
            normalize_base_path(self.base_path.clone())
        )
    }

    pub fn chat_completions_url(&self) -> String {
        format!("{}{}", self.base_url(), self.chat_completions_path())
    }
}

/// Errors while constructing Venice configuration.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VeniceConfigError {
    #[error("VENICE_API_KEY is not set")]
    MissingApiKey,
}

/// Errors while talking to the Venice chat completions API.
#[derive(Debug, Error)]
pub enum VeniceError {
    #[error(transparent)]
    Config(#[from] VeniceConfigError),
    #[error("failed to build Venice Rig client: {0}")]
    BuildClient(#[from] RigClientError),
}

/// Venice client backed directly by Rig's OpenAI-compatible completions API.
#[derive(Debug, Clone)]
pub struct VeniceClient {
    config: VeniceConfig,
    inner: VeniceCompletionsClient,
}

impl VeniceClient {
    pub fn new(config: VeniceConfig) -> Result<Self, VeniceError> {
        let inner = openai::Client::builder()
            .api_key(config.api_key.expose())
            .base_url(config.base_url())
            .build()?
            .completions_api();

        Ok(Self { config, inner })
    }

    pub fn from_env() -> Result<Self, VeniceError> {
        Self::new(VeniceConfig::from_env()?)
    }

    pub fn config(&self) -> &VeniceConfig {
        &self.config
    }

    pub fn rig_client(&self) -> &VeniceCompletionsClient {
        &self.inner
    }

    pub fn into_rig_client(self) -> VeniceCompletionsClient {
        self.inner
    }

    pub fn completion_model(&self, model: impl Into<String>) -> VeniceCompletionModel {
        self.inner.completion_model(model)
    }

    pub fn agent(&self, model: impl Into<String>) -> VeniceAgentBuilder {
        self.inner.agent(model)
    }
}

fn normalize_base_path(base_path: String) -> String {
    let trimmed = base_path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::{VeniceClient, VeniceConfig};
    use crate::completion::ToolDefinition;
    use crate::tool::Tool;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize)]
    struct EchoArgs {
        text: String,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("echo tool failed")]
    struct EchoError;

    struct EchoTool;

    impl Tool for EchoTool {
        const NAME: &'static str = "echo";

        type Error = EchoError;
        type Args = EchoArgs;
        type Output = String;

        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_owned(),
                description: "Echo a string back to the model.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The text to echo."
                        }
                    },
                    "required": ["text"]
                }),
            }
        }

        async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(args.text)
        }
    }

    #[test]
    fn builds_expected_base_url() {
        let config = VeniceConfig::new("test-key")
            .with_authority("example.com")
            .with_base_path("custom/api");

        assert_eq!(config.base_url(), "https://example.com/custom/api");
        assert_eq!(
            config.chat_completions_path(),
            "/custom/api/chat/completions"
        );
    }

    #[tokio::test]
    async fn agent_builder_accepts_rig_tools() {
        let client = VeniceClient::new(VeniceConfig::new("test-key")).unwrap();

        let _agent = client
            .agent("venice-uncensored")
            .preamble("Use tools when they help.")
            .tool(EchoTool)
            .build();
    }
}
