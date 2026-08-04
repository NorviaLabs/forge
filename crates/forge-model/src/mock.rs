use async_trait::async_trait;
use forge_types::{ModelResponse, ModelStreamEvent};
use std::sync::Mutex;

use crate::{ModelClient, ModelError, ModelRequest, StreamEventTx};

enum MockStep {
    Response(ModelResponse),
    StreamError { deltas: Vec<String>, error: String },
}

/// Deterministic client for tests and offline demos.
pub struct MockModelClient {
    responses: Mutex<Vec<MockStep>>,
    /// The most recent request passed to `complete`/`complete_with_stream`,
    /// for tests that need to assert on what was actually sent (e.g. the
    /// reasoning-effort value).
    last_request: Mutex<Option<ModelRequest>>,
}

impl MockModelClient {
    pub fn script(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(MockStep::Response).collect()),
            last_request: Mutex::new(None),
        }
    }

    pub fn stream_error(deltas: Vec<String>, error: impl Into<String>) -> Self {
        Self {
            responses: Mutex::new(vec![MockStep::StreamError {
                deltas,
                error: error.into(),
            }]),
            last_request: Mutex::new(None),
        }
    }

    /// The most recent request this client received, if any.
    pub fn last_request(&self) -> Option<ModelRequest> {
        self.last_request.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelClient for MockModelClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        *self.last_request.lock().unwrap() = Some(req);
        let mut g = self.responses.lock().unwrap();
        if g.is_empty() {
            return Ok(ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            });
        }
        match g.remove(0) {
            MockStep::Response(response) => Ok(response),
            MockStep::StreamError { error, .. } => Err(ModelError::Transport(error)),
        }
    }

    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        *self.last_request.lock().unwrap() = Some(req);
        let mut g = self.responses.lock().unwrap();
        if g.is_empty() {
            return Ok(ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            });
        }
        match g.remove(0) {
            MockStep::Response(response) => {
                if let Some(tx) = tx {
                    if let Some(ref thinking) = response.thinking {
                        if !thinking.is_empty() {
                            let _ = tx.send(ModelStreamEvent::ThinkingDelta {
                                text: thinking.clone(),
                            });
                        }
                    }
                    if !response.text.is_empty() {
                        let _ = tx.send(ModelStreamEvent::TextDelta {
                            text: response.text.clone(),
                        });
                    }
                    if let Some(ref usage) = response.usage {
                        let _ = tx.send(ModelStreamEvent::Usage {
                            usage: usage.clone(),
                        });
                    }
                    let _ = tx.send(ModelStreamEvent::MessageEnd);
                }
                Ok(response)
            }
            MockStep::StreamError { deltas, error } => {
                if let Some(tx) = tx {
                    for text in deltas {
                        let _ = tx.send(ModelStreamEvent::TextDelta { text });
                    }
                }
                Err(ModelError::Transport(error))
            }
        }
    }

    fn clear_provider_env(&self) {}
}
