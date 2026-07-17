use async_trait::async_trait;
use forge_types::ModelResponse;
use std::sync::Mutex;

use crate::{ModelClient, ModelError, ModelRequest};

/// Deterministic client for tests and offline demos.
pub struct MockModelClient {
    responses: Mutex<Vec<ModelResponse>>,
}

impl MockModelClient {
    pub fn script(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl ModelClient for MockModelClient {
    async fn complete(&self, _req: ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut g = self.responses.lock().unwrap();
        if g.is_empty() {
            return Ok(ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            });
        }
        Ok(g.remove(0))
    }

    fn clear_provider_env(&self) {}
}
