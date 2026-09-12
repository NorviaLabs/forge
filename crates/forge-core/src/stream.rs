//! Model streaming helpers shared by the agent loop and presentation layer.
//!
//! Providers emit [`ModelStreamEvent`]s while a completion is in flight. The
//! agent core accumulates them here so token usage and thinking text survive on
//! the final [`ModelResponse`], and so tool-call streaming can surface as turn
//! events without duplicating provider-specific parsing in the TUI.

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

/// Maximum number of provider events allowed between the blocking relay and
/// the async agent loop. The provider-facing `std::sync::mpsc` API cannot be
/// made bounded here, but this prevents an unbounded Tokio queue in the core.
const STREAM_EVENT_BUFFER_CAPACITY: usize = 64;

/// Combine adjacent text/thinking deltas for presentation. This does not alter
/// the session accumulator or durable turn events: those still observe every
/// provider event in order. All non-delta events remain distinct, including
/// usage, tool calls, and message boundaries.
fn coalesce_forward_events(events: Vec<ModelStreamEvent>) -> Vec<ModelStreamEvent> {
    let mut coalesced = Vec::with_capacity(events.len());
    for event in events {
        match (coalesced.last_mut(), event) {
            (
                Some(ModelStreamEvent::TextDelta { text: previous }),
                ModelStreamEvent::TextDelta { text },
            ) => previous.push_str(&text),
            (
                Some(ModelStreamEvent::ThinkingDelta { text: previous }),
                ModelStreamEvent::ThinkingDelta { text },
            ) => previous.push_str(&text),
            (_, event) => coalesced.push(event),
        }
    }
    coalesced
}

/// Observe every event in `events`, then forward a coalesced presentation view.
fn observe_stream_events(
    session: &mut AgentSession,
    events: Vec<ModelStreamEvent>,
    forward: Option<&StreamEventTx>,
    acc: &mut ModelStepAccumulator,
) {
    for event in &events {
        accumulate_stream_event(acc, event);
        if let Some(turn) = stream_turn_event(event) {
            session.events.push(turn);
        }
    }
    if let Some(tx) = forward {
        for event in coalesce_forward_events(events) {
            let _ = tx.send(event);
        }
    }
}

/// Drain at most one bounded queue's worth of immediately ready events.
fn drain_ready_stream_events(
    rx: &mut tokio::sync::mpsc::Receiver<ModelStreamEvent>,
    first: ModelStreamEvent,
    session: &mut AgentSession,
    forward: Option<&StreamEventTx>,
    acc: &mut ModelStepAccumulator,
) {
    let mut events = vec![first];
    while events.len() < STREAM_EVENT_BUFFER_CAPACITY {
        let Ok(event) = rx.try_recv() else {
            break;
        };
        events.push(event);
    }
    observe_stream_events(session, events, forward, acc);
}

