//! The scripted fake provider (`fake-provider` feature): a deterministic
//! model for end-to-end tests and the desktop self-test.
//!
//! A scenario is a list of steps, each answering one model request:
//! - text, optionally streamed in chunks;
//! - tool calls;
//! - an error;
//! - a *hold*, which blocks mid-request until the test releases it, or until
//!   the turn is cancelled (the request future is dropped).
//!
//! Every request is recorded. In strict mode, a request beyond the script is
//! an error rather than a silent default.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use forge_types::{ModelResponse, ModelStreamEvent, ToolCall};
use serde::Deserialize;
use tokio::sync::Notify;

use crate::{ModelClient, ModelError, ModelRequest, StreamEventTx};

/// The message strict mode fails with once the script runs out.
pub const SCRIPT_EXHAUSTED: &str = "fake provider script exhausted";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Fail requests beyond the script instead of answering "done".
    #[serde(default = "strict_by_default")]
    pub strict: bool,
    pub steps: Vec<Step>,
}

fn strict_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Answer with `text`, streamed as `chunks` when given (they must join
    /// to `text`).
    Text {
        text: String,
        #[serde(default)]
        chunks: Option<Vec<String>>,
    },
    /// Answer with tool calls (and optional text).
    ToolCalls {
        #[serde(default)]
        text: String,
        calls: Vec<ToolCall>,
    },
    /// Fail the request.
    Error { message: String },
    /// Stream `before`, then wait until [`ScriptedModelClient::release`] is
    /// called with `id` (or the request is cancelled), then answer `then`.
    Hold {
        id: String,
        #[serde(default)]
        before: String,
        then: Box<Step>,
    },
}

pub struct ScriptedModelClient {
    steps: Mutex<std::collections::VecDeque<Step>>,
    strict: bool,
    requests: Mutex<Vec<ModelRequest>>,
    holds: Mutex<HashMap<String, Arc<HoldGate>>>,
}

#[derive(Default)]
struct HoldGate {
    reached: Notify,
    released: Notify,
}

