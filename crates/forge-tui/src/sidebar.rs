//! Sidebar panels (TUI-03 / tui-sidebar.md).

use crate::theme;
use forge_core::AgentSession;
use forge_types::{MessageRole, SessionStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone)]
pub struct SidebarModel {
    pub session_id: String,
    pub journal_dir: String,
    pub status: String,
    pub surface: String,
    pub role: String,
    pub provider: String,
    pub model: String,
    pub objective: Option<String>,
    pub files_changed: Option<usize>,
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
    pub activity: Vec<String>,
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

    pub fn from_session_with_activity(session: &AgentSession, activity_lines: &[String]) -> Self {
        let id = session.session_id.to_string();
        let short = if id.len() > 8 { &id[..8] } else { &id };
        let status = match session.status {
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::AwaitingHitl => "awaiting hitl",
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
            objective,
            files_changed: None,
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
            activity: activity_lines.to_vec(),
            session_allows: Vec::new(),
            pending_approval: session.pending_hitl.is_some(),
        }
    }
}

#[allow(dead_code)]
pub struct SidebarWidget<'a> {
    pub model: &'a SidebarModel,
    pub view: InspectorView,
}

impl Widget for SidebarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(theme::border())
            .style(theme::panel_alt());
        let inner = block.inner(area);
        block.render(area, buf);

        Paragraph::new(self.lines(inner.height)).render(inner, buf);
    }
}

impl SidebarWidget<'_> {
    fn lines(&self, height: u16) -> Vec<Line<'static>> {
        let mut lines = vec![self.tabs(), Line::from("")];
        match self.view {
            InspectorView::Task => self.task_lines(&mut lines),
            InspectorView::Context => self.context_lines(&mut lines),
            InspectorView::Runtime => self.runtime_lines(&mut lines),
        }
        lines.truncate(height as usize);
        lines
    }

    fn tabs(&self) -> Line<'static> {
        let tab = |view, label| {
            let style = if self.view == view {
                theme::brand()
            } else {
                theme::dim()
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

    fn task_lines(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled(
            "CURRENT TASK",
            theme::metadata_style(),
        )));
        if self.model.busy || self.model.objective.is_some() {
            lines.push(kv(
                "Objective",
                self.model.objective.as_deref().unwrap_or("Not available"),
            ));
            lines.push(kv("Stage", self.stage()));
            lines.push(kv(
                "Files changed",
                self.model
                    .files_changed
                    .map(|count| count.to_string())
                    .as_deref()
                    .unwrap_or("Not available"),
            ));
            lines.push(kv(
                "Validation",
                self.model.validation.as_deref().unwrap_or("Not available"),
            ));
            lines.push(kv(
                "Elapsed",
                self.model.elapsed.as_deref().unwrap_or("Not available"),
            ));
        } else {
            lines.push(Line::from(Span::styled("No active task", theme::muted())));
            lines.push(kv("Repository", "Not available"));
            lines.push(kv("Changes", "Not available"));
        }
    }

    fn context_lines(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled("CONTEXT", theme::metadata_style())));
        lines.push(kv("Model", present(&self.model.model)));
        lines.push(kv("Provider", present(&self.model.provider)));
        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        lines.push(kv("Window", &format!("{pct:.0}%")));
        lines.push(kv(
            "Context tokens",
            &format!("{} / {}", self.model.ctx_used, self.model.ctx_total),
        ));
        lines.push(kv("Tokens used", &self.model.tokens_used.to_string()));
        if let Some((before, after)) = self.model.context_reset {
            lines.push(kv("Compaction", &format!("{before:.0}% → {after:.0}%")));
        } else {
            lines.push(kv("Compaction", "Not active"));
        }
        lines.push(kv("Offloaded results", "Recent activity"));
        lines.push(kv(
            "Instructions",
            &format!("{} skills", self.model.skills.len()),
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
            &self.model.session_allows.len().to_string(),
        ));
        lines.push(kv("Messages", &self.model.message_count.to_string()));
        lines.push(kv(
            "Tool results",
            &self.model.tool_message_count.to_string(),
        ));
        lines.push(kv("Tools", &self.model.tools.len().to_string()));
        for name in &self.model.tools {
            lines.push(Line::from(Span::styled(
                format!("· {name}"),
                theme::muted(),
            )));
        }
        if !self.model.activity.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Recent", theme::metadata_style())));
            for summary in self.model.activity.iter().rev() {
                let text: String = summary.chars().take(34).collect();
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

fn present(value: &str) -> &str {
    if value.trim().is_empty() {
        "Not available"
    } else {
        value
    }
}

fn kv(label: &'static str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label} "), theme::dim()),
        Span::styled(value.to_string(), theme::text()),
    ])
}

#[allow(dead_code)]
fn status_style(s: &str) -> ratatui::style::Style {
    match s {
        "awaiting hitl" => theme::warn(),
        "failed" => theme::danger(),
        "completed" => theme::ok(),
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
        assert!(!m.tools.is_empty() || m.status == "completed");
        assert!(m.ctx_pct >= 0.0);
        assert!(m.skills.iter().any(|s| s == "inspect"));

        let m = SidebarModel::from_session_with_activity(&s, &["model started".into()]);
        assert_eq!(m.activity, vec!["model started"]);
    }
}
