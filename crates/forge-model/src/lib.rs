//! Model providers (model-providers.md).

mod mock;
mod openai_compat;

pub use mock::MockModelClient;
pub use openai_compat::OpenAiCompatibleClient;

use async_trait::async_trait;
use forge_config::{Config, ModelProviderKind};
use forge_types::{Message, ModelResponse, ToolDescriptor};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("http error: {0}")]
    Http(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("missing API key (set FORGE_API_KEY or model.api_key)")]
    MissingApiKey,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub model: String,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError>;
}

/// Build a client from config. Phase 1: real HTTP for OpenAI-compatible;
/// Anthropic/xAI use the same wire shape when `base_url` points at a compatible proxy,
/// otherwise a thin OpenAI-compat client with provider-specific default base URLs.
pub fn client_from_config(cfg: &Config) -> Result<Box<dyn ModelClient>, ModelError> {
    let api_key = cfg
        .model
        .api_key
        .clone()
        .or_else(|| std::env::var("FORGE_API_KEY").ok());

    let (base, key_required) = match cfg.model.provider {
        ModelProviderKind::OpenaiCompatible => (
            cfg.model
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
            true,
        ),
        ModelProviderKind::Anthropic => (
            cfg.model
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com/v1".into()),
            true,
        ),
        ModelProviderKind::Xai => (
            cfg.model
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.x.ai/v1".into()),
            true,
        ),
    };

    // Phase 1: all three providers go through OpenAI-compatible chat/completions when base_url
    // is OpenAI-shaped. Anthropic native Messages API can be added later behind the same trait.
    // For Anthropic default URL we still use OpenAI-compat client only if user set a compat proxy;
    // otherwise use OpenAI-compat with xAI/OpenAI defaults, and for Anthropic default use a note.
    let use_openai_wire = matches!(
        cfg.model.provider,
        ModelProviderKind::OpenaiCompatible | ModelProviderKind::Xai
    ) || cfg.model.base_url.is_some();

    if !use_openai_wire && matches!(cfg.model.provider, ModelProviderKind::Anthropic) {
        // Prefer OpenAI-compatible endpoint via base_url override; without it, still construct
        // client pointing at Anthropic — callers without a proxy will get HTTP errors (documented).
        let key = api_key.ok_or(ModelError::MissingApiKey)?;
        return Ok(Box::new(OpenAiCompatibleClient::new(
            base,
            key,
            cfg.model.model.clone(),
        )));
    }

    let key = if key_required {
        api_key.ok_or(ModelError::MissingApiKey)?
    } else {
        api_key.unwrap_or_default()
    };

    Ok(Box::new(OpenAiCompatibleClient::new(
        base,
        key,
        cfg.model.model.clone(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{MessageRole, ToolCall};

    #[tokio::test]
    async fn mock_returns_text() {
        let client = MockModelClient::script(vec![ModelResponse {
            text: "hello".into(),
            tool_calls: vec![],
            usage: None,
        }]);
        let resp = client
            .complete(ModelRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: "hi".into(),
                    tool_call_id: None,
                    name: None,
                }],
                tools: vec![],
                model: "mock".into(),
            })
            .await
            .unwrap();
        assert_eq!(resp.text, "hello");
    }

    #[tokio::test]
    async fn mock_tool_call() {
        let client = MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a.txt"}),
            }],
            usage: None,
        }]);
        let resp = client
            .complete(ModelRequest {
                messages: vec![],
                tools: vec![],
                model: "mock".into(),
            })
            .await
            .unwrap();
        assert_eq!(resp.tool_calls[0].name, "read_file");
    }
}
