//! Authoritative task/attempt lifecycle. `ActiveTaskState` is the single
//! source of truth for "what task is active, is Forge working/waiting, and
//! why" — the header, transcript, composer, and queue all read it rather
//! than keeping their own copy.
//!
//! This module is intentionally I/O-free: transition legality and state
//! mutation are pure so they can be unit-tested without a `Journal` or a
//! `ModelClient`. The async, journal-writing wrapper lives on `AgentSession`
//! in `lib.rs`, since it needs the session's own `Journal` handle.

use forge_types::{AttemptId, SessionId, TaskId, TaskLifecycle, WaitReason};

/// Why a transition was requested — carried through to the journal/event
/// log for diagnostics, distinct from the lifecycle value itself.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransitionReason {
    /// A new task started (direct dispatch or queue promotion).
    NewTask,
    /// The completion evaluator authorized `Working -> Completed`.
    Completion(crate::CompletionReason),
    /// A turn ended in failure for a reason not modeled as a
    /// `CompletionReason` (e.g. step-limit exhaustion, repeated invalid tool
    /// calls) — see `AgentSession::finalize_turn_failure`'s `category` arg.
    TurnFailure,
    /// The user explicitly cancelled the active attempt.
    UserCancel,
    /// A persisted `Working`/`Waiting` state could not be proven still alive on restore.
    StaleOnResume,
    /// The runtime paused for human-in-the-loop tool approval.
    HitlWait,
    /// A HITL request was resolved (approve or deny), resuming the attempt.
    HitlResolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("illegal lifecycle transition: {from:?} -> {to:?}")]
    Illegal {
        from: TaskLifecycle,
        to: TaskLifecycle,
    },
}

/// The legal-transition table. Terminal states (`Completed`/`Failed`/
/// `Cancelled`/`Interrupted`) have no outgoing edge here by design — the
/// only way out of a terminal attempt is `ActiveTaskState::start_new_task`,
/// a distinct, explicit escape hatch (fresh task/attempt ids), never a
/// resumption of the old one.
pub(crate) fn is_legal_transition(from: TaskLifecycle, to: TaskLifecycle) -> bool {
    use TaskLifecycle::*;
    matches!(
        (from, to),
        (Ready, Working)
            | (Working, Waiting)
            | (Working, Completed)
            | (Working, Failed)
            | (Working, Cancelled)
            | (Working, Interrupted)
            | (Waiting, Working)
            | (Waiting, Cancelled)
            | (Waiting, Interrupted)
    )
}

/// Runtime-owned lifecycle state for the one active task attempt in a
/// session. Forge runs a single task at a time, so this is a plain struct,
/// not a registry — `task_id`/`attempt_id` are monotonic counters, not keys
/// into a lookup table.
#[derive(Debug, Clone)]
pub struct ActiveTaskState {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub lifecycle: TaskLifecycle,
    pub wait_reason: Option<WaitReason>,
    pub cancel_requested: bool,
    pub revision: u64,
}

