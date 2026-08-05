//! Model providers — native Rust transports; mock for CI.

mod mock;
mod native;
mod normalize;
mod prompt_cache;

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

/// Render a status code exactly as `reqwest::StatusCode` does — `429 Too Many
/// Requests` — so that moving the code out of the message and into a field leaves
/// `Display` byte-for-byte unchanged.
///
/// `from_u16` only rejects values outside 100–999, which a real response cannot
/// produce, so the fallback is unreachable in practice.
fn status_label(status: u16) -> String {
    reqwest::StatusCode::from_u16(status)
        .map(|code| code.to_string())
        .unwrap_or_else(|_| status.to_string())
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("http error: {0}")]
    Http(String),
    /// Provider failure with no HTTP status attached — a stream-level error, or a
    /// pre-flight check that never reached the network.
    #[error("provider error: {0}")]
    Provider(String),
    /// Provider returned a non-success HTTP status.
    ///
    /// `status` is a field rather than text formatted into the message, so a caller
    /// can tell a 429 from a 401 and decide whether to retry without parsing
    /// `Display`. See [`ModelError::is_retryable`].
    #[error("provider error: HTTP {}: {detail}", status_label(*status))]
    ProviderStatus { status: u16, detail: String },
    /// Provider rejected the credential and the transport has specific guidance for
    /// the operator. Keeps that wording while still carrying the status, so
    /// [`ModelError::is_auth_failure`] works without the message having to say so.
    #[error("provider error: {message}")]
    ProviderAuth { status: u16, message: String },
    #[error("missing API key (connect the provider, set its API key env var, or model.api_key)")]
    MissingApiKey,
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

impl ModelError {
    /// HTTP status the provider returned, when the failure carried one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::ProviderStatus { status, .. } | Self::ProviderAuth { status, .. } => {
                Some(*status)
            }
            _ => None,
        }
    }

    /// The credential was absent or refused. Retrying will not help; the operator
    /// has to reconnect or supply a key.
    pub fn is_auth_failure(&self) -> bool {
        match self {
            Self::MissingApiKey | Self::ProviderAuth { .. } => true,
            Self::ProviderStatus { status, .. } => *status == 401 || *status == 403,
            _ => false,
        }
    }

    /// Rate limited. Separated from the rest of [`Self::is_retryable`] because it is
    /// the case that wants a backoff rather than an immediate retry.
    pub fn is_rate_limited(&self) -> bool {
        self.status() == Some(429)
    }

    /// The failure looks transient, so a retry may succeed without operator action:
    /// a network fault, a rate limit, or a server-side error.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::ProviderStatus { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
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
    use std::sync::mpsc;

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
                    outcome: Default::default(),
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
    async fn mock_stream_error_emits_partial_deltas() {
        let client =
            MockModelClient::stream_error(vec!["partial ".into(), "answer".into()], "network lost");
        let (tx, rx) = mpsc::channel();
        let error = client
            .complete_with_stream(
                ModelRequest {
                    messages: vec![],
                    tools: vec![],
                    model: "mock".into(),
                    reasoning_effort: None,
                    prompt_cache: true,
                },
                Some(tx),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("network lost"));
        let deltas: String = rx
            .try_iter()
            .filter_map(|event| match event {
                ModelStreamEvent::TextDelta { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "partial answer");
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

    #[tokio::test]
    async fn complete_with_stream_emits_message_boundaries() {
        let client = MockModelClient::script(vec![ModelResponse {
            text: "hello".into(),
            tool_calls: vec![],
            usage: None,
            thinking: Some("think".into()),
        }]);
        let (tx, rx) = mpsc::channel();
        let resp = client
            .complete_with_stream(
                ModelRequest {
                    messages: vec![],
                    tools: vec![],
                    model: "mock".into(),
                    reasoning_effort: None,
                    prompt_cache: false,
                },
                Some(tx),
            )
            .await
            .unwrap();
        assert_eq!(resp.text, "hello");
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(events.len(), 3);
    }

    /// The guarantee this refactor rests on: moving the status out of the message
    /// and into a field must not change what an operator sees. Builds the message
    /// the old way — formatting a `StatusCode` in — and compares.
    #[test]
    fn provider_status_display_is_unchanged_from_the_formatted_message() {
        for (code, detail) in [
            (400_u16, "bad request body"),
            (401, "invalid api key"),
            (429, "rate limited"),
            (500, ""),
            (503, "upstream unavailable"),
        ] {
            let legacy_status = reqwest::StatusCode::from_u16(code).unwrap();
            let legacy = format!("provider error: HTTP {legacy_status}: {detail}");
            let typed = ModelError::ProviderStatus {
                status: code,
                detail: detail.to_string(),
            };
            assert_eq!(typed.to_string(), legacy, "Display changed for HTTP {code}");
        }
    }

    /// The bespoke Codex auth message keeps its exact wording while gaining a status.
    #[test]
    fn provider_auth_display_is_unchanged_from_the_bare_provider_message() {
        let message = "Forge's Codex login expired. Run `/connect openai_codex` again.";
        let legacy = ModelError::Provider(message.to_string());
        let typed = ModelError::ProviderAuth {
            status: 401,
            message: message.to_string(),
        };
        assert_eq!(typed.to_string(), legacy.to_string());
    }

    /// The point of the change: a caller can tell these apart without parsing text.
    #[test]
    fn status_classification_separates_retryable_from_auth_failure() {
        let rate_limited = ModelError::ProviderStatus {
            status: 429,
            detail: String::new(),
        };
        assert_eq!(rate_limited.status(), Some(429));
        assert!(rate_limited.is_retryable());
        assert!(rate_limited.is_rate_limited());
        assert!(!rate_limited.is_auth_failure());

        let unauthorized = ModelError::ProviderStatus {
            status: 401,
            detail: String::new(),
        };
        assert!(unauthorized.is_auth_failure());
        assert!(
            !unauthorized.is_retryable(),
            "a rejected credential must not be retried"
        );

        let server_fault = ModelError::ProviderStatus {
            status: 503,
            detail: String::new(),
        };
        assert!(server_fault.is_retryable());
        assert!(!server_fault.is_rate_limited());

        let bad_request = ModelError::ProviderStatus {
            status: 400,
            detail: String::new(),
        };
        assert!(
            !bad_request.is_retryable(),
            "a malformed request will fail again identically"
        );

        // Transport faults are retryable; a missing key is not.
        assert!(ModelError::Transport("connection reset".into()).is_retryable());
        assert!(ModelError::MissingApiKey.is_auth_failure());
        assert!(!ModelError::MissingApiKey.is_retryable());

        // Codex's bespoke 401 is recognised as auth despite its custom wording.
        let codex = ModelError::ProviderAuth {
            status: 401,
            message: "login expired".into(),
        };
        assert!(codex.is_auth_failure());
        assert_eq!(codex.status(), Some(401));

        // Errors with no HTTP status report none rather than a placeholder.
        assert_eq!(ModelError::Protocol("bad sse".into()).status(), None);
    }
}
