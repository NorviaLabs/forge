//! Model providers — native Rust transports; mock for CI.

mod mock;
mod native;
mod normalize;

pub use mock::MockModelClient;
pub use native::NativeModelClient;
pub use normalize::{
    complete_result_from_value, forge_messages_to_wire, tools_to_openai_functions,
};

use async_trait::async_trait;
use forge_config::{Config, ModelProviderKind};
use forge_types::{Message, ModelResponse, ModelStreamEvent, ToolDescriptor};
use thiserror::Error;

/// Best-effort stream events from `complete_with_stream` (cross-thread, non-async).
pub type StreamEventTx = std::sync::mpsc::Sender<ModelStreamEvent>;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("http error: {0}")]
    Http(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("missing API key (connect the provider, set its API key env var, or model.api_key)")]
    MissingApiKey,
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub prompt_cache: bool,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError>;

    /// Like `complete`, optionally emitting `ModelStreamEvent`s on `tx` as tokens arrive.
    /// Default: non-streaming `complete`, then a single `TextDelta` of the full text.
    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let resp = self.complete(req).await?;
        if let Some(tx) = tx {
            if let Some(ref thinking) = resp.thinking {
                if !thinking.is_empty() {
                    let _ = tx.send(ModelStreamEvent::ThinkingDelta {
                        text: thinking.clone(),
                    });
                }
            }
            if !resp.text.is_empty() {
                let _ = tx.send(ModelStreamEvent::TextDelta {
                    text: resp.text.clone(),
                });
            }
            let _ = tx.send(ModelStreamEvent::MessageEnd);
        }
        Ok(resp)
    }

    /// Inject provider credentials into the active transport.
    /// Default: no-op (mock).
    fn apply_provider_env(&self, _pairs: &[(String, String)]) {}

    /// Clear provider credentials from the transport.
    /// Default: no-op (mock).
    fn clear_provider_env(&self) {}
}

/// Build the native production client or the offline mock.
pub fn client_from_config(cfg: &Config) -> Result<Box<dyn ModelClient>, ModelError> {
    match cfg.model.provider {
        ModelProviderKind::Mock => Ok(Box::new(MockModelClient::script(vec![ModelResponse {
            text: "mock idle — configure a response script in tests".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]))),
        ModelProviderKind::Native => {
            let client = NativeModelClient::from_config(cfg)?;
            Ok(Box::new(client))
        }
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
            thinking: None,
        }]);
        let resp = client
            .complete(ModelRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: "hi".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                }],
                tools: vec![],
                model: "mock".into(),
                reasoning_effort: None,
                prompt_cache: true,
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
            thinking: None,
        }]);
        let resp = client
            .complete(ModelRequest {
                messages: vec![],
                tools: vec![],
                model: "mock".into(),
                reasoning_effort: None,
                prompt_cache: true,
            })
            .await
            .unwrap();
        assert_eq!(resp.tool_calls[0].name, "read_file");
    }

    #[test]
    fn factory_mock() {
        let mut cfg = Config::default();
        cfg.model.provider = ModelProviderKind::Mock;
        let c = client_from_config(&cfg).unwrap();
        // type erased — just ensure constructs
        let _ = c;
    }
}
