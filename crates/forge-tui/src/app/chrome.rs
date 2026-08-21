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
        let event = transcript
            .events()
            .iter()
            .rev()
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
        self.poll_repo_header();
        self.connected_cached();
        self.refresh_progress_state();
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
            .map(|WorkspaceView::File(path)| {
                relative_display(self.session_view.workspace_root(), path)
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

    pub(super) fn status_report_rows(&self) -> Vec<StatusRow> {
        use crate::overlays::thousands;

        let m = self.refresh_status_model();
        let mut rows = session_chrome_rows(&m);

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
