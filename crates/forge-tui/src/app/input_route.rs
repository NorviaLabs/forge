//! Input-routing decision for the composer: where does a submitted line go,
//! based on authoritative lifecycle state — never on `busy`/streaming flags
//! alone. Pure and side-effect-free so it's testable without a `TuiApp`.

use forge_core::ActiveTaskState;
use forge_types::{TaskLifecycle, WaitReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputRoute {
    /// `Ready` or a terminal attempt: dispatch immediately as a new task.
    StartNewTask,
    /// `Working`: not answering anything active, becomes a future queue item.
    QueueFutureTask,
    /// `Waiting` on `WaitReason::Clarification` — no runtime producer exists
    /// yet, kept for structural completeness (see `forge_types::WaitReason`).
    AnswerClarification,
    /// `Waiting` on `WaitReason::Selection` — same caveat as above.
    ResolveSelection,
    /// `Waiting`, but the input doesn't resolve the active request (an
    /// unparseable approval answer, or a wait reason with no defined
    /// resolution path yet). Must not be silently queued or dispatched as a
    /// new task while something is still outstanding.
    RejectStaleResponse,
}

/// Classify a submitted composer line given the authoritative lifecycle.
/// `overlay_open` lets a caller reflect that an approval overlay is already
/// intercepting keys (in which case composer text should never reach this
/// classifier for `Waiting` at all) — kept as an explicit input rather than
/// inferred, so this stays a pure function of its arguments.
pub(crate) fn classify_input(
    active: &ActiveTaskState,
    overlay_open: bool,
    _line: &str,
) -> InputRoute {
    if active.lifecycle == TaskLifecycle::Waiting {
        // An approval decision is made on the inline approval card, never by
        // typing in the composer; any composer text arriving while `Waiting`
        // on approval must not be treated as a fresh dispatch or a future
        // queue item.
        if overlay_open {
            return InputRoute::RejectStaleResponse;
        }
        return match &active.wait_reason {
            Some(WaitReason::Approval { .. }) => InputRoute::RejectStaleResponse,
            Some(WaitReason::Clarification { .. }) => InputRoute::AnswerClarification,
            Some(WaitReason::Selection { .. }) => InputRoute::ResolveSelection,
            // `MissingConfiguration`/`ExternalAction` (and any future
            // variant) have no defined resolution-via-composer-text path.
            _ => InputRoute::RejectStaleResponse,
        };
    }
    if active.lifecycle == TaskLifecycle::Working {
        return InputRoute::QueueFutureTask;
    }
    InputRoute::StartNewTask
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::HitlPayload;
    use uuid::Uuid;

    fn state(lifecycle: TaskLifecycle, wait_reason: Option<WaitReason>) -> ActiveTaskState {
        let mut s = ActiveTaskState::new(Uuid::new_v4());
        // Force the lifecycle/wait_reason directly for classifier testing —
        // legality of getting there is exercised in `forge_core::lifecycle`.
        s.lifecycle = lifecycle;
        s.wait_reason = wait_reason;
        s
    }

    fn approval() -> WaitReason {
        WaitReason::Approval {
            request_id: "r1".into(),
            payload: HitlPayload {
                call_id: "r1".into(),
                tool: "bash".into(),
                args_redacted: serde_json::json!({}),
                reason: "policy requires human approval".into(),
            },
        }
    }

    #[test]
    fn ready_starts_a_new_task() {
        let s = state(TaskLifecycle::Ready, None);
        assert_eq!(
            classify_input(&s, false, "do something"),
            InputRoute::StartNewTask
        );
    }

    #[test]
    fn terminal_states_start_a_new_task() {
        for lifecycle in [
            TaskLifecycle::Completed,
            TaskLifecycle::Failed,
            TaskLifecycle::Cancelled,
            TaskLifecycle::Interrupted,
        ] {
            let s = state(lifecycle, None);
            assert_eq!(classify_input(&s, false, "again"), InputRoute::StartNewTask);
        }
    }

    #[test]
    fn working_queues_future_task() {
        let s = state(TaskLifecycle::Working, None);
        assert_eq!(
            classify_input(&s, false, "do this next"),
            InputRoute::QueueFutureTask
        );
    }

    #[test]
    fn waiting_on_approval_is_always_rejected_not_queued_or_dispatched() {
        // Approval is decided on the inline card, never by typing in the
        // composer — any composer line arriving while `Waiting` on approval
        // is stale, regardless of its content.
        let s = state(TaskLifecycle::Waiting, Some(approval()));
        assert_eq!(
            classify_input(&s, false, "yes"),
            InputRoute::RejectStaleResponse
        );
        assert_eq!(
            classify_input(&s, false, "run the tests instead"),
            InputRoute::RejectStaleResponse
        );
        assert_eq!(
            classify_input(&s, false, ""),
            InputRoute::RejectStaleResponse
        );
    }

    #[test]
    fn waiting_with_overlay_open_is_rejected_not_dispatched_or_queued() {
        // When the overlay is intercepting keys, composer text (which
        // wouldn't reach this classifier in production anyway) must not be
        // treated as a fresh dispatch or a future queue item.
        let s = state(TaskLifecycle::Waiting, Some(approval()));
        assert_eq!(
            classify_input(&s, true, "yes"),
            InputRoute::RejectStaleResponse
        );
    }

    #[test]
    fn waiting_on_clarification_or_selection_route_distinctly() {
        let clarification = state(
            TaskLifecycle::Waiting,
            Some(WaitReason::Clarification {
                request_id: "r2".into(),
            }),
        );
        assert_eq!(
            classify_input(&clarification, false, "OAuth 2.0"),
            InputRoute::AnswerClarification
        );

        let selection = state(
            TaskLifecycle::Waiting,
            Some(WaitReason::Selection {
                request_id: "r3".into(),
            }),
        );
        assert_eq!(
            classify_input(&selection, false, "option 2"),
            InputRoute::ResolveSelection
        );
    }
}
