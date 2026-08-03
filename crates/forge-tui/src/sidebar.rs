//! Sidebar panels (TUI-03 / tui-sidebar.md).

use crate::status_glyph::{status_dot, Status};
use crate::theme;
use forge_core::AgentSession;
use forge_types::{MessageRole, TaskLifecycle};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

/// One entry in the Task tab's "Recent Activity" list — the same feed the
/// bottom panel's Activity tab and the Runtime tab's "Recent" list read
/// from, just filtered to the newest few and colored by outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarActivityEntry {
    pub summary: String,
    pub status: Status,
}

#[derive(Debug, Clone)]
pub struct SidebarModel {
    pub session_id: String,
    pub journal_dir: String,
    pub status: String,
    pub surface: String,
    pub role: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    /// Active oversight level (`PermissionMode::label()`), cycled with Alt+P.
    pub permission_mode: String,
    /// Connect profile (provider route) serving `model`, e.g. `openai`,
    /// `openrouter` — shown only when it disambiguates the active route.
    pub route: Option<String>,
    pub objective: Option<String>,
    pub repo_name: Option<String>,
    pub branch: Option<String>,
    pub files_changed: Option<usize>,
    pub git_status_loading: bool,
    pub git_status_error: bool,
    pub validation: Option<String>,
    pub elapsed: Option<String>,
    pub ctx_pct: f64,
    pub ctx_used: usize,
    pub ctx_total: usize,
    pub tokens_used: u64,
    pub message_count: usize,
    pub tool_message_count: usize,
    pub busy: bool,
    pub step: String,
    pub context_reset: Option<(f64, f64)>,
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub activity: Vec<SidebarActivityEntry>,
    pub session_allows: Vec<String>,
    pub pending_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorView {
    #[default]
    Task,
    Context,
    Runtime,
}

impl InspectorView {
    pub fn next(self) -> Self {
        match self {
            Self::Task => Self::Context,
            Self::Context => Self::Runtime,
            Self::Runtime => Self::Task,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Task => Self::Runtime,
            Self::Context => Self::Task,
            Self::Runtime => Self::Context,
        }
    }
}

impl SidebarModel {
    pub fn from_session(session: &AgentSession) -> Self {
        Self::from_session_with_activity(session, &[])
    }

    pub fn from_session_with_activity(
        session: &AgentSession,
        activity: &[SidebarActivityEntry],
    ) -> Self {
        let id = session.session_id.to_string();
        let short = if id.len() > 8 { &id[..8] } else { &id };
        // Turn lifecycle only — not tool/activity phase names.
        let status = match session.active_task.lifecycle {
            TaskLifecycle::Ready => "Ready",
            TaskLifecycle::Working => "Working",
            TaskLifecycle::Completed => "Completed",
            TaskLifecycle::Failed => "Failed",
            TaskLifecycle::Waiting => "Waiting",
            TaskLifecycle::Cancelled => "Cancelled",
            TaskLifecycle::Interrupted => "Interrupted",
            // `TaskLifecycle` is `#[non_exhaustive]`; label an unrecognised status honestly
            // rather than guessing at a concrete one.
            _ => "Unknown",
        };
        let mut tools = session.list_tools();
        tools.sort();
        let context = session.token_usage_report();
        let objective = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.content.trim().to_string())
            .filter(|content| !content.is_empty());
        Self {
            session_id: short.to_string(),
            journal_dir: session.journal_dir().display().to_string(),
            status: status.into(),
            surface: "tui".into(),
            role: "generator".into(),
            provider: String::new(),
            model: session.active_model.clone(),
            effort: String::new(),
            permission_mode: String::new(),
            route: None,
            objective,
            repo_name: None,
            branch: None,
            files_changed: None,
            git_status_loading: false,
            git_status_error: false,
            validation: None,
            elapsed: None,
            ctx_pct: session.context_usage_ratio(),
            ctx_used: context.context_tokens_est,
            ctx_total: context.context_capacity,
            tokens_used: context.api.total_api_tokens(),
            message_count: context.message_count,
            tool_message_count: context.tool_message_count,
            busy: false,
            step: String::new(),
            context_reset: None,
            skills: session.loaded_skill_names(),
            tools,
            activity: activity.to_vec(),
            session_allows: Vec::new(),
            pending_approval: session.pending_hitl().is_some(),
        }
    }
}

