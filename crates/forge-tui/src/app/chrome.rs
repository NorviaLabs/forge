//! Operator-facing chrome for [`TuiApp`]: notices, toasts, feedback, the
//! activity summary, and the status header and footer.
//!
//! Split out of `app.rs` per #19. Everything here reports state back to the
//! operator rather than changing it — transient notices and toasts, error and
//! info feedback, the activity feed summary, and the header and footer models
//! describing the current turn, repository and token limits.
//!
//! Named `chrome` to match the vocabulary already used in the crate
//! (`clear_error_chrome`, `apply_connection_chrome`, `session_chrome_lines`), and
//! to avoid confusion with the crate-level `activity` module.
//!
//! Methods are moved verbatim. The `FooterLimits`, `FooterLimitsCache` and
//! `ActivitySummaryModel` types stay in `mod.rs`: unlike `ApprovalIdentity` in
//! `approvals.rs`, they are also used by free functions and `TuiApp` fields
//! there, so they are not exclusively this module's concern.

use super::*;

impl TuiApp {
    pub(super) fn push_notice(&mut self, lines: Vec<String>) {
        self.notices = lines;
        self.notices_until = Some(Instant::now() + Duration::from_secs(3));
    }

    pub(super) fn tick_notices(&mut self) {
        if self
            .notices_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notices.clear();
            self.notices_until = None;
        }
    }

    pub(super) fn push_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((Instant::now(), text.into()));
        // Also mirror briefly into feedback (auto-cleared in draw/tick)
        if let Some((_, ref t)) = self.toast {
            self.set_feedback(FeedbackSeverity::Ok, t.clone());
        }
    }

    pub(super) fn tick_toast(&mut self) {
        if let Some((at, _)) = &self.toast {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast = None;
                if self.feedback.severity == FeedbackSeverity::Ok {
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                }
            }
        }
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_message = text.clone();
        self.feedback = FeedbackModel { text, severity };
    }

    /// Operator errors remain visible in chat, feedback, and activity.
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        self.set_feedback(FeedbackSeverity::Error, msg.clone());
        // Replace prior error banners — don't accumulate red clutter in the chat.
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        self.ui_banners.push(ChatItem::Banner {
            text: msg.clone(),
            kind: BannerKind::Error,
        });
        self.activity
            .push(ActivityKind::Error, FeedbackSeverity::Error, msg);
        self.busy_phase = BusyPhase::Idle;
    }

    /// Drop ephemeral error UI (call on new user turn / Esc).
    pub(super) fn clear_error_chrome(&mut self) {
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        if self.feedback.severity == FeedbackSeverity::Error {
            self.feedback = FeedbackModel::default();
            self.status_message.clear();
        }
    }

    pub fn report_info(&mut self, text: impl Into<String>) {
        self.set_feedback(FeedbackSeverity::Info, text);
    }

    pub fn push_activity(
        &mut self,
        kind: ActivityKind,
        severity: FeedbackSeverity,
        summary: impl Into<String>,
    ) {
        self.activity.push(kind, severity, summary);
    }

    pub fn refresh_status_model(&self) -> StatusModel {
        self.refresh_status_model_with_connected(self.is_provider_connected())
    }

    pub(super) fn refresh_status_model_with_connected(
        &self,
        provider_connected: bool,
    ) -> StatusModel {
        let repo = self.repo_header();
        let id = self.session.session_id.to_string();
        let short = if id.len() > 8 {
            id[..8].to_string()
        } else {
            id
        };
        let turn_cancelled = !self.busy
            && (self.last_exit == ExitCode::Canceled
                || self.session.status == forge_types::SessionStatus::Cancelled);
        StatusModel {
            status: self.session.status,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            effort: self.reasoning_effort.to_string(),
            ctx_pct: self.session.context_usage_ratio(),
            busy: self.busy,
            busy_phase: self.busy_phase.clone(),
            connect_profile: self.connect_profile.clone(),
            provider_connected,
            web_search_label: self.web_search_label.clone(),
            tools_visible: self.session.list_tools().len(),
            prompt_cache_hits: self.session.token_usage.prompt_cache_hits,
            prompt_cache_writes: self.session.token_usage.prompt_cache_writes,
            repo_name: repo.repo_name.clone(),
            branch: repo.branch.clone(),
            dirty: repo.dirty,
            resource: self.workspace_resource_label(),
            // Workspace secondary metadata only — never overall task lifecycle.
            activity: self.workspace_activity_label(),
            turn_cancelled,
            progress_description: self.header_progress_description(),
            failure_category: self.header_failure_category(),
            waiting_detail: self.header_waiting_detail(),
        }
    }

    /// Typed progress for the header while Working. Structured sources only.
    fn header_progress_description(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        // Prefer durable progress.json in_progress when present.
        if let Ok(text) = std::fs::read_to_string(self.runtime.cwd.join(".forge/progress.json")) {
            if let Ok(doc) = serde_json::from_str::<ProgressDocument>(&text) {
                let step = doc.in_progress.trim();
                if !step.is_empty() {
                    return Some(step.to_string());
                }
            }
        }
        self.busy_phase.progress_description()
    }

    fn header_waiting_detail(&self) -> Option<String> {
        if self.session.pending_hitl.is_some()
            || self.session.status == forge_types::SessionStatus::AwaitingHitl
        {
            return Some("Approval required".into());
        }
        if self.busy && matches!(self.busy_phase, BusyPhase::Connect) {
            return Some("Your input required".into());
        }
        None
    }

    fn header_failure_category(&self) -> Option<String> {
        if self.session.status != forge_types::SessionStatus::Failed {
            return None;
        }
        // Prefer the latest structured turn_failed event category.
        for event in self.session.events.iter().rev() {
            if event.kind == "turn_failed" || event.kind == "validation_exhausted" {
                let detail = event.detail.as_str();
                let category = detail.split(':').next().unwrap_or(detail).trim();
                return Some(failure_category_label(category));
            }
        }
        for message in self.session.messages.iter().rev() {
            if message.role != MessageRole::Assistant {
                continue;
            }
            if let Some(rest) = message.content.strip_prefix(forge_core::TURN_FAILED_MARKER) {
                let summary = rest.trim();
                if summary.contains("repeated invalid tool") {
                    return Some("Tool retries exhausted".into());
                }
                if summary.contains("couldn't complete this turn") {
                    return Some("Turn incomplete".into());
                }
                return None;
            }
        }
        None
    }

    fn workspace_resource_label(&self) -> Option<String> {
        match &self.workspace_navigation.current {
            WorkspaceView::Conversation => None,
            WorkspaceView::File(path) => {
                Some(relative_display(self.session.workspace_root(), path))
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => Some("Review changes".into()),
            WorkspaceView::Run(id) => self
                .run
                .current
                .as_ref()
                .filter(|record| record.id == *id)
                .map(|record| format!("Run: {}", record.invocation.summary()))
                .or_else(|| Some("Run".into())),
        }
    }

    fn workspace_activity_label(&self) -> Option<String> {
        match &self.workspace_navigation.current {
            WorkspaceView::Diff(DiffCommandContext::Current) => {
                let total = self.file_explorer.git_status.status.len();
                (total > 0).then(|| format!("{} of {} changes", self.diff_selected + 1, total))
            }
            WorkspaceView::Run(id) => self
                .run
                .current
                .as_ref()
                .filter(|record| record.id == *id)
                .map(|record| {
                    (match record.state {
                        RunState::Queued => "Queued",
                        RunState::Running => "Running",
                        RunState::Succeeded => "Succeeded",
                        RunState::Failed => "Failed",
                        RunState::Cancelled => "Cancelled",
                        RunState::StartFailed => "Could not start",
                        RunState::CaptureFailed => "Capture failed",
                    })
                    .to_string()
                }),
            _ => {
                let changes = self.file_explorer.git_status.status.len();
                if changes > 0 {
                    Some(format!("{changes} changes · Review"))
                } else {
                    // Do not mirror task lifecycle/progress into secondary activity.
                    None
                }
            }
        }
    }

    pub(super) fn activity_summary(&self) -> Option<ActivitySummaryModel> {
        // Approval is represented by the blocking overlay, not a background summary.
        if self.overlay.is_some() || self.session.pending_hitl.is_some() {
            return None;
        }

        if let Some(record) = self.run.current.as_ref() {
            let command = record.invocation.summary();
            if matches!(
                record.state,
                RunState::Failed | RunState::StartFailed | RunState::CaptureFailed
            ) {
                return Some(ActivitySummaryModel {
                    label: format!("Run failed: {command}"),
                    action_label: Some("Inspect"),
                    action: Some(ActivitySummaryAction::OpenRun(record.id.clone())),
                    kind: BannerKind::Error,
                });
            }
            if matches!(record.state, RunState::Queued | RunState::Running) {
                return Some(ActivitySummaryModel {
                    label: format!("Running {command}"),
                    action_label: Some("View output"),
                    action: Some(ActivitySummaryAction::OpenRun(record.id.clone())),
                    kind: BannerKind::Info,
                });
            }
        }

        let changes = self.file_explorer.git_status.status.len();
        if changes > 0 {
            let files = if changes == 1 { "file" } else { "files" };
            return Some(ActivitySummaryModel {
                label: format!("{changes} {files} changed"),
                action_label: Some("Review"),
                action: Some(ActivitySummaryAction::ReviewChanges),
                kind: BannerKind::Info,
            });
        }

        if self.busy && matches!(self.busy_phase, BusyPhase::Model) {
            return Some(ActivitySummaryModel {
                label: "Forge is thinking".into(),
                action_label: None,
                action: None,
                kind: BannerKind::Info,
            });
        }

        None
    }

    pub(super) fn activity_summary_cache_key(
        &self,
    ) -> Option<(String, Option<&'static str>, BannerKind)> {
        self.activity_summary()
            .map(|summary| (summary.label, summary.action_label, summary.kind))
    }

    pub(super) fn activity_summary_command(&self) -> Option<SemanticCommand> {
        match self.activity_summary()?.action? {
            ActivitySummaryAction::OpenRun(id) => {
                Some(SemanticCommand::OpenRun(RunCommandTarget::Id(id)))
            }
            ActivitySummaryAction::ReviewChanges => {
                Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            }
        }
    }

    pub(super) fn activate_activity_summary(&mut self) {
        match self.activity_summary().and_then(|summary| summary.action) {
            Some(ActivitySummaryAction::OpenRun(id)) => {
                self.navigate_to_workspace_view(WorkspaceView::Run(id));
            }
            Some(ActivitySummaryAction::ReviewChanges) => {
                self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current));
            }
            None => {}
        }
    }

    /// Read the cached repo header. This is a plain field read: it must never
    /// spawn a subprocess, because callers sit on the render path.
    pub(super) fn repo_header(&self) -> RepoHeaderCache {
        self.repo_header.clone()
    }

    /// Advance the off-thread repo-header refresh. Non-blocking and safe to call
    /// from the render loop, matching `GitStatusCache::poll`.
    ///
    /// The previous value is retained while a refresh is in flight and when a
    /// refresh fails, so the header never blanks mid-update (FORGE-DESIGN 9.7).
    pub(super) fn poll_repo_header(&mut self) {
        // A cwd change makes the cached header describe the wrong directory, so
        // read through synchronously: the next frame must be correct, and this
        // only happens on a workspace switch, never frame to frame.
        if self.repo_header_cwd != self.runtime.cwd {
            self.repo_header_cwd = self.runtime.cwd.clone();
            self.repo_header = load_repo_header(&self.repo_header_cwd);
            self.repo_header_refreshed_at = Instant::now();
            // Drop any refresh still in flight for the previous directory.
            self.repo_header_rx = None;
            return;
        }

        if let Some(rx) = self.repo_header_rx.take() {
            match rx.try_recv() {
                Ok(header) => {
                    self.repo_header = header;
                    self.repo_header_refreshed_at = Instant::now();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.repo_header_rx = Some(rx);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Keep the last known header; retry after the next TTL window.
                    self.repo_header_refreshed_at = Instant::now();
                }
            }
            return;
        }

        if self.repo_header_refreshed_at.elapsed() < REPO_HEADER_TTL {
            return;
        }

        let cwd = self.runtime.cwd.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_repo_header(&cwd));
        });
        self.repo_header_rx = Some(rx);
    }

    #[cfg(test)]
    pub(super) fn busy_status_detail(&self) -> Option<String> {
        self.busy.then(|| {
            let label = if !self.stream_thinking.is_empty() && self.stream_preview.is_empty() {
                "Thinking..."
            } else {
                "Working..."
            };
            let elapsed = self
                .turn_started
                .map(|started| started.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            format!("{label} {}", format_elapsed_tenths(elapsed))
        })
    }

    #[allow(dead_code)]
    fn footer_limits(&mut self, provider: &str) -> FooterLimits {
        if let Some(rx) = &self.footer_limits_rx {
            match rx.try_recv() {
                Ok((provider, limits)) => {
                    self.footer_limits_cache = Some(FooterLimitsCache {
                        provider,
                        fetched_at: Instant::now(),
                        limits,
                    });
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if provider != OPENAI_CODEX_PROFILE_ID {
            return FooterLimits::default();
        }

        let (cached_limits, needs_refresh) = match self
            .footer_limits_cache
            .as_ref()
            .filter(|cache| cache.provider == provider)
        {
            Some(cache) => (
                Some(cache.limits.clone()),
                cache.fetched_at.elapsed() >= Duration::from_secs(60),
            ),
            None => (None, true),
        };
        if needs_refresh && self.footer_limits_rx.is_none() {
            let provider = provider.to_string();
            let request_provider = provider.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let report = forge_connect::provider_cost_report(
                    &request_provider,
                    "",
                    0,
                    0,
                    &CredentialStore::user_default(),
                )
                .unwrap_or_default();
                let _ = tx.send((request_provider, footer_limits_from_report(&report)));
            });
            self.footer_limits_rx = Some(rx);
        }

        cached_limits.unwrap_or_default()
    }
}
