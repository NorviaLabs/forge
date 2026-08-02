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

        for turn in 0..session.max_turns() {
            let response = session
                .run_model_step_with_stream(turn, stream_tx.clone())
                .await?;

            match session.apply_model_response(response).await? {
                ApplyOutcome::Done(response) | ApplyOutcome::Hitl(response) => return Ok(response),
                ApplyOutcome::Continue => {}
            }
        }

        session.fail_max_turns().await?;
        Err(LoopError::Other("max_turns exceeded".into()))
    }
}
