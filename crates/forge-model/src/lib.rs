//! Model providers — Phase 5 wire + normalization.

mod mock;
mod normalize;
mod wire;

pub use mock::MockModelClient;
pub use normalize::{complete_result_from_value, forge_messages_to_wire, tools_to_openai_functions};
pub use wire::{error_codes, CompleteParams, WireEnvelope, WireErrorBody, WireType, WIRE_VERSION};

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
    #[error("missing API key")]
    MissingApiKey,
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("worker: {0}")]
    Worker(String),
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

pub fn client_from_config(cfg: &Config) -> Result<Box<dyn ModelClient>, ModelError> {
    match cfg.model.provider {
        ModelProviderKind::Mock => Ok(Box::new(MockModelClient::script(vec![ModelResponse {
            text: "mock".into(),
            tool_calls: vec![],
            usage: None,
        }]))),
        ModelProviderKind::Litellm => Err(ModelError::Other(
            "LiteLLM client not wired yet (Phase 5 in progress)".into(),
        )),
    }
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
