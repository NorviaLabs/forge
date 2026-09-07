//! Which runtime owns the Session an operator action addresses.
//!
//! Forge has two runtime ownership modes during the multisession migration:
//! a legacy directly-owned AgentSession for single-session workspaces, and
//! repository-supervised Session actors addressed through snapshots + commands.
//!
//! Routing must depend on ownership, not on whether a Session happens to be the
//! repository's primary worktree. Once the primary is adopted by the supervisor
//! it must automatically take the supervised path like every other Session.

use super::*;

/// Runtime ownership behind the currently selected Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectedRuntime {
    /// Legacy single-session mode: TuiApp owns the AgentSession directly.
    Direct,
    /// Repository mode: a supervisor actor owns the Session.
    Supervised(uuid::Uuid),
}

impl TuiApp {
    pub(crate) fn selected_runtime(&self) -> SelectedRuntime {
        if self
            .supervisor
            .as_ref()
            .is_some_and(|supervisor| supervisor.snapshots.contains_key(&self.selected_session_id))
        {
            SelectedRuntime::Supervised(self.selected_session_id)
        } else {
            SelectedRuntime::Direct
        }
    }

    pub(crate) fn selected_is_supervised(&self) -> bool {
        matches!(self.selected_runtime(), SelectedRuntime::Supervised(_))
    }

    /// The supervisor's latest view of the selected Session, when that
    /// Session is actor-owned rather than directly owned by the TUI.
    pub(crate) fn selected_snapshot(&self) -> Option<&forge_session::SessionRuntimeSnapshot> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => None,
            SelectedRuntime::Supervised(session_id) => self
                .supervisor
                .as_ref()
                .and_then(|supervisor| supervisor.snapshots.get(&session_id)),
        }
    }

    pub(crate) fn selected_details(
        &self,
    ) -> Option<&forge_session::SessionDetailsSnapshot> {
        self.selected_snapshot().and_then(|snapshot| snapshot.details.as_ref())
    }

    pub(crate) fn selected_queue_messages(&self) -> Vec<String> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| {
                    session
                        .queue()
                        .visible()
                        .map(|item| item.text.clone())
                        .collect()
                })
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_snapshot()
                .map(|snapshot| {
                    let mut queued: Vec<String> = snapshot
                        .queued_prompts
                        .iter()
                        .map(|(_, text)| text.clone())
                        .collect();
                    if let Some(details) = snapshot.details.as_ref() {
                        queued.extend(details.queue.iter().map(|item| item.text.clone()));
                    }
                    queued
                })
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_background_tasks(&self) -> Vec<forge_core::BackgroundTaskHandle> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| session.background().list().cloned().collect())
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.background.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_token_usage_report(&self) -> forge_core::TokenUsageReport {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .expect("direct runtime must exist in direct mode")
                .token_usage_report(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .expect("supervised runtime details must exist for an active session")
                .token_usage_report
                .clone(),
        }
    }

    /// The label of the selected Session, for messages that name it.
    pub(crate) fn selected_session_label(&self) -> String {
        self.session_chrome
            .iter()
            .find(|session| session.session_id == self.selected_session_id)
            .map(|session| session.label.clone())
            .unwrap_or_else(|| "the selected session".into())
    }

    /// Gate an operation that has not yet been routed through the supervisor.
    ///
    /// This is intentionally about runtime ownership, not primary-vs-sibling
    /// identity. As migration progresses these gates disappear operation by
    /// operation until repository mode no longer needs direct ownership.
    pub(crate) fn require_direct_session(&mut self, action: &str) -> bool {
        if !self.selected_is_supervised() {
            return true;
        }
        let label = self.selected_session_label();
        self.set_feedback(
            FeedbackSeverity::Warn,
            format!("{action} is not available for supervised session '{label}' yet"),
        );
        false
    }
}