#[allow(dead_code)]
pub struct SidebarWidget<'a> {
    pub model: &'a SidebarModel,
    pub view: InspectorView,
    pub focused: bool,
}

impl Widget for SidebarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = if self.focused {
            " INSPECTOR · NAV "
        } else {
            " INSPECTOR "
        };
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(if self.focused {
                theme::active_panel_border()
            } else {
                theme::inactive_panel_border()
            })
            .title(Span::styled(
                title,
                if self.focused {
                    theme::active_panel_title()
                } else {
                    theme::inactive_panel_title()
                },
            ))
            .style(theme::panel_alt());
        let inner = block.inner(area);
        block.render(area, buf);

        Paragraph::new(self.lines(inner.width, inner.height)).render(inner, buf);
    }
}

impl SidebarWidget<'_> {
    fn lines(&self, width: u16, height: u16) -> Vec<Line<'static>> {
        let mut lines = vec![self.tabs(), Line::from("")];
        match self.view {
            InspectorView::Task => self.task_lines(&mut lines, width),
            InspectorView::Context => self.context_lines(&mut lines),
            InspectorView::Runtime => self.runtime_lines(&mut lines),
        }
        lines.truncate(height as usize);
        lines
    }

    fn tabs(&self) -> Line<'static> {
        let tab = |view, label| {
            let style = if self.view == view {
                theme::active_tab(self.focused)
            } else {
                theme::inactive_tab()
            };
            Span::styled(label, style)
        };
        Line::from(vec![
            tab(InspectorView::Task, "Task"),
            Span::raw(" | "),
            tab(InspectorView::Context, "Context"),
            Span::raw(" | "),
            tab(InspectorView::Runtime, "Runtime"),
        ])
    }

    fn task_lines(&self, lines: &mut Vec<Line<'static>>, width: u16) {
        lines.push(Line::from(Span::styled(
            "CURRENT TASK",
            theme::metadata_style(),
        )));
        if self.model.busy || self.model.objective.is_some() {
            if let Some(obj) = self.model.objective.as_deref() {
                lines.push(kv_aligned("Objective", truncate(obj, 60), TASK_KEY_WIDTH));
            }
            lines.push(kv_aligned("Stage", self.stage(), TASK_KEY_WIDTH));
            lines.push(kv_aligned("Changes", self.changes_label(), TASK_KEY_WIDTH));
            if self.model.validation.is_some() {
                lines.push(kv_aligned(
                    "Validation",
                    self.model.validation.as_deref().unwrap_or("Not run"),
                    TASK_KEY_WIDTH,
                ));
            } else {
                lines.push(kv_aligned("Validation", "Not run", TASK_KEY_WIDTH));
            }
            if let Some(elapsed) = self.model.elapsed.as_deref() {
                lines.push(kv_aligned("Elapsed", elapsed, TASK_KEY_WIDTH));
            }
        } else {
            lines.push(Line::from(Span::styled("No active task", theme::muted())));
            lines.push(kv_aligned(
                "Repository",
                self.repository_label(),
                TASK_KEY_WIDTH,
            ));
            lines.push(kv_aligned("Changes", self.changes_label(), TASK_KEY_WIDTH));
        }
        self.recent_activity_lines(lines, width);
    }

    /// "Recent Activity" — the newest few entries from the same
    /// [`ActivityFeed`](crate::activity::ActivityFeed) the bottom panel's
    /// Activity tab renders, so the inspector stays useful without a second
    /// event-tracking mechanism.
    fn recent_activity_lines(&self, lines: &mut Vec<Line<'static>>, width: u16) {
        if self.model.activity.is_empty() {
            return;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "RECENT ACTIVITY",
            theme::metadata_style(),
        )));
        // Dot + gap take 2 columns; leave the rest for the summary text.
        let max_chars = width.saturating_sub(2) as usize;
        for entry in self.model.activity.iter().rev().take(8) {
            lines.push(Line::from(vec![
                status_dot(entry.status),
                Span::raw(" "),
                Span::styled(truncate(&entry.summary, max_chars), theme::text()),
            ]));
        }
    }

    fn repository_label(&self) -> String {
        repository_label(&self.model.repo_name, &self.model.branch)
    }

    fn changes_label(&self) -> String {
        changes_label(
            self.model.git_status_loading,
            self.model.git_status_error,
            self.model.files_changed,
        )
    }

    fn context_lines(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled("CONTEXT", theme::metadata_style())));
        lines.push(kv("Model", present(&self.model.model)));
        lines.push(kv("Provider", present(&self.model.provider)));
        lines.push(kv("Effort", present(&self.model.effort)));
        lines.push(kv("Mode", present(&self.model.permission_mode)));
        if let Some(route) = self.model.route.as_deref().filter(|r| !r.is_empty()) {
            lines.push(kv("Route", route));
        }
        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        lines.push(kv("Window", format!("{pct:.0}%")));
        lines.push(kv(
            "Context tokens",
            format!("{} / {}", self.model.ctx_used, self.model.ctx_total),
        ));
        lines.push(kv("Tokens used", self.model.tokens_used.to_string()));
        if let Some((before, after)) = self.model.context_reset {
            lines.push(kv("Fresh context", format!("{before:.0}% → {after:.0}%")));
        } else {
            lines.push(kv("Fresh context", "Not active"));
        }
        lines.push(kv("Preserved details", "Recent activity"));
        lines.push(kv(
            "Instructions",
            format!("{} skills", self.model.skills.len()),
        ));
        for name in &self.model.skills {
            lines.push(Line::from(Span::styled(format!("· {name}"), theme::text())));
        }
    }

    fn runtime_lines(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled("RUNTIME", theme::metadata_style())));
        lines.push(kv("Session", &self.model.session_id));
        lines.push(kv("Journal", &self.model.journal_dir));
        lines.push(kv("State", &self.model.status));
        lines.push(kv("Surface", &self.model.surface));
        lines.push(kv("Role", &self.model.role));
        lines.push(kv(
            "Approval",
            if self.model.pending_approval {
                "waiting"
            } else {
                "none"
            },
        ));
        lines.push(kv(
            "Session allows",
            self.model.session_allows.len().to_string(),
        ));
        lines.push(kv("Messages", self.model.message_count.to_string()));
        lines.push(kv(
            "Tool results",
            self.model.tool_message_count.to_string(),
        ));
        lines.push(kv("Tools", self.model.tools.len().to_string()));
        for name in &self.model.tools {
            lines.push(Line::from(Span::styled(
                format!("· {name}"),
                theme::muted(),
            )));
        }
        if !self.model.activity.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Recent", theme::metadata_style())));
            for entry in self.model.activity.iter().rev() {
                let text: String = entry.summary.chars().take(34).collect();
                lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    theme::muted(),
                )));
            }
        }
    }

    fn stage(&self) -> &str {
        if self.model.busy {
            present(&self.model.step)
        } else {
            &self.model.status
        }
    }
}

