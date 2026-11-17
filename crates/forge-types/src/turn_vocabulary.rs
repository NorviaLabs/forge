//! The stored names of how a turn ended and of what was observed during
//! it. Turn records keep them as text (`forge_durable::TurnRecord`'s
//! `outcome` and `signals`); the session that writes them and the report
//! that reads them back both go through these enums, so they cannot drift
//! apart. The names are serde's, and journals already hold them: never
//! rename one.

use serde::{Deserialize, Serialize};

use crate::TaskLifecycle;

/// Something observed during a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSignalKind {
    /// A tool call paused for the user's approval.
    ApprovalRequested,
    /// The paused call asked to leave the sandbox.
    SandboxEscalationRequested,
    ApprovalGranted,
    ApprovalDenied,
    /// The agent asked the user a question.
    QuestionAsked,
    QuestionAnswered,
    QuestionDismissed,
    /// A test, check, lint or build command exited zero.
    VerificationPassed,
    /// A verification command exited non-zero or could not run.
    VerificationFailed,
    /// A verification command was still running when its result came back.
    VerificationUnfinished,
    /// A tool call reported an error.
    ToolFailed,
    /// The provider request failed.
    ModelCallFailed,
    /// The turn stopped on a runtime error other than the provider's (journal
    /// I/O, an invalid transition, …).
    RuntimeError,
    /// The conversation was compacted to fit the context window.
    ContextCompacted,
    /// The turn switched to a model tier.
    TierApplied,
    /// A tier was asked for but nothing could be changed.
    TierUnavailable,
    /// This failed subagent turn will be re-run on the strong tier.
    Escalated,
    /// This failed subagent turn qualified, but was not re-run.
    EscalationSkipped,
    /// The user stopped the turn.
    UserCancelled,
    /// The parent agent interrupted this subagent's turn.
    InterruptedByParent,
    /// Forge restarted while the turn was running.
    InterruptedByRestart,
    /// The user sent a message while the turn was running (or waiting on
    /// them). It is queued, and runs after this turn.
    UserMessageQueued,
    /// How this subagent's model was chosen, when its caller named no tier
    /// or model: `<ranking|exploration|fallback>: <reason>`. On each of its
    /// turns.
    ModelChosen,
    /// The model and effort Auto chose for this main-session turn, and why:
    /// `<ranked|fallback|stepped_up|stepped_down>: <reason>`.
    AutoModel,
}

impl TurnSignalKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::ApprovalRequested,
        Self::SandboxEscalationRequested,
        Self::ApprovalGranted,
        Self::ApprovalDenied,
        Self::QuestionAsked,
        Self::QuestionAnswered,
        Self::QuestionDismissed,
        Self::VerificationPassed,
        Self::VerificationFailed,
        Self::VerificationUnfinished,
        Self::ToolFailed,
        Self::ModelCallFailed,
        Self::RuntimeError,
        Self::ContextCompacted,
        Self::TierApplied,
        Self::TierUnavailable,
        Self::Escalated,
        Self::EscalationSkipped,
        Self::UserCancelled,
        Self::InterruptedByParent,
        Self::InterruptedByRestart,
        Self::UserMessageQueued,
        Self::ModelChosen,
        Self::AutoModel,
    ];

    /// The stored name (serde's).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequested => "approval_requested",
            Self::SandboxEscalationRequested => "sandbox_escalation_requested",
            Self::ApprovalGranted => "approval_granted",
            Self::ApprovalDenied => "approval_denied",
            Self::QuestionAsked => "question_asked",
            Self::QuestionAnswered => "question_answered",
            Self::QuestionDismissed => "question_dismissed",
            Self::VerificationPassed => "verification_passed",
            Self::VerificationFailed => "verification_failed",
            Self::VerificationUnfinished => "verification_unfinished",
            Self::ToolFailed => "tool_failed",
            Self::ModelCallFailed => "model_call_failed",
            Self::RuntimeError => "runtime_error",
            Self::ContextCompacted => "context_compacted",
            Self::TierApplied => "tier_applied",
            Self::TierUnavailable => "tier_unavailable",
            Self::Escalated => "escalated",
            Self::EscalationSkipped => "escalation_skipped",
            Self::UserCancelled => "user_cancelled",
            Self::InterruptedByParent => "interrupted_by_parent",
            Self::InterruptedByRestart => "interrupted_by_restart",
            Self::UserMessageQueued => "user_message_queued",
            Self::ModelChosen => "model_chosen",
            Self::AutoModel => "auto_model",
        }
    }

    /// The kind a stored name names; `None` for a name this build does not
    /// know (a newer Forge's).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == name)
    }
}

