//! Multi-step turn orchestration.
//!
//! The session owns durable state and the individual model-step operations;
//! this coordinator only decides when to continue, return, or fail a turn.

use forge_model::StreamEventTx;
use forge_types::{ModelResponse, TaskLifecycle};

use crate::{AgentSession, ApplyOutcome, LoopError};

pub(crate) struct TurnCoordinator;

impl TurnCoordinator {
    pub(crate) async fn run(
        session: &mut AgentSession,
        stream_tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, LoopError> {
        if session.active_task.lifecycle == TaskLifecycle::Waiting {
            return Err(LoopError::AwaitingHitl);
        }
        // Continuing a session whose last attempt ended in failure is a fresh
        // attempt, not a resumption of the poisoned one. A terminal lifecycle
        // blocks the completion evaluator (see `apply_model_response`), so a
        // successful continuation would otherwise leave the session permanently
        // Failed — wedged — and the old turn's dangling tool calls would ride
        // into the next request. Mirrors what a new user message does.
        if session.active_task.lifecycle == TaskLifecycle::Failed {
            session.start_fresh_attempt().await?;
        }

        let mut turn = 0u32;
        loop {
            let response = session
                .run_model_step_with_stream(turn, stream_tx.clone())
                .await?;
            turn = turn.wrapping_add(1);

            match session.apply_model_response(response).await? {
                ApplyOutcome::Done(response) | ApplyOutcome::Hitl(response) => return Ok(response),
                ApplyOutcome::Continue => {}
                // A turn that ends at a tool boundary while the future-task
                // queue is non-empty hands off to the queue instead of
                // completing. The direct TUI loop performs this transition
                // itself (`yield_current_turn_for_queue`) after
                // `apply_model_response`; the coordinator must do the same so
                // the lifecycle leaves `Working` — otherwise the next prompt's
                // `start_fresh_attempt` (illegal from `Working`) wedges the
                // session. Mirrors `apply_model_response`'s own
                // terminal-transition behaviour for a normal completion.
                ApplyOutcome::YieldToQueue(response) => {
                    session.yield_current_turn_for_queue().await?;
                    return Ok(response);
                }
            }
        }
    }
}