impl ActiveTaskState {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            task_id: TaskId(0),
            attempt_id: AttemptId(0),
            lifecycle: TaskLifecycle::Ready,
            wait_reason: None,
            cancel_requested: false,
            revision: 0,
        }
    }

    /// Transition to any lifecycle value other than `Waiting`. Clears any
    /// stale `wait_reason` — only `Waiting` may carry one.
    pub fn try_transition(&mut self, to: TaskLifecycle) -> Result<u64, TransitionError> {
        debug_assert!(
            to != TaskLifecycle::Waiting,
            "use enter_waiting to transition into Waiting with a reason"
        );
        if !is_legal_transition(self.lifecycle, to) {
            return Err(TransitionError::Illegal {
                from: self.lifecycle,
                to,
            });
        }
        self.lifecycle = to;
        self.wait_reason = None;
        self.revision += 1;
        Ok(self.revision)
    }

    /// Transition into `Waiting`, atomically attaching the structured reason
    /// a response must correlate against to resume the attempt.
    pub fn enter_waiting(&mut self, reason: WaitReason) -> Result<u64, TransitionError> {
        if !is_legal_transition(self.lifecycle, TaskLifecycle::Waiting) {
            return Err(TransitionError::Illegal {
                from: self.lifecycle,
                to: TaskLifecycle::Waiting,
            });
        }
        self.lifecycle = TaskLifecycle::Waiting;
        self.wait_reason = Some(reason);
        self.revision += 1;
        Ok(self.revision)
    }

    /// Start a brand-new task: legal from `Ready` or any terminal state
    /// (never from `Working`/`Waiting` — a task can't overlap another).
    /// Deliberately bypasses `is_legal_transition`'s CAS table: continuing
    /// after a terminal outcome is an explicit new attempt, not a resumption
    /// of the old one, so the usual "no edge out of terminal" rule doesn't
    /// apply here.
    pub fn start_new_task(&mut self, task_id: TaskId) -> Result<u64, TransitionError> {
        if self.lifecycle == TaskLifecycle::Working || self.lifecycle == TaskLifecycle::Waiting {
            return Err(TransitionError::Illegal {
                from: self.lifecycle,
                to: TaskLifecycle::Working,
            });
        }
        self.task_id = task_id;
        self.attempt_id = AttemptId(1);
        self.lifecycle = TaskLifecycle::Working;
        self.wait_reason = None;
        self.cancel_requested = false;
        self.revision += 1;
        Ok(self.revision)
    }

    /// Directly reconstruct from persisted state (session restoration).
    /// Bypasses the transition CAS table on purpose — restoration reconciles
    /// historical state, it does not "transition" into it from some prior
    /// in-memory value that never existed this process.
    ///
    /// Task/attempt ids are not themselves persisted (Forge keeps no
    /// cross-restart task registry — see the module doc), so a restored
    /// non-`Ready` state gets placeholder id `1`: it only needs to be
    /// distinct from `0` (no task) and monotonically increase from here for
    /// the remainder of this process's lifetime, not match whatever the
    /// task was actually numbered before the restart.
    pub fn from_restored(
        session_id: SessionId,
        lifecycle: TaskLifecycle,
        wait_reason: Option<WaitReason>,
    ) -> Self {
        let has_task = lifecycle != TaskLifecycle::Ready;
        Self {
            session_id,
            task_id: TaskId(has_task as u64),
            attempt_id: AttemptId(has_task as u64),
            lifecycle,
            wait_reason,
            cancel_requested: false,
            revision: 0,
        }
    }

    /// True only while `Waiting` on this exact request — used to reject
    /// stale/superseded responses (a different request id, a task that has
    /// since moved on, or a request that was already consumed).
    pub fn is_active_wait(&self, request_id: &str) -> bool {
        matches!(
            (&self.lifecycle, &self.wait_reason),
            (TaskLifecycle::Waiting, Some(reason)) if reason.request_id() == request_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::HitlPayload;
    use uuid::Uuid;

    fn approval(request_id: &str) -> WaitReason {
        WaitReason::Approval {
            request_id: request_id.into(),
            payload: HitlPayload {
                call_id: request_id.into(),
                tool: "bash".into(),
                args_redacted: serde_json::json!({}),
                reason: "policy requires human approval".into(),
                sandbox_escalation: false,
                denied_host: None,
            },
        }
    }

    #[test]
    fn ready_to_working_is_legal() {
        assert!(is_legal_transition(
            TaskLifecycle::Ready,
            TaskLifecycle::Working
        ));
    }

    #[test]
    fn working_can_reach_every_terminal_and_waiting_state() {
        for to in [
            TaskLifecycle::Waiting,
            TaskLifecycle::Completed,
            TaskLifecycle::Failed,
            TaskLifecycle::Cancelled,
            TaskLifecycle::Interrupted,
        ] {
            assert!(
                is_legal_transition(TaskLifecycle::Working, to),
                "Working -> {to:?} should be legal"
            );
        }
    }

    #[test]
    fn waiting_can_resume_cancel_or_interrupt_but_never_complete_directly() {
        assert!(is_legal_transition(
            TaskLifecycle::Waiting,
            TaskLifecycle::Working
        ));
        assert!(is_legal_transition(
            TaskLifecycle::Waiting,
            TaskLifecycle::Cancelled
        ));
        assert!(is_legal_transition(
            TaskLifecycle::Waiting,
            TaskLifecycle::Interrupted
        ));
        assert!(!is_legal_transition(
            TaskLifecycle::Waiting,
            TaskLifecycle::Completed
        ));
        assert!(!is_legal_transition(
            TaskLifecycle::Waiting,
            TaskLifecycle::Failed
        ));
    }

    #[test]
    fn terminal_states_have_no_outgoing_transition() {
        for from in [
            TaskLifecycle::Completed,
            TaskLifecycle::Failed,
            TaskLifecycle::Cancelled,
            TaskLifecycle::Interrupted,
        ] {
            for to in [
                TaskLifecycle::Ready,
                TaskLifecycle::Working,
                TaskLifecycle::Waiting,
                TaskLifecycle::Completed,
                TaskLifecycle::Failed,
                TaskLifecycle::Cancelled,
                TaskLifecycle::Interrupted,
            ] {
                assert!(
                    !is_legal_transition(from, to),
                    "{from:?} -> {to:?} must be illegal: terminal states never resume"
                );
            }
        }
    }

    #[test]
    fn ready_cannot_skip_straight_to_a_terminal_state() {
        for to in [
            TaskLifecycle::Waiting,
            TaskLifecycle::Completed,
            TaskLifecycle::Failed,
            TaskLifecycle::Cancelled,
            TaskLifecycle::Interrupted,
        ] {
            assert!(!is_legal_transition(TaskLifecycle::Ready, to));
        }
    }

    #[test]
    fn active_task_state_starts_ready_with_zeroed_ids() {
        let state = ActiveTaskState::new(Uuid::new_v4());
        assert_eq!(state.lifecycle, TaskLifecycle::Ready);
        assert_eq!(state.task_id, TaskId(0));
        assert_eq!(state.attempt_id, AttemptId(0));
        assert_eq!(state.revision, 0);
        assert!(state.wait_reason.is_none());
    }

    #[test]
    fn try_transition_rejects_illegal_move_and_preserves_current_state() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        let err = state.try_transition(TaskLifecycle::Completed).unwrap_err();
        assert_eq!(
            err,
            TransitionError::Illegal {
                from: TaskLifecycle::Ready,
                to: TaskLifecycle::Completed,
            }
        );
        // Rejected transition must not mutate state.
        assert_eq!(state.lifecycle, TaskLifecycle::Ready);
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn try_transition_bumps_revision_on_success() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        state.start_new_task(TaskId(1)).unwrap();
        let rev = state.try_transition(TaskLifecycle::Completed).unwrap();
        assert_eq!(rev, 2);
        assert_eq!(state.revision, 2);
        assert_eq!(state.lifecycle, TaskLifecycle::Completed);
    }

    #[test]
    fn enter_waiting_attaches_reason_and_leaving_waiting_clears_it() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        state.start_new_task(TaskId(1)).unwrap();
        state.enter_waiting(approval("req-1")).unwrap();
        assert_eq!(state.lifecycle, TaskLifecycle::Waiting);
        assert!(state.is_active_wait("req-1"));
        assert!(!state.is_active_wait("req-stale"));

        state.try_transition(TaskLifecycle::Working).unwrap();
        assert!(state.wait_reason.is_none());
        assert!(!state.is_active_wait("req-1"));
    }

    #[test]
    fn cancelling_while_waiting_invalidates_the_outstanding_request() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        state.start_new_task(TaskId(1)).unwrap();
        state.enter_waiting(approval("req-1")).unwrap();
        state.try_transition(TaskLifecycle::Cancelled).unwrap();
        assert!(!state.is_active_wait("req-1"));
        assert_eq!(state.lifecycle, TaskLifecycle::Cancelled);
    }

    #[test]
    fn start_new_task_bypasses_terminal_lockout_and_resets_attempt() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        state.start_new_task(TaskId(1)).unwrap();
        state.try_transition(TaskLifecycle::Failed).unwrap();
        assert_eq!(state.lifecycle, TaskLifecycle::Failed);

        let rev = state.start_new_task(TaskId(2)).unwrap();
        assert_eq!(state.lifecycle, TaskLifecycle::Working);
        assert_eq!(state.task_id, TaskId(2));
        assert_eq!(state.attempt_id, AttemptId(1));
        assert_eq!(rev, state.revision);
    }

    #[test]
    fn start_new_task_rejected_while_a_task_is_already_active() {
        let mut state = ActiveTaskState::new(Uuid::new_v4());
        state.start_new_task(TaskId(1)).unwrap();
        assert_eq!(state.lifecycle, TaskLifecycle::Working);
        let err = state.start_new_task(TaskId(2)).unwrap_err();
        assert_eq!(
            err,
            TransitionError::Illegal {
                from: TaskLifecycle::Working,
                to: TaskLifecycle::Working,
            }
        );
        // Must not have clobbered the still-active first task.
        assert_eq!(state.task_id, TaskId(1));
    }

    #[test]
    fn is_active_wait_false_when_not_waiting() {
        let state = ActiveTaskState::new(Uuid::new_v4());
        assert!(!state.is_active_wait("anything"));
    }

    #[test]
    fn from_restored_ready_gets_zeroed_ids() {
        let state = ActiveTaskState::from_restored(Uuid::new_v4(), TaskLifecycle::Ready, None);
        assert_eq!(state.task_id, TaskId(0));
        assert_eq!(state.attempt_id, AttemptId(0));
    }

    #[test]
    fn from_restored_non_ready_gets_placeholder_ids_and_preserves_wait_reason() {
        let reason = approval("req-9");
        let state =
            ActiveTaskState::from_restored(Uuid::new_v4(), TaskLifecycle::Waiting, Some(reason));
        assert_eq!(state.task_id, TaskId(1));
        assert_eq!(state.attempt_id, AttemptId(1));
        assert!(state.is_active_wait("req-9"));
    }
}