fn repository_label(repo_name: &Option<String>, branch: &Option<String>) -> String {
    match (repo_name, branch) {
        (Some(repo), Some(branch)) => format!("{repo}/{branch}"),
        (Some(repo), None) => repo.clone(),
        (None, Some(branch)) => branch.clone(),
        (None, None) => "Not available".into(),
    }
}

fn changes_label(loading: bool, error: bool, files_changed: Option<usize>) -> String {
    if loading {
        return "Loading…".into();
    }
    if error {
        return "Unavailable".into();
    }
    match files_changed {
        Some(0) | None => "Clean".into(),
        Some(1) => "1 modified file".into(),
        Some(n) => format!("{n} modified files"),
    }
}

fn present(value: &str) -> &str {
    if value.trim().is_empty() {
        "Not available"
    } else {
        value
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn kv(label: &'static str, value: impl AsRef<str>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label} "), theme::dim()),
        Span::styled(value.as_ref().to_string(), theme::text()),
    ])
}

/// Widest label in the Task tab's key/value block ("Validation", "Repository").
const TASK_KEY_WIDTH: usize = 10;

/// Like [`kv`], but pads the label to a fixed column width so values line up
/// vertically regardless of label length.
fn kv_aligned(label: &'static str, value: impl AsRef<str>, width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<width$} "), theme::dim()),
        Span::styled(value.as_ref().to_string(), theme::text()),
    ])
}