impl AgentSession {
    /// Prepare, complete, and drain stream events for one model step.
    pub async fn run_model_step_with_stream(
        &mut self,
        turn: u32,
        forward: Option<StreamEventTx>,
    ) -> Result<ModelResponse, LoopError> {
        let req = self.prepare_model_step(turn).await?;
        let (tx, std_rx) = std::sync::mpsc::channel();
        let (async_tx, mut rx) = tokio::sync::mpsc::channel(STREAM_EVENT_BUFFER_CAPACITY);
        let relay = tokio::task::spawn_blocking(move || {
            while let Ok(event) = std_rx.recv() {
                // `blocking_send` applies backpressure to the relay without
                // blocking Tokio's worker threads. It returns when cancellation
                // drops the receiver, so the relay cannot keep a cancelled step
                // alive.
                if async_tx.blocking_send(event).is_err() {
                    break;
                }
            }
        });
        let model = self.model.clone();
        let mut handle =
            crate::IsolatedTask::spawn(
                async move { model.complete_with_stream(req, Some(tx)).await },
            );

        let mut acc = ModelStepAccumulator::default();
        let cancel_token = self.cancel_token.clone();
        let turn_cancel_token = self.turn_cancel_token.clone();
        let mut stream_open = true;
        let response = loop {
            if handle.is_finished() {
                break handle
                    .join()
                    .await
                    .map_err(|error| LoopError::Other(format!("model task join: {error}")))?
                    .ok_or(LoopError::Cancelled)?
                    .map_err(|error| LoopError::Other(error.to_string()))?;
            }
            // Only ever `Some` for a subagent session (see `AgentSession::cancel_token`'s
            // doc comment) — the foreground session is cancelled via the TUI's own
            // `cancel_requested` bool instead, checked in `app/turn.rs`.
            tokio::select! {
                event = rx.recv(), if stream_open => {
                    if let Some(event) = event {
                        drain_ready_stream_events(
                            &mut rx,
                            event,
                            self,
                            forward.as_ref(),
                            &mut acc,
                        );
                    } else {
                        stream_open = false;
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                }
                _ = async {
                    if let Some(token) = cancel_token.clone() {
                        token.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    handle.abort();
                    return Err(LoopError::Cancelled);
                }
                _ = turn_cancel_token.cancelled() => {
                    handle.abort();
                    return Err(LoopError::Cancelled);
                }
            }
        };
        // The relay may be blocked in `blocking_send` because the bounded
        // channel is full. Drain until it closes *before* joining it: joining
        // first would deadlock precisely when a fast model finishes after a
        // large stream burst. Once the model task has returned its sender is
        // dropped, so the relay eventually closes `rx` after preserving every
        // pending provider event.
        //
        // A provider that retains a sender clone keeps `rx` open past the
        // response, so the drain must keep observing cancellation rather than
        // waiting on a channel that can never close. Already-queued events are
        // still preserved; cancellation only stops the drain from waiting
        // forever on a source that will never end. Returning drops `rx`, which
        // unblocks the relay's `blocking_send`/`recv` so its thread exits.
        loop {
            tokio::select! {
                event = rx.recv() => {
                    if let Some(event) = event {
                        drain_ready_stream_events(&mut rx, event, self, forward.as_ref(), &mut acc);
                    } else {
                        break;
                    }
                }
                _ = async {
                    if let Some(token) = cancel_token.clone() {
                        token.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    return Err(LoopError::Cancelled);
                }
                _ = turn_cancel_token.cancelled() => {
                    return Err(LoopError::Cancelled);
                }
            }
        }
        relay
            .await
            .map_err(|error| LoopError::Other(format!("stream relay join: {error}")))?;
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

    #[test]
    fn coalescing_combines_only_adjacent_display_deltas() {
        let events = coalesce_forward_events(vec![
            ModelStreamEvent::TextDelta {
                text: "one ".into(),
            },
            ModelStreamEvent::TextDelta { text: "two".into() },
            ModelStreamEvent::Usage {
                usage: Usage::default(),
            },
            ModelStreamEvent::TextDelta {
                text: "three".into(),
            },
            ModelStreamEvent::ThinkingDelta { text: "a".into() },
            ModelStreamEvent::ThinkingDelta { text: "b".into() },
            ModelStreamEvent::MessageEnd,
        ]);

        assert!(matches!(
            &events[0],
            ModelStreamEvent::TextDelta { text } if text == "one two"
        ));
        assert!(matches!(events[1], ModelStreamEvent::Usage { .. }));
        assert!(matches!(
            &events[2],
            ModelStreamEvent::TextDelta { text } if text == "three"
        ));
        assert!(matches!(
            &events[3],
            ModelStreamEvent::ThinkingDelta { text } if text == "ab"
        ));
        assert!(matches!(events[4], ModelStreamEvent::MessageEnd));
    }

    /// Models a provider that returns from `complete_with_stream` while a
    /// clone of the synchronous sender stays alive past the response, so the
    /// std channel never closes. The final drain must still observe
    /// cancellation rather than waiting on a channel that can never end.
    ///
    /// The retained clone is handed back through `held` rather than leaked on
    /// purpose: the relay's `std::sync::mpsc::Receiver::recv` parks while any
    /// sender lives, and the Tokio runtime waits for blocking-pool tasks on
    /// shutdown. The test drops the clone before returning so the relay exits.
    struct LeakyStreamingModel {
        held: std::sync::Arc<std::sync::Mutex<Option<forge_model::StreamEventTx>>>,
    }

    #[async_trait::async_trait]
    impl forge_model::ModelClient for LeakyStreamingModel {
        async fn complete(
            &self,
            _req: forge_model::ModelRequest,
        ) -> Result<ModelResponse, forge_model::ModelError> {
            unreachable!("streaming path only")
        }

        async fn complete_with_stream(
            &self,
            _req: forge_model::ModelRequest,
            tx: Option<forge_model::StreamEventTx>,
        ) -> Result<ModelResponse, forge_model::ModelError> {
            let tx = tx.expect("core supplies a stream sender");
            tx.send(ModelStreamEvent::TextDelta { text: "x".into() })
                .unwrap();
            // Retain a clone: the channel stays open after this returns.
            *self.held.lock().unwrap() = Some(tx.clone());
            Ok(ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            })
        }

        fn clear_provider_env(&self) {}
    }

    #[tokio::test]
    async fn final_drain_observes_turn_cancel_when_provider_retains_sender() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::LoopConfig {
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("j"),
            enable_governance: false,
            ..Default::default()
        };
        let held = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut session = AgentSession::create(
            cfg,
            std::sync::Arc::new(LeakyStreamingModel { held: held.clone() }),
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap();
        session.messages.push(forge_types::Message::new(
            forge_types::MessageRole::User,
            "hello",
        ));

        let token = session.begin_turn_cancellation_scope();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token.cancel();
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.run_model_step_with_stream(0, None),
        )
        .await
        .expect("final drain must observe cancellation instead of hanging");

        assert!(matches!(result, Err(LoopError::Cancelled)));

        // Release the retained sender so the relay's blocking `recv` returns
        // and runtime shutdown does not wait on a parked blocking thread.
        drop(held.lock().unwrap().take());
    }
}
