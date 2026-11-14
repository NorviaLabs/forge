//! Task lifecycle transitions, the prompt queue, and background tasks.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

impl AgentSession {
    pub async fn yield_current_turn_for_queue(&mut self) -> Result<(), LoopError> {
        self.transition(TaskLifecycle::Completed, TransitionReason::QueueYield)
            .await
    }

    /// Read-only view of queued future-task instructions.
    pub fn queue(&self) -> &TaskQueue {
        &self.tasks.queue
    }

    /// Read-only view of in-flight and recently-finished background tasks.
    pub fn background(&self) -> &BackgroundTaskRegistry {
        &self.tasks.background
    }

    /// Request cancellation of a background task through the session-owned
    /// runtime. Returns `false` for unknown or already-terminal tasks.
    pub fn cancel_background_task(&mut self, id: BackgroundTaskId) -> bool {
        self.tasks.background.cancel(id)
    }

    /// Why the active task is parked, if it is. Approvals and questions have
    /// their own payload views; this is for the reasons that carry none.
    pub fn wait_reason(&self) -> Option<&WaitReason> {
        self.active_task.wait_reason.as_ref()
    }

    /// The HITL payload the active task is waiting on, if any — a
    /// convenience view over `active_task.wait_reason`'s `Approval` variant.
    pub fn pending_hitl(&self) -> Option<&HitlPayload> {
        match &self.active_task.wait_reason {
            Some(WaitReason::Approval { payload, .. }) => Some(payload),
            _ => None,
        }
    }

    /// Validate and apply a lifecycle transition, journaling it durably.
    /// The single path through which `active_task.lifecycle` may change
    /// (other than `enter_waiting`/`start_new_task`, which carry their own
    /// payload) — every direct field write this crate used to do goes
    /// through here instead. Rejects illegal transitions rather than
    /// silently coercing state; the current valid state is preserved on
    /// failure since `ActiveTaskState::try_transition` never partially
    /// applies a rejected move.
    pub(crate) async fn transition(
        &mut self,
        to: TaskLifecycle,
        reason: TransitionReason,
    ) -> Result<(), LoopError> {
        let from = self.active_task.lifecycle;
        if let Err(err) = self.active_task.try_transition(to) {
            tracing::warn!(
                from = ?from,
                to = ?to,
                reason = ?reason,
                error = %err,
                "rejected illegal lifecycle transition"
            );
            return Err(LoopError::from(err));
        }
        self.journal.append_status(self.session_id, to).await?;
        if from == TaskLifecycle::Waiting {
            self.note_waiting_ended();
        }
        if matches!(
            to,
            TaskLifecycle::Completed | TaskLifecycle::Failed | TaskLifecycle::Cancelled
        ) {
            self.flush_audit_at_boundary().await?;
        }
        tracing::debug!(
            from = ?from,
            to = ?to,
            revision = self.active_task.revision,
            reason = ?reason,
            "lifecycle transition"
        );
        if matches!(
            to,
            TaskLifecycle::Completed
                | TaskLifecycle::Failed
                | TaskLifecycle::Cancelled
                | TaskLifecycle::Interrupted
        ) {
            // The turn is over: record it on the model it ran on, then hand
            // the session back to the model a turn-scoped tier replaced.
            self.record_finished_turn(to, &reason).await;
            self.restore_turn_tier();
        }
        Ok(())
    }

    /// Transition into `Waiting`, atomically attaching the structured reason
    /// a response must correlate against to resume the attempt.
    pub(crate) async fn enter_waiting(
        &mut self,
        wait: WaitReason,
        reason: TransitionReason,
    ) -> Result<(), LoopError> {
        let from = self.active_task.lifecycle;
        let approval = match &wait {
            WaitReason::Approval { payload, .. } => Some(payload.clone()),
            _ => None,
        };
        if let Err(err) = self.active_task.enter_waiting(wait) {
            tracing::warn!(
                from = ?from,
                to = ?TaskLifecycle::Waiting,
                reason = ?reason,
                error = %err,
                "rejected illegal lifecycle transition"
            );
            return Err(LoopError::from(err));
        }
        self.journal
            .append_status(self.session_id, TaskLifecycle::Waiting)
            .await?;
        self.note_waiting_started();
        self.record_wait_signal();
        if let Some(payload) = approval {
            self.audit_hitl_request(&payload);
            self.flush_audit_at_boundary().await?;
        }
        tracing::debug!(
            from = ?from,
            to = ?TaskLifecycle::Waiting,
            revision = self.active_task.revision,
            reason = ?reason,
            "lifecycle transition"
        );
        Ok(())
    }

    /// Start a brand-new task (direct dispatch or queue promotion): fresh
    /// task id, attempt reset to 1, `-> Working`. Legal from `Ready` or any
    /// terminal state; rejected while a task is already `Working`/`Waiting`
    /// — at most one active attempt per session.
    pub(crate) async fn transition_to_new_task(
        &mut self,
        task_id: TaskId,
    ) -> Result<(), LoopError> {
        self.active_task
            .start_new_task(task_id)
            .map_err(LoopError::from)?;
        self.begin_turn_telemetry();
        self.journal
            .append_status(self.session_id, TaskLifecycle::Working)
            .await?;
        Ok(())
    }

    /// Record why the turn just started waiting on the user.
    fn record_wait_signal(&mut self) {
        let signals: Vec<(TurnSignalKind, Option<String>)> = match &self.active_task.wait_reason {
            Some(WaitReason::Approval { payload, .. }) => {
                let mut signals = vec![(
                    TurnSignalKind::ApprovalRequested,
                    Some(payload.tool.clone()),
                )];
                if payload.sandbox_escalation {
                    signals.push((
                        TurnSignalKind::SandboxEscalationRequested,
                        Some(payload.tool.clone()),
                    ));
                }
                signals
            }
            Some(WaitReason::Question { .. }) => vec![(TurnSignalKind::QuestionAsked, None)],
            _ => Vec::new(),
        };
        for (kind, detail) in signals {
            self.record_signal(kind, detail);
        }
    }

    /// Enqueue a future-task instruction. Journals the enqueue before
    /// returning — callers must not report "queued" to the user until this
    /// succeeds.
    pub async fn enqueue_task(&mut self, text: &str) -> Result<QueuedTask, LoopError> {
        let item = self.tasks.queue.enqueue(self.session_id, text);
        self.journal
            .append_queue_enqueued(self.session_id, item.id, &item.text)
            .await?;
        Ok(item)
    }

    /// Cancel the named queued instruction. Promotion or removal never
    /// redirects a stale frontend action to the next visible item.
    pub async fn cancel_queued_item(
        &mut self,
        id: forge_types::QueueItemId,
    ) -> Result<Option<QueuedTask>, LoopError> {
        let Some(item) = self.tasks.queue.remove(id) else {
            return Ok(None);
        };
        self.journal
            .append_queue_removed(self.session_id, item.id)
            .await?;
        Ok(Some(item))
    }
}