#[allow(dead_code)]
fn status_style(s: &str) -> ratatui::style::Style {
    match s {
        "Waiting" | "Waiting for you" => theme::warn(),
        "Failed" => theme::danger(),
        "Completed" => theme::ok(),
        "Working" => theme::info(),
        "Cancelled" | "Ready" => theme::muted(),
        "Interrupted" => theme::warn(),
        _ => theme::info(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn sidebar_from_session() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge/skills/inspect")).unwrap();
        std::fs::write(dir.path().join(".forge/skills/inspect/SKILL.md"), "inspect").unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(
            LoopConfig {
                max_turns: 3,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,

                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        s.run_user_message("hi").await.unwrap();
        let m = SidebarModel::from_session(&s);
        assert!(!m.session_id.is_empty());
        assert!(!m.tools.is_empty() || m.status == "Completed");
        assert!(m.ctx_pct >= 0.0);
        assert!(m.skills.iter().any(|s| s == "inspect"));

        let m = SidebarModel::from_session_with_activity(
            &s,
            &[SidebarActivityEntry {
                summary: "model started".into(),
                status: Status::Info,
            }],
        );
        assert_eq!(
            m.activity,
            vec![SidebarActivityEntry {
                summary: "model started".into(),
                status: Status::Info,
            }]
        );
    }

    fn model() -> SidebarModel {
        SidebarModel {
            session_id: String::new(),
            journal_dir: String::new(),
            status: String::new(),
            surface: String::new(),
            role: String::new(),
            provider: String::new(),
            model: String::new(),
            effort: String::new(),
            permission_mode: String::new(),
            route: None,
            objective: None,
            repo_name: None,
            branch: None,
            files_changed: None,
            git_status_loading: false,
            git_status_error: false,
            validation: None,
            elapsed: None,
            ctx_pct: 0.0,
            ctx_used: 0,
            ctx_total: 0,
            tokens_used: 0,
            message_count: 0,
            tool_message_count: 0,
            busy: false,
            step: String::new(),
            context_reset: None,
            skills: Vec::new(),
            tools: Vec::new(),
            activity: Vec::new(),
            session_allows: Vec::new(),
            pending_approval: false,
        }
    }

    #[test]
    fn task_lines_hide_unknown_values_when_idle() {
        let mut m = model();
        m.busy = false;
        m.objective = None;
        m.repo_name = Some("forge".into());
        m.branch = Some("main".into());
        m.files_changed = Some(0);
        m.git_status_loading = false;
        m.git_status_error = false;

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let text = render_lines(&widget);
        assert!(text.contains("No active task"), "{text}");
        assert!(text.contains("Repository"), "{text}");
        assert!(text.contains("forge/main"), "{text}");
        assert!(text.contains("Changes"), "{text}");
        assert!(text.contains("Clean"), "{text}");
        assert!(!text.contains("Stage"), "{text}");
        assert!(!text.contains("Validation"), "{text}");
        assert!(!text.contains("Elapsed"), "{text}");
    }

    #[test]
    fn task_lines_show_active_task_details() {
        let mut m = model();
        m.busy = true;
        m.objective = Some("Add source search across files".into());
        m.step = "Implementing".into();
        m.files_changed = Some(2);
        m.elapsed = Some("1.2s".into());

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let text = render_lines(&widget);
        assert!(text.contains("Objective"), "{text}");
        assert!(text.contains("Add source search across files"), "{text}");
        assert!(text.contains("Stage"), "{text}");
        assert!(text.contains("Implementing"), "{text}");
        assert!(text.contains("Changes"), "{text}");
        assert!(text.contains("2 modified files"), "{text}");
        assert!(text.contains("Validation"), "{text}");
        assert!(text.contains("Not run"), "{text}");
        assert!(text.contains("Elapsed"), "{text}");
        assert!(text.contains("1.2s"), "{text}");
    }

    #[test]
    fn task_lines_truncate_long_objective() {
        let mut m = model();
        m.busy = true;
        m.objective = Some("a".repeat(100));

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let text = render_lines(&widget);
        assert!(
            !text.contains(&"a".repeat(100)),
            "long objective should be truncated"
        );
        assert!(text.contains("…"), "{text}");
    }

    #[test]
    fn inspector_view_cycles_forward_and_backward() {
        assert_eq!(InspectorView::Task.next(), InspectorView::Context);
        assert_eq!(InspectorView::Context.next(), InspectorView::Runtime);
        assert_eq!(InspectorView::Runtime.next(), InspectorView::Task);
        assert_eq!(InspectorView::Task.previous(), InspectorView::Runtime);
        assert_eq!(InspectorView::Context.previous(), InspectorView::Task);
        assert_eq!(InspectorView::Runtime.previous(), InspectorView::Context);
    }

    #[test]
    fn context_view_shows_model_tokens_reset_and_skills() {
        let mut m = model();
        m.model = "openai/gpt-5".into();
        m.provider = "native".into();
        m.ctx_pct = 1.42;
        m.ctx_used = 1234;
        m.ctx_total = 5678;
        m.tokens_used = 42;
        m.context_reset = Some((92.0, 18.0));
        m.skills = vec!["rust".into(), "testing".into()];

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Context,
            focused: true,
        };
        let text = render_lines(&widget);
        assert!(text.contains("INSPECTOR"));
        assert!(text.contains("CONTEXT"), "{text}");
        assert!(text.contains("openai/gpt-5"), "{text}");
        assert!(text.contains("100%"), "{text}");
        assert!(text.contains("1234 / 5678"), "{text}");
        assert!(text.contains("42"), "{text}");
        assert!(text.contains("92% → 18%"), "{text}");
        assert!(text.contains("2 skills"), "{text}");
        assert!(text.contains("· rust"), "{text}");
    }

    #[test]
    fn context_view_shows_effort_and_route() {
        let mut m = model();
        m.model = "openai/gpt-5.6".into();
        m.provider = "native".into();
        m.effort = "Extra High".into();
        m.route = Some("openrouter".into());

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Context,
            focused: true,
        };
        let text = render_lines(&widget);
        assert!(text.contains("Extra High"), "{text}");
        assert!(text.contains("openrouter"), "{text}");
    }

    #[test]
    fn context_view_hides_route_line_when_not_connected() {
        let mut m = model();
        m.model = "mock".into();
        m.effort = "Auto".into();
        m.route = None;

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Context,
            focused: true,
        };
        let text = render_lines(&widget);
        assert!(text.contains("Auto"), "{text}");
        assert!(!text.contains("Route"), "{text}");
    }

    #[test]
    fn runtime_view_shows_approval_tools_allows_and_recent_activity() {
        let mut m = model();
        m.session_id = "abcdef12".into();
        m.journal_dir = "/tmp/forge/journal".into();
        m.status = "Waiting".into();
        m.surface = "tui".into();
        m.role = "generator".into();
        m.pending_approval = true;
        m.session_allows = vec!["git status".into(), "read_file".into()];
        m.message_count = 7;
        m.tool_message_count = 3;
        m.tools = vec!["git".into(), "read_file".into()];
        m.activity = vec![
            SidebarActivityEntry {
                summary: "first activity line that is intentionally long".into(),
                status: Status::Info,
            },
            SidebarActivityEntry {
                summary: "latest activity".into(),
                status: Status::Success,
            },
        ];

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Runtime,
            focused: false,
        };
        let text = render_lines(&widget);
        assert!(text.contains("RUNTIME"), "{text}");
        assert!(text.contains("abcdef12"), "{text}");
        assert!(text.contains("Waiting"), "{text}");
        assert!(text.contains("Approval"), "{text}");
        assert!(text.contains("waiting"), "{text}");
        assert!(text.contains("Session allows"), "{text}");
        assert!(text.contains("2"), "{text}");
        assert!(text.contains("Messages"), "{text}");
        assert!(text.contains("Tool results"), "{text}");
        assert!(text.contains("· git"), "{text}");
        assert!(text.contains("Recent"), "{text}");
        assert!(text.contains("latest activity"), "{text}");
    }

    fn line_plain(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn task_lines_show_recent_activity_most_recent_first_with_dots_and_ellipsis() {
        let mut m = model();
        m.busy = true;
        m.objective = Some("ship the sidebar activity feed".into());
        m.activity = vec![
            SidebarActivityEntry {
                summary: "oldest entry".into(),
                status: Status::Info,
            },
            SidebarActivityEntry {
                summary: "a very long activity summary that should not fit and needs truncation"
                    .into(),
                status: Status::Error,
            },
            SidebarActivityEntry {
                summary: "newest entry".into(),
                status: Status::Success,
            },
        ];

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let lines = widget.lines(30, 40);
        let header_idx = lines
            .iter()
            .position(|l| line_plain(l) == "RECENT ACTIVITY")
            .expect("RECENT ACTIVITY header present");
        let first_entry = line_plain(&lines[header_idx + 1]);
        assert!(
            first_entry.contains("newest entry"),
            "most recent entry should render first: {first_entry}"
        );

        let joined = lines.iter().map(line_plain).collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains('…'),
            "long entry should be truncated with an ellipsis:\n{joined}"
        );
        assert!(
            !joined
                .contains("a very long activity summary that should not fit and needs truncation"),
            "long entry should not overflow the panel width untruncated:\n{joined}"
        );
        assert!(
            joined.contains('●'),
            "entries should be prefixed with a status dot:\n{joined}"
        );
    }

    #[test]
    fn task_lines_cap_recent_activity_at_eight_entries() {
        let mut m = model();
        m.busy = true;
        m.activity = (0..12)
            .map(|i| SidebarActivityEntry {
                summary: format!("entry {i}"),
                status: Status::Info,
            })
            .collect();

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let lines = widget.lines(30, 100);
        let dot_lines = lines
            .iter()
            .filter(|l| line_plain(l).starts_with('●'))
            .count();
        assert_eq!(dot_lines, 8);
    }

    #[test]
    fn task_key_value_rows_align_values_in_a_fixed_column() {
        let mut m = model();
        m.busy = true;
        m.objective = Some("short".into());
        m.elapsed = Some("1.2s".into());

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let lines = widget.lines(60, 40);
        let label_widths: Vec<usize> = lines
            .iter()
            .filter_map(|l| {
                let first = l.spans.first()?;
                (l.spans.len() == 2 && first.style == theme::dim())
                    .then(|| first.content.chars().count())
            })
            .collect();
        assert!(
            label_widths.len() >= 3,
            "expected multiple key/value rows: {label_widths:?}"
        );
        assert!(
            label_widths.iter().all(|w| *w == label_widths[0]),
            "key column widths should all match: {label_widths:?}"
        );
    }

    #[test]
    fn task_view_truncates_gracefully_at_minimum_supported_height() {
        let mut m = model();
        m.busy = true;
        m.objective = Some("ship the sidebar activity feed".into());
        m.activity = (0..8)
            .map(|i| SidebarActivityEntry {
                summary: format!("activity {i}"),
                status: Status::Info,
            })
            .collect();

        let widget = SidebarWidget {
            model: &m,
            view: InspectorView::Task,
            focused: false,
        };
        let text = render_lines_sized(&widget, 30, 10);
        assert!(text.contains("CURRENT TASK"), "{text}");
        assert!(text.contains("Objective"), "{text}");
    }

    fn render_lines_sized(widget: &SidebarWidget<'_>, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(
                    SidebarWidget {
                        model: widget.model,
                        view: widget.view,
                        focused: widget.focused,
                    },
                    f.area(),
                );
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn labels_cover_repository_present_status_and_truncation_edges() {
        assert_eq!(repository_label(&Some("forge".into()), &None), "forge");
        assert_eq!(repository_label(&None, &Some("main".into())), "main");
        assert_eq!(repository_label(&None, &None), "Not available");
        assert_eq!(present(""), "Not available");
        assert_eq!(present("  "), "Not available");
        assert_eq!(present("value"), "value");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "abc…");
        assert_eq!(status_style("Waiting"), theme::warn());
        assert_eq!(status_style("Failed"), theme::danger());
        assert_eq!(status_style("Completed"), theme::ok());
        assert_eq!(status_style("Working"), theme::info());
        assert_eq!(status_style("Other"), theme::info());
    }

    #[test]
    fn changes_label_reflects_git_status_states() {
        assert_eq!(changes_label(false, false, None), "Clean");
        assert_eq!(changes_label(false, false, Some(1)), "1 modified file");
        assert_eq!(changes_label(false, false, Some(3)), "3 modified files");
        assert_eq!(changes_label(true, false, Some(3)), "Loading…");
        assert_eq!(changes_label(false, true, Some(3)), "Unavailable");
    }

    fn render_lines(widget: &SidebarWidget<'_>) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(
                    SidebarWidget {
                        model: widget.model,
                        view: widget.view,
                        focused: widget.focused,
                    },
                    f.area(),
                );
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .join("")
    }
}
