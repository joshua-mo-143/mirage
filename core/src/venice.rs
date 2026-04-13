use rig::agent::{Agent, AgentBuilder};
use rig::http_client::Error as RigClientError;
use rig::prelude::CompletionClient;
use rig::providers::openai;
use std::env;
use std::fmt;
use thiserror::Error;

const DEFAULT_AUTHORITY: &str = "api.venice.ai";
const DEFAULT_BASE_PATH: &str = "/api/v1";

/// Concrete OpenAI-compatible completions client used to talk to Venice.
pub type VeniceCompletionsClient = openai::CompletionsClient;
/// Completion model handle returned by the underlying OpenAI-compatible client.
pub type VeniceCompletionModel = openai::CompletionModel;
/// Agent builder configured for the Venice completion model type.
pub type VeniceAgentBuilder = AgentBuilder<VeniceCompletionModel>;
/// Fully built agent configured against the Venice completion model type.
pub type VeniceAgent = Agent<VeniceCompletionModel>;

/// Secret API key wrapper that avoids leaking the key in debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct VeniceApiKey(String);

impl VeniceApiKey {
    /// Wraps an API key string so it can be carried without exposing it in debug output.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the raw API key string for transport-layer configuration.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for VeniceApiKey {
    /// Redacts the secret key when formatting for debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VeniceApiKey(REDACTED)")
    }
}

/// Provider configuration independent from the transport implementation.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VeniceConfig {
    pub authority: String,
    pub base_path: String,
    pub api_key: VeniceApiKey,
}

impl VeniceConfig {
    /// Creates a Venice configuration using the default authority and base path.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            authority: DEFAULT_AUTHORITY.to_owned(),
            base_path: DEFAULT_BASE_PATH.to_owned(),
            api_key: VeniceApiKey::new(api_key),
        }
    }

    /// Builds a Venice configuration from environment variables.
    pub fn from_env() -> Result<Self, VeniceConfigError> {
        let api_key = env::var("VENICE_API_KEY").map_err(|_| VeniceConfigError::MissingApiKey)?;
        Ok(Self::new(api_key))
    }

    /// Overrides the configured API authority.
    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        self
    }

    /// Overrides the configured API base path.
    pub fn with_base_path(mut self, base_path: impl Into<String>) -> Self {
        self.base_path = normalize_base_path(base_path.into());
        self
    }

    /// Returns the normalized base URL used for requests to the Venice API.
    pub fn base_url(&self) -> String {
        format!(
            "https://{}{}",
            self.authority.trim().trim_end_matches('/'),
            normalize_base_path(self.base_path.clone())
        )
    }

    /// Returns the normalized chat completions path relative to the base URL.
    pub fn chat_completions_path(&self) -> String {
        format!(
            "{}/chat/completions",
            normalize_base_path(self.base_path.clone())
        )
    }

    /// Returns the fully qualified chat completions URL.
    pub fn chat_completions_url(&self) -> String {
        format!("{}{}", self.base_url(), self.chat_completions_path())
    }
}

/// Errors while constructing Venice configuration.
#[allow(missing_docs)]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VeniceConfigError {
    #[error("VENICE_API_KEY is not set")]
    MissingApiKey,
}

/// Errors while talking to the Venice chat completions API.
#[allow(missing_docs)]
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
    /// Builds a Venice client from an explicit provider configuration.
    pub fn new(config: VeniceConfig) -> Result<Self, VeniceError> {
        let inner = openai::Client::builder()
            .api_key(config.api_key.expose())
            .base_url(config.base_url())
            .build()?
            .completions_api();

        Ok(Self { config, inner })
    }

    /// Builds a Venice client from environment variables.
    pub fn from_env() -> Result<Self, VeniceError> {
        Self::new(VeniceConfig::from_env()?)
    }

    /// Returns the provider configuration used by this client.
    pub fn config(&self) -> &VeniceConfig {
        &self.config
    }

    /// Returns the underlying Rig-compatible completions client.
    pub fn rig_client(&self) -> &VeniceCompletionsClient {
        &self.inner
    }

    /// Consumes the wrapper and returns the underlying Rig-compatible completions client.
    pub fn into_rig_client(self) -> VeniceCompletionsClient {
        self.inner
    }

    /// Returns a completion model handle for the provided model name.
    pub fn completion_model(&self, model: impl Into<String>) -> VeniceCompletionModel {
        self.inner.completion_model(model)
    }

    /// Returns an agent builder configured for the provided model name.
    pub fn agent(&self, model: impl Into<String>) -> VeniceAgentBuilder {
        self.inner.agent(model)
    }
}

/// Normalizes an API base path into a leading-slash, no-trailing-slash form.
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

    /// Test-only tool arguments for the echo tool.
    #[derive(Deserialize)]
    struct EchoArgs {
        text: String,
    }

    /// Test-only tool error type for validating Rig tool integration.
    #[derive(Debug, thiserror::Error)]
    #[error("echo tool failed")]
    struct EchoError;

    /// Test-only tool implementation used to verify builder compatibility.
    struct EchoTool;

    impl Tool for EchoTool {
        const NAME: &'static str = "echo";

        type Error = EchoError;
        type Args = EchoArgs;
        type Output = String;

        /// Returns the schema exposed to the model for the test echo tool.
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

        /// Returns the provided text unchanged for test purposes.
        async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(args.text)
        }
    }

    /// Verifies that authority and base path normalization produce the expected URL layout.
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

    /// Verifies that the Venice agent builder accepts standard Rig tools.
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
