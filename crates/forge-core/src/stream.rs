//! Model streaming helpers shared by the agent loop and presentation layer.
//!
//! Providers emit [`ModelStreamEvent`]s while a completion is in flight. The
//! agent core accumulates them here so token usage and thinking text survive on
//! the final [`ModelResponse`], and so tool-call streaming can surface as turn
//! events without duplicating provider-specific parsing in the TUI.

use std::sync::mpsc::Receiver;

use forge_model::StreamEventTx;
use forge_types::{ModelResponse, ModelStreamEvent, Usage};

use crate::{AgentSession, LoopError, TurnEvent};

/// Per-model-step state rebuilt from stream events before `apply_model_response`.
#[derive(Debug, Default, Clone)]
pub struct ModelStepAccumulator {
    pub text: String,
    pub thinking: String,
    pub usage: Option<Usage>,
}

/// Apply one stream event to the step accumulator.
pub fn accumulate_stream_event(acc: &mut ModelStepAccumulator, event: &ModelStreamEvent) {
    match event {
        ModelStreamEvent::TextDelta { text } => acc.text.push_str(text),
        ModelStreamEvent::ThinkingDelta { text } => acc.thinking.push_str(text),
        ModelStreamEvent::Usage { usage } => acc.usage = Some(usage.clone()),
        _ => {}
    }
}

/// Merge streamed fields into the provider's final response object.
pub fn merge_streamed_response(
    mut response: ModelResponse,
    acc: &ModelStepAccumulator,
) -> ModelResponse {
    if response.usage.is_none() {
        response.usage = acc.usage.clone();
    }
    if response
        .thinking
        .as_ref()
        .is_none_or(|thinking| thinking.is_empty())
        && !acc.thinking.is_empty()
    {
        response.thinking = Some(acc.thinking.clone());
    }
    response
}

/// Optional durable turn event for a stream event (tool-call start only today).
pub fn stream_turn_event(event: &ModelStreamEvent) -> Option<TurnEvent> {
    match event {
        ModelStreamEvent::ToolCallStart { id, name } => Some(TurnEvent {
            kind: "tool_stream".into(),
            detail: format!("{name} ({id})"),
        }),
        _ => None,
    }
}

/// Forward `event` to `forward` when present and update `acc` + session turn events.
pub fn observe_stream_event(
    session: &mut AgentSession,
    event: &ModelStreamEvent,
    forward: Option<&StreamEventTx>,
    acc: &mut ModelStepAccumulator,
) {
    accumulate_stream_event(acc, event);
    if let Some(turn) = stream_turn_event(event) {
        session.events.push(turn);
    }
    if let Some(tx) = forward {
        let _ = tx.send(event.clone());
    }
}

fn drain_stream_rx(
    rx: &Receiver<ModelStreamEvent>,
    session: &mut AgentSession,
    forward: Option<&StreamEventTx>,
    acc: &mut ModelStepAccumulator,
) {
    while let Ok(event) = rx.try_recv() {
        observe_stream_event(session, &event, forward, acc);
    }
}

impl AgentSession {
    /// Prepare, complete, and drain stream events for one model step.
    pub async fn run_model_step_with_stream(
        &mut self,
        turn: u32,
        forward: Option<StreamEventTx>,
    ) -> Result<ModelResponse, LoopError> {
        let req = self.prepare_model_step(turn).await?;
        let (tx, rx) = std::sync::mpsc::channel();
        let model = self.model.clone();
        let handle = tokio::spawn(async move { model.complete_with_stream(req, Some(tx)).await });

        let mut acc = ModelStepAccumulator::default();
        while !handle.is_finished() {
            drain_stream_rx(&rx, self, forward.as_ref(), &mut acc);
            tokio::task::yield_now().await;
        }
        drain_stream_rx(&rx, self, forward.as_ref(), &mut acc);

        let response = handle
            .await
            .map_err(|error| LoopError::Other(format!("model task join: {error}")))?
            .map_err(|error| LoopError::Other(error.to_string()))?;
        Ok(merge_streamed_response(response, &acc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::Usage;

    #[test]
    fn merge_streamed_response_prefers_stream_usage_and_thinking() {
        let acc = ModelStepAccumulator {
            thinking: "reasoning".into(),
            usage: Some(Usage {
                prompt_tokens: 9,
                completion_tokens: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge_streamed_response(
            ModelResponse {
                text: "answer".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
            &acc,
        );
        assert_eq!(merged.usage.unwrap().prompt_tokens, 9);
        assert_eq!(merged.thinking.as_deref(), Some("reasoning"));
    }

    #[test]
    fn stream_turn_event_maps_tool_call_start() {
        let event = stream_turn_event(&ModelStreamEvent::ToolCallStart {
            id: "c1".into(),
            name: "bash".into(),
        })
        .unwrap();
        assert_eq!(event.kind, "tool_stream");
        assert!(event.detail.contains("bash"));
    }
}