impl ScriptedModelClient {
    pub fn new(scenario: Scenario) -> Result<Self, ModelError> {
        for step in &scenario.steps {
            validate(step)?;
        }
        Ok(Self {
            steps: Mutex::new(scenario.steps.into()),
            strict: scenario.strict,
            requests: Mutex::new(Vec::new()),
            holds: Mutex::new(HashMap::new()),
        })
    }

    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let scenario: Scenario = serde_json::from_str(json)
            .map_err(|error| ModelError::Other(format!("fake provider scenario: {error}")))?;
        Self::new(scenario)
    }

    pub fn from_file(path: &Path) -> Result<Self, ModelError> {
        let json = std::fs::read_to_string(path).map_err(|error| {
            ModelError::Other(format!(
                "fake provider scenario {}: {error}",
                path.display()
            ))
        })?;
        Self::from_json(&json)
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Let the request held at `id` continue.
    pub fn release(&self, id: &str) {
        self.gate(id).released.notify_one();
    }

    /// Wait until a request reaches the hold `id`.
    pub async fn held(&self, id: &str) {
        self.gate(id).reached.notified().await;
    }

    fn gate(&self, id: &str) -> Arc<HoldGate> {
        self.holds
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    fn next(&self, request: ModelRequest) -> Result<Option<Step>, ModelError> {
        self.requests.lock().unwrap().push(request);
        match self.steps.lock().unwrap().pop_front() {
            Some(step) => Ok(Some(step)),
            None if self.strict => Err(ModelError::Other(SCRIPT_EXHAUSTED.into())),
            None => Ok(None),
        }
    }

    async fn run(
        &self,
        step: Step,
        tx: Option<&StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let mut step = step;
        loop {
            match step {
                Step::Hold { id, before, then } => {
                    if !before.is_empty() {
                        send(tx, ModelStreamEvent::TextDelta { text: before });
                    }
                    let gate = self.gate(&id);
                    // Register for the release before announcing the hold, so
                    // a release that follows `held` immediately is not lost.
                    let released = gate.released.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    gate.reached.notify_waiters();
                    gate.reached.notify_one();
                    released.await;
                    step = *then;
                }
                Step::Text { text, chunks } => {
                    for chunk in chunks.unwrap_or_else(|| vec![text.clone()]) {
                        if !chunk.is_empty() {
                            send(tx, ModelStreamEvent::TextDelta { text: chunk });
                        }
                    }
                    send(tx, ModelStreamEvent::MessageEnd);
                    return Ok(response(text, Vec::new()));
                }
                Step::ToolCalls { text, calls } => {
                    if !text.is_empty() {
                        send(tx, ModelStreamEvent::TextDelta { text: text.clone() });
                    }
                    for call in &calls {
                        send(
                            tx,
                            ModelStreamEvent::ToolCallStart {
                                id: call.id.clone(),
                                name: call.name.clone(),
                            },
                        );
                        send(tx, ModelStreamEvent::ToolCallEnd { call: call.clone() });
                    }
                    send(tx, ModelStreamEvent::MessageEnd);
                    return Ok(response(text, calls));
                }
                Step::Error { message } => return Err(ModelError::Provider(message)),
            }
        }
    }
}

fn validate(step: &Step) -> Result<(), ModelError> {
    match step {
        Step::Text {
            text,
            chunks: Some(chunks),
        } if chunks.concat() != *text => Err(ModelError::Other(format!(
            "fake provider scenario: chunks do not join to the text {text:?}"
        ))),
        Step::Hold { then, .. } => validate(then),
        _ => Ok(()),
    }
}

fn send(tx: Option<&StreamEventTx>, event: ModelStreamEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

fn response(text: String, tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        text,
        tool_calls,
        usage: None,
        thinking: None,
    }
}

#[async_trait]
impl ModelClient for ScriptedModelClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        match self.next(req)? {
            Some(step) => self.run(step, None).await,
            None => Ok(response("done".into(), Vec::new())),
        }
    }

    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        match self.next(req)? {
            Some(step) => self.run(step, tx.as_ref()).await,
            None => Ok(response("done".into(), Vec::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ModelRequest {
        ModelRequest {
            messages: Vec::new().into(),
            tools: vec![],
            model: "mock".into(),
            workspace_root: ".".into(),
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
            side_call: false,
        }
    }

    fn deltas(rx: &std::sync::mpsc::Receiver<ModelStreamEvent>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|event| match event {
                ModelStreamEvent::TextDelta { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn streams_text_in_the_scripted_chunks_and_records_requests() {
        let client = ScriptedModelClient::from_json(
            r#"{"steps": [{"text": {"text": "hello world", "chunks": ["hel", "lo ", "world"]}}]}"#,
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let response = client
            .complete_with_stream(request(), Some(tx))
            .await
            .unwrap();
        assert_eq!(response.text, "hello world");
        assert_eq!(deltas(&rx), vec!["hel", "lo ", "world"]);
        assert_eq!(client.requests().len(), 1);
    }

    #[tokio::test]
    async fn answers_with_tool_calls_and_errors() {
        let client = ScriptedModelClient::from_json(
            r#"{"steps": [
                {"tool_calls": {"calls": [{"id": "c1", "name": "bash", "arguments": {"command": "ls"}}]}},
                {"error": {"message": "boom"}}
            ]}"#,
        )
        .unwrap();
        let response = client.complete(request()).await.unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "bash");
        let error = client.complete(request()).await.unwrap_err();
        assert!(error.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn strict_mode_fails_past_the_script_and_lenient_mode_says_done() {
        let strict = ScriptedModelClient::from_json(r#"{"steps": []}"#).unwrap();
        let error = strict.complete(request()).await.unwrap_err();
        assert!(error.to_string().contains(SCRIPT_EXHAUSTED));
        let lenient = ScriptedModelClient::from_json(r#"{"strict": false, "steps": []}"#).unwrap();
        assert_eq!(lenient.complete(request()).await.unwrap().text, "done");
    }

    #[tokio::test]
    async fn a_hold_blocks_until_released() {
        let client = Arc::new(
            ScriptedModelClient::from_json(
                r#"{"steps": [{"hold": {"id": "h1", "before": "thinking", "then": {"text": {"text": "released"}}}}]}"#,
            )
            .unwrap(),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let call = tokio::spawn({
            let client = client.clone();
            async move { client.complete_with_stream(request(), Some(tx)).await }
        });
        client.held("h1").await;
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert!(!call.is_finished(), "the request must wait at the hold");
        client.release("h1");
        let response = call.await.unwrap().unwrap();
        assert_eq!(response.text, "released");
        assert_eq!(deltas(&rx), vec!["thinking", "released"]);
    }

    #[tokio::test]
    async fn a_cancelled_hold_does_not_block_the_next_request() {
        let client = Arc::new(
            ScriptedModelClient::from_json(
                r#"{"steps": [
                    {"hold": {"id": "h1", "then": {"text": {"text": "never"}}}},
                    {"text": {"text": "next"}}
                ]}"#,
            )
            .unwrap(),
        );
        let call = tokio::spawn({
            let client = client.clone();
            async move { client.complete(request()).await }
        });
        client.held("h1").await;
        // A stopped turn drops the request future.
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(client.complete(request()).await.unwrap().text, "next");
    }

    #[test]
    fn scenarios_are_validated() {
        assert!(ScriptedModelClient::from_json(
            r#"{"steps": [{"text": {"text": "abc", "chunks": ["a", "b"]}}]}"#
        )
        .is_err());
        assert!(ScriptedModelClient::from_json(r#"{"steps": [], "bogus": 1}"#).is_err());
        assert!(ScriptedModelClient::from_json(r#"{"steps": [{"unknown": {}}]}"#).is_err());
    }
}
