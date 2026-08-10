//! Task lifecycle transitions, the prompt queue, and background tasks.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

impl AgentSession {
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

    /// The HITL payload the active task is waiting on, if any — a
    /// convenience view over `active_task.wait_reason`'s `Approval` variant
    /// (the only wait reason with a real producer today).
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
        tracing::debug!(
            from = ?from,
            to = ?to,
            revision = self.active_task.revision,
            reason = ?reason,
            "lifecycle transition"
        );
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
        self.journal
            .append_status(self.session_id, TaskLifecycle::Working)
            .await?;
        Ok(())
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

    /// Cancel a still-queued item by its 1-based visible position (matches
    /// the existing keyboard cancel-by-row UX). Returns `None` if the
    /// position is out of range or the item is no longer cancellable
    /// (already promoting/promoted/removed).
    pub async fn cancel_queued_at(
        &mut self,
        one_based: usize,
    ) -> Result<Option<QueuedTask>, LoopError> {
        let Some(item) = self.tasks.queue.remove_at_visible_position(one_based) else {
            return Ok(None);
        };
        self.journal
            .append_queue_removed(self.session_id, item.id)
            .await?;
        Ok(Some(item))
    }

    /// Atomic promotion of the oldest queued item into a new task, started
    /// `Working`. Returns `None` if the queue is empty. On failure to start
    /// the new task, the item is rolled back to `Queued` (never lost) and
    /// the error propagated.
    pub async fn promote_next_queued(&mut self) -> Result<Option<TaskId>, LoopError> {
        let Some(item) = self.tasks.queue.peek_next_queued().cloned() else {
            return Ok(None);
        };
        if !self.tasks.queue.mark_promoting(item.id) {
            // Lost the race to another caller — nothing to do.
            return Ok(None);
        }
        self.journal
            .append_queue_promoting(self.session_id, item.id)
            .await?;

        match self.append_user_message(&item.text).await {
            Ok(()) => {
                self.tasks.queue.mark_promoted(item.id);
                self.journal
                    .append_queue_promoted(self.session_id, item.id, self.active_task.task_id.0)
                    .await?;
                Ok(Some(self.active_task.task_id))
            }
            Err(err) => {
                self.tasks.queue.revert_promoting(item.id);
                Err(err)
            }
        }
    }
}
