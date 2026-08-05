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
//! Methods and chrome-related free functions are moved verbatim. Types such as
//! `FooterLimits` and `ActivitySummaryModel` live in `types.inc.rs`.

use crate::widgets::session_chrome_lines;

use super::util::relative_display;
use super::*;

impl TuiApp {
    pub(super) fn push_notice(&mut self, lines: Vec<String>) {
        self.notice_state.items = lines;
        self.notice_state.until = Some(Instant::now() + Duration::from_secs(3));
    }

    pub(super) fn tick_notices(&mut self) {
        if self
            .notice_state
            .until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notice_state.items.clear();
            self.notice_state.until = None;
        }
    }

    pub(super) fn push_toast(&mut self, text: impl Into<String>) {
        self.toast.current = Some((Instant::now(), text.into()));
        // Also mirror briefly into feedback (auto-cleared in draw/tick)
        if let Some((_, ref t)) = self.toast.current {
            self.set_feedback(FeedbackSeverity::Ok, t.clone());
        }
    }

    pub(super) fn tick_toast(&mut self) {
        if let Some((at, _)) = &self.toast.current {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast.current = None;
                if self.feedback.severity == FeedbackSeverity::Ok {
                    self.feedback = FeedbackModel::default();
                    self.status_state.message.clear();
                }
            }
        }
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_state.message = text.clone();
        self.feedback = FeedbackModel { text, severity };
        self.feedback_until = None;
    }

    pub(super) fn expire_info_feedback(&mut self) {
        if self.feedback.severity == FeedbackSeverity::Info && !self.feedback.is_empty() {
            self.feedback_until = Some(Instant::now() + Duration::from_secs(3));
        }
    }

    pub(super) fn tick_feedback(&mut self) {
        if self
            .feedback_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.feedback = FeedbackModel::default();
            self.status_state.message.clear();
            self.feedback_until = None;
        }
    }

    /// Operator errors remain visible in chat, feedback, and activity.
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        self.set_feedback(FeedbackSeverity::Error, msg.clone());
        // Replace prior error banners — don't accumulate red clutter in the chat.
        self.banner_state.items.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        self.banner_state.items.push(ChatItem::Banner {
            text: msg.clone(),
            kind: BannerKind::Error,
        });
        self.activity
            .push(ActivityKind::Error, FeedbackSeverity::Error, msg);
        self.busy_state.phase = BusyPhase::Idle;
    }

    /// Drop ephemeral error UI (call on new user turn / Esc).
    pub(super) fn clear_error_chrome(&mut self) {
        self.banner_state.items.retain(|b| {
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
            self.status_state.message.clear();
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
        let (vendor_label, route_label) = self
            .connect
            .profile
            .as_deref()
            .map(|pid| self.vendor_route_labels(pid))
            .unwrap_or((None, None));
        StatusModel {
            status: self.session.active_task.lifecycle,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            effort: self.reasoning_effort.value.to_string(),
            ctx_pct: self.session.context_usage_ratio(),
            busy: self.busy_state.active,
            busy_phase: self.busy_state.phase.clone(),
            connect_profile: self.connect.profile.clone(),
            provider_connected,
            vendor_label,
            route_label,
            web_search_label: self.search_status.label.clone(),
            tools_visible: self.session.list_tools().len(),
            prompt_cache_hits: self.session.token_usage.prompt_cache_hits,
            prompt_cache_writes: self.session.token_usage.prompt_cache_writes,
            repo_name: repo.repo_name.clone(),
            branch: repo.branch.clone(),
            dirty: repo.dirty,
            resource: self.workspace_resource_label(),
            // Workspace secondary metadata only — never overall task lifecycle.
            activity: self.workspace_activity_label(),
            progress_description: self.header_progress_description(),
            failure_category: self.header_failure_category(),
            waiting_detail: self.header_waiting_detail(),
        }
    }

    /// Typed progress for the header while Working. Structured sources only.
    fn header_progress_description(&self) -> Option<String> {
        if !self.busy_state.active {
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
        self.busy_state.phase.progress_description()
    }

    fn header_waiting_detail(&self) -> Option<String> {
        if self.session.pending_hitl().is_some()
            || self.session.active_task.lifecycle == forge_types::TaskLifecycle::Waiting
        {
            return Some("Approval required".into());
        }
        if self.busy_state.active && matches!(self.busy_state.phase, BusyPhase::Connect) {
            return Some("Your input required".into());
        }
        None
    }

    /// Structured failure category for the header. Prefers the in-memory
    /// `turn_failed`/`validation_exhausted` event (present for every failure
    /// in the live process — `finalize_turn_failure` always pushes one).
    ///
    /// `TurnEvent`s are not journaled, so they're gone after `/resume`; the
    /// content-marker fallback below is the only signal that currently
    /// survives a resume. Removing it is tracked as a follow-up for the
    /// persistence phase (extending the durable status event to carry the
    /// failure category), not dropped silently.
    fn header_failure_category(&self) -> Option<String> {
        if self.session.active_task.lifecycle != forge_types::TaskLifecycle::Failed {
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
            None => None,
            Some(WorkspaceView::File(path)) => {
                Some(relative_display(self.session.workspace_root(), path))
            }
            Some(WorkspaceView::Diff(DiffCommandContext::Current)) => Some("Review changes".into()),
            Some(WorkspaceView::Run(id)) => self
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
            Some(WorkspaceView::Diff(DiffCommandContext::Current)) => {
                let total = self.workspace_files.explorer.git_status.status.len();
                (total > 0).then(|| format!("{} of {} changes", self.diff_view.selected + 1, total))
            }
            Some(WorkspaceView::Run(id)) => self
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
                let changes = self.workspace_files.explorer.git_status.status.len();
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
        // Approval is represented by the inline approval card, not a background summary.
        if self.overlay.is_some() || self.session.pending_hitl().is_some() {
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

        let changes = self.workspace_files.explorer.git_status.status.len();
        if changes > 0 {
            let files = if changes == 1 { "file" } else { "files" };
            return Some(ActivitySummaryModel {
                label: format!("{changes} {files} changed"),
                action_label: Some("Review"),
                action: Some(ActivitySummaryAction::ReviewChanges),
                kind: BannerKind::Info,
            });
        }

        if self.busy_state.active && matches!(self.busy_state.phase, BusyPhase::Model) {
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

    /// Only used by tests now — the Enter-key shortcut that used to gate on
    /// this (via `current_workspace_is_conversation()`) was dropped when
    /// conversation stopped being a workspace-nav state. Mouse click on the
    /// summary banner (`mouse.rs`'s `ActivitySummary` hit target) dispatches
    /// `ActivateActivitySummary` directly without checking this first.
    #[cfg(test)]
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
                // Unlike the Alt+-> / `ReviewChanges` command path, entering
                // here skipped `capture_diff_snapshot()`, so a review opened
                // via the inline "N files changed" summary could start
                // already marked stale (with Apply disabled) even though
                // nothing has changed since. Snapshot on entry here too so
                // both paths start from the same fresh baseline.
                self.capture_diff_snapshot();
                self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current));
            }
            None => {}
        }
    }

    /// Read the cached repo header. This is a plain field read: it must never
    /// spawn a subprocess, because callers sit on the render path.
    pub(super) fn repo_header(&self) -> RepoHeaderCache {
        self.repo_header_state.cache.clone()
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
        if self.repo_header_state.cwd != self.runtime.cwd {
            self.repo_header_state.cwd = self.runtime.cwd.clone();
            self.repo_header_state.cache = load_repo_header(&self.repo_header_state.cwd);
            self.repo_header_state.refreshed_at = Instant::now();
            // Drop any refresh still in flight for the previous directory.
            self.repo_header_state.refresh_rx = None;
            return;
        }

        if let Some(rx) = self.repo_header_state.refresh_rx.take() {
            match rx.try_recv() {
                Ok(header) => {
                    self.repo_header_state.cache = header;
                    self.repo_header_state.refreshed_at = Instant::now();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.repo_header_state.refresh_rx = Some(rx);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Keep the last known header; retry after the next TTL window.
                    self.repo_header_state.refreshed_at = Instant::now();
                }
            }
            return;
        }

        if self.repo_header_state.refreshed_at.elapsed() < REPO_HEADER_TTL {
            return;
        }

        let cwd = self.runtime.cwd.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_repo_header(&cwd));
        });
        self.repo_header_state.refresh_rx = Some(rx);
    }

    #[cfg(test)]
    pub(super) fn busy_status_detail(&self) -> Option<String> {
        self.busy_state.active.then(|| {
            let label = if !self.stream.thinking.is_empty() && self.stream.preview.is_empty() {
                "Thinking..."
            } else {
                "Working..."
            };
            let elapsed = self
                .timing
                .started
                .map(|started| started.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            format!(
                "{label} {}",
                crate::conversation::format_elapsed_tenths(elapsed)
            )
        })
    }

    #[allow(dead_code)]
    fn footer_limits(&mut self, provider: &str) -> FooterLimits {
        if let Some(rx) = &self.footer_limits.refresh_rx {
            match rx.try_recv() {
                Ok((provider, limits)) => {
                    self.footer_limits.cache = Some(FooterLimitsCache {
                        provider,
                        fetched_at: Instant::now(),
                        limits,
                    });
                    self.footer_limits.refresh_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.footer_limits.refresh_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if provider != OPENAI_CODEX_PROFILE_ID {
            return FooterLimits::default();
        }

        let (cached_limits, needs_refresh) = match self
            .footer_limits
            .cache
            .as_ref()
            .filter(|cache| cache.provider == provider)
        {
            Some(cache) => (
                Some(cache.limits.clone()),
                cache.fetched_at.elapsed() >= Duration::from_secs(60),
            ),
            None => (None, true),
        };
        if needs_refresh && self.footer_limits.refresh_rx.is_none() {
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
            self.footer_limits.refresh_rx = Some(rx);
        }

        cached_limits.unwrap_or_default()
    }

    pub(super) fn status_report_lines(&self) -> Vec<String> {
        let m = self.refresh_status_model();
        let mut lines = session_chrome_lines(&m);
        let id = self.session.session_id.to_string();
        let short = if id.len() > 8 { &id[..8] } else { &id };
        lines.push(format!("session_id={short}"));
        lines.push(format!("journal={}", self.session.journal_dir().display()));
        lines.push(format!("permission_mode={}", self.permission_mode.label()));
        let usage = self.session.token_usage_report();
        lines.push(format!(
            "context_tokens={} / {}",
            usage.context_tokens_est, usage.context_capacity
        ));
        lines.push(format!("messages={}", usage.message_count));
        lines.push(format!("tool_results={}", usage.tool_message_count));
        if let Some((before, after)) = self.conversation_view.context_reset_snapshot {
            lines.push(format!("fresh_context={before:.0}% → {after:.0}%"));
        }
        let skills = self.session.loaded_skill_names();
        if skills.is_empty() {
            lines.push("skills=none".into());
        } else {
            lines.push(format!("skills={}", skills.join(", ")));
        }
        let mut tools = self.session.list_tools();
        tools.sort();
        if tools.is_empty() {
            lines.push("tools=none".into());
        } else {
            lines.push(format!("tools={}", tools.join(", ")));
        }
        lines.push(format!(
            "session_allows={}",
            self.hitl_session.allowed.len()
        ));
        lines
    }
}

// Free functions moved from `app/mod.rs` per #19.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeSession {
    pub(crate) id: uuid::Uuid,
    pub(crate) modified: SystemTime,
}

pub(crate) fn recent_resume_sessions(
    dir: &std::path::Path,
    current: uuid::Uuid,
    limit: usize,
) -> io::Result<Vec<ResumeSession>> {
    let mut sessions = Vec::new();
    if !dir.is_dir() {
        return Ok(sessions);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
        else {
            continue;
        };
        if id == current {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        sessions.push(ResumeSession { id, modified });
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
    sessions.truncate(limit);
    Ok(sessions)
}

/// Build the same compact session rows used by the in-app `/resume` picker.
pub async fn resume_session_items(
    dir: &std::path::Path,
    limit: usize,
) -> io::Result<Vec<ResumeSessionItem>> {
    let sessions = recent_resume_sessions(dir, uuid::Uuid::nil(), limit)?;
    let mut items = Vec::with_capacity(sessions.len());
    for session in sessions {
        let timestamp: chrono::DateTime<chrono::Local> = session.modified.into();
        let title = forge_core::session_title_hint(dir, session.id).await;
        items.push(ResumeSessionItem {
            id: session.id.to_string(),
            modified: timestamp.format("%Y-%m-%d %H:%M").to_string(),
            title,
        });
    }
    Ok(items)
}
pub(crate) fn failure_category_label(category: &str) -> String {
    match category {
        "validation_exhausted" => "Tool retries exhausted".into(),
        "no_final_answer" => "Turn incomplete".into(),
        "max_turns" => "Step limit reached".into(),
        other => {
            // Keep only short snake_case categories; never raw payloads.
            let cleaned = other.replace('_', " ");
            if cleaned.chars().count() <= 28
                && cleaned
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' ')
            {
                let mut chars = cleaned.chars();
                match chars.next() {
                    Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                    None => "Failed".into(),
                }
            } else {
                "Failed".into()
            }
        }
    }
}

pub(crate) fn format_exit_token_usage(report: &forge_core::TokenUsageReport) -> String {
    let api = &report.api;
    format!(
        "Token usage: total={} input={} (+ {} cached) output={} (reasoning {})",
        format_with_commas(api.total_api_tokens()),
        format_with_commas(api.prompt_tokens),
        format_with_commas(api.prompt_cache_hits),
        format_with_commas(api.completion_tokens),
        format_with_commas(api.thinking_tokens_est),
    )
}

pub(crate) fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}
pub(crate) fn footer_usage_summary_with_cost(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
) -> String {
    let cost = cost
        .filter(|_| report.api.prompt_tokens > 0 || report.api.completion_tokens > 0)
        .map(|cost| {
            let input = report.api.prompt_tokens as f64 * cost.input / 1_000_000.0;
            let output = report.api.completion_tokens as f64 * cost.output / 1_000_000.0;
            format!(" · ${:.4}", input + output)
        })
        .unwrap_or_default();
    format!(
        "in {} · out {} · total {}{}",
        format_with_commas(report.api.prompt_tokens),
        format_with_commas(report.api.completion_tokens),
        format_with_commas(report.api.total_api_tokens()),
        cost,
    )
}

pub(super) fn footer_limits_from_report(lines: &[String]) -> FooterLimits {
    FooterLimits {
        usage: lines
            .iter()
            .find(|line| line.starts_with("Session limit:"))
            .cloned()
            .unwrap_or_default(),
        weekly_limit: lines
            .iter()
            .find(|line| line.starts_with("Weekly limit:"))
            .cloned()
            .unwrap_or_default(),
        credits: lines
            .iter()
            .find(|line| line.starts_with("Credits:") || line.starts_with("Credit balance:"))
            .cloned()
            .unwrap_or_default(),
    }
}

#[allow(dead_code)]
pub(super) fn footer_usage_summary(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
    limits: &FooterLimits,
) -> crate::widgets::FooterModel {
    crate::widgets::FooterModel {
        usage: limits.usage.clone(),
        weekly_limit: limits.weekly_limit.clone(),
        credits: limits.credits.clone(),
        usage_summary: footer_usage_summary_with_cost(report, cost),
        ..Default::default()
    }
}

/// How long a cached repo header stays fresh before a background refresh starts.
/// Branch and dirty state change on human timescales, not frame timescales.
const REPO_HEADER_TTL: Duration = Duration::from_secs(2);

/// Read the repo header by shelling out to git. Runs on a worker thread only —
/// never call this from the render path.
pub(super) fn load_repo_header(cwd: &Path) -> RepoHeaderCache {
    let repo_name = cwd
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string);

    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false);

    RepoHeaderCache {
        repo_name,
        branch,
        dirty,
    }
}