/// How a turn ended: its task attempt's terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    /// Recorded at a state that is not terminal; not expected.
    Unknown,
}

impl TurnOutcome {
    pub const ALL: &'static [Self] = &[
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::Interrupted,
        Self::Unknown,
    ];

    /// The outcome of a turn that reached `lifecycle`.
    pub const fn of(lifecycle: TaskLifecycle) -> Self {
        match lifecycle {
            TaskLifecycle::Completed => Self::Completed,
            TaskLifecycle::Failed => Self::Failed,
            TaskLifecycle::Cancelled => Self::Cancelled,
            TaskLifecycle::Interrupted => Self::Interrupted,
            TaskLifecycle::Ready | TaskLifecycle::Working | TaskLifecycle::Waiting => Self::Unknown,
        }
    }

    /// The stored name (serde's).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }

    /// The outcome a stored name names.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|outcome| outcome.as_str() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names are the stored ones: what serde writes, and what a reader
    /// parses back.
    #[test]
    fn every_signal_name_is_its_stored_name() {
        for kind in TurnSignalKind::ALL {
            let stored = serde_json::to_value(kind).unwrap();
            assert_eq!(stored.as_str(), Some(kind.as_str()), "{kind:?}");
            assert_eq!(TurnSignalKind::from_name(kind.as_str()), Some(*kind));
            let parsed: TurnSignalKind = serde_json::from_value(stored).unwrap();
            assert_eq!(parsed, *kind);
        }
        assert_eq!(TurnSignalKind::from_name("no_such_signal"), None);
    }

    #[test]
    fn signal_names_are_unchanged() {
        // Journals already hold these; renaming one would orphan them.
        assert_eq!(TurnSignalKind::ApprovalGranted.as_str(), "approval_granted");
        assert_eq!(TurnSignalKind::ApprovalDenied.as_str(), "approval_denied");
        assert_eq!(
            TurnSignalKind::VerificationPassed.as_str(),
            "verification_passed"
        );
        assert_eq!(
            TurnSignalKind::VerificationFailed.as_str(),
            "verification_failed"
        );
        assert_eq!(
            TurnSignalKind::UserMessageQueued.as_str(),
            "user_message_queued"
        );
        assert_eq!(
            TurnSignalKind::InterruptedByRestart.as_str(),
            "interrupted_by_restart"
        );
        assert_eq!(TurnSignalKind::ModelChosen.as_str(), "model_chosen");
        assert_eq!(TurnSignalKind::AutoModel.as_str(), "auto_model");
        assert_eq!(TurnSignalKind::ALL.len(), 24);
    }

    #[test]
    fn a_turns_outcome_is_named_by_how_it_ended() {
        use crate::TaskLifecycle;
        let cases = [
            (
                TaskLifecycle::Completed,
                TurnOutcome::Completed,
                "completed",
            ),
            (TaskLifecycle::Failed, TurnOutcome::Failed, "failed"),
            (
                TaskLifecycle::Cancelled,
                TurnOutcome::Cancelled,
                "cancelled",
            ),
            (
                TaskLifecycle::Interrupted,
                TurnOutcome::Interrupted,
                "interrupted",
            ),
            (TaskLifecycle::Working, TurnOutcome::Unknown, "unknown"),
            (TaskLifecycle::Waiting, TurnOutcome::Unknown, "unknown"),
            (TaskLifecycle::Ready, TurnOutcome::Unknown, "unknown"),
        ];
        for (lifecycle, outcome, name) in cases {
            assert_eq!(TurnOutcome::of(lifecycle), outcome);
            assert_eq!(outcome.as_str(), name);
            assert_eq!(TurnOutcome::from_name(name), Some(outcome));
            assert_eq!(serde_json::to_value(outcome).unwrap(), name);
        }
        assert_eq!(TurnOutcome::from_name("exploded"), None);
    }
}
