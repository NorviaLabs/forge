//! Input-routing decision for the composer: where does a submitted line go,
//! based on authoritative lifecycle state — never on `busy`/streaming flags
//! alone. Pure and side-effect-free so it's testable without a `TuiApp`.

use forge_core::ActiveTaskState;
use forge_types::{TaskLifecycle, WaitReason};

/// The operator's decision on a pending HITL approval, parsed from a
/// composer line. `remember`/`always` promote a plain approval to a
/// session-scoped rule; eligibility is checked when the action is applied,
/// not here, so the parser stays a pure function of the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalAction {
    /// Approve the pending call once.
    Approve,
    /// Approve and remember this exact Direct invocation for the session.
    Remember,
    /// Approve and persist the suggested allow pattern going forward.
    AllowPattern,
    /// Deny the pending call.
    Deny,
    /// Deny, carrying the rest of the line back to the agent as feedback.
    DenyWithFeedback(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputRoute {
    /// `Ready` or a terminal attempt: dispatch immediately as a new task.
    StartNewTask,
    /// `Working`: not answering anything active, becomes a future queue item.
    QueueFutureTask,
    /// `Waiting` on `WaitReason::Approval` with a recognized approval line.
    ResolveApproval(ApprovalAction),
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

/// Parse an approval line. The verb is matched at a word boundary — `noise`
/// is unrecognized, not a prefix of `no`. `no <text>` carries the greedy
/// tail as deny feedback; `yes remember` / `yes always` promote the plain
/// approve to those actions. Anything unrecognized falls through to
/// `RejectStaleResponse` so the operator gets an explicit nudge (and keeps
/// their text) rather than a silently misrouted message.
pub(crate) fn parse_approval_line(line: &str) -> Option<ApprovalAction> {
    let trimmed = line.trim();
    let (verb, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (trimmed, ""),
    };
    match verb.to_ascii_lowercase().as_str() {
        "y" | "yes" | "approve" => match rest {
            "" => Some(ApprovalAction::Approve),
            r if r.eq_ignore_ascii_case("remember") => Some(ApprovalAction::Remember),
            r if r.eq_ignore_ascii_case("always") || r.eq_ignore_ascii_case("allow") => {
                Some(ApprovalAction::AllowPattern)
            }
            _ => None,
        },
        "remember" if rest.is_empty() => Some(ApprovalAction::Remember),
        "always" | "allow" if rest.is_empty() => Some(ApprovalAction::AllowPattern),
        "n" | "no" | "deny" => {
            if rest.is_empty() {
                Some(ApprovalAction::Deny)
            } else {
                Some(ApprovalAction::DenyWithFeedback(rest.to_owned()))
            }
        }
        _ => None,
    }
}

/// Classify a submitted composer line given the authoritative lifecycle.
/// `overlay_open` lets a caller reflect that an approval overlay is already
/// intercepting keys (in which case composer text should never reach this
/// classifier for `Waiting` at all) — kept as an explicit input rather than
/// inferred, so this stays a pure function of its arguments.
pub(crate) fn classify_input(
    active: &ActiveTaskState,
    overlay_open: bool,
    line: &str,
) -> InputRoute {
    if active.lifecycle == TaskLifecycle::Waiting {
        // The overlay is already intercepting keys and owns resolution;
        // composer text reaching here regardless must not be treated as a
        // fresh dispatch or a future queue item — something is still
        // outstanding.
        if overlay_open {
            return InputRoute::RejectStaleResponse;
        }
        return match &active.wait_reason {
            Some(WaitReason::Approval { .. }) => match parse_approval_line(line) {
                Some(action) => InputRoute::ResolveApproval(action),
                None => InputRoute::RejectStaleResponse,
            },
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
    fn waiting_on_approval_with_yes_resolves_approve() {
        let s = state(TaskLifecycle::Waiting, Some(approval()));
        assert_eq!(
            classify_input(&s, false, "yes"),
            InputRoute::ResolveApproval(ApprovalAction::Approve)
        );
        assert_eq!(
            classify_input(&s, false, "Y"),
            InputRoute::ResolveApproval(ApprovalAction::Approve)
        );
    }

    #[test]
    fn waiting_on_approval_with_no_resolves_deny() {
        let s = state(TaskLifecycle::Waiting, Some(approval()));
        assert_eq!(
            classify_input(&s, false, "no"),
            InputRoute::ResolveApproval(ApprovalAction::Deny)
        );
    }

    #[test]
    fn waiting_on_approval_with_unparseable_text_is_rejected_not_queued_or_dispatched() {
        let s = state(TaskLifecycle::Waiting, Some(approval()));
        assert_eq!(
            classify_input(&s, false, "run the tests instead"),
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

    #[test]
    fn parse_approval_line_matches_verbs_at_word_boundaries() {
        assert_eq!(
            parse_approval_line("  Yes  "),
            Some(ApprovalAction::Approve)
        );
        assert_eq!(parse_approval_line("DENY"), Some(ApprovalAction::Deny));
        assert_eq!(
            parse_approval_line("approve"),
            Some(ApprovalAction::Approve)
        );
        assert_eq!(
            parse_approval_line("remember"),
            Some(ApprovalAction::Remember)
        );
        assert_eq!(
            parse_approval_line("always"),
            Some(ApprovalAction::AllowPattern)
        );
        assert_eq!(
            parse_approval_line("allow"),
            Some(ApprovalAction::AllowPattern)
        );
        assert_eq!(
            parse_approval_line("yes remember"),
            Some(ApprovalAction::Remember)
        );
        assert_eq!(
            parse_approval_line("yes always"),
            Some(ApprovalAction::AllowPattern)
        );
        // `noise` is not a prefix of `no`.
        assert_eq!(parse_approval_line("maybe"), None);
        assert_eq!(parse_approval_line("noise"), None);
        assert_eq!(parse_approval_line("yes nonsense"), None);
        assert_eq!(parse_approval_line("remember this"), None);
    }

    #[test]
    fn parse_approval_line_carries_greedy_tail_as_deny_feedback() {
        assert_eq!(
            parse_approval_line("no use the workspace instead"),
            Some(ApprovalAction::DenyWithFeedback(
                "use the workspace instead".into()
            ))
        );
        assert_eq!(
            parse_approval_line("n  write the test first  "),
            Some(ApprovalAction::DenyWithFeedback(
                "write the test first".into()
            ))
        );
        assert_eq!(parse_approval_line("deny"), Some(ApprovalAction::Deny));
    }
}
