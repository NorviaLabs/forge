//! Which task an operator action addresses.
//!
//! `TuiApp` owns exactly one `AgentSession` — the primary. Every other task in
//! the repository lives inside the supervisor, reachable only as an immutable
//! [`forge_session::TaskRuntimeSnapshot`] plus a command channel. Almost every
//! interaction path in this crate predates that split and reads or mutates
//! `self.session` unconditionally, which is silently wrong while a sibling is
//! selected: an approval meant for the sibling would resolve the primary's.
//!
//! This module is the single place that answers "who am I acting on?", so a
//! path can either route to the supervisor or say plainly that it cannot yet.

use super::*;

/// The runtime behind the currently selected task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectedRuntime {
    /// The session `TuiApp` owns and drives directly.
    Primary,
    /// A supervisor-owned actor, addressed by `SupervisorCommand`.
    Sibling(uuid::Uuid),
}

impl TuiApp {
    pub(crate) fn selected_runtime(&self) -> SelectedRuntime {
        if self.supervisor.is_some() && self.selected_task_id != self.session.session_id {
            SelectedRuntime::Sibling(self.selected_task_id)
        } else {
            SelectedRuntime::Primary
        }
    }

    pub(crate) fn selected_is_sibling(&self) -> bool {
        matches!(self.selected_runtime(), SelectedRuntime::Sibling(_))
    }

    /// The supervisor's latest view of the selected task, or `None` when the
    /// primary is selected (its state is read from `session_view` instead).
    pub(crate) fn selected_snapshot(&self) -> Option<&forge_session::TaskRuntimeSnapshot> {
        match self.selected_runtime() {
            SelectedRuntime::Primary => None,
            SelectedRuntime::Sibling(session_id) => self
                .supervisor
                .as_ref()
                .and_then(|supervisor| supervisor.snapshots.get(&session_id)),
        }
    }

    /// The label of the selected task, for messages that name it.
    pub(crate) fn selected_task_label(&self) -> String {
        self.task_chrome
            .iter()
            .find(|task| task.session_id == self.selected_task_id)
            .map(|task| task.label.clone())
            .unwrap_or_else(|| "the selected task".into())
    }

    /// Gate an action that only the primary session can perform today.
    ///
    /// Returns `true` when it is safe to proceed. Otherwise it explains which
    /// action was refused and how to get to it, rather than quietly running it
    /// against the wrong session.
    pub(crate) fn require_primary_task(&mut self, action: &str) -> bool {
        if !self.selected_is_sibling() {
            return true;
        }
        let label = self.selected_task_label();
        self.set_feedback(
            FeedbackSeverity::Warn,
            format!(
                "{action} applies to this task's own session — switch back from `{label}` first"
            ),
        );
        false
    }
}
