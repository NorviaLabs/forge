//! Which runtime owns the Session an operator action addresses.
//!
//! Forge has two runtime ownership modes: a directly-owned AgentSession for
//! single-session workspaces, and
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
    /// The model the selected session itself records, when it has one.
    ///
    /// Runtime config is startup state and the connect profile is global
    /// credential state; neither is authoritative once sessions can be
    /// switched independently. A session may carry a restored route without a
    /// model (see `restore_saved_auth` and `disconnect`), so callers deciding
    /// identity must key off the model, not the route.
    fn selected_session_model(&self) -> Option<&str> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| session.active_model.as_str()),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.active_model.as_str()),
        }
        .filter(|model| !model.is_empty())
    }

    /// The model identity belonging to the selected session, falling back to
    /// the startup runtime label for a session that has not picked its own.
    pub(crate) fn selected_model_label(&self) -> String {
        self.selected_session_model()
            .map(str::to_string)
            .unwrap_or_else(|| self.runtime.model_label.clone())
    }

    /// Resolve the selected session's provider route for display.
    ///
    /// Credentials remain global, but the route is part of each session's
    /// model selection. Only a session that records its own model may claim a
    /// route; otherwise the global profile preserves the display for legacy
    /// sessions that predate route persistence.
    pub(crate) fn selected_provider_display(
        &self,
    ) -> (String, Option<String>, Option<String>, Option<String>) {
        if self.selected_session_model().is_some() {
            let route_id = self.selected_active_route_id();
            if let Some(profile) = self.connect.registry.get_by_route(&route_id) {
                let route_label =
                    (!profile.route_label.is_empty()).then(|| profile.route_label.clone());
                return (
                    profile.id.clone(),
                    Some(profile.id.clone()),
                    Some(profile.vendor_label.clone()),
                    route_label,
                );
            }
        }
        if let Some(profile_id) = self.connect.profile.as_deref() {
            let (vendor, route) = self.vendor_route_labels(profile_id);
            return (
                self.runtime.provider.clone(),
                Some(profile_id.to_string()),
                vendor,
                route,
            );
        }
        (self.runtime.provider.clone(), None, None, None)
    }

    pub(crate) fn selected_pending_hitl(&self) -> Option<&forge_types::HitlPayload> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .and_then(AgentSession::pending_hitl),
            SelectedRuntime::Supervised(_) => self.session_view.pending_hitl.as_ref(),
        }
    }

    pub(crate) fn selected_pending_question(&self) -> Option<&forge_types::QuestionPayload> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .and_then(AgentSession::pending_question),
            SelectedRuntime::Supervised(_) => self.session_view.pending_question.as_ref(),
        }
    }

    pub(crate) fn selected_message_len(&self) -> usize {
        self.session_runtime.as_ref().map_or_else(
            || self.transcript_view.messages().len(),
            |session| session.messages.len(),
        )
    }

    pub(crate) fn selected_event_len(&self) -> usize {
        self.session_runtime.as_ref().map_or_else(
            || self.transcript_view.events().len(),
            |session| session.events.len(),
        )
    }
    pub(crate) fn set_selected_model(&mut self, model: String) {
        if self.selected_is_supervised() {
            self.try_session_command(forge_session::SupervisorCommand::SetModel {
                session_id: self.selected_session_id,
                model_id: model,
                route_id: self.selected_active_route_id(),
                reasoning_effort: self
                    .selected_details()
                    .and_then(|details| details.reasoning_effort.clone()),
            });
        } else {
            self.session_runtime.set_active_model(model);
        }
    }

    /// Whether the *selected* session currently runs a turn, derived from the
    /// authoritative actor state rather than the restored view flag.
    ///
    /// `busy_state` is saved and restored per session across switches, so a
    /// session that finished while unselected restores stale-busy until the
    /// next supervisor event reconciles it. Routing and presentation must not
    /// read that flag for supervised sessions.
    pub(crate) fn selected_turn_running(&self) -> bool {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self.busy_state.is_active(),
            SelectedRuntime::Supervised(session_id) => self
                .supervisor
                .as_ref()
                .and_then(|supervisor| supervisor.snapshots.get(&session_id))
                .is_some_and(|snapshot| {
                    snapshot.task.turn_state == forge_session::SupervisorTurnState::Running
                }),
        }
    }

    pub(crate) fn selected_input_route(&self, line: &str) -> input_route::InputRoute {
        if !self.selected_is_supervised() {
            return input_route::classify_input(
                &self.session_runtime.active_task,
                self.overlay.is_some(),
                line,
            );
        }
        use input_route::InputRoute;
        if self.session_view.pending_question.is_some() && self.overlay.is_none() {
            InputRoute::AnswerClarification
        } else if self.session_view.lifecycle == forge_types::TaskLifecycle::Waiting {
            InputRoute::RejectStaleResponse
        } else if self.selected_turn_running() {
            InputRoute::QueueFutureTask
        } else {
            InputRoute::StartNewTask
        }
    }
    pub(crate) fn selected_runtime(&self) -> SelectedRuntime {
        if self
            .session_runtime
            .as_ref()
            .is_some_and(|session| session.session_id == self.selected_session_id)
        {
            return SelectedRuntime::Direct;
        }
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

    pub(crate) fn selected_details(&self) -> Option<&forge_session::SessionDetailsSnapshot> {
        self.selected_snapshot()
            .and_then(|snapshot| snapshot.details.as_ref())
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

    pub(crate) fn selected_background_tasks(&self) -> Vec<forge_session::BackgroundTaskSnapshot> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| {
                    session
                        .background()
                        .list()
                        .map(forge_session::BackgroundTaskSnapshot::capture)
                        .collect()
                })
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

    pub(crate) fn selected_journal_dir(&self) -> PathBuf {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| session.journal_dir().to_path_buf())
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.journal_dir.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_active_route_id(&self) -> String {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| session.active_route_id.clone())
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.active_route_id.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_image_input_supported(&self) -> bool {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .is_some_and(AgentSession::image_input_supported),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .is_some_and(|details| details.image_input_supported),
        }
    }

    pub(crate) fn selected_loaded_skill_names(&self) -> Vec<String> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(AgentSession::loaded_skill_names)
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.skills.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_tool_names(&self) -> Vec<String> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(AgentSession::list_tools)
                .unwrap_or_default(),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.tools.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn selected_session_pattern_allow_count(&self) -> usize {
        match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map_or(0, AgentSession::session_pattern_allow_count),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map_or(0, |details| details.session_pattern_allow_count),
        }
    }

    pub(crate) fn try_session_command(
        &mut self,
        command: forge_session::SupervisorCommand,
    ) -> bool {
        let result = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.handle.try_command(command));
        match result {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                self.set_feedback(FeedbackSeverity::Error, error.to_string());
                false
            }
            None => false,
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
}
