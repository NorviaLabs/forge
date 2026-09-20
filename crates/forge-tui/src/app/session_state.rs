//! Session-row state and lifecycle presentation helpers.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionChromeItem {
    pub(crate) session_id: uuid::Uuid,
    pub(crate) slot: Option<u8>,
    pub(crate) label: String,
    pub(crate) branch: String,
    pub(crate) lifecycle: forge_types::TaskLifecycle,
    pub(crate) selected: bool,
    pub(crate) secondary: Option<String>,
    pub(crate) attention: bool,
    pub(crate) updated_at: chrono::DateTime<chrono::Utc>,
}

impl SessionChromeItem {
    pub(crate) fn is_working(&self) -> bool {
        !self.attention && matches!(self.secondary.as_deref(), Some("running") | Some("queued"))
    }

    pub(crate) fn is_queued(&self) -> bool {
        !self.attention && self.secondary.as_deref() == Some("queued")
    }
}

pub(crate) fn session_needs_attention(
    selected: bool,
    turn_state: forge_session::SupervisorTurnState,
    interrupted: bool,
) -> bool {
    use forge_session::SupervisorTurnState;
    if selected
        || matches!(
            turn_state,
            SupervisorTurnState::Running | SupervisorTurnState::Queued
        )
    {
        return false;
    }
    interrupted
        || matches!(
            turn_state,
            SupervisorTurnState::Waiting | SupervisorTurnState::Failed
        )
}

pub(crate) fn session_notice_severity(
    turn_state: forge_session::SupervisorTurnState,
) -> crate::widgets::feedback::FeedbackSeverity {
    use crate::widgets::feedback::FeedbackSeverity as Severity;
    use forge_session::SupervisorTurnState;
    match turn_state {
        SupervisorTurnState::Completed => Severity::Ok,
        SupervisorTurnState::Failed => Severity::Error,
        SupervisorTurnState::Waiting | SupervisorTurnState::Interrupted => Severity::Warn,
        _ => Severity::Info,
    }
}
