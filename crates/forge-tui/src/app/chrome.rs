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
//! `FooterLimits` and `ActivitySummaryModel` live in `types.rs`.

use crate::overlays::StatusRow;
use crate::widgets::session_chrome_rows;

/// Columns a `/status` value may occupy before it is elided.
const STATUS_VALUE_WIDTH: usize = 52;

use super::util::relative_display;
use super::*;

impl TuiApp {
    pub(super) fn poll_supervisor_events(&mut self) {
        let Some(supervisor) = self.supervisor.as_mut() else {
            return;
        };
        let mut events = Vec::new();
        let mut closed = false;
        loop {
            let event = match supervisor.events.try_recv() {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    closed = true;
                    break;
                }
            };
            events.push(event);
        }
        let _ = supervisor;
        if closed {
            self.supervisor = None;
        }
        for event in events {
            match event {
                forge_session::SupervisorEvent::Roster(roster) => {
                    if let Some(supervisor) = self.supervisor.as_mut() {
                        supervisor.snapshots = roster
                            .iter()
                            .map(|snapshot| (snapshot.task.session_id, snapshot.clone()))
                            .collect();
                    }
                    let primary = self
                        .task_chrome
                        .iter()
                        .find(|task| task.session_id == self.session.session_id)
                        .cloned();
                    self.task_chrome = roster
                        .into_iter()
                        .map(|snapshot| TaskChromeItem {
                            session_id: snapshot.task.session_id,
                            slot: snapshot.task.slot,
                            label: snapshot.task.label,
                            branch: snapshot.task.branch,
                            lifecycle: snapshot.session.lifecycle,
                            selected: snapshot.task.session_id == self.selected_task_id,
                            secondary: Some(snapshot.task.turn_state.label().into()),
                            attention: false,
                        })
                        .collect();
                    if let Some(primary) = primary {
                        if !self
                            .task_chrome
                            .iter()
                            .any(|task| task.session_id == primary.session_id)
                        {
                            self.task_chrome.insert(0, primary);
                        }
                    }
                    self.task_strip_selection = self
                        .task_chrome
                        .iter()
                        .position(|task| task.session_id == self.selected_task_id)
                        .unwrap_or(0);
                }
                forge_session::SupervisorEvent::TaskUpdated(snapshot) => {
                    let snapshot = *snapshot;
                    let lifecycle = snapshot.session.lifecycle;
                    if let Some(supervisor) = self.supervisor.as_mut() {
                        supervisor
                            .snapshots
                            .insert(snapshot.task.session_id, snapshot.clone());
                    }
                    if snapshot.task.session_id == self.session.session_id {
                        self.session_view = snapshot.session.clone();
                        self.transcript_view = snapshot.transcript;
                    }
                    if let Some(task) = self
                        .task_chrome
                        .iter_mut()
                        .find(|task| task.session_id == snapshot.task.session_id)
                    {
                        task.lifecycle = lifecycle;
                        task.secondary = Some(snapshot.task.turn_state.label().into());
                    }
                }
                forge_session::SupervisorEvent::Attention {
                    session_id,
                    message,
                    ..
                } => {
                    if let Some(task) = self
                        .task_chrome
                        .iter_mut()
                        .find(|task| task.session_id == session_id)
                    {
                        task.attention = true;
                    }
                    if session_id != self.session.session_id {
                        self.push_toast(message);
                    }
                }
                forge_session::SupervisorEvent::Stream { session_id, event }
                    if session_id == self.session.session_id =>
                {
                    self.apply_supervisor_stream_event(&event);
                }
                forge_session::SupervisorEvent::Selected(Some(session_id)) => {
                    self.task_strip_selection = self
                        .task_chrome
                        .iter()
                        .position(|task| task.session_id == session_id)
                        .unwrap_or(self.task_strip_selection);
                }
                forge_session::SupervisorEvent::Error {
                    session_id,
                    message,
                } => {
                    self.set_feedback(
                        FeedbackSeverity::Error,
                        format!(
                            "task {}: {message}",
                            session_id.map_or_else(|| "unknown".into(), |id| id.to_string())
                        ),
                    );
                }
                forge_session::SupervisorEvent::TrustRequired {
                    operation_id,
                    label,
                    workspace,
                } => {
                    self.overlay = Some(Overlay::TrustTask {
                        operation_id,
                        label,
                        workspace: workspace.display().to_string(),
                    });
                    self.set_feedback(FeedbackSeverity::Warn, "trust required before task can run");
                }
                _ => {}
            }
        }
    }

    fn apply_supervisor_stream_event(&mut self, event: &forge_types::ModelStreamEvent) {
        match event {
            forge_types::ModelStreamEvent::TextDelta { text } => {
                self.stream.preview.push_str(text);
                self.busy_state.start(crate::widgets::BusyPhase::Model);
            }
            forge_types::ModelStreamEvent::ThinkingDelta { text } => {
                self.stream.thinking.push_str(text);
                self.busy_state.start(crate::widgets::BusyPhase::Model);
            }
            forge_types::ModelStreamEvent::ToolCallStart { name, .. } => {
                self.busy_state
                    .set_phase(crate::widgets::BusyPhase::Tool { name: name.clone() });
            }
            forge_types::ModelStreamEvent::Error { message } => {
                self.set_feedback(FeedbackSeverity::Error, message.clone());
            }
            _ => {}
        }
    }

    pub(super) fn push_notice(&mut self, lines: Vec<String>) {
        self.push_notice_with_severity(lines, FeedbackSeverity::Info);
    }

    pub(super) fn push_notice_with_severity(
        &mut self,
        lines: Vec<String>,
        severity: FeedbackSeverity,
    ) {
        self.notice_state.items = lines;
        self.notice_state.until = Some(Instant::now() + Duration::from_secs(7));
        self.set_feedback(severity, self.notice_state.items.join("\n"));
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
        let text = self.toast.show(text);
        self.set_feedback(FeedbackSeverity::Ok, text);
    }

    pub(super) fn tick_toast(&mut self) {
        self.toast.expire(Duration::from_secs(2));
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_state.message = text.clone();
        self.feedback = FeedbackModel { text, severity };
        self.feedback_until = Some(Instant::now() + Duration::from_secs(7));
    }

    pub(super) fn expire_info_feedback(&mut self) {
        if self.feedback.severity == FeedbackSeverity::Info && !self.feedback.is_empty() {
            self.feedback_until = Some(Instant::now() + Duration::from_secs(7));
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
        self.busy_state.set_phase(BusyPhase::Idle);
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

    /// Build a status model outside a frame — `/status`, and tests.
    ///
    /// This captures its own snapshot rather than reusing `session_view`,
    /// which is only refreshed by `draw`. Trusting the last frame's copy here
    /// would report whatever was true when the screen was last painted, which
    /// for a command that runs between frames is not the same thing.
    pub fn refresh_status_model(&self) -> StatusModel {
        self.status_model_from(
            &SessionSnapshot::capture(&self.session),
            &TranscriptSnapshot::capture(&self.session),
            self.is_provider_connected(),
        )
    }

    /// The per-frame form: renders from the snapshot `draw` already captured.
    pub(super) fn refresh_status_model_with_connected(
        &self,
        provider_connected: bool,
    ) -> StatusModel {
        self.status_model_from(
            &self.session_view,
            &self.transcript_view,
            provider_connected,
        )
    }

    fn status_model_from(
        &self,
        session_view: &SessionSnapshot,
        transcript: &TranscriptSnapshot,
        provider_connected: bool,
    ) -> StatusModel {
        let repo = self.repo_header();
        let id = session_view.session_id.to_string();
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
            status: session_view.lifecycle,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            effort: self.reasoning_effort.value.to_string(),
            ctx_pct: session_view.context_usage_ratio,
            busy: self.busy_state.is_active(),
            busy_phase: self.busy_state.phase().clone(),
            connect_profile: self.connect.profile.clone(),
            provider_connected,
            vendor_label,
            route_label,
            web_search_label: self.search_status.label.clone(),
            tools_visible: session_view.tool_count,
            prompt_cache_hits: session_view.prompt_cache_hits,
            prompt_cache_writes: session_view.prompt_cache_writes,
            repo_name: repo.repo_name.clone(),
            branch: repo.branch.clone(),
            dirty: repo.dirty,
            cwd_display: crate::widgets::status::shorten_home_path(&self.runtime.cwd),
            resource: self.workspace_resource_label(),
            activity: None,
            progress_description: self.header_progress_description(),
            failure_category: self.header_failure_category(transcript),
            waiting_detail: self.header_waiting_detail(),
            incomplete_checks: self.header_incomplete_checks(transcript),
        }
    }

    /// Steps that didn't finish on a turn that nonetheless completed.
    ///
    /// Deliberately narrow: only for `Completed`. A turn that actually failed
    /// reports through [`Self::header_failure_category`] instead, so this can
    /// never be used to soften a genuine failure into a footnote.
    fn header_incomplete_checks(&self, transcript: &TranscriptSnapshot) -> Option<String> {
        if self.session_view.lifecycle != forge_types::TaskLifecycle::Completed {
            return None;
        }
        // Events are session-cumulative — only a resume clears them — so an
        // unbounded reverse scan surfaced a crumb from an arbitrarily old turn
        // and kept it on the footer for the rest of the session. The checks
        // event is pushed after that turn's `assistant` answer, so hitting an
        // `assistant` first means the next match belongs to an earlier turn.
        let event = transcript
            .events()
            .iter()
            .rev()
            .take_while(|event| event.kind != "assistant")
            .find(|event| event.kind == "turn_incomplete_checks")?;
        let names = event.detail.trim();
        if names.is_empty() {
            return None;
        }
        // Naming the command is the most useful form, but it shares the footer
        // row with the model/effort/mode identity. A long command pushed that
        // identity out entirely ("OpenAI/gpt-5.6-luna │ Max │ Auto" collapsed
        // to "Op"), which trades one piece of state the user needs for
        // another. Name it only when it is short enough to be free.
        const MAX_NAMED: usize = 24;
        let count = names.split(", ").count();
        Some(match count {
            1 if names.chars().count() <= MAX_NAMED => format!("{names} didn't finish"),
            1 => "1 check didn't finish".to_string(),
            n => format!("{n} checks didn't finish"),
        })
    }

    /// Refresh state that requires I/O. This belongs to the event-loop tick,
    /// never the render path.
    pub(crate) fn tick_render_state(&mut self) {
        if crate::theme::refresh_system() {
            self.render_cache.conversation = None;
        }
        let _ = self.workspace_files.explorer.poll_git();
        // Ordered after `poll_git` so a completed status or patch request is
        // visible to `/diff` on the same tick it lands.
        self.pump_diff_view();
        self.poll_repo_header();
        self.connected_cached();
        self.refresh_progress_state();
        self.stream.advance_reveal(Instant::now());
    }

    fn refresh_progress_state(&mut self) {
        // The event loop ticks every 200 ms to keep input responsive. Avoid a
        // metadata syscall on every tick when nothing is changing; this is a
        // fallback for progress writers outside Forge, not an animation clock.
        const PROGRESS_POLL_INTERVAL: Duration = Duration::from_secs(1);

        let path = self.runtime.cwd.join(".forge/progress.json");
        let now = Instant::now();
        if self.progress_state.path.as_ref() == Some(&path)
            && self
                .progress_state
                .last_checked
                .is_some_and(|last| now.duration_since(last) < PROGRESS_POLL_INTERVAL)
        {
            return;
        }
        self.progress_state.last_checked = Some(now);

        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if self.progress_state.path.as_ref() == Some(&path)
            && modified == self.progress_state.modified
        {
            return;
        }
        self.progress_state.path = Some(path.clone());
        self.progress_state.modified = modified;
        self.progress_state.description = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<ProgressDocument>(&text).ok())
            .and_then(|doc| {
                let step = doc.in_progress.trim().to_string();
                (!step.is_empty()).then_some(step)
            });
    }

    /// Typed progress for the header while Working. Structured sources only.
    fn header_progress_description(&self) -> Option<String> {
        self.busy_state
            .is_active()
            .then(|| {
                self.progress_state
                    .description
                    .clone()
                    .or_else(|| self.busy_state.phase().progress_description())
            })
            .flatten()
    }

    fn header_waiting_detail(&self) -> Option<String> {
        if self.session_view.is_awaiting_approval()
            || self.session_view.lifecycle == forge_types::TaskLifecycle::Waiting
        {
            return Some("Approval required".into());
        }
        if self.busy_state.is_active() && matches!(self.busy_state.phase(), BusyPhase::Connect) {
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
    fn header_failure_category(&self, transcript: &TranscriptSnapshot) -> Option<String> {
        if self.session_view.lifecycle != forge_types::TaskLifecycle::Failed {
            return None;
        }
        // Prefer the latest structured turn_failed event category.
        for event in transcript.events().iter().rev() {
            if event.kind == "turn_failed" || event.kind == "validation_exhausted" {
                let detail = event.detail.as_str();
                let category = detail.split(':').next().unwrap_or(detail).trim();
                return Some(failure_category_label(category));
            }
        }
        for message in transcript.messages().iter().rev() {
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
        self.workspace_navigation
            .current()
            .as_ref()
            .map(|view| match view {
                WorkspaceView::File(path) => {
                    relative_display(self.session_view.workspace_root(), path)
                }
                WorkspaceView::Diff => "Changes".to_string(),
            })
    }

    pub(super) fn activity_summary(&self) -> Option<ActivitySummaryModel> {
        // Review CTA and conversation banner were removed.
        None
    }

    pub(super) fn activity_summary_cache_key(
        &self,
    ) -> Option<(String, Option<&'static str>, BannerKind)> {
        self.activity_summary()
            .map(|summary| (summary.label, summary.action_label, summary.kind))
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
        self.busy_state.is_active().then(|| {
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

    /// Swap the composer's opener for a working prompt once a turn has run.
    ///
    /// The hint was set once at launch and never changed, so `What does this
    /// project do?` was still sitting under a conversation four turns deep —
    /// and a later session opened on a different string entirely. Belongs to
    /// the event-loop tick, not `draw`: rendering must not mutate state.
    pub(super) fn sync_composer_placeholder(&mut self) {
        // Only the started case is claimed here. Before the first turn the
        // placeholder is whatever the launch chose — the opener when there is
        // a workspace to ask about, the generic prompt otherwise — and
        // overriding that would throw away context the launcher had and this
        // does not.
        if self.session.messages.is_empty() {
            return;
        }
        if self.input.hint != crate::app::types::COMPOSER_WORKING {
            self.input.hint = crate::app::types::COMPOSER_WORKING.into();
        }
    }

    pub(super) fn status_report_rows(&self) -> Vec<StatusRow> {
        use crate::overlays::thousands;

        let m = self.refresh_status_model();
        let mut rows = session_chrome_rows(&m);
        if let Some(profile_at) = rows
            .iter()
            .position(|row| matches!(row, StatusRow::Field { label, .. } if label == "Profile"))
        {
            rows.insert(
                profile_at,
                StatusRow::field_with_note(
                    "Thinking",
                    if self.thinking_enabled { "on" } else { "off" },
                    "/thinking to toggle",
                ),
            );
        }

        // Session identity, inserted under the heading `session_chrome_rows`
        // opened, so the group reads as one block.
        let id = self.session_view.session_id.to_string();
        let short = if id.len() > 8 { &id[..8] } else { &id };
        let journal = self.session.journal_dir().display().to_string();
        let at = rows
            .iter()
            .position(|row| matches!(row, StatusRow::Gap))
            .unwrap_or(rows.len());
        rows.splice(
            at..at,
            [
                StatusRow::field("Session", short.to_string()),
                StatusRow::field(
                    "Workspace",
                    crate::path_display::elide_path(
                        &crate::widgets::status::shorten_home_path(&self.runtime.cwd),
                        STATUS_VALUE_WIDTH,
                    ),
                ),
                StatusRow::field(
                    "Journal",
                    crate::path_display::elide_path(&journal, STATUS_VALUE_WIDTH),
                ),
            ],
        );

        let usage = self.session.token_usage_report();
        rows.push(StatusRow::Gap);
        rows.push(StatusRow::Heading("Context".into()));
        rows.push(StatusRow::field_with_note(
            "Used",
            format!(
                "{} / {} tokens",
                thousands(usage.context_tokens_est as u64),
                thousands(usage.context_capacity as u64)
            ),
            format!("{:.1}%", m.ctx_pct * 100.0),
        ));
        rows.push(StatusRow::field(
            "Messages",
            format!(
                "{} · {} tool results",
                usage.message_count, usage.tool_message_count
            ),
        ));
        rows.push(StatusRow::field(
            "Prompt cache",
            format!(
                "{} hits · {} writes",
                m.prompt_cache_hits, m.prompt_cache_writes
            ),
        ));
        if let Some((before, after)) = self.conversation_view.context_reset_snapshot {
            rows.push(StatusRow::field(
                "Fresh context",
                format!("{before:.0}% → {after:.0}%"),
            ));
        }

        rows.push(StatusRow::Gap);
        rows.push(StatusRow::Heading("Capabilities".into()));
        let mut tools = self.session.list_tools();
        tools.sort();
        // `tools` used to name both the count and the list, the same key
        // meaning two different things on two lines. They are separate fields
        // now, and the list is its own section.
        rows.push(StatusRow::field("Tools", m.tools_visible.to_string()));
        let skills = self.session.loaded_skill_names();
        // No skills *list* section: the overlay is height-capped, and the count
        // plus `/skills to browse` already gets you there.
        rows.push(StatusRow::field_with_note(
            "Skills",
            skills.len().to_string(),
            "/skills to browse",
        ));
        rows.push(StatusRow::field(
            "Session allows",
            self.remembered_approval_count().to_string(),
        ));
        if !tools.is_empty() {
            rows.push(StatusRow::Gap);
            rows.push(StatusRow::Section {
                label: "Tools available".into(),
                items: tools,
            });
        }
        rows
    }

    /// `/context` — where the token budget actually goes, broken out by
    /// category. `/status`'s "Used: X / Y tokens" answers "how full am I";
    /// this answers "full of what" for the person debugging a session that
    /// compacted sooner than expected or feels heavier than it should.
    /// Deliberately not folded into the footer or `/status`: it's detail
    /// most turns never need, and inlining it there would cost the row that
    /// is supposed to be scannable at a glance.
    pub(super) fn context_report_rows(&self) -> Vec<StatusRow> {
        use crate::overlays::thousands;

        let usage = self.session.token_usage_report();
        let capacity = usage.context_capacity.max(1) as f64;
        let pct = |n: usize| (n as f64 / capacity) * 100.0;

        let mut rows = vec![
            StatusRow::Heading("Context breakdown".into()),
            StatusRow::field_with_note(
                "System prompt",
                format!("{} tokens", thousands(usage.system_tokens_est as u64)),
                format!("{:.1}%", pct(usage.system_tokens_est)),
            ),
            StatusRow::field_with_note(
                "Tool schemas",
                format!("{} tokens", thousands(usage.tool_schema_tokens_est as u64)),
                format!("{:.1}%", pct(usage.tool_schema_tokens_est)),
            ),
            StatusRow::field_with_note(
                "User messages",
                format!("{} tokens", thousands(usage.user_tokens_est as u64)),
                format!("{:.1}%", pct(usage.user_tokens_est)),
            ),
            StatusRow::field_with_note(
                "Assistant replies",
                format!("{} tokens", thousands(usage.assistant_tokens_est as u64)),
                format!("{:.1}%", pct(usage.assistant_tokens_est)),
            ),
            StatusRow::field_with_note(
                "Tool results",
                format!(
                    "{} tokens ({} msgs)",
                    thousands(usage.tool_tokens_est as u64),
                    usage.tool_message_count
                ),
                format!("{:.1}%", pct(usage.tool_tokens_est)),
            ),
        ];
        if usage.thinking_in_context_est > 0 {
            rows.push(StatusRow::field_with_note(
                "Thinking",
                format!("{} tokens", thousands(usage.thinking_in_context_est as u64)),
                format!("{:.1}%", pct(usage.thinking_in_context_est)),
            ));
        }
        rows.push(StatusRow::Gap);
        rows.push(StatusRow::field_with_note(
            "Total used",
            format!(
                "{} / {} tokens",
                thousands(usage.context_tokens_est as u64),
                thousands(usage.context_capacity as u64)
            ),
            format!("{:.1}% of window", usage.context_pct),
        ));
        rows
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
// ponytail: exercised by tests only for now; no live caller after the footer
// slim-down, wired back up with the upcoming usage-summary display.
#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg(test)]
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

    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--branch"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| {
            let mut lines = text.lines();
            let branch = lines
                .next()
                .and_then(|line| line.strip_prefix("## "))
                .map(|line| line.split("...").next().unwrap_or(line).to_string())
                .filter(|line| !line.is_empty() && !line.starts_with("HEAD "));
            (branch, lines.any(|line| !line.is_empty()))
        });
    let (branch, dirty) = status.unwrap_or_default();

    RepoHeaderCache {
        repo_name,
        branch,
        dirty,
    }
}

impl TuiApp {
    /// Rows a `PageUp`/`PageDown` moves the conversation.
    ///
    /// A page, not a constant. This used to be a hard-coded 5 rows, which on
    /// wrapped prose is about two sentences — paging back through a long
    /// transcript took dozens of presses. Two rows of overlap are kept so the
    /// reader has something to anchor on across the jump.
    pub(crate) fn conversation_page_rows(&self) -> u16 {
        const OVERLAP: u16 = 2;
        const FALLBACK: u16 = 5;
        self.conversation_area
            .map(|area| area.height.saturating_sub(OVERLAP).max(1))
            .unwrap_or(FALLBACK)
    }
}
